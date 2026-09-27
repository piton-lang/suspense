//! Harness integration: sends a compiled prompt to a local coding harness as a
//! one-off run (`claude -p`, `codex exec`, or `opencode run`, whichever the
//! user picked) in the project directory, streaming what it does as it does
//! it. A run can resume the conversation of an earlier one, so the harness
//! keeps its context between prompts, and a task's run can be fed more
//! messages while it works, where the harness allows.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use futures::channel::mpsc;
use serde_json::{Value, json};

use crate::agent::{self, Agent};
use crate::harness_mentions;

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

/// A task's run of the harness: its events, and, where the harness can be
/// fed more while it works, what feeds it.
pub struct Run {
    pub events: mpsc::UnboundedReceiver<HarnessEvent>,
    pub feed: Option<Feed>,
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
    let (tx, rx) = mpsc::unbounded();
    let feed = (fed && agent.can_be_fed()).then(|| Feed::new(tx.clone()));
    std::thread::spawn({
        let feed = feed.clone();
        move || {
            let result = run(
                agent,
                &prompt,
                system_prompt.as_deref(),
                resume.as_ref(),
                &project_dir,
                &tx,
                feed.as_ref(),
            );
            // Nothing more can be sent once the run is over, however it ended.
            if let Some(feed) = &feed {
                feed.end();
            }
            if let Err(err) = result {
                tx.unbounded_send(HarnessEvent::Failed(format!("{err:#}")))
                    .ok();
            }
        }
    });
    Run { events: rx, feed }
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

fn run(
    agent: Agent,
    prompt: &str,
    system_prompt: Option<&str>,
    resume: Option<&Resume>,
    project_dir: &Path,
    tx: &mpsc::UnboundedSender<HarnessEvent>,
    feed: Option<&Feed>,
) -> Result<()> {
    // A conversation is only carried on by the agent it began with, and only
    // Claude Code can carry on a copy of one; otherwise a new one starts.
    let resume = resume.and_then(|resume| {
        let (began, id) = Agent::of_session(&resume.session);
        (began == agent && (!resume.fork || agent == Agent::Claude))
            .then(|| (id.to_string(), resume.fork))
    });
    let mut command = Command::new(agent.command());
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
    let mut child = command
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run `{name}`"))?;

    // The prompt goes over stdin so its length and leading characters never
    // collide with command-line parsing; dropping stdin ends it, unless the
    // run is fed, when it stays open for more.
    let mut stdin = child.stdin.take().context("the harness has no stdin")?;
    stdin.write_all(input.as_bytes())?;
    if let Some(feed) = feed {
        stdin.flush()?;
        feed.lock().stdin = Some(stdin);
    } else {
        drop(stdin);
    }
    // Stopped, the run's input closes at once, and the harness with it.
    let stop = |child: &mut std::process::Child| {
        if let Some(feed) = feed {
            feed.close();
        }
        child.kill().ok();
        child.wait().ok();
    };
    let stdout = child.stdout.take().context("the harness has no stdout")?;
    let stderr = child.stderr.take().context("the harness has no stderr")?;
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
        let line = line?;
        if line.trim().is_empty() {
            // Nothing to parse, but the raw stream shows lines as they arrived.
            if tx.unbounded_send(HarnessEvent::Output(line)).is_err() {
                stop(&mut child);
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
                stop(&mut child);
                return Ok(());
            }
        }
    }

    let status = child.wait()?;
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
        Some("turn.completed") => vec![HarnessEvent::Finished {
            is_error: false,
            result: String::new(),
        }],
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
            let tokens = |pointer: &str| {
                part.pointer(&format!("/tokens{pointer}"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            };
            let mut events = vec![HarnessEvent::Usage {
                context: tokens("/input")
                    + tokens("/output")
                    + tokens("/cache/read")
                    + tokens("/cache/write"),
            }];
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
                .collect()
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
        Some("result") => vec![HarnessEvent::Finished {
            is_error: event
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            result: str_at(event, "/result").unwrap_or_default(),
        }],
        _ => Vec::new(),
    }
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
mod tests {
    use serde_json::json;

    use super::{HarnessEvent, parse};

    /// Shapes taken from a real `claude -p --output-format stream-json
    /// --verbose --include-partial-messages` run.
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
