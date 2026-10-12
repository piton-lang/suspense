//! The filtered repository a restricted run is given in place of the
//! project's own git, as the SandboxScope's git says: a git directory of its
//! own, holding a single commit, with no history, of the files at HEAD the
//! run may read, and nothing else. It is made outside the sandbox, by
//! reading only the permitted objects from the real repository, never by
//! copying it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};

use super::Compiled;
use crate::process::Logged as _;

/// Runs git with `args` in `dir`, giving what it printed.
fn git(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &std::ffi::OsStr)],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut command = crate::process::command("git");
    command.current_dir(dir).args(args);
    // Never the user's or the project's GIT_DIR from the environment.
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    for (name, value) in envs {
        command.env(name, value);
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn_logged().context("could not run git")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("no stdin")?;
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Whether `path` is one `compiled` lets a run read.
fn readable(compiled: &Compiled, path: &Path) -> bool {
    !compiled
        .denied
        .iter()
        .any(|denied| path.starts_with(denied))
        && compiled
            .rules
            .iter()
            .any(|rule| path.starts_with(&rule.path))
}

/// Makes the filtered repository for a run under `compiled` of the project
/// at `project_dir`, at `dest`; none where the project isn't in a git
/// repository. Blocking.
pub fn filtered(project_dir: &Path, compiled: &Compiled, dest: &Path) -> Result<Option<PathBuf>> {
    // The project's own repository, never one named by the environment.
    let Ok(found) = git(project_dir, &["rev-parse", "--show-toplevel"], &[], None) else {
        return Ok(None);
    };
    let top = PathBuf::from(String::from_utf8_lossy(&found).trim_end_matches(['\n', '\r']));
    let top = dunce::canonicalize(&top).unwrap_or(top);
    let none: &[(&str, &std::ffi::OsStr)] = &[];
    let dest_dir = dest.as_os_str();
    let at: &[(&str, &std::ffi::OsStr)] = &[("GIT_DIR", dest_dir)];
    git(
        &top,
        &["init", "-q", "--bare", &dest.to_string_lossy()],
        none,
        None,
    )?;
    // A working tree's repository, its working tree given to the run.
    git(&top, &["config", "core.bare", "false"], at, None)?;
    // The project's root can't be listed by a restricted run, so untracked
    // files aren't looked for.
    git(
        &top,
        &["config", "status.showUntrackedFiles", "no"],
        at,
        None,
    )?;
    let Ok(listed) = git(&top, &["ls-tree", "-r", "-z", "HEAD"], none, None) else {
        // No commit yet: an empty repository.
        return Ok(Some(dest.to_path_buf()));
    };
    let branch = git(&top, &["symbolic-ref", "--short", "-q", "HEAD"], none, None)
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .ok()
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "main".into());
    let mut blobs = String::new();
    let mut index = String::new();
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split(' ');
        let (Some(mode), Some(kind), Some(sha)) = (meta.next(), meta.next(), meta.next()) else {
            continue;
        };
        if kind != "blob" {
            continue;
        }
        let full = top.join(path);
        let Ok(in_project) = full.strip_prefix(&compiled.project) else {
            continue;
        };
        if !readable(compiled, &full) {
            continue;
        }
        blobs.push_str(sha);
        blobs.push('\n');
        index.push_str(&format!("{mode} {sha}\t{}\0", in_project.to_string_lossy()));
    }
    // Only the permitted objects, read from the real repository.
    let pack = git(
        &top,
        &["pack-objects", "--stdout", "-q"],
        none,
        Some(blobs.as_bytes()),
    )?;
    git(&top, &["unpack-objects", "-q"], at, Some(&pack))?;
    git(
        &top,
        &["update-index", "-z", "--index-info"],
        at,
        Some(index.as_bytes()),
    )?;
    let tree = String::from_utf8_lossy(&git(&top, &["write-tree"], at, None)?)
        .trim()
        .to_string();
    let who: &[(&str, &std::ffi::OsStr)] = &[
        ("GIT_DIR", dest_dir),
        ("GIT_AUTHOR_NAME", "Suspense".as_ref()),
        ("GIT_AUTHOR_EMAIL", "suspense@localhost".as_ref()),
        ("GIT_COMMITTER_NAME", "Suspense".as_ref()),
        ("GIT_COMMITTER_EMAIL", "suspense@localhost".as_ref()),
    ];
    let commit = String::from_utf8_lossy(&git(
        &top,
        &[
            "commit-tree",
            &tree,
            "-m",
            "The project at HEAD, as this run may read it",
        ],
        who,
        None,
    )?)
    .trim()
    .to_string();
    let reference = format!("refs/heads/{branch}");
    git(&top, &["update-ref", &reference, &commit], at, None)?;
    git(&top, &["symbolic-ref", "HEAD", &reference], at, None)?;
    Ok(Some(dest.to_path_buf()))
}
