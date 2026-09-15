//! The project directory: the directory holding the `piton.config.pi` the user
//! picked. Kept for the lifetime of the session only.

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

    pub fn set(dir: PathBuf, cx: &mut App) {
        cx.set_global(Self(Some(dir)));
    }

    /// Opens the platform file picker for a `piton.config.pi` file and resolves
    /// to its directory, or `None` if the user cancelled.
    pub fn pick(cx: &App) -> Task<Result<Option<PathBuf>>> {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(format!("Select {CONFIG_FILE_NAME}").into()),
        });

        cx.background_spawn(async move {
            let Some(file) = paths.await??.and_then(|paths| paths.into_iter().next()) else {
                return Ok(None);
            };
            if file.file_name().is_none_or(|name| name != CONFIG_FILE_NAME) {
                anyhow::bail!("Expected {CONFIG_FILE_NAME}, got {}", file.display());
            }
            let dir = file
                .parent()
                .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", file.display()))?;
            Ok(Some(dir.to_path_buf()))
        })
    }
}
