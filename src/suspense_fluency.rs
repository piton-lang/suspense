//! The Suspense fluency: how Suspense works, as a harness working in any
//! project needs to know it to talk about it or write a prompt or question
//! for the user. It is the application's own, not a project's: its source is
//! `suspense-fluency.md` in this repository's `.suspense/system-prompts`,
//! baked in when the application is built, so a build can't be made without
//! it. It is written to `.suspense/suspense-fluency.md` in the project before
//! any Code, Chain, Spec, or Ask prompt is compiled, whenever it isn't there
//! or doesn't hold what the application ships. It is never part of a system
//! prompt: those modes' instructions point at the file through
//! `${SUSPENSE_FLUENCY_FILE}` (see [`crate::system_prompts`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::hidden_anchor::APP_DIR;

/// The file's name in the project's data.
const FILE: &str = "suspense-fluency.md";

/// What the application ships: plain text, with no placeholder and no path
/// of any machine, so it reads the same on the host and in a container.
pub const FLUENCY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.suspense/system-prompts/suspense-fluency.md"
));

/// Writes of the file one at a time, so one prompt never reads it half
/// written by another.
static WRITING: Mutex<()> = Mutex::new(());

/// The file's path from the project directory, as the instructions name it.
pub fn relative_file() -> String {
    format!("{APP_DIR}/{FILE}")
}

/// The Suspense fluency file of the project in `project_dir`.
pub fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

/// Writes what the application ships to the project's file, unless it
/// already holds just that. Returns whether the file holds it once done.
pub fn ensure(project_dir: &Path) -> bool {
    let _writing = WRITING.lock();
    let file = file(project_dir);
    if fs::read_to_string(&file).is_ok_and(|held| held == FLUENCY) {
        return true;
    }
    file.parent()
        .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
        && fs::write(&file, FLUENCY).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{FLUENCY, ensure, file};

    /// It says what each mode is and how prompts and questions are handed
    /// back, with no placeholder, and names no harness's own directory.
    #[test]
    fn the_fluency_is_baked_in_as_plain_text() {
        for said in [
            "Code changes the code",
            "Chain changes the spec, then the code",
            "Freeform passes a prompt",
            "<task-instructions>",
            "suspense-prompt",
            "suspense-question",
            "understanding file",
            "new conversation",
        ] {
            assert!(FLUENCY.contains(said), "it doesn't say {said:?}");
        }
        assert!(!FLUENCY.contains("${"), "it holds a placeholder");
        assert!(!FLUENCY.contains(".claude"), "it names a harness's directory");
    }

    /// Written where it is missing or out of date, and left alone where it
    /// already holds what the application ships.
    #[test]
    fn it_is_written_whenever_it_is_missing_or_out_of_date() {
        let dir =
            std::env::temp_dir().join(format!("suspense-suspense-fluency-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(ensure(&dir));
        assert_eq!(std::fs::read_to_string(file(&dir)).unwrap(), FLUENCY);
        std::fs::write(file(&dir), "An older Suspense's fluency.").unwrap();
        assert!(ensure(&dir));
        assert_eq!(std::fs::read_to_string(file(&dir)).unwrap(), FLUENCY);
        std::fs::remove_dir_all(&dir).ok();
    }
}
