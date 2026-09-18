//! The project's Piton fluency: what `piton claude --print-prompt` prints, run
//! in the project directory, filled in for `${PITON_FLUENCY}` in a mode's
//! system prompt (see [`crate::system_prompts`]). It is run once per project,
//! in the background as the project is opened, and kept while the application
//! runs; a prompt sent before it has finished waits for it. Without `piton`, or
//! when it fails, the fluency is empty.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

type Cache = Mutex<HashMap<PathBuf, Arc<OnceLock<String>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The project's fluency, running `piton` for it the first time it is asked
/// for. Blocks until it has run, so call it off the UI thread.
pub fn get(project_dir: &Path) -> String {
    let cell = cache()
        .lock()
        .unwrap()
        .entry(project_dir.to_path_buf())
        .or_default()
        .clone();
    cell.get_or_init(|| print_prompt("piton", project_dir))
        .clone()
}

/// What `program claude --print-prompt` prints in `project_dir`, or nothing
/// when it can't be run or fails.
fn print_prompt(program: &str, project_dir: &Path) -> String {
    match Command::new(program)
        .args(["claude", "--print-prompt"])
        .current_dir(project_dir)
        .output()
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
}
