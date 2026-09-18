//! Harness integration: sends a compiled prompt to a local coding harness as a
//! one-off run (`claude -p`) in the project directory, streaming what it does
//! as it does it. A run can resume the conversation of an earlier one, so the
//! harness keeps its context between prompts.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use futures::channel::mpsc;
use serde_json::Value;

use crate::harness_mentions;

const HARNESS_COMMAND: &str = "claude";

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
    Finished {
        is_error: bool,
        result: String,
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
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        let result = run(
            &prompt,
            system_prompt.as_deref(),
            resume.as_ref(),
            &project_dir,
            &tx,
        );
        if let Err(err) = result {
            tx.unbounded_send(HarnessEvent::Failed(format!("{err:#}")))
                .ok();
        }
    });
    rx
}

fn run(
    prompt: &str,
    system_prompt: Option<&str>,
    resume: Option<&Resume>,
    project_dir: &Path,
    tx: &mpsc::UnboundedSender<HarnessEvent>,
) -> Result<()> {
    let mut command = Command::new(HARNESS_COMMAND);
    command.args([
        "-p",
        // The spec: the harness always runs in auto mode.
        "--permission-mode",
        "auto",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // A resumed conversation otherwise keeps the system prompt of its
        // first run, and each prompt's own must apply, as its mode may differ.
        "--system-prompt-snapshot",
        "off",
    ]);
    if let Some(resume) = resume {
        command.args(["--resume", &resume.session]);
        if resume.fork {
            command.arg("--fork-session");
        }
    }
    if let Some(system_prompt) = system_prompt {
        command.args(["--append-system-prompt", system_prompt]);
    }
    let mut child = command
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run `{HARNESS_COMMAND} -p`"))?;

    // The prompt goes over stdin so its length and leading characters never
    // collide with command-line parsing; dropping stdin ends it.
    child
        .stdin
        .take()
        .context("the harness has no stdin")?
        .write_all(prompt.as_bytes())?;
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

    let mut finished = false;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if line.trim().is_empty() {
            // Nothing to parse, but the raw stream shows lines as they arrived.
            if tx.unbounded_send(HarnessEvent::Output(line)).is_err() {
                child.kill().ok();
                child.wait().ok();
                return Ok(());
            }
            continue;
        }
        let events = match serde_json::from_str::<Value>(&line) {
            Ok(event) => {
                // The skills and agents this run has are offered as mentions.
                harness_mentions::remember(&event, project_dir);
                parse(&event)
            }
            Err(_) => Vec::new(),
        };
        for event in std::iter::once(HarnessEvent::Output(line)).chain(events) {
            finished |= matches!(event, HarnessEvent::Finished { .. });
            if tx.unbounded_send(event).is_err() {
                child.kill().ok();
                child.wait().ok();
                return Ok(());
            }
        }
    }

    let status = child.wait()?;
    let stderr = stderr.join().unwrap_or_default();
    if !finished {
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!("`{HARNESS_COMMAND} -p` exited with {status}");
        }
        bail!("{stderr}");
    }
    Ok(())
}

/// Translates one line of `stream-json` output. Events from subagents (those
/// with a `parent_tool_use_id`) are left out so the reply reads as one thread.
pub fn parse(event: &Value) -> Vec<HarnessEvent> {
    if event
        .get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null())
    {
        return Vec::new();
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
    if key == "command" {
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
            // A subagent's usage is its own context, not the conversation's.
            json!({ "type": "assistant", "parent_tool_use_id": "t9", "message": { "content": [],
                "usage": { "input_tokens": 90000, "output_tokens": 1 } } }),
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
                HarnessEvent::Usage { context: 16_243 },
                HarnessEvent::ToolFinished {
                    id: "t1".into(),
                    is_error: false
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
