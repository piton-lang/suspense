//! The project directory: the directory holding the `piton.config.pi` the user
//! picked. Kept for the lifetime of the session only. It is set by opening a
//! project (see [`crate::project::open_project`]) or creating one.

use std::path::PathBuf;

use gpui_kit::*;

pub const CONFIG_FILE_NAME: &str = "piton.config.pi";

#[derive(Default)]
pub struct ProjectDirectory(Option<PathBuf>);

impl Global for ProjectDirectory {}

impl ProjectDirectory {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
    }

    pub fn get(cx: &App) -> Option<PathBuf> {
        cx.global::<Self>().0.clone()
    }

    /// Puts the project at `dir` on screen. A folder is the same project
    /// however it was reached, so it is kept by its canonical path.
    pub fn set(dir: PathBuf, cx: &mut App) {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        cx.set_global(Self(Some(dir)));
    }
}
