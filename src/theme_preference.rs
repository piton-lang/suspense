//! The user's light or dark mode choice, saved in the platform's per-user
//! config directory so it carries across launches and projects.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

/// Where the application keeps its per-user files, inside the config directory.
const APP_DIR: &str = "suspense";
const FILE_NAME: &str = "theme";

/// Applies the saved choice to the theme, or follows the system's appearance
/// if none has been made.
pub fn apply(window: &mut Window, cx: &mut App) {
    match file().ok().and_then(|file| load(&file)) {
        Some(mode) => Theme::change(mode, Some(window), cx),
        None => Theme::sync_system_appearance(Some(window), cx),
    }
}

/// Switches the theme to `mode` and saves it as the user's choice.
pub fn set(mode: ThemeMode, window: &mut Window, cx: &mut App) -> Result<()> {
    Theme::change(mode, Some(window), cx);
    save(mode, &file()?)
}

fn file() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| anyhow!("no config directory"))?;
    Ok(config_dir.join(APP_DIR).join(FILE_NAME))
}

fn load(file: &Path) -> Option<ThemeMode> {
    match fs::read_to_string(file).ok()?.trim() {
        "light" => Some(ThemeMode::Light),
        "dark" => Some(ThemeMode::Dark),
        _ => None,
    }
}

fn save(mode: ThemeMode, file: &Path) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(file, mode.name()).with_context(|| format!("could not save {}", file.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use gpui_kit::component::ThemeMode;

    use super::{load, save};

    #[test]
    fn saved_choice_loads_back() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/theme-preference-test");
        let file = dir.join("theme");
        fs::remove_dir_all(&dir).ok();

        assert_eq!(load(&file), None);
        save(ThemeMode::Dark, &file).unwrap();
        assert_eq!(load(&file), Some(ThemeMode::Dark));
        save(ThemeMode::Light, &file).unwrap();
        assert_eq!(load(&file), Some(ThemeMode::Light));
    }
}
