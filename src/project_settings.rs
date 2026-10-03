//! The project's own settings, kept with it in `.suspense/settings.json`, as
//! the ProjectDataScope says, and edited in the settings: for now only
//! whether its Code tasks run in a container.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::hidden_anchor::APP_DIR;

const FILE: &str = "settings.json";

/// What a project's settings say; each left out is as it starts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    /// Whether Code tasks and a Chain prompt's code step run in a container,
    /// as the ContainerEnvironmentScope says; off to start with.
    #[serde(default)]
    pub run_code_tasks_in_container: bool,
}

fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

/// The settings of the project at `project_dir`, as they start where they
/// can't be read.
pub fn read(project_dir: &Path) -> ProjectSettings {
    fs::read_to_string(file(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Saves `settings` with the project at `project_dir`.
pub fn write(project_dir: &Path, settings: &ProjectSettings) -> Result<()> {
    let file = file(project_dir);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(&file, serde_json::to_string_pretty(settings)? + "\n")
        .with_context(|| format!("could not save {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The option is off until it is turned on, and kept once it is.
    #[test]
    fn running_code_in_a_container_is_off_until_turned_on() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/project-settings-test");
        fs::remove_dir_all(&dir).ok();
        assert!(!read(&dir).run_code_tasks_in_container);
        write(
            &dir,
            &ProjectSettings {
                run_code_tasks_in_container: true,
            },
        )
        .unwrap();
        assert!(read(&dir).run_code_tasks_in_container);
        assert!(
            fs::read_to_string(file(&dir))
                .unwrap()
                .contains("\"runCodeTasksInContainer\": true")
        );
    }
}
