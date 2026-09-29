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
/// as `dark-level -12`, in channel values; a mode missing or unreadable is
/// at 0. A line from before, such as `dark -2`, counted the slider's old
/// steps of 2% each, and is read as the same brightness.
fn load_brightness(file: &Path) -> Option<(i32, i32)> {
    let text = fs::read_to_string(file).ok()?;
    // Each mode's level, and its old step, if given.
    let mut found: [(Option<i32>, Option<i32>); 2] = Default::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(value) = value.parse::<i32>() else {
            continue;
        };
        match key {
            "dark-level" => found[0].0 = Some(value),
            "light-level" => found[1].0 = Some(value),
            "dark" => found[0].1 = Some(value),
            "light" => found[1].1 = Some(value),
            _ => {}
        }
    }
    let level = |(level, old): (Option<i32>, Option<i32>)| {
        // An old step was 5.1 channel values, rounded half away from 0.
        level
            .or(old.map(|step| (step * 51 + 5 * step.signum()) / 10))
            .unwrap_or(0)
    };
    Some((level(found[0]), level(found[1])))
}

fn save_brightness_to(dark: i32, light: i32, file: &Path) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(file, format!("dark-level {dark}\nlight-level {light}\n"))
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
        save_brightness_to(-17, 10, &file).unwrap();
        assert_eq!(load_brightness(&file), Some((-17, 10)));
        save_brightness_to(40, 0, &file).unwrap();
        assert_eq!(load_brightness(&file), Some((40, 0)));
        fs::write(&file, "light-level -60\n").unwrap();
        assert_eq!(load_brightness(&file), Some((0, -60)));
        // Saved before, in steps of 2%, it loads as the same brightness.
        fs::write(&file, "dark -3\nlight 2\n").unwrap();
        assert_eq!(load_brightness(&file), Some((-15, 10)));
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
