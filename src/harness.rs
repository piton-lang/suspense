//! Harness integration: sends a compiled prompt to a local coding harness as a
//! one-off run (`claude -p`, `codex exec`, or `opencode run`, whichever the
//! user picked) in the project directory, streaming what it does as it does
//! it. A run can resume the conversation of an earlier one, so the harness
//! keeps its context between prompts, and a task's run can be fed more
//! messages while it works, where the harness allows.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::agent::{self, Agent};
use crate::attached_image;
use crate::container;
use crate::harness_mentions;
use crate::usage::{PlanLimit, Spend, Tally};

/// The directory the harness in use reads its agentic Markdown and reference
/// files from, which a system prompt's `${HARNESS_DIRECTORY}` stands for.
pub fn directory() -> &'static str {
    agent::current().directory()
}

/// Something the harness did, in the order it happened.
#[derive(Debug, PartialEq)]
pub enum HarnessEvent {
    /// The conversation the run is part of, which a later run can resume.
    Session(String),
    /// A raw line of the harness's output, sent before the events parsed
    /// from it.
    Output(String),
    /// A new block of reply text began.
    TextStarted,
    TextDelta(String),
    ToolStarted {
        id: String,
        name: String,
    },
    /// A tool call's input is known; `summary` is its most telling argument.
    ToolInput {
        id: String,
        summary: String,
    },
    ToolFinished {
        id: String,
        is_error: bool,
    },
    /// A tool call's whole input, from the run or from a subagent it started,
    /// for working out the files it names.
    ToolCalled {
        id: String,
        name: String,
        input: Value,
        subagent: bool,
    },
    /// What a tool call gave back, as text, from the run or from a subagent.
    ToolOutput {
        id: String,
        output: String,
        subagent: bool,
    },
    /// The run is over, with the result of the last message it was sent.
    Finished {
        is_error: bool,
        result: String,
    },
    /// The result of a message sent to a fed run while more are still to be
    /// answered, so the run goes on.
    Answered {
        is_error: bool,
        result: String,
    },
    /// A message was sent to the run while it works: as typed, and as the
    /// harness received it.
    Sent {
        text: String,
        compiled: String,
    },
    Failed(String),
    /// The run started a subagent, which it counts as still at work until
    /// the subagent ends.
    SubagentStarted {
        id: String,
        /// What it was started to do.
        description: String,
        /// Which kind of agent it is, such as Explore or Plan, when known.
        kind: Option<String>,
    },
    /// What a running subagent is doing now.
    SubagentProgress {
        id: String,
        activity: String,
    },
    /// A subagent ended: completed, failed, or stopped.
    SubagentEnded {
        id: String,
        state: SubagentState,
    },
    /// How many tokens the conversation's context holds, as of the model's
    /// latest reply: all it was given, cached or not, and what it wrote.
    Usage {
        context: u64,
    },
    /// Tokens and cost the run reported, counted as its `tally` says. A
    /// figure left `None` wasn't reported.
    Spent {
        spend: Spend,
        tally: Tally,
    },
    /// The plan limits the harness reported, each with the share used so
    /// far; a limit not among them is as it was.
    Limits(Vec<PlanLimit>),
    /// The model the run uses.
    Model(String),
    /// The conversation it was to carry on, by its id, couldn't be: the
    /// harness has no such session. The same prompt goes again at once as a
    /// new conversation, and that one is never to be carried on again.
    NewConversation(String),
    /// The run's container is being prepared: its image built, what the
    /// build prints following as output.
    Preparing,
    /// Its container is ready, and the harness about to start in it.
    Prepared,
    /// Podman can't run its container, and what can be done about it, as
    /// the ContainerEnvironmentScope says; the run then fails saying why.
    PodmanUnavailable(Option<container::MachineAction>),
    /// The harness isn't logged in in its container, and must be before the
    /// run can go; the run then fails saying so.
    LoginNeeded,
}

/// How a subagent ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentState {
    Completed,
    Failed,
    Stopped,
}

/// A conversation an earlier run reported, for a run to carry on.
#[derive(Clone, Debug, PartialEq)]
pub struct Resume {
    pub session: String,
    /// Carries it on as a copy with a session of its own, so runs that carry
    /// on the same conversation at once don't write over each other.
    pub fork: bool,
}

/// Runs the harness once with `prompt`, and `system_prompt` appended to its
/// own system prompt, streaming its events. With `resume`, the run continues
/// that conversation. The run is stopped once the receiver is dropped.
pub fn send(
    prompt: String,
    system_prompt: Option<String>,
    resume: Option<Resume>,
    project_dir: PathBuf,
) -> mpsc::UnboundedReceiver<HarnessEvent> {
    send_with_images(prompt, system_prompt, Vec::new(), resume, project_dir)
}

/// What a run is kept off: `root`, where it may not change anything, which
/// Claude Code is told it may not edit, so the harness knows at once, the
/// application putting back whatever changes there all the same, as
/// [`crate::mode_guard`] says; and `unread`, which Claude Code is told it may
/// not read, so it reads the compiled reference instead. That is only a
/// nudge: a shell command can still read it, and nothing stops the run when
/// one does.
///
/// A run in a container, as `container` says, is kept off nothing this way:
/// it sees only what is mounted into it.
#[derive(Clone, Debug, Default)]
pub struct Protected {
    pub root: Option<PathBuf>,
    pub unread: Option<PathBuf>,
    /// The container the run goes in, if it runs in one rather than on the
    /// host.
    pub container: Option<container::Plan>,
}

/// Runs the harness once as [`send`] does, giving it `images` alongside the
/// prompt, in order, as it takes images (see [`run`]).
pub fn send_with_images(
    prompt: String,
    system_prompt: Option<String>,
    images: Vec<PathBuf>,
    resume: Option<Resume>,
    project_dir: PathBuf,
) -> mpsc::UnboundedReceiver<HarnessEvent> {
    send_kept_off(
        prompt,
        system_prompt,
        images,
        resume,
        project_dir,
        Protected::default(),
    )
}

/// Runs the harness once as [`send_with_images`] does, kept off what
/// `protected` says, as a question is kept off the spec's source.
pub fn send_kept_off(
    prompt: String,
    system_prompt: Option<String>,
    images: Vec<PathBuf>,
    resume: Option<Resume>,
    project_dir: PathBuf,
    protected: Protected,
) -> mpsc::UnboundedReceiver<HarnessEvent> {
    start(
        prompt,
        system_prompt,
        images,
        resume,
        project_dir,
        protected,
        false,
    )
    .events
}

/// A task's run of the harness: its events, where the harness can be fed
/// more while it works, what feeds it, and what stops it.
pub struct Run {
    pub events: mpsc::UnboundedReceiver<HarnessEvent>,
    pub feed: Option<Feed>,
    pub stop: Stop,
}

/// Runs the harness for a task, as [`send`] does, but where the harness can
/// be fed more while it works, as Claude Code can, its prompt is the first of
/// a stream of messages and the run's feed sends it more. Such a run is over
/// once the harness has answered every message sent to it; `codex exec` and
/// `opencode run` read a single prompt, and have no feed. The harness is
/// given `images` alongside the prompt, in order.
pub fn send_task(
    prompt: String,
    system_prompt: Option<String>,
    images: Vec<PathBuf>,
    resume: Option<Resume>,
    project_dir: PathBuf,
    protected: Protected,
) -> Run {
    start(
        prompt,
        system_prompt,
        images,
        resume,
        project_dir,
        protected,
        true,
    )
}

fn start(
    prompt: String,
    system_prompt: Option<String>,
    images: Vec<PathBuf>,
    resume: Option<Resume>,
    project_dir: PathBuf,
    protected: Protected,
    fed: bool,
) -> Run {
    let agent = agent::current();
    let program = program(agent);
    let (tx, rx) = mpsc::unbounded();
    let feed = (fed && agent.can_be_fed()).then(|| Feed::new(tx.clone()));
    let stop = Stop::new(feed.clone());
    // A test's own `podman` goes with the run to its thread.
    #[cfg(test)]
    let podman = container::podman_for_test();
    std::thread::spawn({
        let feed = feed.clone();
        let stop = stop.clone();
        move || {
            #[cfg(test)]
            container::use_podman_for_test(podman);
            let mut resume = resume;
            let result = loop {
                let ended = run(
                    agent,
                    &program,
                    &prompt,
                    system_prompt.as_deref(),
                    &images,
                    resume.as_ref(),
                    &project_dir,
                    &protected,
                    &tx,
                    feed.as_ref(),
                    &stop,
                );
                // A conversation the harness has none of is never failed
                // for, nor tried again: the prompt goes again at once as a
                // new one, as the HarnessIntegrationScope says.
                match ended {
                    Ok(Ended::NoConversation) if !stop.is_stopped() => {
                        let Some(left) = resume.take() else {
                            break Ok(());
                        };
                        tx.unbounded_send(HarnessEvent::NewConversation(left.session))
                            .ok();
                        if let Some(feed) = &feed {
                            feed.lock().messages = Messages::default();
                        }
                    }
                    ended => break ended.map(|_| ()),
                }
            };
            // Nothing more can be sent once the run is over, however it ended.
            if let Some(feed) = &feed {
                feed.end();
            }
            // A run stopped on purpose ended as it was asked to.
            if let Err(err) = result
                && !stop.is_stopped()
            {
                tx.unbounded_send(HarnessEvent::Failed(format!("{err:#}")))
                    .ok();
            }
        }
    });
    Run {
        events: rx,
        feed,
        stop,
    }
}

#[cfg(test)]
thread_local! {
    /// A program each test's runs start in place of the agent's own, so
    /// tests can stand in a harness of their own.
    static TEST_PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Has this test's runs start `program` in place of the agent's harness.
#[cfg(test)]
pub fn use_program_for_test(program: Option<PathBuf>) {
    TEST_PROGRAM.set(program);
}

/// The program a run of `agent` starts.
fn program(agent: Agent) -> PathBuf {
    #[cfg(test)]
    if let Some(program) = TEST_PROGRAM.with_borrow(Clone::clone) {
        return program;
    }
    PathBuf::from(agent.command())
}

/// Stops a run straight away, whatever the harness is in the middle of: its
/// input is closed, and its process, and everything that process started,
/// ended, rather than left to finish its turn. Its events end with what it
/// printed so far, without a result or a failure.
#[derive(Clone)]
pub struct Stop(Arc<Mutex<Stopping>>);

struct Stopping {
    stopped: bool,
    /// The harness's process, once started, until the run is over.
    child: Option<Child>,
    feed: Option<Feed>,
}

impl Stop {
    fn new(feed: Option<Feed>) -> Self {
        Self(Arc::new(Mutex::new(Stopping {
            stopped: false,
            child: None,
            feed,
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Stopping> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stops the run: see [`Stop`]. A run not yet started never starts.
    pub fn stop(&self) {
        let mut stopping = self.lock();
        if std::mem::replace(&mut stopping.stopped, true) {
            return;
        }
        if let Some(feed) = &stopping.feed {
            feed.close();
        }
        if let Some(child) = stopping.child.as_mut() {
            kill(child);
        }
    }

    /// Whether the run was stopped.
    pub fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    /// Keeps the run's process, to be ended once it is stopped; one stopped
    /// already is ended at once.
    fn hold(&self, mut child: Child) {
        let mut stopping = self.lock();
        if stopping.stopped {
            kill(&mut child);
        }
        stopping.child = Some(child);
    }

    /// Waits for the run's process to end, however it ends, reaping it.
    fn wait(&self) -> Result<std::process::ExitStatus> {
        loop {
            if let Some(child) = self.lock().child.as_mut()
                && let Some(status) = child.try_wait()?
            {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The harness's process id, once it has started.
    #[cfg(test)]
    pub fn pid(&self) -> Option<u32> {
        self.lock().child.as_ref().map(Child::id)
    }
}

/// Ends `child`, the leader of its own process group, and everything it
/// started, then reaps it, so no zombie is left.
fn kill(child: &mut Child) {
    // Over and reaped already, its id may since have gone to another.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        crate::process::command("kill")
            .args(["-TERM", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
    #[cfg(windows)]
    {
        crate::process::command("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status()
            .ok();
    }
    child.kill().ok();
    child.wait().ok();
}

/// Sends a fed run more messages while it works: each is written to the
/// harness's standard input, which stays open until the harness has answered
/// every message sent to it, or the run is stopped.
#[derive(Clone)]
pub struct Feed(Arc<Mutex<Feeding>>);

struct Feeding {
    /// The harness's standard input, until the run is over.
    stdin: Option<ChildStdin>,
    /// Where the run's events go, for each message sent to be shown in its
    /// place among them, until the run is over.
    events: Option<mpsc::UnboundedSender<HarnessEvent>>,
    messages: Messages,
}

impl Feed {
    fn new(events: mpsc::UnboundedSender<HarnessEvent>) -> Self {
        Self(Arc::new(Mutex::new(Feeding {
            stdin: None,
            events: Some(events),
            messages: Messages::default(),
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Feeding> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sends the run `compiled`, the message typed as `text`, with `images`
    /// after it, which the harness takes once it finishes what it is doing.
    /// Fails, sending nothing, once the run is over or about to be: once its
    /// input is closed, or when an image can't be read. Blocks while the
    /// message is written.
    pub fn send(&self, text: String, compiled: String, images: &[PathBuf]) -> Result<()> {
        // Read before the run's input is taken, which only writes.
        let images = image_blocks(images)?;
        let mut feeding = self.lock();
        let Some(stdin) = feeding.stdin.as_mut() else {
            bail!("the task is over");
        };
        let written = stdin
            .write_all(user_message(&compiled, &images).as_bytes())
            .and_then(|()| stdin.flush());
        if let Err(err) = written {
            feeding.stdin = None;
            return Err(err).context("could not write to the harness");
        }
        feeding.messages.sent += 1;
        if let Some(events) = &feeding.events {
            events
                .unbounded_send(HarnessEvent::Sent { text, compiled })
                .ok();
        }
        Ok(())
    }

    /// Whether messages can still be sent to the run.
    pub fn is_open(&self) -> bool {
        self.lock().stdin.is_some()
    }

    /// Closes the run's input, as stopping it does: the harness takes nothing
    /// more.
    pub fn close(&self) {
        self.lock().stdin = None;
    }

    /// The run is over: nothing more is sent, and its events end.
    fn end(&self) {
        let mut feeding = self.lock();
        feeding.stdin = None;
        feeding.events = None;
    }

    /// Reads a line the harness printed, `events` parsed from it, closing the
    /// run's input once the harness has answered every message: see
    /// [`Messages::events`].
    fn read(&self, line: &Value, events: Vec<HarnessEvent>) -> Vec<HarnessEvent> {
        let mut feeding = self.lock();
        let last = feeding.messages.read(line);
        if last == Some(true) {
            // Closed as the last result is read, under the lock, so a message
            // sent from now on fails rather than going unanswered.
            feeding.stdin = None;
        }
        Messages::relabel(last, events)
    }
}

#[cfg(test)]
impl Feed {
    /// A feed for a run that is `cat`, whose input goes nowhere, and the
    /// events the feed sends.
    pub fn for_test() -> (Self, mpsc::UnboundedReceiver<HarnessEvent>) {
        let (tx, rx) = mpsc::unbounded();
        let feed = Self::new(tx);
        let mut cat = crate::process::command("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        feed.lock().stdin = cat.stdin.take();
        (feed, rx)
    }
}

/// A message for a harness reading `--input-format stream-json`: a user
/// message holding `text`, then `images`, image content blocks from
/// [`image_blocks`], as a line of JSON.
fn user_message(text: &str, images: &[Value]) -> String {
    let content: Vec<Value> = std::iter::once(json!({ "type": "text", "text": text }))
        .chain(images.iter().cloned())
        .collect();
    let mut line = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content,
        },
    })
    .to_string();
    line.push('\n');
    line
}

/// The images at `paths` as Claude Code's image content blocks, each
/// base64-encoded with its media type, in order. Fails, naming it, on one
/// that can't be read or isn't an image that can be attached.
fn image_blocks(paths: &[PathBuf]) -> Result<Vec<Value>> {
    use base64::Engine as _;
    paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path)
                .with_context(|| format!("could not read the attached image {}", path.display()))?;
            let media_type = attached_image::media_type(&bytes).with_context(|| {
                format!(
                    "the attached image {} is not a PNG, JPEG, GIF, or WebP image",
                    path.display()
                )
            })?;
            Ok(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                },
            }))
        })
        .collect()
}

/// The arguments giving `agent` the images at `paths`, attached to the
/// prompt: Codex takes an `--image` per image, and OpenCode a `--file` per
/// image. Claude Code takes them in its input instead (see [`image_blocks`]).
/// The Claude Code permission rule denying its file tools anything under
/// `root`: an absolute path is written after a double slash.
fn denied_edits(root: &Path) -> Option<String> {
    denied("Edit", root)
}

/// The rule telling Claude Code it may not read anything under `root`.
fn denied_reads(root: &Path) -> Option<String> {
    denied("Read", root)
}

/// A rule denying `tool` anything under `root`, an absolute path.
fn denied(tool: &str, root: &Path) -> Option<String> {
    let root = root.to_str()?.trim_end_matches('/');
    root.starts_with('/').then(|| format!("{tool}(/{root}/**)"))
}

/// Every rule `protected` gives Claude Code, joined with commas; none when
/// it keeps the run off nothing.
fn denied_rules(protected: &Protected) -> Option<String> {
    let rules: Vec<String> = [
        protected.root.as_deref().and_then(denied_edits),
        protected.unread.as_deref().and_then(denied_reads),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!rules.is_empty()).then(|| rules.join(","))
}

fn image_args(agent: Agent, paths: &[PathBuf]) -> Vec<std::ffi::OsString> {
    let flag = match agent {
        Agent::Claude => return Vec::new(),
        Agent::Codex => "--image",
        Agent::OpenCode => "--file",
    };
    paths
        .iter()
        .flat_map(|path| [flag.into(), path.clone().into_os_string()])
        .collect()
}

/// How many messages a fed run was sent, its prompt the first, how many the
/// harness took in, and how many results it reported, so the result that
/// answers the last of them is known. Claude Code takes a message in once it
/// finishes what it is doing, and says so by replaying it: one taken in while
/// it is still at work joins what it is doing, and is answered by the same
/// result.
#[derive(Clone, Debug, PartialEq)]
pub struct Messages {
    sent: usize,
    taken: usize,
    results: usize,
    /// The background tasks started and not yet ended, by id, whatever
    /// their kind: subagents, shell commands run in the background, or any
    /// other. While any is going the run goes on, however many results it
    /// has reported: the harness answers again once they end.
    background: std::collections::HashSet<String>,
}

impl Default for Messages {
    fn default() -> Self {
        Self {
            sent: 1,
            taken: 0,
            results: 0,
            background: Default::default(),
        }
    }
}

impl Messages {
    /// Another message was sent.
    pub fn sent(&mut self) {
        self.sent += 1;
    }

    /// Reads a line of Claude Code's output. For a result, returns whether it
    /// answers every message sent: every one was taken in before it, or
    /// there is a result for each (should a harness not replay what it takes).
    pub fn read(&mut self, line: &Value) -> Option<bool> {
        let top_level = line
            .get("parent_tool_use_id")
            .is_none_or(|parent| parent.is_null());
        match str_at(line, "/type").as_deref() {
            Some("user")
                if top_level && line.get("isReplay").and_then(Value::as_bool) == Some(true) =>
            {
                self.taken += 1;
                None
            }
            Some("result") => {
                self.results += 1;
                Some(
                    self.background.is_empty()
                        && (self.taken >= self.sent || self.results >= self.sent),
                )
            }
            Some("system") => {
                // A notification doesn't say what kind of task ended, so it
                // is matched by id against those seen starting.
                if let Some(id) = str_at(line, "/task_id") {
                    match str_at(line, "/subtype").as_deref() {
                        Some("task_started") => {
                            self.background.insert(id);
                        }
                        Some("task_notification") => {
                            self.background.remove(&id);
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// `events`, parsed from `line`, as they are for a fed run: a result that
    /// leaves a message unanswered is an answer, and the run goes on.
    pub fn events(&mut self, line: &Value, events: Vec<HarnessEvent>) -> Vec<HarnessEvent> {
        Self::relabel(self.read(line), events)
    }

    fn relabel(last: Option<bool>, events: Vec<HarnessEvent>) -> Vec<HarnessEvent> {
        if last != Some(false) {
            return events;
        }
        events
            .into_iter()
            .map(|event| match event {
                HarnessEvent::Finished { is_error, result } => {
                    HarnessEvent::Answered { is_error, result }
                }
                event => event,
            })
            .collect()
    }
}

/// How a run ended, short of failing.
enum Ended {
    Done,
    /// The conversation it was to carry on the harness has none of.
    NoConversation,
}

/// Whether the harness said it has no conversation to carry on, as Claude
/// Code's "No conversation found with session ID".
fn no_conversation(said: &str) -> bool {
    let said = said.to_lowercase();
    said.contains("no conversation found")
        || (said.contains("session") && said.contains("not found"))
}

/// What a harness that failed said, in its own words: what it printed on
/// its error output, and how it exited.
fn in_its_own_words(stderr: &str, name: &str, status: std::process::ExitStatus) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("`{name}` exited with {status}, saying nothing about why")
    } else {
        format!("{stderr}\n(`{name}` exited with {status})")
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    agent: Agent,
    program: &Path,
    prompt: &str,
    system_prompt: Option<&str>,
    images: &[PathBuf],
    resume: Option<&Resume>,
    project_dir: &Path,
    protected: &Protected,
    tx: &mpsc::UnboundedSender<HarnessEvent>,
    feed: Option<&Feed>,
    stop: &Stop,
) -> Result<Ended> {
    // A conversation is only carried on by the agent it began with, and only
    // Claude Code can carry on a copy of one; otherwise a new one starts.
    let resume = resume.and_then(|resume| {
        let (began, id) = Agent::of_session(&resume.session);
        (began == agent && (!resume.fork || agent == Agent::Claude))
            .then(|| (id.to_string(), resume.fork))
    });
    // Every image is there to be given before anything is run; one that
    // isn't fails the run, saying so.
    for image in images {
        if !image.is_file() {
            bail!("the attached image {} is missing", image.display());
        }
    }
    // A run in a container needs Podman, its image, and the harness logged
    // in there, before anything is run.
    // What it mounts as the host now stands.
    let contained = protected
        .container
        .as_ref()
        .filter(|_| container::active())
        .map(container::Plan::as_on_host);
    let contained = contained.as_ref();
    let image = match contained {
        Some(plan) => match prepare(plan, tx, stop)? {
            Some(image) => Some(image),
            None => return Ok(Ended::Done),
        },
        None => None,
    };
    let mut command = crate::process::command(program);
    let mut input = prompt_as_given(agent, prompt, system_prompt, resume.is_some());
    match agent {
        Agent::Claude => {
            command.args([
                "-p",
                // The spec: the harness always runs in auto mode.
                "--permission-mode",
                "auto",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                // A resumed conversation otherwise keeps the system prompt of
                // its first run. The project's is the same for every prompt,
                // mode and all, so the prompt cache holds; but one edited since
                // must apply.
                "--system-prompt-snapshot",
                "off",
            ]);
            if let Some((session, fork)) = &resume {
                command.args(["--resume", session]);
                if *fork {
                    command.arg("--fork-session");
                }
            }
            if let Some(system_prompt) = system_prompt {
                command.args(["--append-system-prompt", system_prompt]);
            }
            if let Some(rules) = denied_rules(protected).filter(|_| contained.is_none()) {
                // Joined in one argument, as the flag takes any number of
                // rules and would otherwise take what follows as more.
                command.arg(format!("--disallowedTools={rules}"));
            }
            // Fed, the prompt is the first of a stream of messages, each of
            // which the harness replays as it takes it in. Images go in the
            // same message as the prompt, after it, so a run given any takes
            // its prompt as a message even when it isn't fed.
            if feed.is_some() {
                command.args(["--input-format", "stream-json", "--replay-user-messages"]);
                input = user_message(prompt, &image_blocks(images)?);
            } else if !images.is_empty() {
                command.args(["--input-format", "stream-json"]);
                input = user_message(prompt, &image_blocks(images)?);
            }
        }
        Agent::Codex => {
            command.args(["exec", "--json", "--full-auto", "--skip-git-repo-check"]);
            if resume.is_some() {
                command.arg("resume");
            }
            if !images.is_empty() {
                command.args(image_args(agent, images));
                // `--image` takes any number of files, so the options end
                // before the session and the prompt.
                command.arg("--");
            }
            if let Some((session, _)) = &resume {
                command.arg(session);
            }
            // Read from standard input.
            command.arg("-");
        }
        Agent::OpenCode => {
            command.args(["run", "--format", "json"]);
            if let Some((session, _)) = &resume {
                command.args(["--session", session]);
            }
            command.args(image_args(agent, images));
        }
    }
    let name = invocation(agent);
    if stop.is_stopped() {
        return Ok(Ended::Done);
    }
    // In a container, `podman run` runs the harness with the same arguments.
    if let (Some(plan), Some(image)) = (contained, &image) {
        let args: Vec<std::ffi::OsString> = command.get_args().map(ToOwned::to_owned).collect();
        command = container::podman_command();
        command.args(plan.run_args(image, agent.command(), &args, false));
    }
    command
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so stopping it ends everything it started too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .with_context(|| format!("could not run `{name}`"))?;
    let stdin = child.stdin.take().context("the harness has no stdin");
    let stdout = child.stdout.take().context("the harness has no stdout");
    let stderr = child.stderr.take().context("the harness has no stderr");
    // Held from here on, so stopping the run ends it wherever it is.
    stop.hold(child);
    let (mut stdin, stdout, stderr) = (stdin?, stdout?, stderr?);

    // The prompt goes over stdin so its length and leading characters never
    // collide with command-line parsing; dropping stdin ends it, unless the
    // run is fed, when it stays open for more.
    let written = stdin.write_all(input.as_bytes());
    if stop.is_stopped() {
        return Ok(Ended::Done);
    }
    written?;
    if let Some(feed) = feed {
        stdin.flush()?;
        feed.lock().stdin = Some(stdin);
        // Stopped meanwhile, its input stays closed.
        if stop.is_stopped() {
            feed.close();
        }
    } else {
        drop(stdin);
    }
    // Error output reaches the raw stream as it arrives, and is kept for the
    // error message should the run end without a result.
    let stderr = std::thread::spawn({
        let tx = tx.clone();
        move || {
            let mut text = String::new();
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                tx.unbounded_send(HarnessEvent::Output(line.clone())).ok();
                text.push_str(&line);
                text.push('\n');
            }
            text
        }
    });

    // Whether the harness printed anything of its own.
    let mut harness_spoke = false;
    let mut replying = Replying::default();
    // The run's last result, held back until its process has exited, so the
    // task isn't shown finished while the harness is still at work.
    let mut last = None;
    for line in BufReader::new(stdout).lines() {
        // Stopped, nothing more it prints is taken.
        if stop.is_stopped() {
            break;
        }
        let line = line?;
        // Paths the harness saw in its container are the host's to whatever
        // reads them.
        let line = match contained {
            Some(plan) => plan.map_line(&line),
            None => line,
        };
        if line.trim().is_empty() {
            // Nothing to parse, but the raw stream shows lines as they arrived.
            // Once no one is listening, the run is stopped.
            if tx.unbounded_send(HarnessEvent::Output(line)).is_err() {
                stop.stop();
                return Ok(Ended::Done);
            }
            continue;
        }
        let events = match serde_json::from_str::<Value>(&line) {
            Ok(event) => {
                // The harness itself is running, past anything Podman says.
                harness_spoke = true;
                // The skills and agents this run has are offered as mentions.
                harness_mentions::remember(&event, project_dir);
                let events = parse(&event);
                match feed {
                    Some(feed) => feed.read(&event, events),
                    None => events,
                }
            }
            Err(_) => Vec::new(),
        };
        for event in std::iter::once(HarnessEvent::Output(line)).chain(events) {
            let event = replying.take(event);
            if matches!(event, HarnessEvent::Finished { .. }) {
                last = Some(event);
                continue;
            }
            if tx.unbounded_send(event).is_err() {
                stop.stop();
                return Ok(Ended::Done);
            }
        }
    }

    let status = stop.wait()?;
    // Stopped, it ended as it was asked to; what it left writing to its error
    // output is not waited on.
    if stop.is_stopped() {
        return Ok(Ended::Done);
    }
    let stderr = stderr.join().unwrap_or_default();
    // In a container, Podman failing before the harness started is Podman's
    // failure, said plainly, never the harness's.
    if contained.is_some()
        && !harness_spoke
        && let Some(why) = container::run_failure(status.code(), &stderr)
    {
        bail!("{why}");
    }
    // Carrying on a conversation the harness has none of, it never began:
    // the prompt goes again as a new one.
    let failed_with = match &last {
        Some(HarnessEvent::Finished {
            is_error: true,
            result,
        }) => Some(result.clone()),
        _ => None,
    };
    if resume.is_some()
        && (failed_with.as_deref().is_some_and(no_conversation) || no_conversation(&stderr))
    {
        return Ok(Ended::NoConversation);
    }
    // Its process over, it finishes in the state of its last result; one
    // failed without saying why says what it printed, in its own words.
    if let Some(mut last) = last {
        if let HarnessEvent::Finished {
            is_error: true,
            result,
        } = &mut last
            && result.trim().is_empty()
        {
            *result = in_its_own_words(&stderr, &name, status);
        }
        tx.unbounded_send(last).ok();
    }
    if !replying.finished() {
        // A harness that ends without saying it finished, as OpenCode may,
        // finished with what it last wrote once it exits cleanly.
        if status.success() && agent != Agent::Claude {
            tx.unbounded_send(replying.finish()).ok();
            return Ok(Ended::Done);
        }
        bail!("{}", in_its_own_words(&stderr, &name, status));
    }
    Ok(Ended::Done)
}

/// Gets a run's container ready, as the ContainerEnvironmentScope says:
/// Podman able to run it, its image built, and the harness logged in there.
/// Gives the image to run; none when the run was stopped meanwhile. Fails,
/// having said what can be done about it, when it can't be got ready.
fn prepare(
    plan: &container::Plan,
    tx: &mpsc::UnboundedSender<HarnessEvent>,
    stop: &Stop,
) -> Result<Option<String>> {
    let state = container::podman_state(plan.platform);
    if let Some(why) = container::unavailable(&state, plan.platform) {
        tx.unbounded_send(HarnessEvent::PodmanUnavailable(why.action))
            .ok();
        bail!("{}", why.message);
    }
    // Installed, but unusable: Podman couldn't be accessed, and says why.
    if let Err(why) = container::check_access() {
        bail!("{}", container::access_message(&why));
    }
    let image = container::ensure_image(
        &plan.project_dir,
        &mut || {
            tx.unbounded_send(HarnessEvent::Preparing).ok();
        },
        &mut |line| {
            tx.unbounded_send(HarnessEvent::Output(line)).ok();
        },
    )
    .context("could not prepare the container's image")?;
    if stop.is_stopped() {
        return Ok(None);
    }
    if !logged_in(plan.agent, &image, plan.platform)? {
        tx.unbounded_send(HarnessEvent::LoginNeeded).ok();
        bail!(
            "{} isn't logged in in its container. Log in to run this.",
            plan.agent.label()
        );
    }
    tx.unbounded_send(HarnessEvent::Prepared).ok();
    Ok(Some(image))
}

/// Whether `agent` is logged in in its container, asked of Podman once and
/// remembered once it is, as its credentials stay in its volume.
fn logged_in(agent: Agent, image: &str, platform: container::Platform) -> Result<bool> {
    if KNOWN.lock().unwrap().contains(&agent) {
        return Ok(true);
    }
    let logged_in = container::logged_in(agent, image, platform)?;
    if logged_in {
        KNOWN.lock().unwrap().push(agent);
    }
    Ok(logged_in)
}

/// Forgets that `agent` was logged in in its container, as when it is
/// logged in again, so the next run asks.
pub fn forget_login(agent: Agent) {
    KNOWN.lock().unwrap().retain(|known| *known != agent);
}

/// The harnesses known to be logged in in their containers.
static KNOWN: Mutex<Vec<Agent>> = Mutex::new(Vec::new());

/// How `agent` is run, as an error names it.
fn invocation(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude -p",
        Agent::Codex => "codex exec",
        Agent::OpenCode => "opencode run",
    }
}

/// The prompt `agent` is given, as text, for `prompt` sent with
/// `system_prompt`: the prompt alone for a harness that takes a system prompt
/// of its own, where it goes apart (see [`Agent::takes_system_prompt`]), and
/// otherwise the system prompt ahead of it, marked off from it, on the first
/// prompt of a conversation only, not once `resumed`. What the harness is
/// sent is this, and so is what the raw prompt modal shows.
pub fn prompt_as_given(
    agent: Agent,
    prompt: &str,
    system_prompt: Option<&str>,
    resumed: bool,
) -> String {
    match system_prompt {
        // Ahead of a conversation's first prompt only: it is the same for
        // every prompt, and the conversation already holds it.
        Some(system_prompt) if !agent.takes_system_prompt() && !resumed => {
            with_system_prompt(prompt, system_prompt)
        }
        _ => prompt.to_string(),
    }
}

/// The prompt for a harness that takes no system prompt of its own: the
/// system prompt ahead of it, marked off from it.
fn with_system_prompt(prompt: &str, system_prompt: &str) -> String {
    format!("<system-prompt>\n{system_prompt}\n</system-prompt>\n\n{prompt}")
}

/// A one-off run of the harness in use in `project_dir`, which can only
/// read, glob, and search files, keeps no session where the harness allows,
/// streams its events as JSON, and reads its prompt from standard input.
pub fn one_off_command(project_dir: &Path) -> Command {
    let agent = agent::current();
    let mut command = crate::process::command(agent.command());
    match agent {
        Agent::Claude => command.args([
            "-p",
            "--tools",
            "Read,Glob,Grep",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]),
        Agent::Codex => command.args([
            "exec",
            "--json",
            "--sandbox",
            "read-only",
            "--skip-git-repo-check",
            "-",
        ]),
        Agent::OpenCode => command.args(["run", "--format", "json", "--agent", "plan"]),
    };
    command.current_dir(project_dir);
    command
}

/// Asks the harness in use something quick in `project_dir`, which it
/// answers from `prompt` alone, with no tools where it allows, keeping no
/// session: with a small model at low effort where it can be told one.
/// Returns its reply, trimmed. Blocking.
pub fn ask_quickly(project_dir: &Path, prompt: &str) -> Result<String> {
    let agent = agent::current();
    let mut command = match agent {
        Agent::Claude => {
            let mut command = crate::process::command(agent.command());
            command.args([
                "-p",
                "--model",
                "haiku",
                "--effort",
                "low",
                "--tools",
                "",
                "--no-session-persistence",
            ]);
            command.current_dir(project_dir);
            command
        }
        _ => one_off_command(project_dir),
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run `{}`", invocation(agent)))?;
    let mut stdin = child.stdin.take().context("the harness has no stdin")?;
    let prompt = prompt.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let output = child.wait_with_output()?;
    writer.join().ok();
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if agent == Agent::Claude {
        return Ok(stdout.trim().to_string());
    }
    match reply_of(stdout.lines()) {
        HarnessEvent::Finished {
            is_error: false,
            result,
        } => Ok(result.trim().to_string()),
        HarnessEvent::Finished { result, .. } => bail!("{}", result.trim()),
        _ => unreachable!(),
    }
}

/// How the run whose streamed JSON is `lines` finished: the result it
/// finished with, or, if it never said, what it last wrote.
pub fn reply_of<'a>(lines: impl IntoIterator<Item = &'a str>) -> HarnessEvent {
    let mut replying = Replying::default();
    let mut finished = None;
    for line in lines {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for event in parse(&event) {
            if let event @ HarnessEvent::Finished { .. } = replying.take(event) {
                finished = Some(event);
            }
        }
    }
    finished.unwrap_or_else(|| replying.finish())
}

/// Follows a run's events so it finishes with its reply: a harness that
/// doesn't say what it finished with, as Codex and OpenCode don't, finishes
/// with the text it wrote last.
#[derive(Default)]
pub struct Replying {
    text: String,
    finished: bool,
}

impl Replying {
    /// Passes `event` on, with its reply filled in if it finishes the run.
    pub fn take(&mut self, mut event: HarnessEvent) -> HarnessEvent {
        match &mut event {
            HarnessEvent::TextStarted => self.text.clear(),
            HarnessEvent::TextDelta(delta) => self.text.push_str(delta),
            HarnessEvent::Finished { is_error, result } => {
                self.finished = true;
                if result.is_empty() && !*is_error {
                    *result = self.text.clone();
                }
            }
            _ => {}
        }
        event
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    /// The run finished, with what it last wrote.
    pub fn finish(&mut self) -> HarnessEvent {
        self.take(HarnessEvent::Finished {
            is_error: false,
            result: String::new(),
        })
    }
}

/// Translates one line of a harness's streamed JSON, whichever harness wrote
/// it: OpenCode's events name their session, Codex's types are dotted, and
/// the rest are Claude Code's.
pub fn parse(event: &Value) -> Vec<HarnessEvent> {
    if event.get("sessionID").is_some() {
        return parse_opencode(event);
    }
    match str_at(event, "/type") {
        Some(kind) if kind.contains('.') => parse_codex(event),
        _ => parse_claude(event),
    }
}

/// Translates one line of `codex exec --json` output.
fn parse_codex(event: &Value) -> Vec<HarnessEvent> {
    match str_at(event, "/type").as_deref() {
        Some("thread.started") => str_at(event, "/thread_id")
            .map(|id| HarnessEvent::Session(Agent::Codex.session(&id)))
            .into_iter()
            .collect(),
        // Its reply is its last message, which the run fills in.
        Some("turn.completed") => codex_spent(event)
            .into_iter()
            .chain([HarnessEvent::Finished {
                is_error: false,
                result: String::new(),
            }])
            .collect(),
        Some("turn.failed") => vec![HarnessEvent::Finished {
            is_error: true,
            result: str_at(event, "/error/message").unwrap_or_default(),
        }],
        Some(kind @ ("item.started" | "item.completed")) => {
            codex_item(&event["item"], kind == "item.completed")
        }
        _ => Vec::new(),
    }
}

/// The tokens a Codex turn used, from its `usage`: its input, of which
/// `cached_input_tokens` were read from the cache and
/// `cache_write_input_tokens` written into it, and its output, its reasoning
/// among it. They are its thread's totals so far, earlier runs of it
/// included, since a resumed thread picks its totals up again. Codex reports
/// no cost.
fn codex_spent(event: &Value) -> Option<HarnessEvent> {
    let usage = event.get("usage")?;
    let tokens = |key: &str| usage.get(key).and_then(Value::as_u64);
    let (cached, written) = (
        tokens("cached_input_tokens"),
        tokens("cache_write_input_tokens"),
    );
    let spend = Spend {
        input: tokens("input_tokens").map(|input| {
            input
                .saturating_sub(cached.unwrap_or(0))
                .saturating_sub(written.unwrap_or(0))
        }),
        output: tokens("output_tokens"),
        cache_read: cached,
        cache_write: written,
        cost: None,
    };
    (!spend.is_empty()).then_some(HarnessEvent::Spent {
        spend,
        tally: Tally::Conversation,
    })
}

/// What a Codex item that started or was `done` did.
fn codex_item(item: &Value, done: bool) -> Vec<HarnessEvent> {
    let Some(id) = str_at(item, "/id") else {
        return Vec::new();
    };
    let failed = str_at(item, "/status").as_deref() == Some("failed");
    match str_at(item, "/type").as_deref() {
        Some("agent_message") if done => match str_at(item, "/text") {
            Some(text) => vec![HarnessEvent::TextStarted, HarnessEvent::TextDelta(text)],
            None => Vec::new(),
        },
        Some("command_execution") => {
            let command = str_at(item, "/command").unwrap_or_default();
            let exit_code = item.get("exit_code").and_then(Value::as_i64);
            tool_call(
                id,
                "Bash",
                json!({ "command": command }),
                done.then(|| {
                    (
                        failed || exit_code.is_some_and(|code| code != 0),
                        str_at(item, "/aggregated_output").unwrap_or_default(),
                    )
                }),
            )
        }
        // The files a change touched, each a call of its own.
        Some("file_change") if done => item
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .flat_map(|(ix, change)| {
                let name = match str_at(change, "/kind").as_deref() {
                    Some("add") => "Write",
                    _ => "Edit",
                };
                let id = if ix == 0 {
                    id.clone()
                } else {
                    format!("{id}:{ix}")
                };
                tool_call(
                    id,
                    name,
                    json!({ "file_path": str_at(change, "/path").unwrap_or_default() }),
                    Some((failed, String::new())),
                )
            })
            .collect(),
        Some("mcp_tool_call") => {
            let name = format!(
                "{}:{}",
                str_at(item, "/server").unwrap_or_default(),
                str_at(item, "/tool").unwrap_or_default()
            );
            let output = str_at(item, "/error/message").unwrap_or_else(|| {
                item.pointer("/result/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|block| block.get("text")?.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let input = item.get("arguments").cloned().unwrap_or_else(|| json!({}));
            tool_call(id, &name, input, done.then_some((failed, output)))
        }
        Some("web_search") => tool_call(
            id,
            "WebSearch",
            json!({ "query": str_at(item, "/query").unwrap_or_default() }),
            done.then_some((failed, String::new())),
        ),
        _ => Vec::new(),
    }
}

/// Translates one line of `opencode run --format json` output.
fn parse_opencode(event: &Value) -> Vec<HarnessEvent> {
    let part = &event["part"];
    match str_at(event, "/type").as_deref() {
        Some("step_start") => str_at(event, "/sessionID")
            .map(|id| HarnessEvent::Session(Agent::OpenCode.session(&id)))
            .into_iter()
            .collect(),
        Some("text") => match str_at(part, "/text") {
            Some(text) => vec![HarnessEvent::TextStarted, HarnessEvent::TextDelta(text)],
            None => Vec::new(),
        },
        Some("tool_use") => {
            let Some(id) = str_at(part, "/callID").or_else(|| str_at(part, "/id")) else {
                return Vec::new();
            };
            let tool = str_at(part, "/tool").unwrap_or_default();
            let mut input = part
                .pointer("/state/input")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // Its paths are named as Claude Code's are, so they are found alike.
            if let Some(input) = input.as_object_mut()
                && let Some(path) = input.get("filePath").cloned()
            {
                input.entry("file_path").or_insert(path);
            }
            let finished = match str_at(part, "/state/status").as_deref() {
                Some("completed") => {
                    Some((false, str_at(part, "/state/output").unwrap_or_default()))
                }
                Some("error") => Some((true, str_at(part, "/state/error").unwrap_or_default())),
                _ => None,
            };
            tool_call(id, opencode_tool(&tool), input, finished)
        }
        Some("step_finish") => {
            let reported = |pointer: &str| {
                part.pointer(&format!("/tokens{pointer}"))
                    .and_then(Value::as_u64)
            };
            let tokens = |pointer: &str| reported(pointer).unwrap_or(0);
            let mut events = vec![HarnessEvent::Usage {
                context: tokens("/input")
                    + tokens("/output")
                    + tokens("/cache/read")
                    + tokens("/cache/write"),
            }];
            // Each step's tokens and cost, on top of the steps before it;
            // its reasoning is written, so counts as output.
            let spend = Spend {
                input: reported("/input"),
                output: match (reported("/output"), reported("/reasoning")) {
                    (None, None) => None,
                    (output, reasoning) => Some(output.unwrap_or(0) + reasoning.unwrap_or(0)),
                },
                cache_read: reported("/cache/read"),
                cache_write: reported("/cache/write"),
                cost: part.get("cost").and_then(Value::as_f64),
            };
            if !spend.is_empty() {
                events.push(HarnessEvent::Spent {
                    spend,
                    tally: Tally::More,
                });
            }
            if str_at(part, "/reason").as_deref() == Some("stop") {
                events.push(HarnessEvent::Finished {
                    is_error: false,
                    result: String::new(),
                });
            }
            events
        }
        Some("error") => vec![HarnessEvent::Finished {
            is_error: true,
            result: str_at(event, "/error/data/message")
                .or_else(|| str_at(event, "/error/message"))
                .or_else(|| str_at(event, "/error/name"))
                .unwrap_or_default(),
        }],
        _ => Vec::new(),
    }
}

/// An OpenCode tool, named as Claude Code names it where they are the same.
fn opencode_tool(tool: &str) -> &str {
    match tool {
        "bash" => "Bash",
        "read" => "Read",
        "edit" | "patch" => "Edit",
        "write" => "Write",
        "glob" => "Glob",
        "grep" => "Grep",
        "list" => "LS",
        "webfetch" => "WebFetch",
        "task" => "Task",
        "todowrite" => "TodoWrite",
        other => other,
    }
}

/// A tool call as a harness that reports each call whole reports it: that it
/// started, with its input, and, once `finished`, whether it failed and what
/// it gave back. A call reported again when it finishes is started again,
/// which the reply ignores.
fn tool_call(
    id: String,
    name: &str,
    input: Value,
    finished: Option<(bool, String)>,
) -> Vec<HarnessEvent> {
    let mut events = vec![HarnessEvent::ToolStarted {
        id: id.clone(),
        name: name.to_string(),
    }];
    if let Some(summary) = summarize(&input) {
        events.push(HarnessEvent::ToolInput {
            id: id.clone(),
            summary,
        });
    }
    if let Some((is_error, output)) = finished {
        events.extend([
            HarnessEvent::ToolCalled {
                id: id.clone(),
                name: name.to_string(),
                input,
                subagent: false,
            },
            HarnessEvent::ToolFinished {
                id: id.clone(),
                is_error,
            },
            HarnessEvent::ToolOutput {
                id,
                output,
                subagent: false,
            },
        ]);
    }
    events
}

/// The subagent event one of Claude Code's `system` reports on its tasks
/// stands for. Only a task started of the `local_agent` kind is a subagent,
/// rather than a shell command run in the background; later reports on a
/// task don't say its kind, so they are given for every task, and are for a
/// subagent only where their id is one that started as one.
fn subagent_event(event: &Value) -> Option<HarnessEvent> {
    if str_at(event, "/type").as_deref() != Some("system") {
        return None;
    }
    let id = str_at(event, "/task_id")?;
    match str_at(event, "/subtype").as_deref()? {
        "task_started" if str_at(event, "/task_type").as_deref() == Some("local_agent") => {
            Some(HarnessEvent::SubagentStarted {
                id,
                description: str_at(event, "/description").unwrap_or_default(),
                kind: str_at(event, "/subagent_type"),
            })
        }
        "task_progress" => Some(HarnessEvent::SubagentProgress {
            id,
            activity: str_at(event, "/description").unwrap_or_default(),
        }),
        "task_notification" => Some(HarnessEvent::SubagentEnded {
            id,
            state: match str_at(event, "/status").as_deref() {
                Some("failed") => SubagentState::Failed,
                Some("stopped" | "killed" | "cancelled") => SubagentState::Stopped,
                _ => SubagentState::Completed,
            },
        }),
        _ => None,
    }
}

/// Translates one line of Claude Code's `stream-json` output. Of events from
/// subagents (those with a `parent_tool_use_id`) only their tool calls'
/// inputs and outputs are kept, so the reply reads as one thread.
fn parse_claude(event: &Value) -> Vec<HarnessEvent> {
    let subagent = event
        .get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null());
    let calls = || tool_calls(event, subagent);
    if subagent {
        return match str_at(event, "/type").as_deref() {
            Some("assistant") => calls().collect(),
            Some("user") => tool_outputs(event, subagent).collect(),
            _ => Vec::new(),
        };
    }

    match str_at(event, "/type").as_deref() {
        Some("system") if subagent_event(event).is_some() => {
            subagent_event(event).into_iter().collect()
        }
        Some("system") if str_at(event, "/subtype").as_deref() == Some("init") => {
            str_at(event, "/session_id")
                .map(HarnessEvent::Session)
                .into_iter()
                .chain(str_at(event, "/model").map(HarnessEvent::Model))
                .collect()
        }
        Some("rate_limit_event") => {
            let limits = claude_limits(&event["rate_limit_info"]);
            if limits.is_empty() {
                Vec::new()
            } else {
                vec![HarnessEvent::Limits(limits)]
            }
        }
        Some("stream_event") => {
            let inner = &event["event"];
            match str_at(inner, "/type").as_deref() {
                Some("content_block_start") => {
                    match str_at(inner, "/content_block/type").as_deref() {
                        Some("text") => vec![HarnessEvent::TextStarted],
                        Some("tool_use") => match (
                            str_at(inner, "/content_block/id"),
                            str_at(inner, "/content_block/name"),
                        ) {
                            (Some(id), Some(name)) => vec![HarnessEvent::ToolStarted { id, name }],
                            _ => Vec::new(),
                        },
                        _ => Vec::new(),
                    }
                }
                Some("content_block_delta")
                    if str_at(inner, "/delta/type").as_deref() == Some("text_delta") =>
                {
                    str_at(inner, "/delta/text")
                        .map(HarnessEvent::TextDelta)
                        .into_iter()
                        .collect()
                }
                _ => Vec::new(),
            }
        }
        Some("assistant") => content_blocks(event, "tool_use")
            .filter_map(|block| {
                Some(HarnessEvent::ToolInput {
                    id: str_at(block, "/id")?,
                    summary: summarize(block.get("input")?)?,
                })
            })
            .chain(calls())
            .chain(context_size(event).map(|context| HarnessEvent::Usage { context }))
            .collect(),
        Some("user") => content_blocks(event, "tool_result")
            .filter_map(|block| {
                Some(HarnessEvent::ToolFinished {
                    id: str_at(block, "/tool_use_id")?,
                    is_error: block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .chain(tool_outputs(event, subagent))
            .collect(),
        Some("result") => claude_spent(event)
            .into_iter()
            .chain([HarnessEvent::Finished {
                is_error: event
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                // A failure without a result says why in its errors.
                result: str_at(event, "/result")
                    .filter(|result| !result.is_empty())
                    .or_else(|| {
                        let errors: Vec<&str> = event
                            .get("errors")?
                            .as_array()?
                            .iter()
                            .filter_map(Value::as_str)
                            .collect();
                        (!errors.is_empty()).then(|| errors.join("\n"))
                    })
                    .unwrap_or_default(),
            }])
            .collect(),
        _ => Vec::new(),
    }
}

/// What a Claude Code run has spent, from a `result`. Its `modelUsage`,
/// each model's tokens and cost, subagents' included, and its
/// `total_cost_usd` are the run's totals so far, even for a run fed several
/// messages, each answered with a result of its own; its `usage` is only the
/// last turn's, so is added on, where there is no `modelUsage`.
fn claude_spent(event: &Value) -> Vec<HarnessEvent> {
    let cost = event.get("total_cost_usd").and_then(Value::as_f64);
    if let Some(models) = event.get("modelUsage").and_then(Value::as_object)
        && !models.is_empty()
    {
        let sum = |key: &str| {
            models
                .values()
                .filter_map(|model| model.get(key)?.as_u64())
                .reduce(|a, b| a + b)
        };
        let spend = Spend {
            input: sum("inputTokens"),
            output: sum("outputTokens"),
            cache_read: sum("cacheReadInputTokens"),
            cache_write: sum("cacheCreationInputTokens"),
            cost: cost.or_else(|| {
                models
                    .values()
                    .filter_map(|model| model.get("costUSD")?.as_f64())
                    .reduce(|a, b| a + b)
            }),
        };
        return vec![HarnessEvent::Spent {
            spend,
            tally: Tally::Run,
        }];
    }
    let mut events = Vec::new();
    if let Some(usage) = event.get("usage") {
        let tokens = |key: &str| usage.get(key).and_then(Value::as_u64);
        let spend = Spend {
            input: tokens("input_tokens"),
            output: tokens("output_tokens"),
            cache_read: tokens("cache_read_input_tokens"),
            cache_write: tokens("cache_creation_input_tokens"),
            cost: None,
        };
        if !spend.is_empty() {
            events.push(HarnessEvent::Spent {
                spend,
                tally: Tally::More,
            });
        }
    }
    if cost.is_some() {
        events.push(HarnessEvent::Spent {
            spend: Spend {
                cost,
                ..Spend::default()
            },
            tally: Tally::Run,
        });
    }
    events
}

/// The plan limits in a Claude Code `rate_limit_event`'s `rate_limit_info`:
/// every window in its `unifiedWindows`, each with the share of it used, as
/// a fraction, and when it resets; or, without them, the one limit it names,
/// with its `utilization` where it gives one, and as used up where its
/// status says it was rejected. A limit with no share known is left out.
fn claude_limits(info: &Value) -> Vec<PlanLimit> {
    let resets = |value: &Value| value.get("resetsAt").and_then(Value::as_u64);
    if let Some(windows) = info.get("unifiedWindows").and_then(Value::as_object) {
        let limits: Vec<PlanLimit> = windows
            .iter()
            .filter_map(|(name, window)| {
                Some(PlanLimit {
                    name: name.clone(),
                    used: window.get("utilization")?.as_f64()?,
                    resets_at: resets(window),
                })
            })
            .collect();
        if !limits.is_empty() {
            return limits;
        }
    }
    let Some(name) = str_at(info, "/rateLimitType") else {
        return Vec::new();
    };
    let used = info
        .get("utilization")
        .and_then(Value::as_f64)
        .or_else(|| (str_at(info, "/status").as_deref() == Some("rejected")).then_some(1.));
    used.map(|used| PlanLimit {
        name,
        used,
        resets_at: resets(info),
    })
    .into_iter()
    .collect()
}

/// The whole input of each tool call in an assistant message.
fn tool_calls(event: &Value, subagent: bool) -> impl Iterator<Item = HarnessEvent> + '_ {
    content_blocks(event, "tool_use").filter_map(move |block| {
        Some(HarnessEvent::ToolCalled {
            id: str_at(block, "/id")?,
            name: str_at(block, "/name")?,
            input: block.get("input")?.clone(),
            subagent,
        })
    })
}

/// The text each tool result in a user message gave back, whether a string
/// or a list of blocks.
fn tool_outputs(event: &Value, subagent: bool) -> impl Iterator<Item = HarnessEvent> + '_ {
    content_blocks(event, "tool_result").filter_map(move |block| {
        let output = match block.get("content")? {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return None,
        };
        Some(HarnessEvent::ToolOutput {
            id: str_at(block, "/tool_use_id")?,
            output,
            subagent,
        })
    })
}

/// The tokens in the context as of an assistant message: its input, whether
/// read from or written to the cache or not, and its output.
fn context_size(event: &Value) -> Option<u64> {
    let usage = event.pointer("/message/usage")?;
    let tokens = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(
        tokens("input_tokens")
            + tokens("cache_creation_input_tokens")
            + tokens("cache_read_input_tokens")
            + tokens("output_tokens"),
    )
}

fn str_at(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(str::to_string)
}

fn content_blocks<'a>(event: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> {
    event
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(move |block| block.get("type").and_then(Value::as_str) == Some(kind))
}

/// The most telling argument of a tool call: a command whole, as it is laid
/// out across lines where it is shown, anything else on one line.
fn summarize(input: &Value) -> Option<String> {
    const KEYS: [&str; 7] = [
        "command",
        "file_path",
        "pattern",
        "path",
        "url",
        "query",
        "description",
    ];
    const MAX_CHARS: usize = 120;

    let (key, value) = KEYS
        .iter()
        .find_map(|key| Some((*key, input.get(key)?.as_str()?)))?;
    // A command is shown whole, and a file's path must stay whole to open it.
    if key == "command" || key == "file_path" {
        return Some(value.to_string());
    }
    let line = value.lines().next().unwrap_or_default();
    Some(if line.chars().count() > MAX_CHARS {
        format!("{}…", line.chars().take(MAX_CHARS).collect::<String>())
    } else {
        line.to_string()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::{HarnessEvent, parse};
    use crate::usage::{PlanLimit, Spend, Tally};

    #[test]
    fn claude_code_may_not_edit_a_protected_location() {
        assert_eq!(
            super::denied_edits(std::path::Path::new("/p/spec/")).as_deref(),
            Some("Edit(//p/spec/**)")
        );
        assert_eq!(super::denied_edits(std::path::Path::new("spec")), None);
    }

    /// A run kept off the spec's source is told it may not read it, beside
    /// any location it may not edit; one kept off nothing is told nothing.
    #[test]
    fn claude_code_may_not_read_an_unread_location() {
        use std::path::PathBuf;
        let rules = |root: Option<&str>, unread: Option<&str>| {
            super::denied_rules(&super::Protected {
                root: root.map(PathBuf::from),
                unread: unread.map(PathBuf::from),
                container: None,
            })
        };
        assert_eq!(
            rules(Some("/p/spec"), Some("/p/spec")).as_deref(),
            Some("Edit(//p/spec/**),Read(//p/spec/**)")
        );
        assert_eq!(
            rules(None, Some("/p/spec")).as_deref(),
            Some("Read(//p/spec/**)")
        );
        assert_eq!(rules(None, None), None);
    }

    /// A stand-in harness, written to `dir`: it says which conversation it
    /// is, starts a reply and a tool call, and then works on, leaving a
    /// process of its own running, whose id it writes to `dir/grandchild`.
    /// Each time it runs it adds its arguments as a line of `dir/args`, and
    /// touches `dir/ran`.
    #[cfg(unix)]
    pub(crate) fn slow_harness(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = dir.join("harness.sh");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
echo "$@" >> {args}
echo '{{"type":"system","subtype":"init","session_id":"s1"}}'
echo '{{"type":"stream_event","event":{{"type":"content_block_start","content_block":{{"type":"text"}}}}}}'
echo '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"Working on it."}}}}}}'
echo '{{"type":"stream_event","event":{{"type":"content_block_start","content_block":{{"type":"tool_use","id":"t1","name":"Bash"}}}}}}'
touch {ran}
sleep 30 &
echo $! > {grandchild}
wait
"#,
                args = dir.join("args").display(),
                ran = dir.join("ran").display(),
                grandchild = dir.join("grandchild").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Whether process `pid` is gone, reaped rather than left a zombie.
    #[cfg(unix)]
    pub(crate) fn gone(pid: u32) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => return true,
                // A zombie whose parent is gone too is soon reaped.
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        false
    }

    /// Stopping a run, whichever harness it is, ends it straight away,
    /// mid-turn: its input is closed, its process and everything it started
    /// ended and reaped, and its events end with what it printed so far,
    /// neither failing nor finishing.
    #[cfg(unix)]
    #[test]
    fn stopping_a_run_ends_its_harness_at_once() {
        use futures::StreamExt as _;

        use crate::agent::{self, Agent};

        for agent in [Agent::Claude, Agent::Codex, Agent::OpenCode] {
            let dir = std::env::temp_dir().join(format!(
                "suspense-stop-{}-{}",
                agent.command(),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            agent::set(agent).unwrap();
            super::use_program_for_test(Some(slow_harness(&dir)));
            let super::Run {
                mut events,
                feed,
                stop,
            } = super::send_task(
                "Do it".into(),
                None,
                Vec::new(),
                None,
                dir.clone(),
                Default::default(),
            );
            super::use_program_for_test(None);
            assert_eq!(feed.is_some(), agent == Agent::Claude);

            let mut seen = Vec::new();
            futures::executor::block_on(async {
                while let Some(event) = events.next().await {
                    let started = matches!(event, HarnessEvent::ToolStarted { .. });
                    seen.push(event);
                    if started {
                        break;
                    }
                }
            });
            assert!(seen.contains(&HarnessEvent::Session("s1".into())));
            assert!(seen.contains(&HarnessEvent::TextDelta("Working on it.".into())));
            let grandchild = loop {
                if let Ok(pid) = std::fs::read_to_string(dir.join("grandchild"))
                    && let Ok(pid) = pid.trim().parse::<u32>()
                {
                    break pid;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            let pid = stop.pid().unwrap();

            let stopped = std::time::Instant::now();
            stop.stop();
            assert!(stop.is_stopped());
            assert!(
                std::fs::metadata(format!("/proc/{pid}")).is_err(),
                "{agent:?}: the harness was not ended and reaped at once"
            );
            assert!(gone(grandchild), "{agent:?}: what it started runs on");
            if let Some(feed) = &feed {
                assert!(!feed.is_open(), "the run's input is still open");
            }
            let rest: Vec<_> = futures::executor::block_on(events.collect());
            assert!(
                stopped.elapsed() < std::time::Duration::from_secs(5),
                "{agent:?}: the run did not end straight away"
            );
            assert!(
                rest.iter().all(|event| !matches!(
                    event,
                    HarnessEvent::Failed(_) | HarnessEvent::Finished { .. }
                )),
                "{agent:?}: a stopped run ended with {rest:?}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// What Claude Code reports of usage, in the shapes of a real
    /// `claude -p --output-format stream-json --verbose` run (2.1.281): the
    /// model at its start, its plan limits in a `rate_limit_event`, and a
    /// result's totals so far in `modelUsage` and `total_cost_usd`.
    #[test]
    fn parses_claude_usage() {
        let init = json!({ "type": "system", "subtype": "init", "session_id": "s",
            "model": "claude-haiku-4-5-20251001" });
        assert_eq!(
            parse(&init),
            [
                HarnessEvent::Session("s".into()),
                HarnessEvent::Model("claude-haiku-4-5-20251001".into())
            ]
        );

        let limits = json!({ "type": "rate_limit_event", "session_id": "s", "rate_limit_info": {
            "status": "allowed", "resetsAt": 1790543400, "rateLimitType": "five_hour",
            "overageStatus": "rejected", "isUsingOverage": false,
            "unifiedWindows": {
                "five_hour": { "utilization": 0.1, "resetsAt": 1790543400 },
                "seven_day": { "utilization": 0.42, "resetsAt": 1790708400 } } } });
        assert_eq!(
            parse(&limits),
            [HarnessEvent::Limits(vec![
                PlanLimit {
                    name: "five_hour".into(),
                    used: 0.1,
                    resets_at: Some(1790543400)
                },
                PlanLimit {
                    name: "seven_day".into(),
                    used: 0.42,
                    resets_at: Some(1790708400)
                },
            ])]
        );
        // Without its windows, the one limit it names, where its share is
        // known: given, or used up once rejected.
        let named = |info: serde_json::Value| {
            parse(&json!({ "type": "rate_limit_event", "rate_limit_info": info }))
        };
        assert_eq!(
            named(
                json!({ "status": "allowed_warning", "rateLimitType": "seven_day",
                "utilization": 0.85, "resetsAt": 100 })
            ),
            [HarnessEvent::Limits(vec![PlanLimit {
                name: "seven_day".into(),
                used: 0.85,
                resets_at: Some(100)
            }])]
        );
        assert_eq!(
            named(json!({ "status": "rejected", "rateLimitType": "five_hour" })),
            [HarnessEvent::Limits(vec![PlanLimit {
                name: "five_hour".into(),
                used: 1.,
                resets_at: None
            }])]
        );
        assert_eq!(
            named(json!({ "status": "allowed", "rateLimitType": "five_hour" })),
            []
        );
        assert_eq!(named(json!({})), []);

        // A fed run's second result: `usage` is its last turn's, while
        // `modelUsage` and `total_cost_usd` count the whole run.
        let result = json!({ "type": "result", "subtype": "success", "is_error": false,
            "result": "Bye!", "total_cost_usd": 0.0148501,
            "usage": { "input_tokens": 10, "output_tokens": 61,
                "cache_read_input_tokens": 21679, "cache_creation_input_tokens": 1109 },
            "modelUsage": {
                "claude-haiku-4-5-20251001": { "inputTokens": 20, "outputTokens": 125,
                    "cacheReadInputTokens": 39331, "cacheCreationInputTokens": 5136,
                    "costUSD": 0.0148501, "contextWindow": 200000 } } });
        assert_eq!(
            parse(&result),
            [
                HarnessEvent::Spent {
                    spend: Spend {
                        input: Some(20),
                        output: Some(125),
                        cache_read: Some(39331),
                        cache_write: Some(5136),
                        cost: Some(0.0148501),
                    },
                    tally: Tally::Run,
                },
                HarnessEvent::Finished {
                    is_error: false,
                    result: "Bye!".into()
                }
            ]
        );
        // Without `modelUsage`, the turn's tokens add on, and the cost is the
        // run's so far.
        let result = json!({ "type": "result", "is_error": false, "result": "",
            "total_cost_usd": 0.5, "usage": { "input_tokens": 3, "output_tokens": 4 } });
        assert_eq!(
            &parse(&result)[..2],
            [
                HarnessEvent::Spent {
                    spend: Spend {
                        input: Some(3),
                        output: Some(4),
                        ..Spend::default()
                    },
                    tally: Tally::More,
                },
                HarnessEvent::Spent {
                    spend: Spend {
                        cost: Some(0.5),
                        ..Spend::default()
                    },
                    tally: Tally::Run,
                },
            ]
        );
        // Nothing reported, nothing spent.
        assert_eq!(
            parse(&json!({ "type": "result", "is_error": false, "result": "" })),
            [HarnessEvent::Finished {
                is_error: false,
                result: String::new()
            }]
        );
    }

    /// Codex's turn reports its tokens, of which its cached input was read
    /// from the cache, and no cost; OpenCode's steps report their tokens and
    /// cost, its reasoning counted as output. A turn or step reporting none
    /// spends nothing.
    #[test]
    fn parses_codex_and_opencode_usage() {
        let turn = json!({ "type": "turn.completed", "usage": {
            "input_tokens": 24763, "cached_input_tokens": 24448, "output_tokens": 122 } });
        assert_eq!(
            parse(&turn)[0],
            HarnessEvent::Spent {
                spend: Spend {
                    input: Some(315),
                    output: Some(122),
                    cache_read: Some(24448),
                    ..Spend::default()
                },
                tally: Tally::Conversation,
            }
        );
        // Newer Codex also says what was written into the cache, out of its
        // input, and its reasoning, which its output already counts.
        let turn = json!({ "type": "turn.completed", "usage": {
            "input_tokens": 1000, "cached_input_tokens": 600,
            "cache_write_input_tokens": 300, "output_tokens": 50,
            "reasoning_output_tokens": 20 } });
        assert_eq!(
            parse(&turn)[0],
            HarnessEvent::Spent {
                spend: Spend {
                    input: Some(100),
                    output: Some(50),
                    cache_read: Some(600),
                    cache_write: Some(300),
                    cost: None,
                },
                tally: Tally::Conversation,
            }
        );
        assert_eq!(
            parse(&json!({ "type": "turn.completed" })),
            [HarnessEvent::Finished {
                is_error: false,
                result: String::new()
            }]
        );

        let step = json!({ "type": "step_finish", "sessionID": "ses", "part": {
            "type": "step-finish", "reason": "tool-calls", "cost": 0.0123,
            "tokens": { "input": 50, "output": 7, "reasoning": 3,
                "cache": { "read": 400, "write": 0 } } } });
        assert_eq!(
            parse(&step)[1],
            HarnessEvent::Spent {
                spend: Spend {
                    input: Some(50),
                    output: Some(10),
                    cache_read: Some(400),
                    cache_write: Some(0),
                    cost: Some(0.0123),
                },
                tally: Tally::More,
            }
        );
        let bare = json!({ "type": "step_finish", "sessionID": "ses",
            "part": { "type": "step-finish", "reason": "tool-calls" } });
        assert_eq!(parse(&bare), [HarnessEvent::Usage { context: 0 }]);
    }

    /// A file's path is summarized whole, however long, so it still opens;
    /// other long inputs are cut short.
    #[test]
    fn summaries_keep_file_paths_whole() {
        let path = format!("/project/spec/{}/index.pi", "deep/".repeat(40));
        assert_eq!(
            super::summarize(&json!({ "file_path": path })).as_deref(),
            Some(path.as_str())
        );
        let pattern = "x".repeat(200);
        let cut = super::summarize(&json!({ "pattern": pattern })).unwrap();
        assert!(cut.ends_with('…') && cut.chars().count() == 121, "{cut}");
    }

    /// Codex's items are read as Claude Code's events are: its thread is a
    /// conversation of its own, a command a Bash call reported whole once it
    /// ends, and its messages the reply's text.
    #[test]
    fn parses_codex_events() {
        let lines = [
            json!({ "type": "thread.started", "thread_id": "th" }),
            json!({ "type": "turn.started" }),
            json!({ "type": "item.started", "item": { "id": "i1", "type": "command_execution",
                "command": "ls", "aggregated_output": "", "status": "in_progress" } }),
            json!({ "type": "item.completed", "item": { "id": "i1", "type": "command_execution",
                "command": "ls", "aggregated_output": "a.rs\n", "exit_code": 0, "status": "completed" } }),
            json!({ "type": "item.completed", "item": { "id": "i2", "type": "file_change",
                "changes": [{ "path": "src/a.rs", "kind": "update" }], "status": "completed" } }),
            json!({ "type": "item.completed", "item": { "id": "i3", "type": "agent_message", "text": "Done." } }),
            json!({ "type": "turn.completed", "usage": { "input_tokens": 10, "output_tokens": 2 } }),
        ];
        let events: Vec<HarnessEvent> = lines.iter().flat_map(parse).collect();
        let started = |id: &str, name: &str| HarnessEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
        };
        let input = |id: &str, summary: &str| HarnessEvent::ToolInput {
            id: id.into(),
            summary: summary.into(),
        };
        assert_eq!(
            events,
            [
                HarnessEvent::Session("codex:th".into()),
                started("i1", "Bash"),
                input("i1", "ls"),
                started("i1", "Bash"),
                input("i1", "ls"),
                HarnessEvent::ToolCalled {
                    id: "i1".into(),
                    name: "Bash".into(),
                    input: json!({ "command": "ls" }),
                    subagent: false
                },
                HarnessEvent::ToolFinished {
                    id: "i1".into(),
                    is_error: false
                },
                HarnessEvent::ToolOutput {
                    id: "i1".into(),
                    output: "a.rs\n".into(),
                    subagent: false
                },
                started("i2", "Edit"),
                input("i2", "src/a.rs"),
                HarnessEvent::ToolCalled {
                    id: "i2".into(),
                    name: "Edit".into(),
                    input: json!({ "file_path": "src/a.rs" }),
                    subagent: false
                },
                HarnessEvent::ToolFinished {
                    id: "i2".into(),
                    is_error: false
                },
                HarnessEvent::ToolOutput {
                    id: "i2".into(),
                    output: String::new(),
                    subagent: false
                },
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("Done.".into()),
                HarnessEvent::Spent {
                    spend: Spend {
                        input: Some(10),
                        output: Some(2),
                        ..Spend::default()
                    },
                    tally: Tally::Conversation,
                },
                HarnessEvent::Finished {
                    is_error: false,
                    result: String::new()
                },
            ]
        );
        // The run finishes with its last message.
        let mut replying = super::Replying::default();
        let last = events.into_iter().map(|event| replying.take(event)).last();
        assert_eq!(
            last,
            Some(HarnessEvent::Finished {
                is_error: false,
                result: "Done.".into()
            })
        );
    }

    /// OpenCode's parts are read the same way: its session a conversation of
    /// its own, its tools named as Claude Code's, its paths found alike, and
    /// each step's tokens the context.
    #[test]
    fn parses_opencode_events() {
        let lines = [
            json!({ "type": "step_start", "sessionID": "ses", "part": { "type": "step-start" } }),
            json!({ "type": "tool_use", "sessionID": "ses", "part": { "type": "tool", "tool": "read",
                "callID": "c1", "state": { "status": "completed",
                    "input": { "filePath": "/p/a.rs" }, "output": "fn a() {}" } } }),
            json!({ "type": "text", "sessionID": "ses", "part": { "type": "text", "text": "It's a." } }),
            json!({ "type": "step_finish", "sessionID": "ses", "part": { "type": "step-finish",
                "reason": "stop", "tokens": { "input": 100, "output": 5, "reasoning": 0,
                    "cache": { "read": 1000, "write": 10 } } } }),
        ];
        let events: Vec<HarnessEvent> = lines.iter().flat_map(parse).collect();
        assert_eq!(
            events,
            [
                HarnessEvent::Session("opencode:ses".into()),
                HarnessEvent::ToolStarted {
                    id: "c1".into(),
                    name: "Read".into()
                },
                HarnessEvent::ToolInput {
                    id: "c1".into(),
                    summary: "/p/a.rs".into()
                },
                HarnessEvent::ToolCalled {
                    id: "c1".into(),
                    name: "Read".into(),
                    input: json!({ "filePath": "/p/a.rs", "file_path": "/p/a.rs" }),
                    subagent: false
                },
                HarnessEvent::ToolFinished {
                    id: "c1".into(),
                    is_error: false
                },
                HarnessEvent::ToolOutput {
                    id: "c1".into(),
                    output: "fn a() {}".into(),
                    subagent: false
                },
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("It's a.".into()),
                HarnessEvent::Usage { context: 1115 },
                HarnessEvent::Spent {
                    spend: Spend {
                        input: Some(100),
                        output: Some(5),
                        cache_read: Some(1000),
                        cache_write: Some(10),
                        cost: None,
                    },
                    tally: Tally::More,
                },
                HarnessEvent::Finished {
                    is_error: false,
                    result: String::new()
                },
            ]
        );
    }

    /// A message to a fed run is a user message, one line of JSON, as
    /// `--input-format stream-json` reads them.
    #[test]
    fn messages_are_stream_json_user_messages() {
        let line = super::user_message("Fix \"it\".\nThen test.", &[]);
        assert!(
            line.ends_with('\n') && line.matches('\n').count() == 1,
            "{line}"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": "Fix \"it\".\nThen test." }],
                },
            })
        );
    }

    /// A fed run is over only once the harness has answered every message
    /// sent to it: a message taken in while the harness is at work is
    /// answered by the result of what it was doing, and one taken in after
    /// is answered by a result of its own. Until then, a result is an
    /// answer, and the run goes on.
    /// Claude Code's reports on a subagent become subagent events; its
    /// reports on a shell command run in the background start none.
    #[test]
    fn subagents_are_read_from_task_reports() {
        use super::SubagentState;
        let started = json!({ "type": "system", "subtype": "task_started", "task_id": "a1",
            "description": "Plan the code", "subagent_type": "Plan", "task_type": "local_agent" });
        assert_eq!(
            parse(&started),
            [HarnessEvent::SubagentStarted {
                id: "a1".into(),
                description: "Plan the code".into(),
                kind: Some("Plan".into()),
            }]
        );
        let shell = json!({ "type": "system", "subtype": "task_started", "task_id": "b1",
            "description": "cargo test", "task_type": "local_bash" });
        assert!(parse(&shell).is_empty());
        let progress = json!({ "type": "system", "subtype": "task_progress", "task_id": "a1",
            "description": "Running Read files" });
        assert_eq!(
            parse(&progress),
            [HarnessEvent::SubagentProgress {
                id: "a1".into(),
                activity: "Running Read files".into(),
            }]
        );
        let failed = json!({ "type": "system", "subtype": "task_notification", "task_id": "a1",
            "status": "failed" });
        assert_eq!(
            parse(&failed),
            [HarnessEvent::SubagentEnded {
                id: "a1".into(),
                state: SubagentState::Failed,
            }]
        );
    }

    /// While a background task it started is going, a subagent or a shell
    /// command alike, a fed run goes on past its results: the harness answers
    /// again once the task ends.
    #[test]
    fn a_fed_run_goes_on_while_a_background_task_works() {
        use super::Messages;
        let taken = json!({ "type": "user", "isReplay": true, "parent_tool_use_id": null,
            "message": { "role": "user", "content": [{ "type": "text", "text": "m" }] } });
        let started = json!({ "type": "system", "subtype": "task_started", "task_id": "a1",
            "task_type": "local_agent" });
        let shell = json!({ "type": "system", "subtype": "task_started", "task_id": "b1",
            "task_type": "local_bash" });
        let ended = json!({ "type": "system", "subtype": "task_notification", "task_id": "a1",
            "status": "completed" });
        let shell_ended = json!({ "type": "system", "subtype": "task_notification",
            "task_id": "b1", "status": "killed" });
        let unknown_ended = json!({ "type": "system", "subtype": "task_notification",
            "task_id": "z9", "status": "completed" });
        let result = json!({ "type": "result", "is_error": false, "result": "ok" });
        let mut messages = Messages::default();
        messages.read(&taken);
        messages.read(&started);
        messages.read(&shell);
        assert_eq!(messages.read(&result), Some(false));
        messages.read(&ended);
        // The shell command still holds the run open.
        assert_eq!(messages.read(&result), Some(false));
        // A task never seen starting ends nothing.
        messages.read(&unknown_ended);
        assert_eq!(messages.read(&result), Some(false));
        messages.read(&shell_ended);
        assert_eq!(messages.read(&result), Some(true));
    }

    /// A result while a shell command runs in the background is an answer,
    /// and the run's input stays open, so it can still be fed; the result
    /// after the command's end is its last, and closes it.
    #[test]
    fn a_background_shell_keeps_a_fed_run_open() {
        let (feed, _events) = super::Feed::for_test();
        let taken = json!({ "type": "user", "isReplay": true, "parent_tool_use_id": null,
            "message": { "role": "user", "content": [{ "type": "text", "text": "m" }] } });
        let shell = json!({ "type": "system", "subtype": "task_started", "task_id": "b1",
            "description": "cargo build", "task_type": "local_bash" });
        let ended = json!({ "type": "system", "subtype": "task_notification", "task_id": "b1",
            "status": "completed" });
        let result = json!({ "type": "result", "is_error": false, "result": "ok" });
        feed.read(&taken, parse(&taken));
        // Not a subagent: the panel isn't told of it.
        assert!(feed.read(&shell, parse(&shell)).is_empty());
        assert_eq!(
            feed.read(&result, parse(&result)),
            [HarnessEvent::Answered {
                is_error: false,
                result: "ok".into(),
            }]
        );
        assert!(feed.is_open(), "an answer closed the run's input");
        feed.read(&ended, parse(&ended));
        assert_eq!(
            feed.read(&result, parse(&result)),
            [HarnessEvent::Finished {
                is_error: false,
                result: "ok".into(),
            }]
        );
        assert!(!feed.is_open(), "the last result left the input open");
    }

    /// A run whose last result has come doesn't finish until the harness's
    /// process has exited.
    #[cfg(unix)]
    #[test]
    fn a_run_finishes_once_its_process_exits() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("suspense-exit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let exited = dir.join("exited");
        let script = dir.join("harness.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}}'\n\
                 echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}}'\n\
                 exec >/dev/null\n\
                 sleep 0.5\n\
                 touch {exited}\n",
                exited = exited.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::use_program_for_test(Some(script));
        let super::Run { mut events, .. } = super::send_task(
            "Go.".into(),
            None,
            Vec::new(),
            None,
            dir.clone(),
            Default::default(),
        );
        super::use_program_for_test(None);
        let start = std::time::Instant::now();
        let mut seen = Vec::new();
        loop {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "never finished"
            );
            match events.try_next() {
                Ok(Some(HarnessEvent::Finished { result, .. })) => {
                    assert_eq!(result, "Done.");
                    assert!(exited.exists(), "finished before its process exited");
                    break;
                }
                Ok(Some(event)) => seen.push(event),
                Ok(None) => panic!("the run ended without finishing: {seen:?}"),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fed_run_ends_once_every_message_is_answered() {
        use super::Messages;
        let taken = json!({ "type": "user", "isReplay": true, "parent_tool_use_id": null,
            "message": { "role": "user", "content": [{ "type": "text", "text": "m" }] } });
        let tool_result = json!({ "type": "user", "parent_tool_use_id": null,
            "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1" }] } });
        let result = json!({ "type": "result", "is_error": false, "result": "ok" });
        let finished = |messages: &mut Messages| messages.events(&result, parse(&result));
        let answered = vec![HarnessEvent::Answered {
            is_error: false,
            result: "ok".into(),
        }];
        let last = vec![HarnessEvent::Finished {
            is_error: false,
            result: "ok".into(),
        }];

        // The prompt alone: its result ends the run.
        let mut messages = Messages::default();
        assert_eq!(messages.read(&taken), None);
        assert_eq!(finished(&mut messages), last);

        // A message taken in mid-tool-call joins that turn: one result
        // answers both.
        let mut messages = Messages::default();
        messages.read(&taken);
        messages.sent();
        messages.read(&tool_result);
        messages.read(&taken);
        assert_eq!(finished(&mut messages), last);

        // A message sent as the harness finishes is taken in after its
        // result, and answered by one of its own.
        let mut messages = Messages::default();
        messages.read(&taken);
        messages.sent();
        assert_eq!(finished(&mut messages), answered);
        messages.read(&taken);
        messages.sent();
        assert_eq!(finished(&mut messages), answered);
        messages.read(&taken);
        assert_eq!(finished(&mut messages), last);

        // A harness that doesn't say what it takes in finishes once there is
        // a result for every message.
        let mut messages = Messages::default();
        messages.sent();
        assert_eq!(messages.read(&result), Some(false));
        assert_eq!(messages.read(&result), Some(true));

        // A subagent's messages are its own.
        let mut messages = Messages::default();
        messages.sent();
        messages.read(&json!({ "type": "user", "isReplay": true, "parent_tool_use_id": "t9" }));
        messages.read(&taken);
        assert_eq!(messages.read(&result), Some(false));
    }

    /// A stand-in harness, written to `dir`, that adds its arguments to
    /// `dir/args` and each line of its input to `dir/stdin`, the first line
    /// before it starts; after `messages` lines it answers with a result for
    /// each, and ends.
    #[cfg(unix)]
    pub(crate) fn recording_harness(dir: &std::path::Path, messages: usize) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = dir.join("recording.sh");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
echo "$@" >> {args}
n=0
while [ $n -lt {messages} ] && {{ IFS= read -r line || [ -n "$line" ]; }}; do
  printf '%s\n' "$line" >> {stdin}
  n=$((n + 1))
  if [ $n -eq 1 ]; then
    echo '{{"type":"system","subtype":"init","session_id":"s1"}}'
  fi
done
i=0
while [ $i -lt {messages} ]; do
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"Done."}}'
  i=$((i + 1))
done
"#,
                args = dir.join("args").display(),
                stdin = dir.join("stdin").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// A fresh directory for test `name`, with a PNG and a JPEG in it.
    #[cfg(unix)]
    fn images_dir(name: &str) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
        let dir =
            std::env::temp_dir().join(format!("suspense-harness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, crate::attached_image::tests::png(3, 2, "a")).unwrap();
        let jpeg = dir.join("b.jpg");
        std::fs::write(
            &jpeg,
            b"\xff\xd8\xff\xc0\x00\x11\x08\x00\x04\x00\x05\x03\x01\x22\x00\x02\x11\x01\x03\x11\x01\xff\xd9",
        )
        .unwrap();
        (dir, vec![png, jpeg])
    }

    /// Every event of a run, until it ends.
    fn all_events(
        events: futures::channel::mpsc::UnboundedReceiver<HarnessEvent>,
    ) -> Vec<HarnessEvent> {
        use futures::StreamExt as _;
        futures::executor::block_on(events.collect::<Vec<_>>())
    }

    /// The image content blocks of a stream-json user message: each image's
    /// media type and its bytes, decoded.
    pub(crate) fn image_blocks_of(message: &serde_json::Value) -> Vec<(String, Vec<u8>)> {
        use base64::Engine as _;
        message["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|block| {
                assert_eq!(block["type"], "image");
                assert_eq!(block["source"]["type"], "base64");
                (
                    block["source"]["media_type"].as_str().unwrap().to_string(),
                    base64::engine::general_purpose::STANDARD
                        .decode(block["source"]["data"].as_str().unwrap())
                        .unwrap(),
                )
            })
            .collect()
    }

    /// A message with images holds them after its text, as image content
    /// blocks, base64-encoded with their media type, in order.
    #[cfg(unix)]
    #[test]
    fn messages_carry_images_after_their_text() {
        let (dir, images) = images_dir("blocks");
        let blocks = super::image_blocks(&images).unwrap();
        let line = super::user_message("Look.", &blocks);
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            message["message"]["content"][0],
            json!({ "type": "text", "text": "Look." })
        );
        assert_eq!(
            image_blocks_of(&message),
            [
                ("image/png".to_string(), std::fs::read(&images[0]).unwrap()),
                ("image/jpeg".to_string(), std::fs::read(&images[1]).unwrap()),
            ]
        );
        // A file that isn't an image that can be attached is named.
        let text = dir.join("notes.txt");
        std::fs::write(&text, "hi").unwrap();
        let error = format!("{:#}", super::image_blocks(&[text]).unwrap_err());
        assert!(error.contains("notes.txt"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Claude Code is given a prompt's images in the same stream-json user
    /// message as its text, after it: a question's run, which isn't fed,
    /// takes its input as stream-json for them, and a task's run, which is,
    /// takes them in its first message and in each message fed to it.
    #[cfg(unix)]
    #[test]
    fn claude_is_given_images_in_its_messages() {
        use crate::agent::{self, Agent};
        let (dir, images) = images_dir("claude");
        agent::set(Agent::Claude).unwrap();

        super::use_program_for_test(Some(recording_harness(&dir, 1)));
        let events = super::send_with_images(
            "What is this?".into(),
            None,
            images.clone(),
            None,
            dir.clone(),
        );
        let events = all_events(events);
        super::use_program_for_test(None);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Finished { .. })),
            "{events:?}"
        );
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(args.contains("--input-format stream-json"), "{args}");
        assert!(!args.contains("--replay-user-messages"), "{args}");
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        let message: serde_json::Value =
            serde_json::from_str(stdin.lines().next().unwrap()).unwrap();
        assert_eq!(message["message"]["content"][0]["text"], "What is this?");
        let given = image_blocks_of(&message);
        assert_eq!(given.len(), 2);
        assert_eq!(given[0].1, std::fs::read(&images[0]).unwrap());

        // Without images, nothing changes: the prompt is given as text.
        std::fs::remove_file(dir.join("args")).unwrap();
        super::use_program_for_test(Some(recording_harness(&dir, 1)));
        all_events(super::send("Plain.".into(), None, None, dir.clone()));
        super::use_program_for_test(None);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(!args.contains("--input-format"), "{args}");

        // A task's run, fed a message with an image of its own.
        std::fs::remove_file(dir.join("stdin")).unwrap();
        super::use_program_for_test(Some(recording_harness(&dir, 2)));
        let super::Run { events, feed, .. } = super::send_task(
            "Fix this.".into(),
            None,
            vec![images[0].clone()],
            None,
            dir.clone(),
            Default::default(),
        );
        super::use_program_for_test(None);
        let feed = feed.expect("Claude Code is fed");
        let start = std::time::Instant::now();
        while !feed.is_open() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        feed.send("And this.".into(), "And this.".into(), &images[1..])
            .unwrap();
        let events = all_events(events);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Finished { .. })),
            "{events:?}"
        );
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        let messages: Vec<serde_json::Value> = stdin
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages.len(), 2, "{stdin}");
        assert_eq!(messages[0]["message"]["content"][0]["text"], "Fix this.");
        assert_eq!(
            image_blocks_of(&messages[0]),
            [("image/png".to_string(), std::fs::read(&images[0]).unwrap())]
        );
        assert_eq!(messages[1]["message"]["content"][0]["text"], "And this.");
        assert_eq!(
            image_blocks_of(&messages[1]),
            [("image/jpeg".to_string(), std::fs::read(&images[1]).unwrap())]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Codex is given an `--image` per image, and OpenCode a `--file` per
    /// image, in order, the prompt still over standard input; Codex's
    /// options end before the session and the prompt, since `--image` takes
    /// any number of files. An image that is missing fails the run before
    /// anything is run.
    #[cfg(unix)]
    #[test]
    fn codex_and_opencode_are_given_images_as_arguments() {
        use crate::agent::{self, Agent};
        let (dir, images) = images_dir("args");
        let (a, b) = (images[0].display(), images[1].display());
        let cases = [
            (
                Agent::Codex,
                None,
                format!(
                    "exec --json --full-auto --skip-git-repo-check --image {a} --image {b} -- -"
                ),
            ),
            (
                Agent::Codex,
                Some("codex:t1"),
                format!(
                    "exec --json --full-auto --skip-git-repo-check resume --image {a} --image {b} -- t1 -"
                ),
            ),
            (
                Agent::OpenCode,
                None,
                format!("run --format json --file {a} --file {b}"),
            ),
            (
                Agent::OpenCode,
                Some("opencode:ses_1"),
                format!("run --format json --session ses_1 --file {a} --file {b}"),
            ),
        ];
        for (agent, session, expected) in cases {
            agent::set(agent).unwrap();
            std::fs::remove_file(dir.join("args")).ok();
            std::fs::remove_file(dir.join("stdin")).ok();
            super::use_program_for_test(Some(recording_harness(&dir, 1)));
            let resume = session.map(|session| super::Resume {
                session: session.into(),
                fork: false,
            });
            all_events(super::send_with_images(
                "Look.".into(),
                None,
                images.clone(),
                resume,
                dir.clone(),
            ));
            super::use_program_for_test(None);
            let args = std::fs::read_to_string(dir.join("args")).unwrap();
            assert_eq!(args.trim_end(), expected, "{agent:?} {session:?}");
            let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
            assert_eq!(stdin.trim_end(), "Look.", "{agent:?}");
        }

        // Missing, nothing runs.
        for agent in Agent::ALL {
            agent::set(agent).unwrap();
            std::fs::remove_file(dir.join("args")).ok();
            super::use_program_for_test(Some(recording_harness(&dir, 1)));
            let events = all_events(super::send_with_images(
                "Look.".into(),
                None,
                vec![dir.join("gone.png")],
                None,
                dir.clone(),
            ));
            super::use_program_for_test(None);
            assert!(
                matches!(events.as_slice(), [HarnessEvent::Failed(error)] if error.contains("gone.png")),
                "{agent:?}: {events:?}"
            );
            assert!(!dir.join("args").exists(), "{agent:?} ran");
        }
        agent::set(Agent::Claude).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Codex and OpenCode get the system prompt ahead of the prompt, marked
    /// off from it.
    #[test]
    fn system_prompts_go_ahead_of_the_prompt() {
        assert_eq!(
            super::with_system_prompt("Fix it.", "Be brief."),
            "<system-prompt>\nBe brief.\n</system-prompt>\n\nFix it."
        );
    }

    /// A harness that takes no system prompt of its own is given it ahead of
    /// a conversation's first prompt only: carrying one on, it already holds
    /// it. One that takes its own is never given it in the prompt.
    #[test]
    fn the_system_prompt_goes_ahead_of_the_first_prompt_only() {
        use super::{Agent, prompt_as_given};
        for agent in [Agent::Codex, Agent::OpenCode] {
            assert_eq!(
                prompt_as_given(agent, "Fix it.", Some("Be brief."), false),
                "<system-prompt>\nBe brief.\n</system-prompt>\n\nFix it."
            );
            assert_eq!(
                prompt_as_given(agent, "Fix it.", Some("Be brief."), true),
                "Fix it."
            );
        }
        for resumed in [false, true] {
            assert_eq!(
                prompt_as_given(Agent::Claude, "Fix it.", Some("Be brief."), resumed),
                "Fix it."
            );
        }
    }

    #[test]
    fn parses_stream_json_events() {
        let lines = [
            json!({ "type": "system", "subtype": "init", "session_id": "s" }),
            json!({ "type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "tool_use", "id": "t1", "name": "Read", "input": {} } } }),
            json!({ "type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "" } } }),
            json!({ "type": "assistant", "parent_tool_use_id": null, "message": { "content": [
                { "type": "tool_use", "id": "t1", "name": "Read", "input": { "file_path": "/tmp/note.txt" } } ],
                "usage": { "input_tokens": 3, "cache_creation_input_tokens": 1200,
                    "cache_read_input_tokens": 15000, "output_tokens": 40 } } }),
            // A subagent's usage is its own context, not the conversation's,
            // and of what it does only its tool calls are kept.
            json!({ "type": "assistant", "parent_tool_use_id": "t9", "message": { "content": [
                { "type": "tool_use", "id": "s1", "name": "Grep", "input": { "pattern": "x" } } ],
                "usage": { "input_tokens": 90000, "output_tokens": 1 } } }),
            json!({ "type": "user", "parent_tool_use_id": "t9", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "s1",
                    "content": [{ "type": "text", "text": "spec/a.pi" }] } ] } }),
            json!({ "type": "user", "parent_tool_use_id": null, "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "1\thello" } ] } }),
            json!({ "type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } } }),
            json!({ "type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "The" } } }),
            json!({ "type": "stream_event", "parent_tool_use_id": "t9", "event": {
                "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "subagent" } } }),
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": "The note says hello." }),
        ];

        let events: Vec<HarnessEvent> = lines.iter().flat_map(parse).collect();
        assert_eq!(
            events,
            [
                HarnessEvent::Session("s".into()),
                HarnessEvent::ToolStarted {
                    id: "t1".into(),
                    name: "Read".into()
                },
                HarnessEvent::ToolInput {
                    id: "t1".into(),
                    summary: "/tmp/note.txt".into()
                },
                HarnessEvent::ToolCalled {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: json!({ "file_path": "/tmp/note.txt" }),
                    subagent: false
                },
                HarnessEvent::Usage { context: 16_243 },
                HarnessEvent::ToolCalled {
                    id: "s1".into(),
                    name: "Grep".into(),
                    input: json!({ "pattern": "x" }),
                    subagent: true
                },
                HarnessEvent::ToolOutput {
                    id: "s1".into(),
                    output: "spec/a.pi".into(),
                    subagent: true
                },
                HarnessEvent::ToolFinished {
                    id: "t1".into(),
                    is_error: false
                },
                HarnessEvent::ToolOutput {
                    id: "t1".into(),
                    output: "1\thello".into(),
                    subagent: false
                },
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("The".into()),
                HarnessEvent::Finished {
                    is_error: false,
                    result: "The note says hello.".into()
                },
            ]
        );
    }

    /// A run in a container is a `podman run` of the harness, with what its
    /// plan mounts, and no rule against reading or editing anything, since
    /// nothing it mustn't touch is there; the harness's output reads as ever.
    #[cfg(unix)]
    #[test]
    fn a_containerised_run_goes_through_podman_unguarded() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("suspense-podman-run-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let log = dir.join("podman.log");
        let script = dir.join("podman");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> {log}\n\
                 case \"$1\" in\n\
                 --version) echo 'podman version 5.0.0' ;;\n\
                 image) exit 0 ;;\n\
                 run)\n\
                   case \"$*\" in\n\
                   *'auth status'*) echo '{{\"loggedIn\": true}}' ;;\n\
                   *) read first\n\
                      echo '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}}'\n\
                      echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}}' ;;\n\
                   esac ;;\n\
                 esac\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::container::use_podman_for_test(Some(script));
        let locations = crate::project_tree::Locations {
            spec: Some(dir.join("spec")),
            code: Some(dir.join("src")),
        };
        let plan = crate::container::Plan::new(
            crate::container::RunKind::Spec,
            crate::agent::Agent::Claude,
            &dir,
            &locations,
            None,
            crate::container::Platform::Linux,
        );
        let protected = super::Protected {
            root: Some(dir.join("src")),
            unread: Some(dir.join("src")),
            container: Some(plan),
        };
        let super::Run { mut events, .. } = super::send_task(
            "Go.".into(),
            Some("System.".into()),
            Vec::new(),
            None,
            dir.clone(),
            protected,
        );
        crate::container::use_podman_for_test(None);
        let start = std::time::Instant::now();
        loop {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(20),
                "never finished"
            );
            match events.try_next() {
                Ok(Some(HarnessEvent::Finished { result, .. })) => {
                    assert_eq!(result, "Done.");
                    break;
                }
                Ok(Some(HarnessEvent::Failed(error))) => panic!("failed: {error}"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("the run ended without finishing"),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let calls = std::fs::read_to_string(&log).unwrap();
        let run = calls
            .lines()
            .find(|line| line.starts_with("run") && line.contains("claude -p"))
            .expect("the harness never ran in a container");
        assert!(
            run.contains("--rm") && run.contains("--userns=keep-id"),
            "{run}"
        );
        assert!(
            run.contains(&format!("src={}/spec", dir.display())),
            "{run}"
        );
        assert!(
            !run.contains(&format!("src={}/src", dir.display())),
            "the code was mounted: {run}"
        );
        assert!(
            !run.contains("--disallowedTools"),
            "a deny rule was given: {run}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Podman failing to run a container, before the harness starts, fails
    /// the run saying Podman couldn't be accessed, with what Podman said,
    /// never that the harness reported an error.
    #[cfg(unix)]
    #[test]
    fn podman_failing_is_said_plainly() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir =
            std::env::temp_dir().join(format!("suspense-podman-fails-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        let script = dir.join("podman");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'podman version 5.0.0' ;;\n\
             info|image) exit 0 ;;\n\
             run)\n\
               case \"$*\" in\n\
               *'auth status'*) echo '{\"loggedIn\": true}' ;;\n\
               *) echo 'Error: crun: setrlimit RLIMIT_NOFILE: Operation not permitted: OCI permission denied' >&2; exit 125 ;;\n\
               esac ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::container::use_podman_for_test(Some(script));
        let plan = crate::container::Plan::new(
            crate::container::RunKind::Spec,
            crate::agent::Agent::Claude,
            &dir,
            &crate::project_tree::Locations {
                spec: Some(dir.join("spec")),
                code: Some(dir.join("src")),
            },
            None,
            crate::container::Platform::Linux,
        );
        let super::Run { mut events, .. } = super::send_task(
            "Go.".into(),
            None,
            Vec::new(),
            None,
            dir.clone(),
            super::Protected {
                container: Some(plan),
                ..Default::default()
            },
        );
        crate::container::use_podman_for_test(None);
        let start = std::time::Instant::now();
        let failed = loop {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(20),
                "never failed"
            );
            match events.try_next() {
                Ok(Some(HarnessEvent::Failed(error))) => break error,
                Ok(Some(HarnessEvent::Finished { .. })) => panic!("it finished"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("ended without failing"),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };
        assert!(
            failed.starts_with("The harness couldn't be run because Podman couldn't be accessed."),
            "{failed}"
        );
        assert!(failed.contains("OCI permission denied"), "{failed}");
        assert!(!failed.contains("harness reported"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Carrying on a conversation the harness has none of, as Claude Code's
    /// "No conversation found", the run never fails for it: the prompt goes
    /// again at once as a new conversation, and the run says which it lost.
    #[cfg(unix)]
    #[test]
    fn a_conversation_the_harness_lost_starts_anew() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir =
            std::env::temp_dir().join(format!("suspense-lost-session-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("harness.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             cat >/dev/null\n\
             case \"$*\" in\n\
             *--resume*)\n\
               echo 'No conversation found with session ID: gone' >&2\n\
               echo '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"session_id\":\"gone\",\"errors\":[\"No conversation found with session ID: gone\"]}'\n\
               exit 1 ;;\n\
             *)\n\
               echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fresh\"}'\n\
               echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Answered.\"}' ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::use_program_for_test(Some(script));
        let mut events = super::send(
            "Why?".into(),
            None,
            Some(super::Resume {
                session: "gone".into(),
                fork: false,
            }),
            dir.clone(),
        );
        super::use_program_for_test(None);
        let start = std::time::Instant::now();
        let mut seen = Vec::new();
        loop {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "never finished: {seen:?}"
            );
            match events.try_next() {
                Ok(Some(HarnessEvent::Finished { is_error, result })) => {
                    assert!(!is_error, "{result}");
                    assert_eq!(result, "Answered.");
                    break;
                }
                Ok(Some(HarnessEvent::Failed(error))) => panic!("failed: {error}"),
                Ok(Some(event)) => seen.push(event),
                Ok(None) => panic!("ended: {seen:?}"),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        assert!(
            seen.contains(&HarnessEvent::NewConversation("gone".into())),
            "{seen:?}"
        );
        assert!(seen.contains(&HarnessEvent::Session("fresh".into())));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A failed result that carries no text says why in the harness's own
    /// words: Claude Code's errors.
    #[test]
    fn a_failure_says_why_in_the_harnesss_words() {
        let events = super::parse(&serde_json::json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "errors": ["Something broke"],
        }));
        assert!(events.contains(&HarnessEvent::Finished {
            is_error: true,
            result: "Something broke".into(),
        }));
    }
}
