//! Runs `piton build` for a project directory.

use std::path::{Path, PathBuf};

use gpui_kit::*;

pub struct BuildOutcome {
    pub success: bool,
    /// The files written, relative to the project directory.
    pub files: Vec<String>,
    /// Everything else the build reported (warnings, errors), trimmed.
    pub report: String,
    /// How many files in the reference folders the build didn't own were
    /// deleted so it could write them, as the SpecBuildScope's owned
    /// reference says.
    pub replaced: usize,
}

impl BuildOutcome {
    /// The passive line saying how many reference files were replaced, if
    /// any were.
    pub fn replaced_note(&self) -> Option<String> {
        match self.replaced {
            0 => None,
            1 => Some("Replaced 1 reference file the build didn't own".into()),
            n => Some(format!("Replaced {n} reference files the build didn't own")),
        }
    }
}

pub fn run(project_dir: PathBuf, cx: &App) -> Task<Result<BuildOutcome>> {
    cx.background_spawn(async move { build(&project_dir) })
}

/// Runs `piton build` in `project_dir`, waiting for it to finish, then
/// writes the project's Piton fluency to its file, however the build went, so
/// the file keeps in step with the project's piton (see
/// [`crate::piton_fluency`]).
pub fn build(project_dir: &Path) -> Result<BuildOutcome> {
    build_with("piton", project_dir)
}

/// Builds as [`build`] does, with `program` in place of `piton`. A build
/// stopped only by files in the reference folders it doesn't own deletes
/// those, and only those, and runs again, as part of the same build: the
/// reference is the build's own output, and nothing else's.
fn build_with(program: &str, project_dir: &Path) -> Result<BuildOutcome> {
    let first = build_once(program, project_dir)?;
    if first.success {
        return Ok(first);
    }
    let Some(unowned) = unowned_reference_files(&first.report) else {
        return Ok(first);
    };
    let mut replaced = 0;
    for file in &unowned {
        if std::fs::remove_file(project_dir.join(file)).is_ok() {
            replaced += 1;
        }
    }
    if replaced == 0 {
        return Ok(first);
    }
    let mut second = build_once(program, project_dir)?;
    second.replaced = replaced;
    Ok(second)
}

/// The build's manifest of the files it owns, in the project's `.piton`.
fn manifest(project_dir: &Path) -> PathBuf {
    project_dir.join(".piton").join("manifest.json")
}

/// The files outside the reference folders the project's build owns now, as
/// its manifest lists them: the guidance it places in the code location.
pub fn owned_outside_reference(project_dir: &Path) -> Vec<String> {
    let folders = reference_folders();
    std::fs::read_to_string(manifest(project_dir))
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|manifest| {
            Some(
                manifest
                    .get("generated")?
                    .as_array()?
                    .iter()
                    .filter_map(|file| file.as_str())
                    .filter(|file| {
                        !folders
                            .iter()
                            .any(|folder| Path::new(file).starts_with(folder))
                    })
                    .map(str::to_string)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// Lists again, in the project's manifest, each of `owned`, files outside
/// the reference folders the build owned, that a build in a Spec run's
/// container left out, having no code location to place them in, while the
/// file is still there: so the host's next build still owns what it wrote,
/// and isn't stopped by it.
pub fn keep_owned(project_dir: &Path, owned: &[String]) {
    let file = manifest(project_dir);
    let Some(mut manifest) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
    else {
        return;
    };
    let Some(generated) = manifest
        .get_mut("generated")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let mut changed = false;
    for owned in owned {
        let listed = generated.iter().any(|file| file.as_str() == Some(owned));
        if !listed && project_dir.join(owned).is_file() {
            generated.push(serde_json::Value::String(owned.clone()));
            changed = true;
        }
    }
    if changed && let Ok(json) = serde_json::to_string_pretty(&manifest) {
        std::fs::write(&file, json + "\n").ok();
    }
}

/// The reference folders a build writes, one per harness: the harness's
/// directory's `reference`.
fn reference_folders() -> Vec<PathBuf> {
    crate::agent::Agent::ALL
        .iter()
        .map(|agent| Path::new(agent.directory()).join("reference"))
        .collect()
}

/// The files a failed build's `report` refused to overwrite, when every
/// error it reports is such a refusal for a file inside a reference folder;
/// none when any error is another, or names a file anywhere else.
fn unowned_reference_files(report: &str) -> Option<Vec<PathBuf>> {
    let folders = reference_folders();
    let errors: Vec<&str> = report
        .lines()
        .filter(|line| line.trim_start().starts_with("error"))
        .collect();
    if errors.is_empty() {
        return None;
    }
    let mut files = Vec::new();
    for error in errors {
        if !error.contains("[unowned-output]") {
            return None;
        }
        // The file is the first thing in backticks.
        let file = error.split('`').nth(1)?;
        let path = Path::new(file);
        let safe = path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
        if !safe || !folders.iter().any(|folder| path.starts_with(folder)) {
            return None;
        }
        files.push(path.to_path_buf());
    }
    Some(files)
}

/// Runs `piton build` once.
fn build_once(program: &str, project_dir: &Path) -> Result<BuildOutcome> {
    {
        let output = crate::process::command(program)
            .arg("build")
            .current_dir(project_dir)
            .output()?;
        crate::piton_fluency::write_with(program, project_dir);

        // `piton build` prints each written file on stdout, as an absolute
        // path, and everything else on stderr.
        let root = dunce::canonicalize(project_dir)
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| relative_to(Path::new(line), &root))
            .collect();

        let report = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Ok(BuildOutcome {
            success: output.status.success(),
            files,
            report: if report.is_empty() && !output.status.success() {
                format!("piton build exited with {}", output.status)
            } else {
                report
            },
            replaced: 0,
        })
    }
}

fn relative_to(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Whether `piton` can't be run, as on a machine that doesn't have it
/// installed, such as CI's: tests that need it say they're skipped and pass.
#[cfg(test)]
pub fn piton_missing() -> bool {
    let missing = crate::process::command("piton")
        .arg("--version")
        .output()
        .is_err();
    if missing {
        eprintln!("skipped: `piton` isn't installed");
    }
    missing
}

#[cfg(all(test, unix))]
mod tests {

    /// A spec build writes the project's fluency to its file, whether the
    /// build succeeds or not.
    #[test]
    fn a_spec_build_writes_the_fluency() {
        let dir =
            std::env::temp_dir().join(format!("suspense-build-fluency-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let piton = dir.join("piton");
        std::fs::write(
            &piton,
            "#!/bin/sh\n\
             if [ \"$1\" = agent ]; then echo '# Fluency'; exit 0; fi\n\
             echo 'build failed' >&2; exit 1\n",
        )
        .unwrap();
        crate::test_scripts::make_executable(&piton);
        let built = super::build_with(piton.to_str().unwrap(), &dir).unwrap();
        assert!(!built.success);
        assert_eq!(
            std::fs::read_to_string(crate::piton_fluency::file(&dir)).unwrap(),
            "# Fluency\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A build stopped only by reference files it doesn't own deletes those
    /// and runs again; one stopped by a file outside the reference, as an
    /// instruction file in the code, deletes nothing and fails as ever.
    #[test]
    fn unowned_reference_files_are_replaced() {
        let dir = std::env::temp_dir().join(format!("suspense-unowned-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join(".claude/reference/app")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let stale = dir.join(".claude/reference/app/Application.md");
        std::fs::write(&stale, "old").unwrap();
        let piton = dir.join("piton");
        std::fs::write(
            &piton,
            "#!/bin/sh\n\
             if [ \"$1\" = agent ]; then exit 1; fi\n\
             if [ -e .claude/reference/app/Application.md ]; then\n\
               echo 'x build stopped; no files were written' >&2\n\
               echo 'error: spec/app/Application.pi:4:15: `.claude/reference/app/Application.md` already exists and no previous build generated it, so Belay will not overwrite it [unowned-output]' >&2\n\
               exit 1\n\
             fi\n\
             echo \"$PWD/.claude/reference/app/Application.md\"\n",
        )
        .unwrap();
        crate::test_scripts::make_executable(&piton);
        let built = super::build_with(piton.to_str().unwrap(), &dir).unwrap();
        assert!(built.success, "{}", built.report);
        assert_eq!(built.replaced, 1);
        assert_eq!(
            built.replaced_note().as_deref(),
            Some("Replaced 1 reference file the build didn't own")
        );
        assert!(!stale.exists());

        // Outside the reference, nothing is deleted.
        assert_eq!(
            super::unowned_reference_files(
                "error: x: `src/CLAUDE.md` already exists and no previous build generated it [unowned-output]"
            ),
            None
        );
        assert_eq!(
            super::unowned_reference_files(
                "error: x: `.claude/reference/a.md` already exists [unowned-output]\nerror: x: something else [empty-value]"
            ),
            None
        );
        assert_eq!(
            super::unowned_reference_files(
                "error: x: `.claude/reference/../../etc.md` exists [unowned-output]"
            ),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Guidance outside the reference that a container's build left out of
    /// the manifest is listed again, while it is still there.
    #[test]
    fn guidance_a_container_build_dropped_stays_owned() {
        let dir = std::env::temp_dir().join(format!("suspense-keep-owned-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join(".piton")).unwrap();
        std::fs::create_dir_all(dir.join("src/ribbon")).unwrap();
        std::fs::write(dir.join("src/ribbon/CLAUDE.md"), "x").unwrap();
        let manifest = dir.join(".piton/manifest.json");
        std::fs::write(
            &manifest,
            r#"{"generated":[".claude/reference/a.md","src/ribbon/CLAUDE.md","src/gone/CLAUDE.md"]}"#,
        )
        .unwrap();
        let owned = super::owned_outside_reference(&dir);
        assert_eq!(owned, ["src/ribbon/CLAUDE.md", "src/gone/CLAUDE.md"]);
        // The container's build rewrote it without them.
        std::fs::write(
            &manifest,
            r#"{"generated":[".claude/reference/a.md"],"targets":{}}"#,
        )
        .unwrap();
        super::keep_owned(&dir, &owned);
        let now: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(
            now["generated"],
            serde_json::json!([".claude/reference/a.md", "src/ribbon/CLAUDE.md"])
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
