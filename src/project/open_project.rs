//! Open Project: a file browser for a project's piton.config.pi, shown in the
//! main window's inset panel; the folder holding the file chosen becomes the
//! project directory.

use std::path::Path;

use gpui_kit::*;

use crate::fs_browser::{Browse, FsBrowser, home};
use crate::project_directory::{CONFIG_FILE_NAME, ProjectDirectory};
use crate::recent_projects;

actions!(suspense, [OpenProject]);

/// The browser for opening a project, starting in the folder a project was
/// last opened from, or else the folder holding the open project, or the home
/// folder.
pub fn picker(cx: &mut App) -> Entity<FsBrowser> {
    let start = recent_projects::get(cx)
        .last_browsed
        .filter(|dir| dir.is_dir())
        .or_else(|| ProjectDirectory::get(cx).and_then(|dir| dir.parent().map(Path::to_path_buf)))
        .unwrap_or_else(home);
    cx.new(|cx| {
        FsBrowser::new(
            format!("Open a project: choose its {CONFIG_FILE_NAME}"),
            Browse::File(Some(CONFIG_FILE_NAME.to_string())),
            start,
            cx,
        )
    })
}

/// Opens the project whose piton.config.pi is `config`, remembering the
/// folder it was chosen in for next time.
pub fn open(config: &Path, cx: &mut App) {
    if let Some(dir) = config.parent() {
        recent_projects::set_last_browsed(dir, cx);
        ProjectDirectory::set(dir.to_path_buf(), cx);
    }
}
