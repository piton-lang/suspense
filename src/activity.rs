//! What is running in the application, for the ribbon's activity list: the
//! task the harness is working on, questions still running, a spec build, and
//! a divergence analysis. Each can be revealed.

use std::path::{Path, PathBuf};

use gpui_kit::SharedString;

/// Something running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// The task the harness is working on.
    Task,
    /// A question still running, by its id.
    Question(usize),
    /// `piton build`, run from the ribbon.
    Build,
    /// A divergence analysis.
    Divergence,
    /// A search for concepts to rescope.
    Rescope,
}

/// A running job, as the activity list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub kind: JobKind,
    /// What the row reads, such as "Building the spec".
    pub title: SharedString,
    /// The first line of its prompt, for a task or a question.
    pub detail: Option<SharedString>,
    /// The open project it runs in, when that isn't the one on screen.
    pub project: Option<PathBuf>,
}

/// Emitted to reveal a job, in its project when that isn't the one on
/// screen.
pub struct RevealJob(pub JobKind, pub Option<PathBuf>);

/// A project's folder name, as a job running in it is headed.
pub fn project_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// `title` headed with the project it runs in, when that isn't the one on
/// screen: "Task · my-project".
pub fn title_in(title: &str, project: Option<&Path>) -> SharedString {
    match project {
        Some(dir) => format!("{title} · {}", project_name(dir)).into(),
        None => title.to_string().into(),
    }
}
