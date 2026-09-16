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

/// Runs `piton build` in `project_dir`, waiting for it to finish.
pub fn build(project_dir: &Path) -> Result<BuildOutcome> {
    {
        let output = Command::new("piton")
            .arg("build")
            .current_dir(project_dir)
            .output()?;

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
