//! The prompt history on disk. Each sent prompt is saved in the project's
//! `.suspense/history` as its hidden anchor's source (see
//! [`crate::hidden_anchor::save`]). Once its run is over, what came of it is
//! saved beside it in a `.json` file of the same name: the compiled prompt the
//! harness received, the harness and the system prompt it received, the
//! harness's output line by line, each message sent to the task while it ran
//! in its place among those lines, any error, and whether the task has been
//! marked done by hand for sending to the other mode.
//! Opening the project again replays them into the tasks they were.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::harness::{self, HarnessEvent, Messages};
use crate::hidden_anchor::{self, HiddenAnchor};

/// What came of a sent prompt, as saved beside it.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The compiled `userPrompt` the harness received; none if the prompt did
    /// not compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    /// The harness the prompt was sent to, by the command that runs it; none
    /// if it was never sent, or was saved before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The system prompt as the harness received it, every placeholder filled
    /// in, apart from the prompt even where the harness was given it ahead of
    /// the prompt; none if none was sent. Only known once `harness` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Each line the harness printed, as JSON where it was, else as a string,
    /// and each message sent to the run while it worked, where it was sent,
    /// as `{"sent": {"text": …, "compiled": …}}`.
    #[serde(default)]
    pub output: Vec<Value>,
    /// Why the run failed, when the harness's output does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The task was cancelled, whatever its output says.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    /// The task has been marked done by hand for sending to the other mode,
    /// as though it had been sent there and finished. Records saved before
    /// this was kept load as not marked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub marked_done: bool,
    /// In a git repository, the tree the working tree was snapshotted as
    /// when the harness started the task, pinned under
    /// `refs/suspense/<task>/before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_before: Option<String>,
    /// The tree it was snapshotted as when the run was over, pinned under
    /// `refs/suspense/<task>/after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_after: Option<String>,
}

impl RunRecord {
    /// Keeps what the prompt was sent to and with: the harness, and the
    /// system prompt as it received it.
    pub fn sent_to(&mut self, agent: Agent, system_prompt: Option<&str>) {
        self.harness = Some(agent.command().to_string());
        self.system_prompt = system_prompt.map(str::to_string);
    }

    /// Holds nothing but the mark: saved only to keep a task marked done
    /// whose run left no record, so the task is still one without a record.
    pub fn holds_only_the_mark(&self) -> bool {
        *self
            == Self {
                marked_done: self.marked_done,
                ..Self::default()
            }
    }

    /// The harness the prompt was sent to, once that was kept.
    pub fn harness(&self) -> Option<Agent> {
        Agent::of_command(self.harness.as_deref()?)
    }

    /// Keeps what of a harness event cannot be replayed from its output: the
    /// raw lines themselves, each message sent in its place among them, and a
    /// failure outside them.
    pub fn note(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::Output(line) => self
                .output
                .push(serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.clone()))),
            HarnessEvent::Sent { text, compiled } => self.output.push(json!({
                "sent": { "text": text, "compiled": compiled },
            })),
            HarnessEvent::Failed(error) => self.error = Some(error.clone()),
            _ => {}
        }
    }

    /// The events its output replays, as the run went: what the harness
    /// printed, each message sent where it was sent, and each result that
    /// left a message unanswered an answer, so the run ends with the result
    /// of its last.
    pub fn events(&self) -> Vec<HarnessEvent> {
        let mut messages = Messages::default();
        let mut events = Vec::new();
        for line in &self.output {
            if let Some(sent) = line.get("sent") {
                let text = |key: &str| {
                    sent.get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                messages.sent();
                events.push(HarnessEvent::Sent {
                    text: text("text"),
                    compiled: text("compiled"),
                });
                continue;
            }
            let parsed = harness::parse(line);
            events.extend(messages.events(line, parsed));
        }
        events
    }
}

/// A prompt in the history, and its run's record if one was saved.
pub struct SavedPrompt {
    pub anchor: HiddenAnchor,
    pub text: String,
    pub record: Option<RunRecord>,
}

/// The record file saved beside `prompt_file`.
fn record_path(prompt_file: &Path) -> PathBuf {
    prompt_file.with_extension("json")
}

/// Saves `record` beside the history file of the prompt it came of.
pub fn save_record(prompt_file: &Path, record: &RunRecord) -> Result<()> {
    let file = record_path(prompt_file);
    let json = serde_json::to_string_pretty(record)?;
    fs::write(&file, json).with_context(|| format!("could not save {}", file.display()))
}

/// Marks the task saved as `prompt_file` done by hand, or not, in the record
/// beside it: the rest of the record is kept as it is, and a task without one
/// is given one holding only the mark. A record that doesn't read back is
/// left alone rather than overwritten.
pub fn save_marked_done(prompt_file: &Path, marked_done: bool) -> Result<()> {
    let file = record_path(prompt_file);
    let mut record: RunRecord = match fs::read_to_string(&file) {
        Ok(json) => serde_json::from_str(&json)
            .with_context(|| format!("could not read {}", file.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => RunRecord::default(),
        Err(err) => {
            return Err(err).with_context(|| format!("could not read {}", file.display()));
        }
    };
    record.marked_done = marked_done;
    save_record(prompt_file, &record)
}

/// The history file of the task whose hidden anchor is `name`, if it was
/// saved: named by the second it was sent and the anchor's name.
pub fn history_file(project_dir: &Path, name: &str) -> Option<PathBuf> {
    let suffix = format!("-{name}.pi");
    fs::read_dir(hidden_anchor::history_dir(project_dir))
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .find(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.ends_with(&suffix))
        })
}

/// The project's prompt history, oldest first. Files that do not read back as
/// a hidden anchor are left out; a record that is missing or does not read
/// back is `None`.
pub fn load(project_dir: &Path) -> Vec<SavedPrompt> {
    load_dir(&hidden_anchor::history_dir(project_dir))
}

/// The questions asked in the project, oldest first, read back like the
/// prompt history.
pub fn load_asks(project_dir: &Path) -> Vec<SavedPrompt> {
    load_dir(&hidden_anchor::asks_dir(project_dir))
}

fn load_dir(dir: &Path) -> Vec<SavedPrompt> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u64, PathBuf)> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "pi"))
        .map(|path| (sent_at(&path), path))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|(_, file)| {
            let (anchor, text) = HiddenAnchor::parse(&fs::read_to_string(&file).ok()?)?;
            let record = fs::read_to_string(record_path(&file))
                .ok()
                .and_then(|json| serde_json::from_str(&json).ok());
            Some(SavedPrompt {
                anchor,
                text,
                record,
            })
        })
        .collect()
}

/// When a history file was sent, from the seconds its name starts with. Not
/// zero-padded, so compared as numbers rather than names.
fn sent_at(file: &Path) -> u64 {
    file.file_name()
        .and_then(|name| name.to_str()?.split_once('-')?.0.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{RunRecord, history_file, load, save_marked_done, save_record};
    use crate::harness::HarnessEvent;
    use crate::hidden_anchor::{self, HiddenAnchor};

    /// A saved prompt loads back with the record saved beside it; one without
    /// a record loads back without one.
    #[test]
    fn prompts_load_back_with_their_records() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/prompt-history-test");
        fs::remove_dir_all(&project_dir).ok();

        let recorded = HiddenAnchor::random();
        let file = hidden_anchor::save(&recorded, "recorded", &project_dir).unwrap();
        let mut record = RunRecord {
            user_prompt: Some("recorded, compiled".into()),
            ..RunRecord::default()
        };
        for event in [
            HarnessEvent::Output(r#"{"type":"result","result":"ok"}"#.into()),
            HarnessEvent::Output("not json".into()),
            HarnessEvent::TextDelta("parsed, not kept".into()),
            HarnessEvent::Failed("it broke".into()),
        ] {
            record.note(&event);
        }
        save_record(&file, &record).unwrap();

        let history = load(&project_dir);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].anchor.name(), recorded.name());
        assert_eq!(history[0].text, "recorded");
        assert_eq!(history[0].record.as_ref(), Some(&record));
        assert_eq!(
            record.output,
            [
                serde_json::json!({ "type": "result", "result": "ok" }),
                serde_json::json!("not json"),
            ]
        );

        fs::remove_file(file.with_extension("json")).unwrap();
        assert!(load(&project_dir)[0].record.is_none());
    }

    /// A task's mark, done by hand for sending to the other mode, is saved
    /// in its record, keeping the rest; a task without a record is given one
    /// of its mark alone. A record saved before marks were kept loads back
    /// unmarked.
    #[test]
    fn marks_are_saved_in_the_record() {
        let project_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/prompt-history-mark-test");
        fs::remove_dir_all(&project_dir).ok();

        let recorded = HiddenAnchor::random();
        let file = hidden_anchor::save(&recorded, "recorded", &project_dir).unwrap();
        assert_eq!(
            history_file(&project_dir, recorded.name()),
            Some(file.clone())
        );
        assert_eq!(history_file(&project_dir, "Prompt_none"), None);
        let old: RunRecord =
            serde_json::from_str(r#"{"userPrompt":"compiled","output":[],"cancelled":true}"#)
                .unwrap();
        assert!(!old.marked_done);
        save_record(&file, &old).unwrap();

        save_marked_done(&file, true).unwrap();
        let history = load(&project_dir);
        let marked = history[0].record.as_ref().unwrap();
        assert!(marked.marked_done && marked.cancelled);
        assert_eq!(marked.user_prompt.as_deref(), Some("compiled"));
        assert!(!marked.holds_only_the_mark());
        save_marked_done(&file, false).unwrap();
        assert!(!load(&project_dir)[0].record.as_ref().unwrap().marked_done);
        // Unmarked, nothing of the mark is written.
        let json = fs::read_to_string(file.with_extension("json")).unwrap();
        assert!(!json.contains("markedDone"), "{json}");

        fs::remove_file(file.with_extension("json")).unwrap();
        save_marked_done(&file, true).unwrap();
        let alone = load(&project_dir).remove(0).record.unwrap();
        assert!(alone.marked_done && alone.holds_only_the_mark());
        assert_eq!(
            alone,
            RunRecord {
                marked_done: true,
                ..RunRecord::default()
            }
        );

        // A record that doesn't read back is left as it was.
        fs::write(file.with_extension("json"), "not json").unwrap();
        assert!(save_marked_done(&file, true).is_err());
        assert_eq!(
            fs::read_to_string(file.with_extension("json")).unwrap(),
            "not json"
        );
        fs::remove_dir_all(&project_dir).ok();
    }
}
