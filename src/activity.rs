//! What is running in the application, for the ribbon's activity list: the
//! task the harness is working on, questions still running, a spec build, and
//! a divergence analysis. Each can be revealed.

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
}

/// Emitted to reveal a job.
pub struct RevealJob(pub JobKind);
