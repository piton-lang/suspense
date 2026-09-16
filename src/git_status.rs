//! The git status of a project's files and folders, read from `git status`,
//! for colouring the project tree. A folder takes the most pressing status
//! among what it holds, so a change stays visible while it is collapsed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::ColorName;
use gpui_kit::{App, Hsla};

/// A file's or folder's git status, least pressing first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Ignored,
    Untracked,
    Added,
    Modified,
    Conflicted,
}

impl Status {
    /// The colour a name with this status is shown in, readable in both the
    /// light and the dark theme.
    pub fn color(self, cx: &App) -> Hsla {
        let dark = cx.theme().is_dark();
        let shade = |name: ColorName| name.scale(if dark { 400 } else { 700 });
        match self {
            Status::Conflicted => shade(ColorName::Red),
            Status::Modified => shade(ColorName::Amber),
            Status::Added => shade(ColorName::Green),
            Status::Untracked => shade(ColorName::Teal),
            Status::Ignored => cx.theme().muted_foreground.opacity(0.6),
        }
    }
}

/// The statuses of a repository's changed and ignored paths.
#[derive(Debug, Default, PartialEq)]
pub struct GitStatus {
    /// Files and folders `git status` names, by absolute path.
    paths: HashMap<PathBuf, Status>,
    /// Folders holding a changed file, with the most pressing status of what
    /// they hold.
    folders: HashMap<PathBuf, Status>,
    /// Ignored folders, whose contents are all ignored.
    ignored_folders: Vec<PathBuf>,
}

impl GitStatus {
    /// The status of the repository `project_dir` is in, or `None` outside of
    /// one or without git.
    pub fn read(project_dir: &Path) -> Option<Self> {
        // The repository's top, reached from the project directory as given
        // (`../..`), so paths match the tree's even through a symlink.
        let up = Command::new("git")
            .args(["rev-parse", "--show-cdup"])
            .current_dir(project_dir)
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let mut top = project_dir.to_path_buf();
        for _ in String::from_utf8_lossy(&up.stdout)
            .trim()
            .split('/')
            .filter(|part| *part == "..")
        {
            top.pop();
        }
        let output = Command::new("git")
            .args([
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
            ])
            .current_dir(project_dir)
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        Some(Self::parse(&top, &output.stdout))
    }

    /// Reads `git status --porcelain=v1 -z` output, whose paths are relative
    /// to the repository's `top`.
    pub fn parse(top: &Path, output: &[u8]) -> Self {
        let mut status = Self::default();
        let mut records = output.split(|byte| *byte == 0);
        while let Some(record) = records.next() {
            if record.len() < 4 {
                continue;
            }
            let (x, y) = (record[0], record[1]);
            let path = String::from_utf8_lossy(&record[3..]);
            // A rename or copy is followed by the path it came from.
            if matches!(x, b'R' | b'C') {
                records.next();
            }
            let kind = match (x, y) {
                (b'!', b'!') => Status::Ignored,
                (b'?', b'?') => Status::Untracked,
                (b'D', b'D') | (b'A', b'A') | (b'U', _) | (_, b'U') => Status::Conflicted,
                (b'A', _) => Status::Added,
                _ => Status::Modified,
            };
            let is_folder = path.ends_with('/');
            let absolute = top.join(path.trim_end_matches('/'));
            if kind == Status::Ignored {
                if is_folder {
                    status.ignored_folders.push(absolute.clone());
                }
            } else {
                for folder in absolute.ancestors().skip(1) {
                    if !folder.starts_with(top) {
                        break;
                    }
                    let entry = status.folders.entry(folder.to_path_buf()).or_insert(kind);
                    *entry = (*entry).max(kind);
                }
            }
            status.paths.insert(absolute, kind);
        }
        status
    }

    /// The status `path` shows in the tree: its own, the most pressing among
    /// what a folder holds, or ignored inside an ignored folder. Unchanged
    /// paths have none.
    pub fn of(&self, path: &Path, is_folder: bool) -> Option<Status> {
        if self
            .ignored_folders
            .iter()
            .any(|folder| path.starts_with(folder))
        {
            return Some(Status::Ignored);
        }
        if is_folder {
            let held = self.folders.get(path).copied();
            return match (self.paths.get(path).copied(), held) {
                (Some(own), Some(held)) => Some(own.max(held)),
                (own, held) => own.or(held),
            };
        }
        self.paths.get(path).copied()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{GitStatus, Status};

    /// Each kind of change reads back with its status, and a folder takes the
    /// most pressing of what it holds; ignored folders' contents are ignored.
    #[test]
    fn statuses_and_folders_read_back() {
        let top = Path::new("/repo");
        let output = [
            " M src/main.rs",
            "A  src/new.rs",
            "?? notes/draft.md",
            "UU src/deep/clash.rs",
            "R  src/renamed.rs",
            "src/old.rs",
            "!! target/",
            "!! debug.log",
        ]
        .join("\0");
        let status = GitStatus::parse(top, output.as_bytes());
        let of = |path: &str, folder| status.of(&top.join(path), folder);

        assert_eq!(of("src/main.rs", false), Some(Status::Modified));
        assert_eq!(of("src/new.rs", false), Some(Status::Added));
        assert_eq!(of("notes/draft.md", false), Some(Status::Untracked));
        assert_eq!(of("src/deep/clash.rs", false), Some(Status::Conflicted));
        assert_eq!(of("src/renamed.rs", false), Some(Status::Modified));
        assert_eq!(
            of("src/old.rs", false),
            None,
            "a rename's old path was read as a change"
        );
        assert_eq!(of("src/unchanged.rs", false), None);

        assert_eq!(of("src", true), Some(Status::Conflicted));
        assert_eq!(of("src/deep", true), Some(Status::Conflicted));
        assert_eq!(of("notes", true), Some(Status::Untracked));
        assert_eq!(of("docs", true), None);

        assert_eq!(of("target", true), Some(Status::Ignored));
        assert_eq!(of("target/debug/app", false), Some(Status::Ignored));
        assert_eq!(of("debug.log", false), Some(Status::Ignored));
    }

    /// In a real repository: a changed file, a new untracked one, and an
    /// ignored folder read back with their statuses, and the folders holding
    /// them take the most pressing. A folder outside any repository has none.
    #[test]
    fn reads_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("suspense-git-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("src/main.rs"), "fn main() { changed() }\n").unwrap();
        std::fs::write(dir.join("notes.md"), "new\n").unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/out"), "x").unwrap();

        let status = GitStatus::read(&dir.join("src")).expect("no status in a repository");
        let of = |path: &str, folder| status.of(&dir.join(path), folder);
        assert_eq!(of("src/main.rs", false), Some(Status::Modified));
        assert_eq!(of("src", true), Some(Status::Modified));
        assert_eq!(of("notes.md", false), Some(Status::Untracked));
        assert_eq!(of("build", true), Some(Status::Ignored));
        assert_eq!(of("build/out", false), Some(Status::Ignored));
        assert_eq!(of(".gitignore", false), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
