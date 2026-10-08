//! The project's Piton fluency: what `piton agent --print-fluency` prints, run
//! in the project directory, written to `.suspense/fluency.md` for the work
//! that writes Piton to read. It is never part of a system prompt: the Spec
//! and Chain instructions point at the file through `${PITON_FLUENCY_FILE}`
//! (see [`crate::system_prompts`]). It is written each time the application
//! runs a spec build (see [`crate::piton_build`]), so it keeps in step with
//! the project's piton, and before a Spec or Chain prompt when it has never
//! been written. Without `piton`, or when it fails, nothing is written, and a
//! file written before is left as it was.

use crate::process::Logged as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::hidden_anchor::APP_DIR;

/// The file's name in the project's data.
const FILE: &str = "fluency.md";

/// Writes of the file one at a time, so one prompt never reads it half
/// written by another.
static WRITING: Mutex<()> = Mutex::new(());

/// The fluency file's path from the project directory, as the instructions
/// name it.
pub fn relative_file() -> String {
    format!("{APP_DIR}/{FILE}")
}

/// The fluency file of the project in `project_dir`.
pub fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

/// Writes the project's fluency to its file, as a spec build does, running
/// `piton` for it. Blocks until it has run, so call it off the UI thread.
/// Returns whether the file is there once done: a fluency that can't be
/// printed leaves a file written before as it was.
pub fn write(project_dir: &Path) -> bool {
    write_with("piton", project_dir)
}

/// Writes the fluency as [`write`] does, unless the file is already there.
/// Returns whether it is there once done.
pub fn ensure(project_dir: &Path) -> bool {
    file(project_dir).exists() || write(project_dir)
}

/// Writes the fluency as [`write`] does, printed by `program` rather than
/// `piton`.
pub(crate) fn write_with(program: &str, project_dir: &Path) -> bool {
    // With no path a run in a container can't reach.
    let fluency = crate::system_prompts::without_project_path(
        &print_prompt(program, project_dir),
        project_dir,
    );
    let _writing = WRITING.lock();
    let file = file(project_dir);
    if !fluency.trim().is_empty() {
        let written = file
            .parent()
            .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
            && fs::write(&file, format!("{fluency}\n")).is_ok();
        if written {
            return true;
        }
    }
    file.exists()
}

/// What `program agent --print-fluency` prints in `project_dir`, or nothing
/// when it can't be run or fails.
fn print_prompt(program: &str, project_dir: &Path) -> String {
    match crate::process::command(program)
        .args(["agent", "--print-fluency"])
        .current_dir(project_dir)
        .output_logged()
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::print_prompt;

    /// Without `piton`, the fluency is empty rather than an error.
    #[test]
    fn a_missing_piton_gives_no_fluency() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(print_prompt("no-such-piton-binary", dir), "");
    }

    /// A command that fails gives no fluency either.
    #[test]
    fn a_failing_piton_gives_no_fluency() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(print_prompt("false", dir), "");
    }

    /// What piton prints is written to the file as it printed it; a fluency
    /// that can't be printed writes nothing, and leaves a file written
    /// before as it was.
    #[cfg(unix)]
    #[test]
    fn the_fluency_is_written_to_its_file() {
        use super::{file, write_with};
        let dir = std::env::temp_dir().join(format!("suspense-fluency-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!write_with("no-such-piton-binary", &dir));
        assert!(!file(&dir).exists(), "a missing piton wrote a file");

        let piton = dir.join("piton");
        std::fs::write(&piton, "#!/bin/sh\nprintf '# Fluency\\n\\n${x} {y}\\n'\n").unwrap();
        crate::test_scripts::make_executable(&piton);
        assert!(write_with(piton.to_str().unwrap(), &dir));
        let written = std::fs::read_to_string(file(&dir)).unwrap();
        assert_eq!(written, "# Fluency\n\n${x} {y}\n");

        assert!(write_with("false", &dir), "a failing piton lost the file");
        assert_eq!(std::fs::read_to_string(file(&dir)).unwrap(), written);
        std::fs::remove_dir_all(&dir).ok();
    }
}
