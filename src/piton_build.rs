//! Runs `piton build` for a project directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::*;

pub struct BuildOutcome {
    pub success: bool,
    /// The files written, relative to the project directory.
    pub files: Vec<String>,
    /// Everything else the build reported (warnings, errors), trimmed.
    pub report: String,
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

/// Builds as [`build`] does, with `program` in place of `piton`.
fn build_with(program: &str, project_dir: &Path) -> Result<BuildOutcome> {
    {
        let output = Command::new(program)
            .arg("build")
            .current_dir(project_dir)
            .output()?;
        crate::piton_fluency::write_with(program, project_dir);

        // `piton build` prints each written file on stdout, as an absolute
        // path, and everything else on stderr.
        let root = project_dir
            .canonicalize()
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
    let missing = std::process::Command::new("piton")
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
    use std::os::unix::fs::PermissionsExt as _;

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
        std::fs::set_permissions(&piton, std::fs::Permissions::from_mode(0o755)).unwrap();
        let built = super::build_with(piton.to_str().unwrap(), &dir).unwrap();
        assert!(!built.success);
        assert_eq!(
            std::fs::read_to_string(crate::piton_fluency::file(&dir)).unwrap(),
            "# Fluency\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
