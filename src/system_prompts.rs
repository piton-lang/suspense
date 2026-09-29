//! The system prompt each mode gives a prompt's hidden anchor. Each is a
//! template saved with the project, one file per mode in
//! `.suspense/system-prompts`, edited by hand or in the settings window (see
//! [`crate::settings_window`]). In a template, `${CODE_LOCATION}` and
//! `${SPEC_LOCATION}` stand for `codeRoot` and `root` in `piton.config.pi`,
//! `${HARNESS_DIRECTORY}` for the harness's directory (see
//! [`crate::harness::directory`]), `${SPEC_READING}` for the spec-reading
//! prompt injected into it, saved beside the templates, and
//! `${PITON_FLUENCY}` for the project's Piton fluency (see
//! [`crate::piton_fluency`]), written like Piton interpolations and filled in
//! before the prompt is compiled. `${UNDERSTANDING_FILE}` stands for the
//! task's understanding file (see [`crate::understanding`]); it reaches the
//! compiled prompt as written, and is filled in as the prompt is sent, once
//! the task's history record is named. Otherwise, like the prompt it is sent with, a template is Piton
//! prose, so it can reference the spec. A Code task sent to Spec is also given
//! the code-to-spec prompt, saved beside the templates, whose `${CODE_PROMPT}`
//! and `${CODE_RESULT}` are filled in with the code task's prompt and final
//! output as it is sent, as text (see [`fill_code_task`]). The code task a
//! Chain task hands on to is given the spec-to-code prompt in the same way,
//! its `${SPEC_PROMPT}` and `${SPEC_RESULT}` filled in with the Chain task's.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::chat_input::SendMode;
use crate::hidden_anchor::APP_DIR;

pub const CODE_LOCATION: &str = "${CODE_LOCATION}";
pub const SPEC_LOCATION: &str = "${SPEC_LOCATION}";
pub const HARNESS_DIRECTORY: &str = "${HARNESS_DIRECTORY}";
pub const SPEC_READING: &str = "${SPEC_READING}";
pub const PITON_FLUENCY: &str = "${PITON_FLUENCY}";
pub const UNDERSTANDING_FILE: &str = "${UNDERSTANDING_FILE}";
/// In the code-to-spec prompt, the code task's prompt as it was typed, and its
/// final output: filled in as the prompt is sent, never read as Piton.
pub const CODE_PROMPT: &str = "${CODE_PROMPT}";
pub const CODE_RESULT: &str = "${CODE_RESULT}";
/// In the spec-to-code prompt, the spec task's prompt as it was typed, and its
/// final output, filled in as the code task's are in the code-to-spec prompt.
pub const SPEC_PROMPT: &str = "${SPEC_PROMPT}";
pub const SPEC_RESULT: &str = "${SPEC_RESULT}";

/// What CODE_RESULT is filled in with for a code task that left no final
/// output.
pub const NO_CODE_RESULT: &str = "The code task left no final output: it failed, was \
    cancelled, or said nothing after its last tool call.";

/// What SPEC_RESULT is filled in with for a spec task that left no final
/// output.
pub const NO_SPEC_RESULT: &str = "The spec task left no final output: it said nothing after \
    its last tool call.";

const DIR: &str = "system-prompts";

/// A prompt saved in `.suspense/system-prompts`: a mode's template, the
/// spec-reading prompt injected into them, the code-to-spec prompt added to
/// a Code task sent to Spec, or the spec-to-code prompt added to the Code task
/// a Chain task hands on to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prompt {
    Mode(SendMode),
    SpecReading,
    CodeToSpec,
    SpecToCode,
}

impl Prompt {
    /// Every prompt: the modes' templates, in order, then the injected ones.
    /// Freeform, which sends no system prompt, has none.
    pub const ALL: [Prompt; 7] = [
        Prompt::Mode(SendMode::ALL[0]),
        Prompt::Mode(SendMode::ALL[1]),
        Prompt::Mode(SendMode::ALL[2]),
        Prompt::Mode(SendMode::ALL[3]),
        Prompt::SpecReading,
        Prompt::CodeToSpec,
        Prompt::SpecToCode,
    ];

    /// The name its file is saved under.
    pub fn key(self) -> &'static str {
        match self {
            Prompt::Mode(mode) => mode.key(),
            Prompt::SpecReading => "spec-reading",
            Prompt::CodeToSpec => "code-to-spec",
            Prompt::SpecToCode => "spec-to-code",
        }
    }

    /// What it is called in the settings.
    pub fn label(self) -> &'static str {
        match self {
            Prompt::Mode(mode) => mode.label(),
            Prompt::SpecReading => "Spec reading",
            Prompt::CodeToSpec => "Code to spec",
            Prompt::SpecToCode => "Spec to code",
        }
    }
}

impl From<SendMode> for Prompt {
    fn from(mode: SendMode) -> Self {
        Prompt::Mode(mode)
    }
}

/// The text a prompt starts with, and is reset to: this repository's own
/// `.suspense/system-prompts`, baked in when the application is built, so
/// editing those files changes the defaults the next build ships with.
pub fn default_prompt(prompt: impl Into<Prompt>) -> &'static str {
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
    // Trimmed as a saved prompt is when it loads, so one left as the default
    // reads as the default.
    match prompt.into() {
        Prompt::Mode(SendMode::Code) => baked!("code"),
        Prompt::Mode(SendMode::Both) => baked!("combined"),
        Prompt::Mode(SendMode::Spec) => baked!("spec"),
        Prompt::Mode(SendMode::Ask) => baked!("ask"),
        // Freeform sends no system prompt, so it has no template.
        Prompt::Mode(SendMode::Freeform) => "",
        Prompt::SpecReading => baked!("spec-reading"),
        Prompt::CodeToSpec => baked!("code-to-spec"),
        Prompt::SpecToCode => baked!("spec-to-code"),
    }
    .trim_end()
}

/// The directory the project's prompts are saved in.
pub fn dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(DIR)
}

/// The file `prompt` is saved in.
pub fn file(prompt: impl Into<Prompt>, project_dir: &Path) -> PathBuf {
    dir(project_dir).join(format!("{}.md", prompt.into().key()))
}

/// `prompt` for the project: its saved file, or the default while there is
/// none.
pub fn load(prompt: impl Into<Prompt>, project_dir: &Path) -> Result<String> {
    let prompt = prompt.into();
    let file = file(prompt, project_dir);
    match fs::read_to_string(&file) {
        Ok(text) => Ok(text.trim_end().to_string()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(default_prompt(prompt).to_string()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Saves `text` as `prompt` for the project.
pub fn save(prompt: impl Into<Prompt>, text: &str, project_dir: &Path) -> Result<()> {
    let dir = dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let file = file(prompt, project_dir);
    fs::write(&file, format!("{}\n", text.trim_end()))
        .with_context(|| format!("could not save {}", file.display()))
}

/// Saves the default of every prompt the project has no file for, so each can
/// be found and edited by hand.
pub fn save_missing(project_dir: &Path) -> Result<()> {
    for prompt in Prompt::ALL {
        if !file(prompt, project_dir).exists() {
            save(prompt, default_prompt(prompt), project_dir)?;
        }
    }
    Ok(())
}

/// `template` with `placeholder` filled in with `value`; an empty value
/// leaves no paragraph of its own behind.
fn fill_paragraph(template: &str, placeholder: &str, value: &str) -> String {
    let template = if value.trim().is_empty() {
        let template = template.replace(&format!("\n\n{placeholder}"), "");
        match template.strip_prefix(&format!("{placeholder}\n\n")) {
            Some(rest) => rest.to_string(),
            None => template,
        }
    } else {
        template.to_string()
    };
    template.replace(placeholder, value.trim_end())
}

/// `template` with the spec-reading prompt `reading` injected, then the code
/// and spec locations, the harness's directory, and the Piton fluency filled
/// in, in it and in what was injected; an empty reading or fluency leaves no
/// paragraph of its own behind. In `reading`, `${SPEC_READING}` is left as
/// written.
pub fn fill(template: &str, code: &str, spec: &str, reading: &str, fluency: &str) -> String {
    let template = fill_paragraph(template, SPEC_READING, reading)
        .replace(CODE_LOCATION, code)
        .replace(SPEC_LOCATION, spec)
        .replace(HARNESS_DIRECTORY, crate::harness::directory());
    // Last, so what piton printed reaches the harness exactly as printed.
    fill_paragraph(&template, PITON_FLUENCY, fluency)
        .trim_end()
        .to_string()
}

/// A compiled system prompt with the task it was handed from filled in: for a
/// Spec task sent from a Code task, `${CODE_PROMPT}` with its prompt as typed,
/// and `${CODE_RESULT}` with its final output, or with [`NO_CODE_RESULT`] when
/// it left none; for the Code task a Chain task hands on to, `${SPEC_PROMPT}`
/// and `${SPEC_RESULT}` in the same way, with [`NO_SPEC_RESULT`]. All are
/// taken as text: filled in at once, in one pass, so none is filled in again
/// from what another holds, and after compiling, so nothing in them is ever
/// read as Piton.
pub fn fill_code_task(system_prompt: &str, prompt: &str, result: Option<&str>) -> String {
    let result = result
        .map(str::trim_end)
        .filter(|result| !result.trim().is_empty());
    let code_result = result.unwrap_or(NO_CODE_RESULT);
    let spec_result = result.unwrap_or(NO_SPEC_RESULT);
    let prompt = prompt.trim_end();
    let mut filled = String::with_capacity(system_prompt.len() + prompt.len() + code_result.len());
    let mut rest = system_prompt;
    loop {
        let next = [
            (CODE_PROMPT, prompt),
            (CODE_RESULT, code_result),
            (SPEC_PROMPT, prompt),
            (SPEC_RESULT, spec_result),
        ]
        .into_iter()
        .filter_map(|(placeholder, value)| {
            rest.find(placeholder).map(|at| (at, placeholder, value))
        })
        .min_by_key(|(at, ..)| *at);
        let Some((at, placeholder, value)) = next else {
            filled.push_str(rest);
            return filled;
        };
        filled.push_str(&rest[..at]);
        filled.push_str(value);
        rest = &rest[at + placeholder.len()..];
    }
}

/// A compiled system prompt with `${UNDERSTANDING_FILE}` filled in with
/// `path`, the understanding file's path from the project directory, or with
/// nothing for a question, which has none.
pub fn fill_understanding(system_prompt: &str, path: Option<&str>) -> String {
    system_prompt.replace(UNDERSTANDING_FILE, path.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        CODE_PROMPT, CODE_RESULT, NO_CODE_RESULT, NO_SPEC_RESULT, PITON_FLUENCY, Prompt,
        SPEC_PROMPT, SPEC_READING, SPEC_RESULT, UNDERSTANDING_FILE, default_prompt, file, fill,
        fill_code_task, fill_understanding, load, save, save_missing,
    };
    use crate::chat_input::SendMode;

    /// Freeform sends no system prompt: it has no template, none is saved
    /// for it, and a prompt sent in it is given none.
    #[test]
    fn freeform_has_no_system_prompt() {
        assert!(!Prompt::ALL.contains(&Prompt::Mode(SendMode::Freeform)));
        assert_eq!(default_prompt(SendMode::Freeform), "");
        let dir =
            std::env::temp_dir().join(format!("suspense-freeform-prompt-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        save_missing(&dir).unwrap();
        assert!(!file(SendMode::Freeform, &dir).exists());
        assert!(
            crate::hidden_anchor::system_prompt(SendMode::Freeform, &dir, "")
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// Each mode's default is this repository's own saved template for it,
    /// injects the spec-reading prompt, which tells the harness to follow
    /// references in its own directory, gives the Piton fluency, and fills in
    /// the configured locations and the fluency. Code, Chain, and
    /// Spec then end asking for the understanding file; Ask ends with the
    /// fluency.
    #[test]
    fn defaults_are_this_repositorys_templates() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let reading = default_prompt(Prompt::SpecReading);
        let saved = fs::read_to_string(file(Prompt::SpecReading, manifest)).unwrap();
        assert_eq!(reading, saved.trim_end());
        assert!(
            reading.starts_with("Before executing anything, read the spec it touches.")
                && reading.contains(
                    "Links to ${HARNESS_DIRECTORY}/reference/… in the prompt are the spec for \
                     what they name. Read each one before changing anything it covers."
                ),
            "the spec reading doesn't say to follow references: {reading}"
        );
        for mode in SendMode::ALL {
            let saved = fs::read_to_string(file(mode, manifest)).unwrap();
            assert_eq!(default_prompt(mode), saved.trim_end(), "{mode:?}");
            assert!(
                default_prompt(mode).contains(&format!("\n\n{SPEC_READING}\n\n{PITON_FLUENCY}")),
                "{mode:?} doesn't inject the spec reading"
            );
            let understanding = "Before changing anything, write the constraints this task \
                must meet to ${UNDERSTANDING_FILE}, and keep it current";
            let asks = mode == SendMode::Ask;
            let ending = if asks {
                format!("\n\n{PITON_FLUENCY}")
            } else {
                format!("\n\n{PITON_FLUENCY}\n\n{understanding}")
            };
            let template = default_prompt(mode);
            assert!(
                if asks {
                    template.ends_with(&ending)
                } else {
                    template.contains(&ending) && template.ends_with("rather than adding another.")
                },
                "{mode:?} doesn't end as it should: {template}"
            );
            let filled = fill(template, "./src", "./spec", reading, "# Piton fluency");
            assert!(filled.contains("\n\n# Piton fluency"), "{mode:?}: {filled}");
            assert!(
                filled.contains("Links to .claude/reference/… in the prompt"),
                "{mode:?}: {filled}"
            );
            // Filled in only once the task's history record is named.
            assert_eq!(filled.contains(UNDERSTANDING_FILE), !asks, "{mode:?}");
            let sent = fill_understanding(
                &filled,
                Some(".suspense/history/1-Prompt_a.understanding.md"),
            );
            assert!(!sent.contains("${"), "{mode:?} left a placeholder: {sent}");
            assert_eq!(
                sent.contains("to .suspense/history/1-Prompt_a.understanding.md, and"),
                !asks,
                "{mode:?}: {sent}"
            );
            // Without piton, nothing is left in its place.
            let without = fill(template, "./src", "./spec", reading, "");
            assert!(
                without.contains("covers.\n\nBefore changing")
                    || asks && without.ends_with("covers."),
                "{mode:?}: {without}"
            );
        }
        assert!(
            fill(default_prompt(SendMode::Code), "./src", "./spec", "", "").contains("./src"),
            "the code location isn't filled in"
        );
    }

    /// The code-to-spec prompt's default is this repository's own saved
    /// one, which says what the spec gives it to say; it is filled in as a
    /// template is, but for its own two placeholders.
    #[test]
    fn code_to_spec_default_is_this_repositorys() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let default = default_prompt(Prompt::CodeToSpec);
        let saved = fs::read_to_string(file(Prompt::CodeToSpec, manifest)).unwrap();
        assert_eq!(default, saved.trim_end());
        assert_eq!(
            default,
            "This prompt was first sent to change the code, and the code has been\n\
             changed to meet it. Now write the spec to describe what was built:\n\
             read the code the task changed, and change the spec at ${SPEC_LOCATION}\n\
             so it describes that code as it now is, without changing the code at\n\
             ${CODE_LOCATION}. Where the code did something the prompt didn't ask\n\
             for, describe what the code does, and say so in your reply.\n\
             \n\
             The prompt the code task was sent:\n\
             \n\
             ${CODE_PROMPT}\n\
             \n\
             What the code task said it built, its final output:\n\
             \n\
             ${CODE_RESULT}"
        );
        assert_eq!(Prompt::CodeToSpec.key(), "code-to-spec");
        assert_eq!(Prompt::CodeToSpec.label(), "Code to spec");
        assert_eq!(Prompt::ALL[Prompt::ALL.len() - 2], Prompt::CodeToSpec);
        let filled = fill(default, "./src", "./spec", "Read.", "");
        assert!(filled.contains("the spec at ./spec\n") && filled.contains("\n./src."));
        assert!(filled.contains(CODE_PROMPT) && filled.contains(CODE_RESULT));
    }

    /// The spec-to-code prompt, given to the Code task a Chain task hands on
    /// to, is this repository's own saved one, the last prompt, filled in as a
    /// template is but for the spec task's prompt and final output, which the
    /// spec task fills in as a code task fills in its own.
    #[test]
    fn spec_to_code_default_is_this_repositorys() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let default = default_prompt(Prompt::SpecToCode);
        let saved = fs::read_to_string(file(Prompt::SpecToCode, manifest)).unwrap();
        assert_eq!(default, saved.trim_end());
        assert_eq!(Prompt::SpecToCode.key(), "spec-to-code");
        assert_eq!(Prompt::SpecToCode.label(), "Spec to code");
        assert_eq!(Prompt::ALL.last(), Some(&Prompt::SpecToCode));
        let filled = fill(default, "./src", "./spec", "Read.", "");
        assert!(filled.contains("./src") && filled.contains("./spec"));
        assert!(filled.contains(SPEC_PROMPT) && filled.contains(SPEC_RESULT));
        assert_eq!(
            fill_code_task(
                "Asked ${SPEC_PROMPT}; did ${SPEC_RESULT}",
                "it",
                Some("that")
            ),
            "Asked it; did that"
        );
        assert_eq!(
            fill_code_task("Did: ${SPEC_RESULT}", "it", None),
            format!("Did: {NO_SPEC_RESULT}")
        );
    }

    /// The code task fills in its two placeholders as text, in one pass, so
    /// neither is filled in from what the other holds; one that left no final
    /// output gives a line saying so.
    #[test]
    fn code_tasks_fill_in_as_text() {
        let template = "Asked:\n\n${CODE_PROMPT}\n\nDid:\n\n${CODE_RESULT}\n\n${CODE_PROMPT}";
        assert_eq!(
            fill_code_task(
                template,
                "Put ${CODE_RESULT} {here}\n",
                Some("Said ${CODE_PROMPT} \\ ")
            ),
            "Asked:\n\nPut ${CODE_RESULT} {here}\n\nDid:\n\nSaid ${CODE_PROMPT} \\\n\n\
             Put ${CODE_RESULT} {here}"
        );
        for none in [None, Some(""), Some(" \n ")] {
            assert_eq!(
                fill_code_task("Did: ${CODE_RESULT}", "x", none),
                format!("Did: {NO_CODE_RESULT}")
            );
        }
        assert_eq!(
            fill_code_task("Nothing to fill.", "x", None),
            "Nothing to fill."
        );
    }

    /// The spec reading is injected with its own placeholders filled in, and
    /// leaves no paragraph behind when blank; the fluency is taken as printed.
    #[test]
    fn injects_the_spec_reading() {
        let template = "${SPEC_READING}\n\nCode in ${CODE_LOCATION}.\n\n${PITON_FLUENCY}";
        assert_eq!(
            fill(
                template,
                "./src",
                "./spec",
                "See ${HARNESS_DIRECTORY} and ${SPEC_LOCATION}, not ${SPEC_READING}.",
                "Keep ${CODE_LOCATION}",
            ),
            "See .claude and ./spec, not ${SPEC_READING}.\n\nCode in ./src.\n\nKeep ${CODE_LOCATION}"
        );
        assert_eq!(
            fill(template, "./src", "./spec", "  \n", ""),
            "Code in ./src."
        );
        assert_eq!(
            fill("A.\n\n${SPEC_READING}\n\nB.", "", "", "", ""),
            "A.\n\nB."
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
            default_prompt(SendMode::Spec)
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
        for prompt in Prompt::ALL {
            assert!(
                file(prompt, &project_dir).exists(),
                "{prompt:?} was not saved"
            );
        }
        assert_eq!(
            load(SendMode::Spec, &project_dir).unwrap(),
            "Only the spec, in ${SPEC_LOCATION}."
        );
        assert_eq!(
            load(SendMode::Ask, &project_dir).unwrap(),
            default_prompt(SendMode::Ask)
        );
    }
}
