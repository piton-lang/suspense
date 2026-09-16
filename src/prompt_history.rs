//! The prompt history on disk. Each sent prompt is saved in the project's
//! `.suspense/history` as its hidden anchor's source (see
//! [`crate::hidden_anchor::save`]). Once its run is over, what came of it is
//! saved beside it in a `.json` file of the same name: the compiled prompt the
//! harness received, the harness's output line by line, and any error. Opening
//! the project again replays them into the tasks they were.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::HarnessEvent;
use crate::hidden_anchor::{self, HiddenAnchor};

/// What came of a sent prompt, as saved beside it.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The compiled `userPrompt` the harness received; none if the prompt did
    /// not compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    /// Each line the harness printed, as JSON where it was, else as a string.
    #[serde(default)]
    pub output: Vec<Value>,
    /// Why the run failed, when the harness's output does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunRecord {
    /// Keeps what of a harness event cannot be replayed from its output: the
    /// raw lines themselves, and a failure outside them.
    pub fn note(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::Output(line) => self
                .output
                .push(serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.clone()))),
            HarnessEvent::Failed(error) => self.error = Some(error.clone()),
            _ => {}
        }
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

    use super::{RunRecord, load, save_record};
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
}
