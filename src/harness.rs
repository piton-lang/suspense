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
use crate::harness_mentions;
use crate::usage::{PlanLimit, Spend};

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
    /// How many tokens the conversation's context holds, as of the model's
    /// latest reply: all it was given, cached or not, and what it wrote.
    Usage {
        context: u64,
    },
    /// Tokens and cost the run reported: its totals so far when `total`,
    /// or more on top of what it reported before otherwise. A figure left
    /// `None` wasn't reported.
    Spent {
        spend: Spend,
        total: bool,
    },
    /// The plan limits the harness reported, each with the share used so
    /// far; a limit not among them is as it was.
    Limits(Vec<PlanLimit>),
    /// The model the run uses.
    Model(String),
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
    start(prompt, system_prompt, resume, project_dir, false).events
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
/// `opencode run` read a single prompt, and have no feed.
pub fn send_task(
    prompt: String,
    system_prompt: Option<String>,
    resume: Option<Resume>,
    project_dir: PathBuf,
) -> Run {
    start(prompt, system_prompt, resume, project_dir, true)
}

fn start(
    prompt: String,
    system_prompt: Option<String>,
    resume: Option<Resume>,
    project_dir: PathBuf,
    fed: bool,
) -> Run {
    let agent = agent::current();
    let program = program(agent);
    let (tx, rx) = mpsc::unbounded();
    let feed = (fed && agent.can_be_fed()).then(|| Feed::new(tx.clone()));
    let stop = Stop::new(feed.clone());
    std::thread::spawn({
        let feed = feed.clone();
        let stop = stop.clone();
        move || {
            let result = run(
                agent,
                &program,
                &prompt,
                system_prompt.as_deref(),
                resume.as_ref(),
                &project_dir,
                &tx,
                feed.as_ref(),
                &stop,
            );
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
        Command::new("kill")
            .args(["-TERM", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
    #[cfg(windows)]
    {
        Command::new("taskkill")
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

    /// Sends the run `compiled`, the message typed as `text`, which the
    /// harness takes once it finishes what it is doing. Fails, sending
    /// nothing, once the run is over or about to be: once its input is
    /// closed. Blocks while the message is written.
    pub fn send(&self, text: String, compiled: String) -> Result<()> {
        let mut feeding = self.lock();
        let Some(stdin) = feeding.stdin.as_mut() else {
            bail!("the task is over");
        };
        let written = stdin
            .write_all(user_message(&compiled).as_bytes())
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
        let mut cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        feed.lock().stdin = cat.stdin.take();
        (feed, rx)
    }
}

/// A message for a harness reading `--input-format stream-json`: a user
/// message holding `text`, as a line of JSON.
fn user_message(text: &str) -> String {
    let mut line = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }],
        },
    })
    .to_string();
    line.push('\n');
    line
}

/// How many messages a fed run was sent, its prompt the first, how many the
/// harness took in, and how many results it reported, so the result that
/// answers the last of them is known. Claude Code takes a message in once it
/// finishes what it is doing, and says so by replaying it: one taken in while
/// it is still at work joins what it is doing, and is answered by the same
/// result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Messages {
    sent: usize,
    taken: usize,
    results: usize,
}

impl Default for Messages {
    fn default() -> Self {
        Self {
            sent: 1,
            taken: 0,
            results: 0,
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
                Some(self.taken >= self.sent || self.results >= self.sent)
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

#[allow(clippy::too_many_arguments)]
fn run(
    agent: Agent,
    program: &Path,
    prompt: &str,
    system_prompt: Option<&str>,
    resume: Option<&Resume>,
    project_dir: &Path,
    tx: &mpsc::UnboundedSender<HarnessEvent>,
    feed: Option<&Feed>,
    stop: &Stop,
) -> Result<()> {
    // A conversation is only carried on by the agent it began with, and only
    // Claude Code can carry on a copy of one; otherwise a new one starts.
    let resume = resume.and_then(|resume| {
        let (began, id) = Agent::of_session(&resume.session);
        (began == agent && (!resume.fork || agent == Agent::Claude))
            .then(|| (id.to_string(), resume.fork))
    });
    let mut command = Command::new(program);
    let mut input = prompt.to_string();
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
                // its first run, and each prompt's own must apply, as its mode
                // may differ.
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
            // Fed, the prompt is the first of a stream of messages, each of
            // which the harness replays as it takes it in.
            if feed.is_some() {
                command.args(["--input-format", "stream-json", "--replay-user-messages"]);
                input = user_message(prompt);
            }
        }
        Agent::Codex => {
            command.args(["exec", "--json", "--full-auto", "--skip-git-repo-check"]);
            if let Some((session, _)) = &resume {
                command.args(["resume", session]);
            }
            // Read from standard input.
            command.arg("-");
        }
        Agent::OpenCode => {
            command.args(["run", "--format", "json"]);
            if let Some((session, _)) = &resume {
                command.args(["--session", session]);
            }
        }
    }
    if agent != Agent::Claude
        && let Some(system_prompt) = system_prompt
    {
        input = with_system_prompt(prompt, system_prompt);
    }
    let name = invocation(agent);
    if stop.is_stopped() {
        return Ok(());
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
        return Ok(());
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

    let mut replying = Replying::default();
    for line in BufReader::new(stdout).lines() {
        // Stopped, nothing more it prints is taken.
        if stop.is_stopped() {
            break;
        }
        let line = line?;
        if line.trim().is_empty() {
            // Nothing to parse, but the raw stream shows lines as they arrived.
            // Once no one is listening, the run is stopped.
            if tx.unbounded_send(HarnessEvent::Output(line)).is_err() {
                stop.stop();
                return Ok(());
            }
            continue;
        }
        let events = match serde_json::from_str::<Value>(&line) {
            Ok(event) => {
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
            if tx.unbounded_send(replying.take(event)).is_err() {
                stop.stop();
                return Ok(());
            }
        }
    }

    let status = stop.wait()?;
    // Stopped, it ended as it was asked to; what it left writing to its error
    // output is not waited on.
    if stop.is_stopped() {
        return Ok(());
    }
    let stderr = stderr.join().unwrap_or_default();
    if !replying.finished() {
        // A harness that ends without saying it finished, as OpenCode may,
        // finished with what it last wrote once it exits cleanly.
        if status.success() && agent != Agent::Claude {
            tx.unbounded_send(replying.finish()).ok();
            return Ok(());
        }
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!("`{name}` exited with {status}");
        }
        bail!("{stderr}");
    }
    Ok(())
}

/// How `agent` is run, as an error names it.
fn invocation(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude -p",
        Agent::Codex => "codex exec",
        Agent::OpenCode => "opencode run",
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
    let mut command = Command::new(agent.command());
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
            let mut command = Command::new(agent.command());
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
/// `cached_input_tokens` were read from the cache, and its output. Codex
/// reports no cost and nothing written to the cache.
fn codex_spent(event: &Value) -> Option<HarnessEvent> {
    let usage = event.get("usage")?;
    let tokens = |key: &str| usage.get(key).and_then(Value::as_u64);
    let cached = tokens("cached_input_tokens");
    let spend = Spend {
        input: tokens("input_tokens").map(|input| input.saturating_sub(cached.unwrap_or(0))),
        output: tokens("output_tokens"),
        cache_read: cached,
        ..Spend::default()
    };
    (!spend.is_empty()).then_some(HarnessEvent::Spent {
        spend,
        total: false,
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
                    total: false,
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
                result: str_at(event, "/result").unwrap_or_default(),
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
        return vec![HarnessEvent::Spent { spend, total: true }];
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
                total: false,
            });
        }
    }
    if cost.is_some() {
        events.push(HarnessEvent::Spent {
            spend: Spend {
                cost,
                ..Spend::default()
            },
            total: true,
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
    use crate::usage::{PlanLimit, Spend};

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
            } = super::send_task("Do it".into(), None, None, dir.clone());
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

    /// Shapes taken from a real `claude -p --output-format stream-json
    /// --verbose --include-partial-messages` run.
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
                    total: true,
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
                    total: false,
                },
                HarnessEvent::Spent {
                    spend: Spend {
                        cost: Some(0.5),
                        ..Spend::default()
                    },
                    total: true,
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
                total: false,
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
                total: false,
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
                    total: false,
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
                    total: false,
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
        let line = super::user_message("Fix \"it\".\nThen test.");
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

    /// Codex and OpenCode get the system prompt ahead of the prompt, marked
    /// off from it.
    #[test]
    fn system_prompts_go_ahead_of_the_prompt() {
        assert_eq!(
            super::with_system_prompt("Fix it.", "Be brief."),
            "<system-prompt>\nBe brief.\n</system-prompt>\n\nFix it."
        );
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
}
