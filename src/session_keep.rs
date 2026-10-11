//! Keeps a conversation's session from expiring, as the
//! HarnessIntegrationScope's context says: once a run is over, the harness's
//! session for its conversation is copied into the project's data, a
//! `.session` folder beside the run's history record; and before a
//! conversation is carried on whose session the harness no longer has, the
//! latest copy is put back where the harness looks for it, under the same
//! session id.
//!
//! A session is whatever the harness keeps under its sessions folder named
//! by the session's id: Claude Code's transcript `<id>.jsonl` and its folder
//! `<id>/`, beneath the folder of the run's working directory; Codex's
//! `rollout-…-<id>.jsonl` beneath its dated folders; and OpenCode's files
//! named by the id across its storage. Each is copied at its path from the
//! sessions folder, so it goes back to the same place.

use std::path::{Path, PathBuf};

use crate::agent::Agent;
use crate::hidden_anchor::APP_DIR;

/// How deep beneath a sessions folder a session's files are looked for.
const DEPTH: usize = 6;

/// The settings every Claude Code run is given, so it never deletes a
/// session for its age: its transcript cleanup set further off than any
/// session will ever be old. Passed with the run, never written to the
/// user's own settings.
pub const CLAUDE_SETTINGS: &str = r#"{"cleanupPeriodDays":3650000}"#;

/// Where `agent` keeps its sessions for a run: the conversation's sessions
/// folder it mounts, for a run in a container, or the harness's own folder
/// on the host.
pub fn sessions_root(agent: Agent, mounted: Option<&Path>) -> Option<PathBuf> {
    if let Some(mounted) = mounted {
        return Some(mounted.to_path_buf());
    }
    let home = dirs::home_dir()?;
    Some(home.join(match agent {
        Agent::Claude => ".claude/projects",
        Agent::Codex => ".codex/sessions",
        Agent::OpenCode => ".local/share/opencode/storage",
    }))
}

/// The folder Claude Code keeps a working directory's transcripts in, named
/// by the directory with every character but letters and digits a dash.
fn claude_folder(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Where beneath `root` to look for a session of `agent` run in `cwd`:
/// Claude Code's folder for the directory, and the whole of the others'.
fn search_root(agent: Agent, root: &Path, cwd: &Path) -> PathBuf {
    match agent {
        Agent::Claude => root.join(claude_folder(cwd)),
        Agent::Codex | Agent::OpenCode => root.to_path_buf(),
    }
}

/// Every file or folder beneath `dir` named by `id`, a folder taken whole.
fn named_by(dir: &Path, id: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let named = entry.file_name().to_string_lossy().contains(id);
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if named {
                found.push(path);
            } else if is_dir && depth < DEPTH {
                folders.push((path, depth + 1));
            }
        }
    }
    found
}

/// Whether the harness still has the session `id`.
pub fn has(agent: Agent, root: &Path, cwd: &Path, id: &str) -> bool {
    !id.is_empty() && !named_by(&search_root(agent, root, cwd), id).is_empty()
}

/// Copies the session `id`, as the harness left it beneath `root`, into
/// `kept`, the `.session` folder beside the run's history record, replacing
/// what an earlier copy there held. Nothing is copied where the harness has
/// no such session.
pub fn keep(agent: Agent, root: &Path, cwd: &Path, id: &str, kept: &Path) -> std::io::Result<()> {
    if id.is_empty() {
        return Ok(());
    }
    let searched = search_root(agent, root, cwd);
    let found = named_by(&searched, id);
    if found.is_empty() {
        return Ok(());
    }
    std::fs::remove_dir_all(kept).ok();
    for path in found {
        let relative = path.strip_prefix(&searched).unwrap_or(&path);
        copy(&path, &kept.join(relative))?;
    }
    Ok(())
}

/// Puts the latest copy of the session `id` the project keeps back beneath
/// `root`, where the harness looks for it, at the same paths; whether one
/// was put back.
pub fn restore(agent: Agent, root: &Path, cwd: &Path, id: &str, project_dir: &Path) -> bool {
    let Some(kept) = latest_copy(project_dir, id) else {
        return false;
    };
    let searched = search_root(agent, root, cwd);
    let Ok(entries) = std::fs::read_dir(&kept) else {
        return false;
    };
    let mut restored = false;
    for entry in entries.flatten() {
        let to = searched.join(entry.file_name());
        if copy(&entry.path(), &to).is_ok() {
            restored = true;
        }
    }
    restored && has(agent, root, cwd, id)
}

/// The latest `.session` folder the project's history holds for the
/// session `id`.
fn latest_copy(project_dir: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let history = project_dir.join(APP_DIR).join("history");
    std::fs::read_dir(history)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "session") && path.is_dir())
        .filter(|path| !named_by(path, id).is_empty())
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        })
}

/// Copies the file or folder `from` to `to`, its folders made as needed.
fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)?.flatten() {
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session copied after a run is put back, under the same id, once
    /// the harness has lost it, and a conversation with no copy isn't.
    #[test]
    fn sessions_are_kept_and_put_back() {
        let dir = std::env::temp_dir().join(format!("suspense-session-keep-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let (root, project) = (dir.join("claude-projects"), dir.join("project"));
        let cwd = Path::new("/workspace");
        let folder = root.join(claude_folder(cwd));
        std::fs::create_dir_all(folder.join("abc/subagents")).unwrap();
        std::fs::write(folder.join("abc.jsonl"), "transcript\n").unwrap();
        std::fs::write(folder.join("abc/subagents/x.jsonl"), "sub\n").unwrap();
        std::fs::write(folder.join("other.jsonl"), "other\n").unwrap();
        let kept = project.join(".suspense/history/1-Task.session");
        keep(Agent::Claude, &root, cwd, "abc", &kept).unwrap();
        assert!(kept.join("abc.jsonl").is_file());
        assert!(kept.join("abc/subagents/x.jsonl").is_file());
        assert!(!kept.join("other.jsonl").exists());

        std::fs::remove_dir_all(&root).unwrap();
        assert!(!has(Agent::Claude, &root, cwd, "abc"));
        assert!(restore(Agent::Claude, &root, cwd, "abc", &project));
        assert_eq!(std::fs::read_to_string(folder.join("abc.jsonl")).unwrap(), "transcript\n");
        assert!(!restore(Agent::Claude, &root, cwd, "nope", &project));
        std::fs::remove_dir_all(&dir).ok();
    }
}
