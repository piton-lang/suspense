//! Commit notes: a one-line summary of each Code, Chain, or Spec task, written
//! by the harness once the task is done, that build up in the git panel above
//! the commit message and go into the next commit. They are saved with the
//! project, in `.suspense/commit-notes.json`, so they survive a restart, and
//! can be edited or removed before committing.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

use crate::hidden_anchor::APP_DIR;

const FILE: &str = "commit-notes.json";

/// The most of a task's prompt and of its summary handed to the harness, so a
/// note is always quick to write.
const MAX_CHARS: usize = 4_000;

/// What the harness replies when the task changed nothing worth a note.
const NO_NOTE: &str = "NONE";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub text: String,
}

/// Bumped whenever the notes change, so whatever shows them reads them again.
#[derive(Default)]
pub struct NotesVersion(pub usize);

impl Global for NotesVersion {}

/// Tells whatever shows the notes that they changed.
pub fn changed(cx: &mut App) {
    let version = cx
        .try_global::<NotesVersion>()
        .map_or(0, |version| version.0);
    cx.set_global(NotesVersion(version + 1));
}

/// The file the project's notes are saved in.
pub fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

/// The project's notes, oldest first; none if there are none, or they can't be
/// read.
pub fn load(project_dir: &Path) -> Vec<Note> {
    fs::read_to_string(file(project_dir))
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Saves the project's notes, removing the file once there are none.
pub fn save(project_dir: &Path, notes: &[Note]) -> Result<()> {
    let file = file(project_dir);
    if notes.is_empty() {
        match fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("could not remove {}", file.display()));
            }
            _ => return Ok(()),
        }
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(&file, serde_json::to_string_pretty(notes)?)
        .with_context(|| format!("could not save {}", file.display()))
}

/// Adds a note after the others.
pub fn add(project_dir: &Path, text: &str) -> Result<()> {
    let mut notes = load(project_dir);
    let id = notes
        .iter()
        .map(|note| note.id)
        .max()
        .map_or(1, |id| id + 1);
    notes.push(Note {
        id,
        text: text.to_string(),
    });
    save(project_dir, &notes)
}

/// Replaces the text of note `id`.
pub fn edit(project_dir: &Path, id: u64, text: &str) -> Result<()> {
    let mut notes = load(project_dir);
    if let Some(note) = notes.iter_mut().find(|note| note.id == id) {
        note.text = text.to_string();
    }
    save(project_dir, &notes)
}

/// Removes note `id`.
pub fn remove(project_dir: &Path, id: u64) -> Result<()> {
    let mut notes = load(project_dir);
    notes.retain(|note| note.id != id);
    save(project_dir, &notes)
}

/// The commit message: what was typed, then each note that isn't blank as a
/// line of its own, after a blank line when something was typed.
pub fn message(typed: &str, notes: &[Note]) -> String {
    let typed = typed.trim();
    let lines: Vec<String> = notes
        .iter()
        .map(|note| note.text.trim())
        .filter(|text| !text.is_empty())
        .map(|text| format!("- {text}"))
        .collect();
    match (typed.is_empty(), lines.is_empty()) {
        (_, true) => typed.to_string(),
        (true, false) => lines.join("\n"),
        (false, false) => format!("{typed}\n\n{}", lines.join("\n")),
    }
}

/// Writes a task's note, `None` if it changed nothing: from what was asked
/// and the harness's summary of what it did, quickly, with a small model, at
/// low effort, and no tools.
pub fn summarize(project_dir: &Path, prompt: &str, result: &str) -> Result<Option<String>> {
    let cut = |text: &str| text.chars().take(MAX_CHARS).collect::<String>();
    let request = format!(
        "Below is a task given to a coding agent, and the agent's own summary of what it \
         did. Write a one-line summary of the change it made, for a list of changes in a \
         commit message: in the imperative mood, at most 72 characters, with no trailing \
         period, quotes, or leading dash. If the agent changed no files, reply with \
         exactly {NO_NOTE}. Reply with only the line.\n\nTask:\n{}\n\nWhat the agent \
         did:\n{}",
        cut(prompt),
        cut(result)
    );
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--model",
            "haiku",
            "--effort",
            "low",
            "--tools",
            "",
            "--no-session-persistence",
        ])
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run claude")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("claude has no stdin"))?;
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let output = child.wait_with_output()?;
    writer.join().ok();
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(clean(&String::from_utf8_lossy(&output.stdout)))
}

/// The note in the harness's reply: its first line with anything around it
/// trimmed, or `None` for no note.
fn clean(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = line
        .trim_start_matches(['-', '*', ' '])
        .trim_matches(['"', '\'', '`'])
        .trim_end_matches('.')
        .trim();
    (!line.is_empty() && line != NO_NOTE).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Note, add, clean, edit, load, message, remove, save};

    /// Notes are added in order, edited, and removed, and saved with the
    /// project as they change; none left removes the file.
    #[test]
    fn notes_are_saved_with_the_project() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/commit-notes-test");
        std::fs::remove_dir_all(&dir).ok();
        assert!(load(&dir).is_empty());

        add(&dir, "Add the ribbon").unwrap();
        add(&dir, "Fix scrolling").unwrap();
        let notes = load(&dir);
        assert_eq!(
            notes
                .iter()
                .map(|note| note.text.as_str())
                .collect::<Vec<_>>(),
            ["Add the ribbon", "Fix scrolling"]
        );
        edit(&dir, notes[1].id, "Fix tree scrolling").unwrap();
        remove(&dir, notes[0].id).unwrap();
        assert_eq!(
            load(&dir),
            [Note {
                id: notes[1].id,
                text: "Fix tree scrolling".into()
            }]
        );
        save(&dir, &[]).unwrap();
        assert!(!dir.join(".suspense/commit-notes.json").exists());
    }

    /// The typed message comes first, then the notes as lines of their own;
    /// blank notes are left out.
    #[test]
    fn notes_go_into_the_commit_message() {
        let notes = [
            Note {
                id: 1,
                text: "Add the ribbon".into(),
            },
            Note {
                id: 2,
                text: "  ".into(),
            },
            Note {
                id: 3,
                text: "Fix scrolling".into(),
            },
        ];
        assert_eq!(
            message("Tidy the UI\n", &notes),
            "Tidy the UI\n\n- Add the ribbon\n- Fix scrolling"
        );
        assert_eq!(message("", &notes), "- Add the ribbon\n- Fix scrolling");
        assert_eq!(message("Only typed", &[]), "Only typed");
        assert_eq!(message(" ", &[]), "");
    }

    #[test]
    fn replies_are_cleaned_into_a_note() {
        assert_eq!(
            clean("\n- \"Add a git panel.\"\n"),
            Some("Add a git panel".into())
        );
        assert_eq!(clean("NONE"), None);
        assert_eq!(clean("  \n"), None);
    }
}
