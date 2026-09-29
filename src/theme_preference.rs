//! The user's light or dark mode choice, and each mode's brightness, saved in
//! the platform's per-user config directory so they carry across launches and
//! projects.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
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

/// Saves the mode showing as the user's choice, and each mode's brightness,
/// as the Brightness slider leaves them. Tests neither read nor write it.
pub fn save_appearance(cx: &App) -> Result<()> {
    save_brightness(cx);
    if cfg!(test) {
        return Ok(());
    }
    save(cx.theme().mode, &file()?)
}

fn file() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| anyhow!("no config directory"))?;
    Ok(config_dir.join(APP_DIR).join(FILE_NAME))
}

/// Where each mode's brightness is kept, beside the light or dark mode choice.
const BRIGHTNESS_FILE_NAME: &str = "brightness";

/// Restores each mode's saved brightness, 0 for a mode never set. Call before
/// the theme's mode is applied, so the window opens at it.
pub fn restore_brightness(cx: &mut App) {
    let (dark, light) = brightness_file()
        .and_then(|file| load_brightness(&file))
        .unwrap_or_default();
    crate::theme::set_brightness(true, dark, cx);
    crate::theme::set_brightness(false, light, cx);
}

/// Saves both modes' brightness as they are now. It is only a convenience, so
/// failing to is ignored, and the setting still holds for the session.
pub fn save_brightness(cx: &App) {
    let (dark, light) = (
        crate::theme::brightness_of(true, cx),
        crate::theme::brightness_of(false, cx),
    );
    if let Some(file) = brightness_file() {
        save_brightness_to(dark, light, &file).ok();
    }
}

/// The brightness file; none in tests, which neither read nor write it.
fn brightness_file() -> Option<PathBuf> {
    let file = dirs::config_dir()?.join(APP_DIR).join(BRIGHTNESS_FILE_NAME);
    (!cfg!(test)).then_some(file)
}

/// Dark and light mode's brightness from `file`, a line for each mode, such
/// as `dark -2`; a mode missing or unreadable is at 0.
fn load_brightness(file: &Path) -> Option<(i32, i32)> {
    let text = fs::read_to_string(file).ok()?;
    let mut steps = (0, 0);
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(mode), Some(step)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(step) = step.parse::<i32>() else {
            continue;
        };
        match mode {
            "dark" => steps.0 = step,
            "light" => steps.1 = step,
            _ => {}
        }
    }
    Some(steps)
}

fn save_brightness_to(dark: i32, light: i32, file: &Path) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(file, format!("dark {dark}\nlight {light}\n"))
        .with_context(|| format!("could not save {}", file.display()))
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

    use super::{load, load_brightness, save, save_brightness_to};

    /// Each mode's brightness is saved on its own, and loads back as it was;
    /// never saved, both are at 0 once anything is; a mode missing is at 0.
    #[test]
    fn brightness_is_saved_per_mode_and_restored() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/brightness-preference-test");
        let file = dir.join("brightness");
        fs::remove_dir_all(&dir).ok();

        assert_eq!(load_brightness(&file), None);
        save_brightness_to(-3, 2, &file).unwrap();
        assert_eq!(load_brightness(&file), Some((-3, 2)));
        save_brightness_to(1, 0, &file).unwrap();
        assert_eq!(load_brightness(&file), Some((1, 0)));
        fs::write(&file, "light -4\n").unwrap();
        assert_eq!(load_brightness(&file), Some((0, -4)));
        fs::remove_dir_all(&dir).ok();
    }

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
