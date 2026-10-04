//! Snapshots of a git project's working tree, taken as the harness starts a
//! task and as its run ends, so the files that changed while it ran can be
//! listed and diffed.
//!
//! Only git's plumbing is used, against a private index in
//! `.suspense/snapshot-index`: `git add -A` then `git write-tree`, with
//! `GIT_INDEX_FILE` pointing there, so the repository's own index, HEAD,
//! branches, and working tree are never touched, and nothing is committed.
//! The index is kept between snapshots, so each hashes again only the files
//! whose timestamps changed. Each tree is pinned under
//! `refs/suspense/<task>/before` or `/after` so git's garbage collection
//! keeps it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// The private index, in the project's data.
const INDEX: &str = ".suspense/snapshot-index";

/// The repository `project_dir` is in, by its top directory; none if it
/// isn't in one, and then nothing is snapshotted.
pub fn repo_top(project_dir: &Path) -> Option<PathBuf> {
    let output = crate::process::command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(top.trim_end_matches(['\n', '\r'])))
}

/// Git, run in `top` against the private index of `project_dir`.
fn git(top: &Path, project_dir: &Path) -> Command {
    let mut command = crate::process::command("git");
    command
        .current_dir(top)
        .env("GIT_INDEX_FILE", project_dir.join(INDEX));
    command
}

fn run(mut command: Command, what: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("could not run git to {what}"))?;
    if !output.status.success() {
        bail!(
            "git could not {what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Snapshots the working tree of the repository `project_dir` is in, as git
/// sees it, `.gitignore` followed, and pins the tree under
/// `refs/suspense/<task>/<which>`. Returns the tree's hash.
pub fn take(project_dir: &Path, task: &str, which: &str) -> Result<String> {
    let top = repo_top(project_dir).context("the project isn't a git repository")?;
    std::fs::create_dir_all(project_dir.join(".suspense")).context("could not create .suspense")?;
    // The private index is never in the snapshot, whether or not the
    // project's .gitignore says so. Named when git already ignores it, git
    // would refuse, so it is excluded only when it isn't.
    let index = project_dir.join(INDEX);
    let ignored = crate::process::command("git")
        .current_dir(&top)
        .args(["check-ignore", "-q", "--no-index"])
        .arg(&index)
        .status()
        .is_ok_and(|status| status.success());
    let index = index.strip_prefix(&top).unwrap_or(&index);
    let mut add = git(&top, project_dir);
    add.args(["add", "-A", "--", ":/"]);
    if !ignored {
        add.arg(format!(":(top,exclude){}", index.display()))
            .arg(format!(":(top,exclude){}.lock", index.display()));
    }
    run(add, "snapshot the working tree")?;
    let mut write = git(&top, project_dir);
    write.arg("write-tree");
    let tree = run(write, "write the snapshot's tree")?.trim().to_string();
    let mut pin = crate::process::command("git");
    pin.current_dir(&top).args([
        "update-ref",
        &format!("refs/suspense/{task}/{which}"),
        &tree,
    ]);
    run(pin, "keep the snapshot")?;
    Ok(tree)
}

/// How a file changed between two snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    pub fn letter(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
        }
    }
}

/// A file that changed between two snapshots, by its path from the
/// repository's top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: PathBuf,
    /// Where a renamed file was before.
    pub from: Option<PathBuf>,
}

/// The files that changed between the trees `before` and `after`, in path
/// order, as `git diff --name-status` gives them.
pub fn changes(top: &Path, before: &str, after: &str) -> Result<Vec<Change>> {
    let mut diff = crate::process::command("git");
    diff.current_dir(top)
        .args(["diff", "--name-status", "-z", "-M", before, after]);
    let mut changes = parse(&run(diff, "list the changed files")?);
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Whether `path` is something the application keeps for itself rather than
/// one of the project's own files: anything in a `.suspense` directory, or
/// the draft, `.suspense-draft.pi`, wherever it is.
pub fn is_application_data(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".suspense-draft.pi")
        || path
            .components()
            .any(|part| part.as_os_str() == ".suspense")
}

/// `changes` with what the application keeps for itself left out: a file
/// renamed into it is listed as deleted from where it was, one renamed out of
/// it as added where it is now.
pub fn project_own(changes: Vec<Change>) -> Vec<Change> {
    changes
        .into_iter()
        .filter_map(|change| {
            let from_kept = change.from.as_deref().map(is_application_data);
            match (is_application_data(&change.path), from_kept) {
                (true, Some(false)) => Some(Change {
                    kind: ChangeKind::Deleted,
                    path: change.from?,
                    from: None,
                }),
                (true, _) => None,
                (false, Some(true)) => Some(Change {
                    kind: ChangeKind::Added,
                    path: change.path,
                    from: None,
                }),
                (false, _) => Some(change),
            }
        })
        .collect()
}

/// Reads `git diff --name-status -z`: each status, then its path, or for a
/// rename or copy its old path and its new.
fn parse(output: &str) -> Vec<Change> {
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let kind = match status.chars().next() {
            Some('A') | Some('C') => ChangeKind::Added,
            Some('D') => ChangeKind::Deleted,
            Some('R') => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        let copied = status.starts_with('C');
        let (from, path) = if kind == ChangeKind::Renamed || copied {
            let (Some(from), Some(to)) = (fields.next(), fields.next()) else {
                break;
            };
            ((!copied).then(|| PathBuf::from(from)), to)
        } else {
            let Some(path) = fields.next() else {
                break;
            };
            (None, path)
        };
        changes.push(Change {
            kind,
            path: PathBuf::from(path),
            from,
        });
    }
    changes
}

/// The contents of `path`, from the repository's top, in the tree `tree`;
/// none where it isn't in it.
pub fn contents(top: &Path, tree: &str, path: &Path) -> Option<Vec<u8>> {
    let output = crate::process::command("git")
        .current_dir(top)
        .arg("cat-file")
        .arg("blob")
        .arg(format!("{tree}:{}", path.to_string_lossy()))
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the application keeps, in any `.suspense` directory or as the
    /// draft, is left out; a rename across that line is an addition or a
    /// deletion of the project's own file.
    #[test]
    fn leaves_out_what_the_application_keeps() {
        let change = |kind, path: &str, from: Option<&str>| Change {
            kind,
            path: PathBuf::from(path),
            from: from.map(PathBuf::from),
        };
        let changes = vec![
            change(ChangeKind::Added, ".suspense/history/1-Task.pi", None),
            change(ChangeKind::Modified, "app/.suspense/queue/a.pi", None),
            change(ChangeKind::Modified, "spec/.suspense-draft.pi", None),
            change(ChangeKind::Modified, "src/main.rs", None),
            change(ChangeKind::Modified, "docs/suspense.md", None),
            change(ChangeKind::Renamed, ".suspense/notes.md", Some("notes.md")),
            change(ChangeKind::Renamed, "plan.md", Some(".suspense/plan.md")),
            change(
                ChangeKind::Renamed,
                ".suspense/b.md",
                Some(".suspense/a.md"),
            ),
        ];
        assert_eq!(
            project_own(changes),
            vec![
                change(ChangeKind::Modified, "src/main.rs", None),
                change(ChangeKind::Modified, "docs/suspense.md", None),
                change(ChangeKind::Deleted, "notes.md", None),
                change(ChangeKind::Added, "plan.md", None),
            ]
        );
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// Two snapshots list what changed between them, leave the repository's
    /// own index, HEAD, and history as they were, follow .gitignore, and are
    /// pinned under the private refs.
    #[test]
    fn snapshots_list_what_changed_and_touch_nothing_else() {
        let dir = std::env::temp_dir().join(format!("suspense-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        git_in(&dir, &["init", "-q"]);
        git_in(&dir, &["config", "user.email", "t@t"]);
        git_in(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(dir.join("moved.txt"), "a long enough line to rename\n").unwrap();
        git_in(&dir, &["add", "kept.txt"]);
        git_in(&dir, &["commit", "-qm", "first"]);
        let before = take(&dir, "Prompt_a", "before").unwrap();
        std::fs::write(dir.join("kept.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::rename(dir.join("moved.txt"), dir.join("renamed.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), "hi\n").unwrap();
        std::fs::write(dir.join("ignored.txt"), "no\n").unwrap();
        let after = take(&dir, "Prompt_a", "after").unwrap();

        let changed = changes(&dir, &before, &after).unwrap();
        let listed: Vec<_> = changed
            .iter()
            .map(|c| (c.kind.letter(), c.path.display().to_string()))
            .collect();
        assert_eq!(
            listed,
            [
                ("D", "gone.txt".to_string()),
                ("M", "kept.txt".to_string()),
                ("A", "new.txt".to_string()),
                ("R", "renamed.txt".to_string()),
            ]
        );
        assert_eq!(changed[3].from, Some(PathBuf::from("moved.txt")));
        assert_eq!(
            contents(&dir, &before, Path::new("kept.txt")).unwrap(),
            b"one\n"
        );
        assert_eq!(
            contents(&dir, &after, Path::new("kept.txt")).unwrap(),
            b"two\n"
        );
        assert!(contents(&dir, &before, Path::new("new.txt")).is_none());

        // The repository's own index and history are as they were: only
        // kept.txt is staged or committed, and there is still one commit.
        let mut status = Command::new("git");
        status
            .current_dir(&dir)
            .args(["diff", "--cached", "--name-only"]);
        assert_eq!(run(status, "diff").unwrap(), "");
        let mut log = Command::new("git");
        log.current_dir(&dir).args(["rev-list", "--count", "HEAD"]);
        assert_eq!(run(log, "log").unwrap().trim(), "1");
        let mut refs = Command::new("git");
        refs.current_dir(&dir)
            .args(["for-each-ref", "--format=%(refname)", "refs/suspense/"]);
        assert_eq!(
            run(refs, "refs").unwrap(),
            "refs/suspense/Prompt_a/after\nrefs/suspense/Prompt_a/before\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn name_status_reads_every_kind() {
        let parsed = parse("M\0a\0R087\0b\0c\0A\0d\0D\0e\0");
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[1].kind, ChangeKind::Renamed);
        assert_eq!(parsed[1].from, Some(PathBuf::from("b")));
        assert_eq!(parsed[1].path, PathBuf::from("c"));
    }
}
