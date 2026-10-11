//! What a task's mode keeps it off, as the HarnessIntegrationScope's
//! protected locations say. A run in a container is kept off what isn't
//! mounted into it, and that is the only enforcement there is. A run on the
//! host, a Code task or a Chain prompt's code step, is only nudged: told,
//! where its harness takes rules, not to edit the spec location, nor read
//! it. Nothing is ever put back, restored, or deleted, and nothing is
//! watched to undo what happens; spec files a Code task's tool calls report
//! changing are named in a passive notice, left as it left them.

use std::path::PathBuf;

use crate::chat_input::SendMode;
use crate::project_tree::Locations;

/// The mode a run sent in `mode` is nudged for: its own on the host, and
/// none in a container, where nothing it may not change is mounted.
pub fn guarded(mode: Option<SendMode>, in_container: bool) -> Option<SendMode> {
    mode.filter(|_| !in_container)
}

/// The location a task sent in `mode` on the host is told it may not edit:
/// the spec, for a Code task, and none for the rest.
pub fn protected(mode: SendMode, locations: &Locations) -> Option<PathBuf> {
    match mode {
        SendMode::Code => locations.spec.clone(),
        SendMode::Spec | SendMode::Both | SendMode::Ask | SendMode::Freeform => None,
    }
}

/// The location a run in `mode` is told it may not read, so it learns the
/// spec from its compiled reference: the spec's source, for a Code task.
pub fn unread(mode: SendMode, locations: &Locations) -> Option<PathBuf> {
    match mode {
        // A question runs in a container that holds the spec's source read
        // only, for `piton slice`, and is told nothing of keeping off it.
        SendMode::Code => locations.spec.clone(),
        SendMode::Ask | SendMode::Both | SendMode::Spec | SendMode::Freeform => None,
    }
}

/// Of `edited`, the files a task's tool calls report changing, those in
/// `root`, the location it was told not to change, relative to `project_dir`.
pub fn changed_in(root: &std::path::Path, edited: &[PathBuf], project_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut changed: Vec<PathBuf> = edited
        .iter()
        .filter(|path| path.starts_with(root))
        .map(|path| path.strip_prefix(project_dir).unwrap_or(path).to_path_buf())
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a Code task is told off a location, the spec.
    #[test]
    fn a_code_task_is_told_off_the_spec() {
        let locations = Locations {
            spec: Some("/p/spec".into()),
            code: Some("/p/src".into()),
        };
        assert_eq!(protected(SendMode::Code, &locations), Some("/p/spec".into()));
        for mode in [SendMode::Spec, SendMode::Both, SendMode::Ask, SendMode::Freeform] {
            assert_eq!(protected(mode, &locations), None);
        }
    }

    /// Only the files in the location told off are named, from the project.
    #[test]
    fn changed_files_in_the_spec_are_named() {
        let edited = [
            PathBuf::from("/p/spec/a.pi"),
            PathBuf::from("/p/src/main.rs"),
            PathBuf::from("/p/spec/a.pi"),
        ];
        assert_eq!(
            changed_in(std::path::Path::new("/p/spec"), &edited, std::path::Path::new("/p")),
            [PathBuf::from("spec/a.pi")]
        );
    }

    /// A run in a container is guarded for nothing, and told off reading
    /// nothing; on the host it keeps its rule.
    #[test]
    fn containerised_runs_are_not_guarded() {
        assert_eq!(super::guarded(Some(SendMode::Code), true), None);
        assert_eq!(super::guarded(Some(SendMode::Code), false), Some(SendMode::Code));
    }
}
