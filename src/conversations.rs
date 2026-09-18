//! Which conversation of the tasks, and of the questions, was last left for a
//! new one, kept in `.suspense/conversations.json` so a project reopened after
//! a reset starts fresh rather than carrying on the latest conversation in its
//! history. It names the conversation left, so once a run after the reset has
//! begun a conversation of its own, that one is carried on as ever. The
//! harness keeps its conversations on this machine, so this file is machine
//! only.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::hidden_anchor::APP_DIR;

const FILE: &str = "conversations.json";

/// The conversations a project keeps apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Those of Code, Chain, and Spec.
    Tasks,
    /// Those of Ask.
    Questions,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Left {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left_tasks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left_questions: Option<String>,
}

impl Left {
    fn of(&mut self, kind: Kind) -> &mut Option<String> {
        match kind {
            Kind::Tasks => &mut self.left_tasks,
            Kind::Questions => &mut self.left_questions,
        }
    }
}

fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

fn read(project_dir: &Path) -> Left {
    fs::read_to_string(file(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The conversation of `kind` last left for a new one in the project, if any.
pub fn left(project_dir: &Path, kind: Kind) -> Option<String> {
    read(project_dir).of(kind).take()
}

/// Records that the conversation `id` of `kind` was left for a new one.
pub fn leave(project_dir: &Path, kind: Kind, id: &str) -> Result<()> {
    let mut left = read(project_dir);
    *left.of(kind) = Some(id.to_string());
    let file = file(project_dir);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(&file, serde_json::to_string_pretty(&left)?)
        .with_context(|| format!("could not save {}", file.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Kind, leave, left};

    /// What was left is kept per kind, and each leaving replaces the last.
    #[test]
    fn left_conversations_are_kept_with_the_project() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/conversations-test");
        fs::remove_dir_all(&dir).ok();
        assert_eq!(left(&dir, Kind::Tasks), None);
        leave(&dir, Kind::Tasks, "a").unwrap();
        leave(&dir, Kind::Questions, "q").unwrap();
        leave(&dir, Kind::Tasks, "b").unwrap();
        assert_eq!(left(&dir, Kind::Tasks).as_deref(), Some("b"));
        assert_eq!(left(&dir, Kind::Questions).as_deref(), Some("q"));
    }
}
