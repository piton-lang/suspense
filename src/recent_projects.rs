//! The projects opened most recently, newest first, and the folder a project
//! was last opened from, saved in the platform's per-user config directory so
//! they carry across launches. Every project opened is noted as it opens, and
//! the application opens the last one again when it starts.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::project_directory::{CONFIG_FILE_NAME, ProjectDirectory};

/// How many recent projects are kept.
pub const MAX_RECENT: usize = 5;

/// What is saved.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecentProjects {
    /// The projects' folders, newest first.
    pub projects: Vec<PathBuf>,
    /// The folder a project was last opened from, in the project browser.
    pub last_browsed: Option<PathBuf>,
}

impl RecentProjects {
    /// Notes `dir` as opened just now.
    pub fn note(&mut self, dir: &Path) {
        self.projects.retain(|project| project != dir);
        self.projects.insert(0, dir.to_path_buf());
        self.projects.truncate(MAX_RECENT);
    }

    /// The recent projects still there to open: each still holding its
    /// piton.config.pi.
    pub fn openable(&self) -> Vec<PathBuf> {
        self.projects
            .iter()
            .filter(|dir| dir.join(CONFIG_FILE_NAME).is_file())
            .cloned()
            .collect()
    }

    /// Reads what `file` holds, or nothing when it can't be read.
    pub fn load(file: &Path) -> Self {
        fs::read_to_string(file)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        fs::write(file, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("could not save {}", file.display()))
    }
}

/// The recent projects as the application keeps them, and the file they are
/// saved in, when there is one.
struct Store {
    recent: RecentProjects,
    file: Option<PathBuf>,
}

impl Global for Store {}

/// Where they are saved for the user.
pub fn default_file() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("suspense")
            .join("recent-projects.json"),
    )
}

/// Starts keeping the recent projects in `file`, loading what it holds, and
/// noting every project opened from now on. Returns the last project opened,
/// if it can still be opened.
pub fn init(file: Option<PathBuf>, cx: &mut App) -> Option<PathBuf> {
    let recent = file
        .as_deref()
        .map(RecentProjects::load)
        .unwrap_or_default();
    let last = recent.openable().into_iter().next();
    cx.set_global(Store { recent, file });
    cx.observe_global::<ProjectDirectory>(|cx| {
        if let Some(dir) = ProjectDirectory::get(cx) {
            update(cx, |recent| recent.note(&dir));
        }
    })
    .detach();
    last
}

/// The recent projects, or none while they aren't kept.
pub fn get(cx: &App) -> RecentProjects {
    cx.try_global::<Store>()
        .map(|store| store.recent.clone())
        .unwrap_or_default()
}

/// Notes that a project was last opened from `dir`.
pub fn set_last_browsed(dir: &Path, cx: &mut App) {
    let dir = dir.to_path_buf();
    update(cx, move |recent| recent.last_browsed = Some(dir));
}

fn update(cx: &mut App, change: impl FnOnce(&mut RecentProjects)) {
    if cx.try_global::<Store>().is_none() {
        return;
    }
    let store = cx.global_mut::<Store>();
    change(&mut store.recent);
    if let Some(file) = &store.file {
        // Only a convenience: a list that can't be saved is kept for the
        // session.
        store.recent.save(file).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECENT, RecentProjects};

    /// Opening a project moves it to the front, once, keeping the newest few;
    /// only those still holding a config can be opened; and it all saves and
    /// loads back.
    #[test]
    fn notes_saves_and_loads_recent_projects() {
        let dir = std::env::temp_dir().join(format!("suspense-recent-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let mut recent = RecentProjects::default();
        for n in 0..7 {
            let project = dir.join(format!("p{n}"));
            std::fs::create_dir_all(&project).unwrap();
            std::fs::write(project.join("piton.config.pi"), "").unwrap();
            recent.note(&project);
        }
        recent.note(&dir.join("p4"));
        let names: Vec<String> = recent
            .projects
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["p4", "p6", "p5", "p3", "p2"]);
        assert_eq!(recent.projects.len(), MAX_RECENT);

        std::fs::remove_file(dir.join("p6/piton.config.pi")).unwrap();
        assert_eq!(recent.openable().len(), 4);

        recent.last_browsed = Some(dir.clone());
        let file = dir.join("recent.json");
        recent.save(&file).unwrap();
        assert_eq!(RecentProjects::load(&file), recent);
        assert_eq!(
            RecentProjects::load(&dir.join("missing.json")),
            RecentProjects::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
