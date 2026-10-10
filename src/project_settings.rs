//! The project's own settings, in `.suspense/settings.json`, as the
//! ProjectDataScope says: the harness its runs go to, as the
//! HarnessIntegrationScope says, and whether its Spec runs may read the
//! code, as the ContainerEnvironmentScope says. A project without the file, or with one
//! that doesn't read back, has every setting off.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::hidden_anchor::APP_DIR;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectSettings {
    /// The command of the harness its runs go to, until one is saved the
    /// first its config builds for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// A Spec task, or a Chain prompt's spec step, is given the code
    /// location, read only.
    pub spec_reads_code: bool,
    /// Such a run may read the whole project as well, read only, but for its
    /// git directory and data; only ever on while `spec_reads_code` is.
    pub spec_reads_project: bool,
}

/// The settings file of the project at `project_dir`.
pub fn file(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join("settings.json")
}

impl ProjectSettings {
    /// The settings of the project at `project_dir`.
    pub fn load(project_dir: &Path) -> Self {
        std::fs::read_to_string(file(project_dir))
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Saves these as the settings of the project at `project_dir`.
    pub fn save(&self, project_dir: &Path) -> Result<()> {
        let file = file(project_dir);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("could not save {}", file.display()))
    }
}

/// Whether the project at `project_dir` lets its Spec runs read the code.
pub fn spec_reads_code(project_dir: &Path) -> bool {
    ProjectSettings::load(project_dir).spec_reads_code
}

/// Whether the project at `project_dir` lets its Spec runs read the whole
/// project too, which it does only while they may read the code.
pub fn spec_reads_project(project_dir: &Path) -> bool {
    let settings = ProjectSettings::load(project_dir);
    settings.spec_reads_code && settings.spec_reads_project
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off to start with, and kept once saved.
    #[test]
    fn settings_save_and_load_back() {
        let dir = std::env::temp_dir().join(format!("suspense-settings-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        assert!(!spec_reads_code(&dir));
        ProjectSettings { spec_reads_code: true, spec_reads_project: false, ..Default::default() }.save(&dir).unwrap();
        assert!(spec_reads_code(&dir) && !spec_reads_project(&dir));
        // The whole project only ever with the code.
        ProjectSettings { spec_reads_code: false, spec_reads_project: true, ..Default::default() }.save(&dir).unwrap();
        assert!(!spec_reads_project(&dir));
        ProjectSettings { spec_reads_code: true, spec_reads_project: true, ..Default::default() }.save(&dir).unwrap();
        assert!(spec_reads_project(&dir));
        let saved = std::fs::read_to_string(file(&dir)).unwrap();
        assert!(saved.contains("\"specReadsCode\": true"), "{saved}");
        std::fs::write(file(&dir), "not json").unwrap();
        assert!(!spec_reads_code(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
