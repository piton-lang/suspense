//! The system prompt each mode gives a prompt's hidden anchor. Each is a
//! template saved with the project, one file per mode in
//! `.suspense/system-prompts`, edited by hand or in the settings window (see
//! [`crate::settings_window`]). In a template, `${CODE_LOCATION}` and
//! `${SPEC_LOCATION}` stand for `codeRoot` and `root` in `piton.config.pi`,
//! written like Piton interpolations and filled in before the prompt is
//! compiled. Otherwise, like the prompt it is sent with, a template is Piton
//! prose, so it can reference the spec.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::chat_input::SendMode;
use crate::hidden_anchor::APP_DIR;

pub const CODE_LOCATION: &str = "${CODE_LOCATION}";
pub const SPEC_LOCATION: &str = "${SPEC_LOCATION}";

const DIR: &str = "system-prompts";

/// The template a mode starts with, and is reset to: this repository's own
/// `.suspense/system-prompts`, baked in when the application is built, so
/// editing those files changes the defaults the next build ships with.
pub fn default_template(mode: SendMode) -> &'static str {
    macro_rules! baked {
        ($key:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/.suspense/system-prompts/",
                $key,
                ".md"
            ))
        };
    }
    // Trimmed as a saved template is when it loads, so a template left as
    // the default reads as the default.
    match mode {
        SendMode::Code => baked!("code"),
        SendMode::Both => baked!("combined"),
        SendMode::Spec => baked!("spec"),
        SendMode::Ask => baked!("ask"),
    }
    .trim_end()
}

/// The directory the project's templates are saved in.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(DIR)
}

/// The file `mode`'s template is saved in.
pub fn file(mode: SendMode, project_dir: &Path) -> PathBuf {
    dir(project_dir).join(format!("{}.md", mode.key()))
}

/// `mode`'s template for the project: its saved file, or the default while
/// there is none.
pub fn load(mode: SendMode, project_dir: &Path) -> Result<String> {
    let file = file(mode, project_dir);
    match fs::read_to_string(&file) {
        Ok(template) => Ok(template.trim_end().to_string()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(default_template(mode).to_string()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Saves `template` as `mode`'s for the project.
pub fn save(mode: SendMode, template: &str, project_dir: &Path) -> Result<()> {
    let dir = dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let file = file(mode, project_dir);
    fs::write(&file, format!("{}\n", template.trim_end()))
        .with_context(|| format!("could not save {}", file.display()))
}

/// Saves the default template of every mode the project has no file for, so
/// each can be found and edited by hand.
pub fn save_missing(project_dir: &Path) -> Result<()> {
    for mode in SendMode::ALL {
        if !file(mode, project_dir).exists() {
            save(mode, default_template(mode), project_dir)?;
        }
    }
    Ok(())
}

/// `template` with the code and spec locations filled in.
pub fn fill(template: &str, code: &str, spec: &str) -> String {
    template
        .replace(CODE_LOCATION, code)
        .replace(SPEC_LOCATION, spec)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{default_template, file, fill, load, save, save_missing};
    use crate::chat_input::SendMode;

    /// Each mode's default is this repository's own saved template for it,
    /// and fills in the configured locations.
    #[test]
    fn defaults_are_this_repositorys_templates() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for mode in SendMode::ALL {
            let saved = fs::read_to_string(file(mode, manifest)).unwrap();
            assert_eq!(default_template(mode), saved.trim_end(), "{mode:?}");
            let filled = fill(default_template(mode), "./src", "./spec");
            assert!(
                !filled.contains("${"),
                "{mode:?} left a placeholder: {filled}"
            );
        }
        assert!(
            fill(default_template(SendMode::Code), "./src", "./spec").contains("./src"),
            "the code location isn't filled in"
        );
    }

    /// Templates load as their default until saved, and as saved after;
    /// saving the missing ones leaves an edited one alone.
    #[test]
    fn templates_are_saved_with_the_project() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/system-prompts-test");
        fs::remove_dir_all(&project_dir).ok();

        assert_eq!(
            load(SendMode::Spec, &project_dir).unwrap(),
            default_template(SendMode::Spec)
        );
        save(
            SendMode::Spec,
            "Only the spec, in ${SPEC_LOCATION}.\n\n",
            &project_dir,
        )
        .unwrap();
        assert_eq!(
            load(SendMode::Spec, &project_dir).unwrap(),
            "Only the spec, in ${SPEC_LOCATION}."
        );

        save_missing(&project_dir).unwrap();
        for mode in SendMode::ALL {
            assert!(file(mode, &project_dir).exists(), "{mode:?} was not saved");
        }
        assert_eq!(
            load(SendMode::Spec, &project_dir).unwrap(),
            "Only the spec, in ${SPEC_LOCATION}."
        );
        assert_eq!(
            load(SendMode::Ask, &project_dir).unwrap(),
            default_template(SendMode::Ask)
        );
    }
}
