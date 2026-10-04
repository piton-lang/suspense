//! The hidden anchor. Prompts are typed as plain text, but behind the scenes
//! they are the `userPrompt` of a randomly-named, exported Piton anchor whose
//! imports `piton lsp` adds automatically (see [`crate::piton_lsp`]), so they
//! are never written or seen. Each sent prompt is saved as a `.pi` file in the
//! project's prompt history and compiled; the harness receives the prompt with
//! each reference in it resolved.
//!
//! A prompt may hold anything, pasted text included, so it isn't written as
//! Piton prose, where braces, colons, leading dashes, comments, and
//! backslashes all mean something. It is written in a multi-line escape block
//! instead, which Piton keeps exactly as it is. Each `@{Name}` or
//! `${Name.path}` in it whose name is imported is also listed, once, in the
//! anchor's `references`, which is what `piton compile` resolves; the prompt's
//! text around them is taken from the saved source. A reference to a name
//! that isn't imported, such as a pasted `${HOME}`, is only text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::chat_input::SendMode;
use crate::project_directory::CONFIG_FILE_NAME;
use crate::system_prompts;

/// Indentation of each prompt line inside the anchor's `userPrompt` block.
pub const PROMPT_INDENT: &str = "        ";

/// Where the application keeps its files, relative to the project directory.
pub const APP_DIR: &str = ".suspense";
const HISTORY_DIR: &str = "history";
/// Where questions asked from the Ask tab are saved, apart from the history.
const ASKS_DIR: &str = "asks";

/// The hidden anchor's imports: names by module.
#[derive(Clone, Debug, Default)]
pub struct Imports(BTreeMap<String, BTreeSet<String>>);

impl Imports {
    /// Adds the names of every `from <module> import A, B` line in `source`,
    /// ignoring other lines. Returns whether any name was new.
    pub fn add_from_source(&mut self, source: &str) -> bool {
        let mut added = false;
        for line in source.lines() {
            let Some((module, names)) = line
                .trim()
                .strip_prefix("from ")
                .and_then(|rest| rest.split_once(" import "))
            else {
                continue;
            };
            let names: Vec<&str> = names
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            if names.is_empty() {
                continue;
            }
            let imported = self.0.entry(absolute_module(module.trim())).or_default();
            for name in names {
                added |= imported.insert(name.to_string());
            }
        }
        added
    }

    /// Whether `name` is imported.
    pub fn has(&self, name: &str) -> bool {
        self.0.values().any(|names| {
            names
                .iter()
                .any(|imported| imported.rsplit(' ').next() == Some(name))
        })
    }

    /// The modules names are imported from, absolute from the spec root.
    pub fn modules(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Adds every name in `other`.
    pub fn extend(&mut self, other: &Imports) {
        for (module, names) in &other.0 {
            self.0
                .entry(module.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
    }
}

/// Relative imports come from a draft sitting at the spec root; the absolute
/// form resolves the same from anywhere in the project, history files included.
fn absolute_module(module: &str) -> String {
    match module.strip_prefix("./") {
        Some(path) => format!("/{path}"),
        None => module.to_string(),
    }
}

pub struct HiddenAnchor {
    name: String,
    pub imports: Imports,
    /// The mode the prompt was sent in, written after the `userPrompt`.
    pub mode: Option<SendMode>,
    /// Sent with the `piton slice` of each spec it references, rather than
    /// links to them, written as `sliced: true` after the mode.
    pub sliced: bool,
    /// Whether the prompt starts a new conversation rather than carrying on
    /// the one its tab shares, written as `newConversation` after `sliced`;
    /// none for a prompt saved before this was kept.
    pub new_conversation: Option<bool>,
    /// Text attached to the prompt, written as `attachedText` after the mode:
    /// each piece a list of quoted lines, taken as it is rather than as Piton.
    pub attached_text: Vec<String>,
    /// Images attached to the prompt, by their paths from the project
    /// directory once saved in its data, written as `attachedImages` after
    /// the attached text: each path an escape block, as attached text is.
    pub attached_images: Vec<String>,
    /// The `systemPrompt` written after the attached images, if any.
    pub system_prompt: Option<String>,
    /// For a Code task sent to Spec, the code task it was sent from, and for
    /// a chain's code step, the chain's spec step, written as `codeTask` after
    /// the system prompt, so it is sent the same way again.
    pub code_task: Option<CodeTask>,
    /// For a task sent to the other mode, from Code or from Spec, the name of
    /// the hidden anchor of the task it was sent from, written as `sentFrom`
    /// after the code task; none for any other prompt, and for one saved
    /// before this was kept.
    pub sent_from: Option<String>,
    /// For a Chain task, and the code step it goes on to, that the code step
    /// is followed by a post-build spec update, written as
    /// `postBuildSpecUpdate: true` after `sentFrom`.
    pub post_build_update: bool,
}

/// What is attached to a prompt: pieces of text, and images saved in the
/// project's data, by their paths from the project directory, each in the
/// order it was attached.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attached {
    pub text: Vec<String>,
    pub images: Vec<String>,
}

impl From<Vec<String>> for Attached {
    fn from(text: Vec<String>) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }
}

/// The task a task was handed on from: for a Spec task sent from Code, the
/// code task, and for a chain's code step, the chain's spec step. Its prompt
/// as typed, and its final output, none if it left none, fill in the
/// code-to-spec or spec-to-code prompt's placeholders as text (see
/// [`system_prompts::fill_code_task`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodeTask {
    pub prompt: String,
    pub result: Option<String>,
}

/// A compiled hidden anchor.
pub struct CompiledPrompt {
    pub user_prompt: String,
    pub system_prompt: Option<String>,
    /// The code task it was sent from, which fills in its instructions as it
    /// is sent (see [`Self::instructions_as_sent`]).
    pub code_task: Option<CodeTask>,
    /// The images attached to it, to give the harness alongside the prompt,
    /// in the order they were attached; never part of the prompt's text.
    pub images: Vec<PathBuf>,
}

impl CompiledPrompt {
    /// Its instructions as they head its message: `${UNDERSTANDING_FILE}`
    /// filled in with `understanding`, the understanding file's path from the
    /// project directory, or nothing for a question; then the code task it was
    /// sent from, last, so nothing in what that holds is filled in.
    pub fn instructions_as_sent(&self, understanding: Option<&str>) -> Option<String> {
        let system_prompt = self.system_prompt.as_deref()?;
        let filled = system_prompts::fill_understanding(system_prompt, understanding);
        Some(match &self.code_task {
            Some(code) => {
                system_prompts::fill_code_task(&filled, &code.prompt, code.result.as_deref())
            }
            None => filled,
        })
    }
}

impl HiddenAnchor {
    /// A freshly named anchor with no imports and no system prompt.
    pub fn random() -> Self {
        Self {
            name: Self::random_name(),
            imports: Imports::default(),
            mode: None,
            sliced: false,
            new_conversation: None,
            attached_text: Vec::new(),
            attached_images: Vec::new(),
            system_prompt: None,
            code_task: None,
            sent_from: None,
            post_build_update: false,
        }
    }

    /// What is attached to it.
    #[cfg(test)]
    pub fn attached(&self) -> Attached {
        Attached {
            text: self.attached_text.clone(),
            images: self.attached_images.clone(),
        }
    }

    /// Attaches `attached` to it, in place of anything attached before.
    pub fn attach(&mut self, attached: Attached) {
        self.attached_text = attached.text;
        self.attached_images = attached.images;
    }

    /// A fresh name for an anchor, as [`Self::random`] gives it, so a prompt
    /// can be known by its anchor's name before the anchor is resolved.
    pub fn random_name() -> String {
        format!(
            "Prompt_{:016x}",
            RandomState::new().hash_one(SystemTime::now())
        )
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Names it `name`, one from [`Self::random_name`].
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }

    /// The Piton source of this anchor with `prompt` as its `userPrompt`.
    pub fn source(&self, prompt: &str) -> String {
        let mut source = String::new();
        for (module, names) in &self.imports.0 {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            writeln!(source, "from {module} import {}", names.join(", ")).ok();
        }
        writeln!(source, "\nexport anchor {}:\n    userPrompt:", self.name).ok();
        push_block(&mut source, PROMPT_INDENT, prompt);
        if let Some(mode) = self.mode {
            source.push('\n');
            writeln!(source, "{MODE_PREFIX}{}", mode.key()).ok();
        }
        if self.sliced {
            source.push('\n');
            writeln!(source, "{SLICED_LINE}").ok();
        }
        if let Some(new_conversation) = self.new_conversation {
            source.push('\n');
            writeln!(source, "{NEW_CONVERSATION_PREFIX}{new_conversation}").ok();
        }
        if !self.attached_text.is_empty() {
            source.push('\n');
            source.push_str(ATTACHED_TEXT_LINE);
            source.push('\n');
            for (index, text) in self.attached_text.iter().enumerate() {
                writeln!(source, "{PROMPT_INDENT}{ATTACHMENT_KEY}{}:", index + 1).ok();
                push_block(&mut source, ATTACHMENT_INDENT, text);
            }
        }
        if !self.attached_images.is_empty() {
            source.push('\n');
            source.push_str(ATTACHED_IMAGES_LINE);
            source.push('\n');
            for (index, path) in self.attached_images.iter().enumerate() {
                writeln!(source, "{PROMPT_INDENT}{IMAGE_KEY}{}:", index + 1).ok();
                push_block(&mut source, ATTACHMENT_INDENT, path);
            }
        }
        if let Some(system_prompt) = &self.system_prompt {
            source.push('\n');
            source.push_str(SYSTEM_PROMPT_LINE);
            source.push('\n');
            push_block(&mut source, PROMPT_INDENT, system_prompt);
        }
        if let Some(code_task) = &self.code_task {
            source.push('\n');
            source.push_str(CODE_TASK_LINE);
            source.push('\n');
            writeln!(source, "{PROMPT_INDENT}{CODE_PROMPT_KEY}").ok();
            push_block(&mut source, ATTACHMENT_INDENT, &code_task.prompt);
            if let Some(result) = &code_task.result {
                writeln!(source, "{PROMPT_INDENT}{CODE_RESULT_KEY}").ok();
                push_block(&mut source, ATTACHMENT_INDENT, result);
            }
        }
        if let Some(sent_from) = &self.sent_from {
            source.push('\n');
            writeln!(source, "{SENT_FROM_PREFIX}{sent_from}").ok();
        }
        if self.post_build_update {
            source.push('\n');
            writeln!(source, "{POST_BUILD_UPDATE_LINE}").ok();
        }
        let references = self.references(prompt);
        if !references.is_empty() {
            source.push('\n');
            source.push_str(REFERENCES_LINE);
            source.push('\n');
            for reference in references {
                writeln!(source, "{PROMPT_INDENT}- {reference}").ok();
            }
        }
        source
    }

    /// The anchor as `piton lsp` is shown it while the prompt is typed: the
    /// prompt as prose, laid out line for line as typed, so positions in it
    /// match the input's, with anything outside a reference that Piton would
    /// read as more than text blanked out, character for character. Pasted
    /// braces, quotes, or colons then never stop the server finding what
    /// needs importing.
    pub fn draft_source(&self, prompt: &str) -> String {
        let mut source = String::new();
        for (module, names) in &self.imports.0 {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            writeln!(source, "from {module} import {}", names.join(", ")).ok();
        }
        writeln!(source, "\nanchor {}:\n    userPrompt:", self.name).ok();
        for line in prompt.split('\n') {
            source.push_str(PROMPT_INDENT);
            source.push_str(&blank_for_lsp(line.trim_end_matches('\r')));
            source.push('\n');
        }
        source
    }

    /// Each reference to an imported name in `prompt`, then in the system
    /// prompt, once, in the order they first appear.
    fn references<'a>(&'a self, prompt: &'a str) -> Vec<&'a str> {
        let mut references = Vec::new();
        for text in std::iter::once(prompt).chain(self.system_prompt.as_deref()) {
            for piece in pieces(text) {
                if let Piece::Reference(reference, name) = piece
                    && self.imports.has(name)
                    && !references.contains(&reference)
                {
                    references.push(reference);
                }
            }
        }
        references
    }

    /// `text` with each reference to an imported name replaced by what it
    /// resolved to, and everything else as it is.
    fn resolve(&self, text: &str, resolved: &BTreeMap<String, String>) -> String {
        pieces(text)
            .into_iter()
            .map(|piece| match piece {
                Piece::Reference(reference, name) if self.imports.has(name) => {
                    resolved.get(reference).map_or(reference, String::as_str)
                }
                Piece::Reference(text, _) | Piece::Text(text) => text,
            })
            .collect()
    }

    /// Reads back an anchor saved with [`Self::source`], with the prompt it
    /// holds. Anchors saved by earlier versions, their text as quoted pieces
    /// or as prose, read back too.
    pub fn parse(source: &str) -> Option<(Self, String)> {
        let mut imports = Imports::default();
        let mut lines = source.lines();
        let name = loop {
            let line = lines.next()?;
            if let Some(name) = line
                .strip_prefix("export ")
                .unwrap_or(line)
                .strip_prefix("anchor ")
                .and_then(|rest| rest.strip_suffix(':'))
            {
                break name.to_string();
            }
            imports.add_from_source(line);
        };
        if lines.next()?.trim() != "userPrompt:" {
            return None;
        }
        let lines: Vec<&str> = lines.collect();
        let (prompt, mut rest) = match read_escaped(&lines, PROMPT_INDENT) {
            Some(read) => read,
            None => {
                // Prompt lines are indented deeper, so only the properties
                // themselves match these lines.
                let property = |line: &&str| {
                    *line == SYSTEM_PROMPT_LINE
                        || *line == ATTACHED_TEXT_LINE
                        || *line == ATTACHED_IMAGES_LINE
                        || *line == SLICED_LINE
                        || line.starts_with(NEW_CONVERSATION_PREFIX)
                        || line.starts_with(MODE_PREFIX)
                };
                let end = lines.iter().position(property).unwrap_or(lines.len());
                let prompt = &lines[..end];
                let prompt = prompt.strip_suffix(&[""]).unwrap_or(prompt);
                (read_block(prompt), &lines[end..])
            }
        };
        // A blank line separates each property from the next.
        rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        let mut mode = None;
        if let Some(key) = rest.first().and_then(|line| line.strip_prefix(MODE_PREFIX)) {
            mode = SendMode::from_key(key.trim());
            rest = &rest[1..];
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        let sliced = rest.first() == Some(&SLICED_LINE);
        if sliced {
            rest = &rest[1..];
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        // Saved only by versions that keep whether a prompt starts a new
        // conversation.
        let new_conversation = rest
            .first()
            .and_then(|line| line.strip_prefix(NEW_CONVERSATION_PREFIX))
            .and_then(|value| match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        if new_conversation.is_some() {
            rest = &rest[1..];
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        let mut attached_text = Vec::new();
        if rest.first() == Some(&ATTACHED_TEXT_LINE) {
            rest = &rest[1..];
            let mut quoted: Vec<Vec<String>> = Vec::new();
            while let Some(line) = rest.first() {
                if line
                    .strip_prefix(PROMPT_INDENT)
                    .and_then(|key| key.strip_prefix(ATTACHMENT_KEY))
                    .is_some_and(|key| key.ends_with(':'))
                {
                    let (text, after) = read_escaped(&rest[1..], ATTACHMENT_INDENT)?;
                    attached_text.push(text);
                    rest = after;
                    continue;
                } else if *line == OLD_ATTACHMENT_ITEM {
                    quoted.push(Vec::new());
                } else if let Some(line) = line.strip_prefix(OLD_ATTACHMENT_LINE_PREFIX) {
                    quoted.last_mut()?.push(unquote(line)?);
                } else {
                    break;
                }
                rest = &rest[1..];
            }
            attached_text.extend(quoted.into_iter().map(|lines| lines.join("\n")));
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        // Saved only by versions that attach images.
        let mut attached_images = Vec::new();
        if rest.first() == Some(&ATTACHED_IMAGES_LINE) {
            rest = &rest[1..];
            while rest.first().is_some_and(|line| {
                line.strip_prefix(PROMPT_INDENT)
                    .and_then(|key| key.strip_prefix(IMAGE_KEY))
                    .is_some_and(|key| key.ends_with(':'))
            }) {
                let (path, after) = read_escaped(&rest[1..], ATTACHMENT_INDENT)?;
                attached_images.push(path);
                rest = after;
            }
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        let system_prompt = match rest.first() {
            Some(&SYSTEM_PROMPT_LINE) => Some(match read_escaped(&rest[1..], PROMPT_INDENT) {
                Some((text, after)) => {
                    rest = after.strip_prefix(&[""]).unwrap_or(after);
                    text
                }
                None => {
                    let end = rest[1..]
                        .iter()
                        .position(|line| *line == REFERENCES_LINE)
                        .map_or(rest.len(), |end| end + 1);
                    let text = read_block(&rest[1..end]);
                    rest = &rest[end..];
                    text
                }
            }),
            _ => None,
        };
        // Saved only by versions that send a Code task to Spec with it.
        let code_task = match rest.first() {
            Some(&CODE_TASK_LINE) => {
                let key = |line: Option<&&str>, key: &str| {
                    line.and_then(|line| line.strip_prefix(PROMPT_INDENT)) == Some(key)
                };
                if !key(rest.get(1), CODE_PROMPT_KEY) {
                    return None;
                }
                let (prompt, after) = read_escaped(&rest[2..], ATTACHMENT_INDENT)?;
                let (result, after) = if key(after.first(), CODE_RESULT_KEY) {
                    let (result, after) = read_escaped(&after[1..], ATTACHMENT_INDENT)?;
                    (Some(result), after)
                } else {
                    (None, after)
                };
                rest = after.strip_prefix(&[""]).unwrap_or(after);
                Some(CodeTask { prompt, result })
            }
            _ => None,
        };
        // Saved only by versions that keep which task a task sent to the other
        // mode was sent from.
        let sent_from = rest
            .first()
            .and_then(|line| line.strip_prefix(SENT_FROM_PREFIX))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        if rest
            .first()
            .is_some_and(|line| line.starts_with(SENT_FROM_PREFIX))
        {
            rest = &rest[1..];
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        // Saved only by versions that send a chain as its steps.
        let post_build_update = rest.first() == Some(&POST_BUILD_UPDATE_LINE);
        Some((
            Self {
                name,
                imports,
                mode,
                sliced,
                new_conversation,
                attached_text,
                attached_images,
                system_prompt,
                code_task,
                sent_from,
                post_build_update,
            },
            prompt,
        ))
    }

    /// The zero-based line of [`Self::source`] holding the prompt's first line.
    pub fn prompt_first_line(&self) -> u32 {
        self.imports.0.len() as u32 + 3
    }
}

/// The start of the `mode` line of [`HiddenAnchor::source`].
const MODE_PREFIX: &str = "    mode: ";

/// The line of [`HiddenAnchor::source`] saying the prompt is sent sliced.
const SLICED_LINE: &str = "    sliced: true";

/// The start of the `newConversation` line of [`HiddenAnchor::source`].
const NEW_CONVERSATION_PREFIX: &str = "    newConversation: ";

/// The line opening the `systemPrompt` property of [`HiddenAnchor::source`].
const SYSTEM_PROMPT_LINE: &str = "    systemPrompt:";

/// The line opening the `codeTask` property of [`HiddenAnchor::source`], and
/// the keys of the code task's prompt and final output in it, each followed
/// by its escape block.
const CODE_TASK_LINE: &str = "    codeTask:";
const CODE_PROMPT_KEY: &str = "prompt:";
const CODE_RESULT_KEY: &str = "result:";

/// The start of the `sentFrom` line of [`HiddenAnchor::source`].
const SENT_FROM_PREFIX: &str = "    sentFrom: ";

/// The line of [`HiddenAnchor::source`] saying a chain's code step is
/// followed by a post-build spec update.
const POST_BUILD_UPDATE_LINE: &str = "    postBuildSpecUpdate: true";

/// The line opening the `references` property of [`HiddenAnchor::source`].
const REFERENCES_LINE: &str = "    references:";

/// The line opening the `attachedText` property of [`HiddenAnchor::source`],
/// what each piece of text in it is keyed by, followed by its number, and
/// the indentation of the piece's escape block.
const ATTACHED_TEXT_LINE: &str = "    attachedText:";
const ATTACHMENT_KEY: &str = "text";
const ATTACHMENT_INDENT: &str = "            ";

/// The line opening the `attachedImages` property of
/// [`HiddenAnchor::source`], and what each image's path in it is keyed by,
/// followed by its number; each path is an escape block, as attached text is.
const ATTACHED_IMAGES_LINE: &str = "    attachedImages:";
const IMAGE_KEY: &str = "image";

/// Adds `text` to `source` as a multi-line escape block at `indent`, which
/// Piton keeps exactly as it is. Its fence is three backslashes, or one more
/// than any line of the text made only of backslashes, so nothing in the text
/// closes it early.
fn push_block(source: &mut String, indent: &str, text: &str) {
    let fence = "\\".repeat(fence_length(text));
    writeln!(source, "{indent}{fence}").ok();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            source.push('\n');
        } else {
            writeln!(source, "{indent}{line}").ok();
        }
    }
    writeln!(source, "{indent}{fence}").ok();
}

/// How many backslashes fence `text`'s escape block.
fn fence_length(text: &str) -> usize {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.chars().all(|c| c == '\\'))
        .map(|line| line.len() + 1)
        .max()
        .unwrap_or(0)
        .max(3)
}

/// The text of an escape block written by [`push_block`] at `indent` at the
/// start of `lines`, and the lines after it; `None` when `lines` don't start
/// with one.
fn read_escaped<'a, 'b>(lines: &'a [&'b str], indent: &str) -> Option<(String, &'a [&'b str])> {
    let fence = lines.first()?.strip_prefix(indent)?;
    if fence.len() < 3 || !fence.chars().all(|c| c == '\\') {
        return None;
    }
    let close = format!("{indent}{fence}");
    let end = 1 + lines[1..].iter().position(|line| *line == close)?;
    let text = lines[1..end]
        .iter()
        .map(|line| line.strip_prefix(indent).unwrap_or(line.trim_start()))
        .collect::<Vec<_>>()
        .join("\n");
    Some((text, &lines[end + 1..]))
}

/// The line opening each piece of attached text, and the start of each of its
/// quoted lines, as earlier versions wrote them.
const OLD_ATTACHMENT_ITEM: &str = "        -";
const OLD_ATTACHMENT_LINE_PREFIX: &str = "            - ";

/// The line a quoted string, as earlier versions wrote them, holds.
fn unquote(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let mut line = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        line.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(line)
}

/// What the harness receives for a prompt: its compiled userPrompt, then any
/// attached text, each piece fenced so nothing in it closes the fence early.
pub fn with_attached_text(user_prompt: &str, attached_text: &[String]) -> String {
    if attached_text.is_empty() {
        return user_prompt.to_string();
    }
    let mut prompt = format!("{user_prompt}\n\nAttached text:");
    for text in attached_text {
        let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat((longest + 1).max(3));
        prompt.push_str(&format!("\n\n{fence}\n{text}\n{fence}"));
    }
    prompt
}

/// What the harness receives for a Freeform prompt: `prompt` just as it was
/// typed, then any text attached to it, and no system prompt. Nothing is
/// compiled, so neither `piton compile` nor `piton slice` is run.
pub fn freeform(prompt: &str, attached_text: &[String]) -> CompiledPrompt {
    CompiledPrompt {
        user_prompt: with_attached_text(prompt, attached_text),
        system_prompt: None,
        code_task: None,
        images: Vec::new(),
    }
}

/// The images at `paths`, from `project_dir`, as files for the harness.
pub fn image_files(paths: &[String], project_dir: &Path) -> Vec<PathBuf> {
    paths.iter().map(|path| project_dir.join(path)).collect()
}

/// The start of each piece of a line, as earlier versions wrote them.
const PIECE_PREFIX: &str = "            - ";

/// The text of a property's lines as earlier versions saved them: each line a
/// list of quoted pieces and references, or prose.
fn read_block(block: &[&str]) -> String {
    let line_item = format!("{PROMPT_INDENT}-");
    if block.first() != Some(&line_item.as_str()) {
        return block
            .iter()
            .map(|line| line.strip_prefix(PROMPT_INDENT).unwrap_or(line.trim()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let mut lines: Vec<String> = Vec::new();
    for line in block {
        if *line == line_item {
            lines.push(String::new());
        } else if let (Some(piece), Some(current)) =
            (line.strip_prefix(PIECE_PREFIX), lines.last_mut())
        {
            match unquote(piece) {
                Some(text) => current.push_str(&text),
                None => current.push_str(piece),
            }
        }
    }
    lines.join("\n")
}

/// A piece of a prompt's line: text, or a reference or interpolation with the
/// name it starts with.
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Text(&'a str),
    Reference(&'a str, &'a str),
}

/// `line` split into text and the `@{Name.path}` and `${Name.path}` in it.
fn pieces(line: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut text_start = 0;
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let reference = (rest.starts_with("@{") || rest.starts_with("${"))
            .then(|| rest[2..].find('}'))
            .flatten()
            .map(|close| &rest[2..2 + close])
            .filter(|path| is_identifier_path(path));
        match reference {
            Some(path) => {
                if text_start < at {
                    pieces.push(Piece::Text(&line[text_start..at]));
                }
                let end = at + 2 + path.len() + 1;
                let name = path.split('.').next().unwrap_or(path);
                pieces.push(Piece::Reference(&line[at..end], name));
                at = end;
                text_start = end;
            }
            None => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if text_start < line.len() {
        pieces.push(Piece::Text(&line[text_start..]));
    }
    pieces
}

/// Whether `path` is a dotted path of identifiers, each perhaps with
/// kebab-case tails, as a Piton reference is.
fn is_identifier_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|segment| {
            let mut parts = segment.split('-');
            let head = parts.next().unwrap_or_default();
            head.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && parts.all(|tail| {
                    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
        })
}

/// `line` for `piton lsp`: references, and one being typed, kept as they are;
/// letters, digits, and spaces between words kept; anything else, and
/// leading whitespace, replaced with as many underscores as it is UTF-16 units
/// long, so every position in the line stays where it was.
fn blank_for_lsp(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        if rest.starts_with("@{") || rest.starts_with("${") {
            let name_len = rest[2..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
                .unwrap_or(rest.len() - 2);
            let mut end = 2 + name_len;
            if rest[end..].starts_with('}') {
                end += 1;
            }
            out.push_str(&rest[..end]);
            at += end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        let leading = out.chars().all(|c| c == '_');
        if c.is_alphanumeric() && c.len_utf16() == 1 || (c == ' ' && !leading) {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
        at += c.len_utf8();
    }
    out
}

/// The instructions a prompt sent in `mode` is given at the top of its
/// message: the mode's template, the project's own, with the code and spec
/// locations and the fluency file filled in, but not the spec-reading
/// prompt, which the conversation's system prompt gives (see
/// [`project_system_prompt`]). Spec and Chain, which write Piton, have the
/// fluency file written first if it never has been (see
/// [`crate::piton_fluency`]), and every mode with instructions has the
/// Suspense fluency file written where it is missing or out of date (see
/// [`crate::suspense_fluency`]). A question's end with the card format (see
/// [`system_prompts::ask_cards`]). `${UNDERSTANDING_FILE}` is left as written,
/// filled in as the prompt is sent. None for a mode with none, as Freeform,
/// or a template left empty.
pub fn instructions(mode: SendMode, project_dir: &Path) -> Result<Option<String>> {
    if matches!(mode, SendMode::Spec | SendMode::Both) {
        crate::piton_fluency::ensure(project_dir);
    }
    // Freeform sends no instructions, and is never pointed at it.
    if mode != SendMode::Freeform {
        crate::suspense_fluency::ensure(project_dir);
    }
    let filled = filled_instructions(mode.into(), Some(mode), project_dir)?;
    // Every question is told the card format, after its project's own Ask
    // instructions, on a paragraph of its own.
    if mode == SendMode::Ask {
        return joined(filled, Some(system_prompts::ask_cards().to_string()));
    }
    Ok(filled)
}

/// Which of the project's locations a run in `mode` can see, the code and
/// the spec: in a container, only what is mounted, as the
/// ContainerEnvironmentScope says; on the host, both.
pub fn seen_locations(mode: Option<SendMode>, project_dir: &Path) -> (bool, bool) {
    use crate::container::RunKind;
    let _ = project_dir;
    match RunKind::of(mode) {
        Some(RunKind::Spec) => (false, true),
        // A question sees both, read only.
        Some(RunKind::Question) => (true, true),
        None => (true, true),
    }
}

/// The code and spec locations from the project's config, as a run in
/// `mode` fills them in: each it can't see, as nothing.
fn locations_for(mode: Option<SendMode>, project_dir: &Path) -> Result<(String, String)> {
    let (sees_code, sees_spec) = seen_locations(mode, project_dir);
    // Relative to the project directory, so they point where the run finds
    // them, on the host or in a container.
    let code = if sees_code {
        system_prompts::relative_location(&config_value(project_dir, "codeRoot")?, project_dir)
    } else {
        String::new()
    };
    let spec = if sees_spec {
        system_prompts::relative_location(&config_value(project_dir, "root")?, project_dir)
    } else {
        String::new()
    };
    Ok((code, spec))
}

/// The instructions a Code task sent to Spec is given: Spec's, as
/// [`instructions`] gives them, then, on a paragraph of its own, the project's
/// code-to-spec prompt, filled in as a template is. Its `${CODE_PROMPT}` and
/// `${CODE_RESULT}` are left as written, to be filled in with the code task
/// as it is sent (see [`CompiledPrompt::instructions_as_sent`]), so nothing
/// the code task said is compiled. Either left empty once filled in leaves
/// the other alone.
pub fn code_to_spec_instructions(project_dir: &Path) -> Result<Option<String>> {
    joined(
        instructions(SendMode::Spec, project_dir)?,
        filled_instructions(
            system_prompts::Prompt::CodeToSpec,
            Some(SendMode::Spec),
            project_dir,
        )?,
    )
}

/// The instructions a chain's code step is given: Code's, as
/// [`instructions`] gives them, then, on a paragraph of its own, the project's
/// spec-to-code prompt, filled in as a template is. Its `${SPEC_PROMPT}` and
/// `${SPEC_RESULT}` are left as written, to be filled in with the spec step
/// as it is sent, as a code task sent to Spec fills in its own.
pub fn spec_to_code_instructions(project_dir: &Path) -> Result<Option<String>> {
    joined(
        instructions(SendMode::Code, project_dir)?,
        filled_instructions(
            system_prompts::Prompt::SpecToCode,
            Some(SendMode::Code),
            project_dir,
        )?,
    )
}

/// The instructions a prompt in `mode` is given, handed on from another task
/// or not, as [`instructions`], [`code_to_spec_instructions`], or
/// [`spec_to_code_instructions`] gives them.
pub fn instructions_for(
    mode: SendMode,
    handed_on: bool,
    project_dir: &Path,
) -> Result<Option<String>> {
    match (handed_on, mode) {
        (true, SendMode::Spec) => code_to_spec_instructions(project_dir),
        (true, SendMode::Code) => spec_to_code_instructions(project_dir),
        _ => instructions(mode, project_dir),
    }
}

fn joined(first: Option<String>, second: Option<String>) -> Result<Option<String>> {
    Ok(match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}\n\n{second}")),
        (first, second) => first.or(second),
    })
}

/// `prompt`, the project's own, filled in as instructions are (see
/// [`system_prompts::fill_instructions`]); none when it is empty once filled
/// in.
fn filled_instructions(
    prompt: system_prompts::Prompt,
    mode: Option<SendMode>,
    project_dir: &Path,
) -> Result<Option<String>> {
    let template = system_prompts::load(prompt, project_dir)?;
    if template.trim().is_empty() {
        return Ok(None);
    }
    // Only what the run sees in its container is named.
    let (code, spec) = locations_for(mode, project_dir)?;
    // The fluency file, where it has been written.
    let fluency_file = crate::piton_fluency::file(project_dir)
        .exists()
        .then(crate::piton_fluency::relative_file);
    let suspense_file = crate::suspense_fluency::file(project_dir)
        .exists()
        .then(crate::suspense_fluency::relative_file);
    let filled = system_prompts::fill_instructions(
        &template,
        &code,
        &spec,
        system_prompts::Fluency {
            piton: fluency_file.as_deref(),
            suspense: suspense_file.as_deref(),
        },
    );
    Ok((!filled.trim().is_empty()).then_some(filled))
}

/// The project's system prompt, the same for every prompt of every
/// conversation in it, whatever the mode: its system template with the code
/// and spec locations and its spec-reading prompt filled in. It holds no
/// fluency.
#[cfg(test)]
pub fn project_system_prompt(project_dir: &Path) -> Result<Option<String>> {
    project_system_prompt_for(None, project_dir)
}

/// The project's system prompt as a conversation of runs in `mode` is sent
/// it: filled in only with the locations those runs can see, as the
/// ModeSystemPromptsScope says, so it is the same for every prompt of one
/// lane's conversation.
pub fn project_system_prompt_for(
    mode: Option<SendMode>,
    project_dir: &Path,
) -> Result<Option<String>> {
    let template = system_prompts::load(system_prompts::Prompt::System, project_dir)?;
    let reading = system_prompts::load(system_prompts::Prompt::SpecReading, project_dir)?;
    let (code, spec) = locations_for(mode, project_dir)?;
    Ok(system_prompts::project_system_prompt(
        &code, &spec, &template, &reading,
    ))
}

/// The mode of a prompt saved before modes were, read from the default system
/// prompt it was given; `None` for one sent without a system prompt, or with
/// an edited one.
pub fn mode_of(system_prompt: &str) -> Option<SendMode> {
    [
        ("We're working on the code ", SendMode::Code),
        ("We're working on the code,", SendMode::Code),
        ("We're working on both ", SendMode::Both),
        ("We're working on the spec ", SendMode::Spec),
        ("We're only asking a question ", SendMode::Ask),
    ]
    .into_iter()
    .find_map(|(start, mode)| system_prompt.starts_with(start).then_some(mode))
}

/// The path the chat input's unsent text is presented to `piton lsp` under.
/// Nothing is written there. It sits inside the spec root because `piton lsp`
/// only resolves imports, and offers auto-imports, for documents under it.
pub fn draft_path(project_dir: &Path) -> PathBuf {
    let spec_root = spec_root(project_dir).unwrap_or_default();
    project_dir.join(spec_root).join(".suspense-draft.pi")
}

/// The project's prompt history directory.
pub fn history_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(HISTORY_DIR)
}

/// Saves `prompt`, as `anchor`'s source, to the project's prompt history.
pub fn save(anchor: &HiddenAnchor, prompt: &str, project_dir: &Path) -> Result<PathBuf> {
    save_in(&history_dir(project_dir), anchor, prompt)
}

/// Compiles `prompt`, as `anchor`'s source, without keeping it: saved only
/// for as long as it takes to compile, out of the history.
pub fn preview(anchor: &HiddenAnchor, prompt: &str, project_dir: &Path) -> Result<CompiledPrompt> {
    let dir = project_dir.join(APP_DIR).join("preview");
    let file = save_in(&dir, anchor, prompt)?;
    let compiled = compile(anchor, &file, project_dir);
    fs::remove_file(&file).ok();
    // Gone too once nothing else is being previewed.
    fs::remove_dir(&dir).ok();
    compiled
}

/// Records in the saved prompt at `file` that it started a new
/// conversation, as one sent again afresh, when the conversation it was to
/// carry on couldn't be, did.
pub fn mark_new_conversation(file: &Path) -> Result<()> {
    let source =
        fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    let Some((mut anchor, text)) = HiddenAnchor::parse(&source) else {
        anyhow::bail!("could not read the prompt saved in {}", file.display());
    };
    anchor.new_conversation = Some(true);
    fs::write(file, anchor.source(&text))
        .with_context(|| format!("could not save {}", file.display()))
}

/// Where the project's questions are saved, apart from the history.
pub fn asks_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(ASKS_DIR)
}

/// Saves a question, as `anchor`'s source, where it can be compiled without
/// joining the prompt history.
pub fn save_ask(anchor: &HiddenAnchor, prompt: &str, project_dir: &Path) -> Result<PathBuf> {
    save_in(&asks_dir(project_dir), anchor, prompt)
}

fn save_in(dir: &Path, anchor: &HiddenAnchor, prompt: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let sent_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let file = dir.join(format!("{sent_at}-{}.pi", anchor.name));
    fs::write(&file, anchor.source(prompt))
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(file)
}

/// Compiles `file`, a saved `anchor`, and returns its compiled prompt text:
/// the prompt as it was saved, with each reference resolved as `piton compile`
/// resolves it, a link to its reference document for an `@{…}` and its text
/// for a `${…}`.
pub fn compile(anchor: &HiddenAnchor, file: &Path, project_dir: &Path) -> Result<CompiledPrompt> {
    let source =
        fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    let (saved, prompt) =
        HiddenAnchor::parse(&source).ok_or_else(|| anyhow!("{} is no prompt", file.display()))?;
    let output = crate::process::command("piton")
        .arg("compile")
        .arg(file)
        .current_dir(project_dir)
        .output()
        .context("could not run `piton compile`")?;
    if !output.status.success() {
        let mut message = String::from_utf8_lossy(&output.stdout).into_owned();
        message.push_str(&String::from_utf8_lossy(&output.stderr));
        return Err(anyhow!("{}", message.trim()));
    }

    let compiled: Value =
        serde_json::from_slice(&output.stdout).context("`piton compile` did not print JSON")?;
    let compiled = compiled
        .get(&anchor.name)
        .ok_or_else(|| anyhow!("the compiled output has no {}", anchor.name))?;
    let values = compiled
        .get("references")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let file_dir = file
        .parent()
        .map(|dir| dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()))
        .unwrap_or_default();
    let references: Vec<(&str, &Value)> =
        saved.references(&prompt).into_iter().zip(values).collect();
    let resolved: BTreeMap<String, String> = references
        .iter()
        .map(|&(reference, value)| {
            let text = match value {
                // Sliced, the reference names what its slice, sent after the
                // prompt, is headed by.
                Value::String(target) if reference.starts_with('@') && saved.sliced => target
                    .rsplit_once(':')
                    .map_or_else(|| target.clone(), |(_, dotted)| dotted.to_string()),
                Value::String(target) if reference.starts_with('@') => {
                    link(target, &file_dir, project_dir).unwrap_or_else(|| target.clone())
                }
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (reference.to_string(), text)
        })
        .collect();
    let mut user_prompt = saved.resolve(&prompt, &resolved);
    if saved.sliced {
        // Each spec the prompt itself, not its system prompt, refers to,
        // once, in order.
        let in_prompt: Vec<&str> = pieces(&prompt)
            .into_iter()
            .filter_map(|piece| match piece {
                Piece::Reference(reference, _) => Some(reference),
                Piece::Text(_) => None,
            })
            .collect();
        let targets: Vec<&str> = references
            .iter()
            .filter(|(reference, _)| reference.starts_with('@') && in_prompt.contains(reference))
            .filter_map(|(_, value)| value.as_str())
            .collect();
        if !targets.is_empty() {
            user_prompt.push_str("\n\nSpec slices:");
            for target in targets {
                user_prompt.push_str("\n\n");
                user_prompt.push_str(slice(target, &file_dir, project_dir).trim_end());
            }
        }
    }
    Ok(CompiledPrompt {
        user_prompt: with_attached_text(&user_prompt, &saved.attached_text),
        system_prompt: saved
            .system_prompt
            .as_deref()
            .map(|text| saved.resolve(text, &resolved)),
        code_task: saved.code_task,
        images: image_files(&saved.attached_images, project_dir),
    })
}

/// A Markdown link, from the project directory, to where the project's build
/// writes the document `target` names: `piton compile`'s reference, a module's
/// output path relative to `file_dir` and the dotted path in it after a colon.
/// A module's document is in the first adapter's reference root, at its path
/// under the spec root, or in the compiled shape, at its path under the shape
/// root; the link goes to the heading for the anchor, or the property, it
/// names when the document is built.
fn link(target: &str, file_dir: &Path, project_dir: &Path) -> Option<String> {
    let (module, dotted) = target.rsplit_once(':')?;
    let module = normalize(&file_dir.join(module)).with_extension("pi");
    let canonical = |dir: PathBuf| dunce::canonicalize(&dir).unwrap_or(dir);
    let reference_root = PathBuf::from(reference_root(project_dir));
    let shape_root = config_value(project_dir, "shapeRoot")
        .ok()
        .map(|root| canonical(project_dir.join(root)));
    let document =
        match shape_root.and_then(|root| module.strip_prefix(root).ok().map(Path::to_path_buf)) {
            Some(relative) => reference_root.join("shape").join(relative),
            None => reference_root.join(
                module
                    .strip_prefix(canonical(spec_dir(project_dir)?))
                    .ok()?,
            ),
        }
        .with_extension("md");
    let fragment = fs::read_to_string(project_dir.join(&document))
        .ok()
        .and_then(|text| fragment(&text, dotted))
        .map(|slug| format!("#{slug}"))
        .unwrap_or_default();
    let document = document
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("[{dotted}]({document}{fragment})"))
}

/// What `piton slice` prints, run in the project directory, for the anchor or
/// property `target` names, `piton compile`'s reference: a module's output
/// path relative to `file_dir` and the dotted path in it after a colon. A slice
/// that can't be had says why in its place.
fn slice(target: &str, file_dir: &Path, project_dir: &Path) -> String {
    let Some((module, dotted)) = target.rsplit_once(':') else {
        return format!("Could not slice {target}: it names no spec.");
    };
    let module = normalize(&file_dir.join(module)).with_extension("pi");
    let project = dunce::canonicalize(project_dir)
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let module = module.strip_prefix(&project).unwrap_or(&module);
    let output = crate::process::command("piton")
        .arg("slice")
        .arg(format!("{}#{dotted}", module.display()))
        .current_dir(project_dir)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            let mut message = String::from_utf8_lossy(&output.stdout).into_owned();
            message.push_str(&String::from_utf8_lossy(&output.stderr));
            format!("Could not slice {dotted}: {}", message.trim())
        }
        Err(err) => format!("Could not slice {dotted}: could not run `piton slice`: {err}"),
    }
}

/// `path` with its `.` and `..` parts worked out, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// Where the first adapter in the project's `piton.config.pi` writes its
/// reference documents, `.claude/reference` for Claude Code, `.codex/reference`
/// for Codex, and `.opencode/reference` for OpenCode.
pub fn reference_root(project_dir: &Path) -> &'static str {
    let config = fs::read_to_string(project_dir.join(CONFIG_FILE_NAME)).unwrap_or_default();
    config
        .lines()
        .skip_while(|line| line.trim() != "adapters:")
        .find_map(|line| match line.trim() {
            "- {ClaudeCodeAdapter}" => Some(".claude/reference"),
            "- {CodexAdapter}" => Some(".codex/reference"),
            "- {OpenCodeAdapter}" => Some(".opencode/reference"),
            _ => None,
        })
        .unwrap_or(".claude/reference")
}

/// The fragment of the heading in a reference document for `dotted`, an
/// anchor's name perhaps followed by the path of a property in it, as Belay
/// links it: the anchor's own heading, then each property's beneath it as far
/// as they have headings, its slug made as GitHub makes it, a repeated one
/// gaining `-1`, `-2`, and so on through the document.
fn fragment(document: &str, dotted: &str) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let headings: Vec<(usize, String, String)> = headings(document)
        .into_iter()
        .map(|(level, title)| {
            let base = heading_slug(&title);
            let count = counts.entry(base.clone()).or_default();
            let slug = match *count {
                0 => base,
                n => format!("{base}-{n}"),
            };
            *count += 1;
            (level, title, slug)
        })
        .collect();
    let mut parts = dotted.split('.');
    let anchor = title_case(parts.next()?);
    let mut position = headings
        .iter()
        .position(|(level, title, _)| *level == 1 && *title == anchor)?;
    for part in parts {
        let wanted = title_case(part);
        let level = headings[position].0;
        match headings[position + 1..]
            .iter()
            .take_while(|heading| heading.0 > level)
            .position(|heading| heading.0 == level + 1 && heading.1 == wanted)
        {
            Some(offset) => position += 1 + offset,
            None => break,
        }
    }
    Some(headings[position].2.clone())
}

/// The level and title of each ATX heading in a Markdown document, outside
/// fenced code.
fn headings(document: &str) -> Vec<(usize, String)> {
    let mut headings = Vec::new();
    let mut fence: Option<&str> = None;
    for line in document.lines() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker));
        if let Some(marker) = marker {
            match fence {
                Some(open) if open == marker => fence = None,
                None => fence = Some(marker),
                _ => {}
            }
            continue;
        }
        if fence.is_some() || line.starts_with([' ', '\t']) {
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        let rest = &line[level..];
        if (1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')) {
            headings.push((level, rest.trim().trim_end_matches('#').trim().to_string()));
        }
    }
    headings
}

/// A heading's fragment, as GitHub makes it: lowercase, spaces as hyphens,
/// and other punctuation dropped.
fn heading_slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.trim().chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            slug.extend(c.to_lowercase());
        } else if c == ' ' {
            slug.push('-');
        }
    }
    slug
}

/// A Piton name as Belay titles it: split into words at a lowercase letter
/// or digit followed by a capital, at a letter followed by a digit, and at
/// `-`, `_`, and spaces, each word capitalized; a run of capitals stays whole.
fn title_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '-' | '_' | ' ') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = i > 0 && {
            let previous = chars[i - 1];
            (previous.is_lowercase() || previous.is_ascii_digit())
                && (c.is_uppercase() || c.is_ascii_digit() && !previous.is_ascii_digit())
        };
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.is_empty() {
        return name.to_string();
    }
    words
        .iter()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads the spec root (`root:`) from the project's `piton.config.pi`.
fn spec_root(project_dir: &Path) -> Result<PathBuf> {
    config_value(project_dir, "root").map(PathBuf::from)
}

/// The spec location, as a directory of the project, when its
/// `piton.config.pi` names one.
pub fn spec_dir(project_dir: &Path) -> Option<PathBuf> {
    let root = spec_root(project_dir).ok()?;
    Some(project_dir.join(root.strip_prefix("./").unwrap_or(&root)))
}

/// The spec files `imports` import from: each module under the spec location,
/// as `<module>.pi` or `<module>/index.pi`, whichever exists. Packages, and
/// modules with no file, are left out.
pub fn spec_files(imports: &Imports, project_dir: &Path) -> Vec<PathBuf> {
    let Some(root) = spec_dir(project_dir) else {
        return Vec::new();
    };
    imports
        .modules()
        .filter_map(|module| {
            let module = module.strip_prefix('/')?;
            [
                root.join(format!("{module}.pi")),
                root.join(module).join("index.pi"),
            ]
            .into_iter()
            .find(|file| file.is_file())
        })
        .collect()
}

/// Reads the first `key:` value in the project's `piton.config.pi`.
pub fn config_value(project_dir: &Path, key: &str) -> Result<String> {
    let config_path = project_dir.join(CONFIG_FILE_NAME);
    let config = fs::read_to_string(&config_path)
        .with_context(|| format!("could not read {}", config_path.display()))?;
    config
        .lines()
        .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix(':'))
        .map(|value| value.trim().to_string())
        .ok_or_else(|| anyhow!("{} has no {key}", config_path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        CodeTask, HiddenAnchor, Imports, MODE_PREFIX, NEW_CONVERSATION_PREFIX,
        POST_BUILD_UPDATE_LINE, PROMPT_INDENT, SYSTEM_PROMPT_LINE, code_to_spec_instructions,
        compile, instructions, mode_of, project_system_prompt, spec_files,
    };
    use crate::chat_input::SendMode;
    use crate::project_directory::CONFIG_FILE_NAME;
    use crate::system_prompts;

    #[test]
    fn merges_import_edits_into_absolute_imports() {
        let mut imports = Imports::default();
        assert!(imports.add_from_source("from ./lib import Concept\n\n"));
        assert!(imports.add_from_source("from ./lib import Concept, Scope\n"));
        assert!(imports.add_from_source("from @piton/belay import ClaudeCodeAdapter\n\n"));
        assert!(!imports.add_from_source("from /lib import Scope\nanchor Other:\n"));

        let anchor = HiddenAnchor {
            name: "Prompt_test".into(),
            imports,
            mode: None,
            sliced: false,
            new_conversation: None,
            attached_text: Vec::new(),
            attached_images: Vec::new(),
            system_prompt: None,
            code_task: None,
            sent_from: None,
            post_build_update: false,
        };
        assert_eq!(
            anchor.source("hi"),
            "from /lib import Concept, Scope\n\
             from @piton/belay import ClaudeCodeAdapter\n\
             \n\
             export anchor Prompt_test:\n    userPrompt:\n        \\\\\\\n        hi\n        \\\\\\\n"
        );
    }

    #[test]
    fn saved_source_parses_back() {
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./a import A\nfrom @piton/belay import B, C\n");
        let prompt = "first\n\n    indented\nlast\n";
        let source = anchor.source(prompt);

        let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(text, prompt);
        assert_eq!(parsed.name(), anchor.name());
        assert_eq!(parsed.source(&text), source);
        assert_eq!(parsed.system_prompt, None);
        assert!(HiddenAnchor::parse("not an anchor").is_none());

        anchor.system_prompt = Some("We're working on\nthe code.".into());
        let source = anchor.source(prompt);
        assert!(
            source.contains(&format!("\n\n{SYSTEM_PROMPT_LINE}\n")),
            "{source}"
        );
        let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(text, prompt);
        assert_eq!(parsed.system_prompt, anchor.system_prompt);
        assert_eq!(parsed.source(&text), source);

        anchor.mode = Some(SendMode::Both);
        let source = anchor.source(prompt);
        assert!(
            source.contains(&format!(
                "\n\n{MODE_PREFIX}combined\n\n{SYSTEM_PROMPT_LINE}\n"
            )),
            "{source}"
        );
        let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(text, prompt);
        assert_eq!(parsed.mode, Some(SendMode::Both));
        assert_eq!(parsed.system_prompt, anchor.system_prompt);
        assert_eq!(parsed.source(&text), source);

        // Whether it starts a new conversation, either way, reads back too.
        for new_conversation in [true, false] {
            anchor.new_conversation = Some(new_conversation);
            let source = anchor.source(prompt);
            assert!(
                source.contains(&format!(
                    "\n\n{MODE_PREFIX}combined\n\n{NEW_CONVERSATION_PREFIX}{new_conversation}\n\n{SYSTEM_PROMPT_LINE}\n"
                )),
                "{source}"
            );
            let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
            assert_eq!(text, prompt);
            assert_eq!(parsed.new_conversation, Some(new_conversation));
            assert_eq!(parsed.system_prompt, anchor.system_prompt);
            assert_eq!(parsed.source(&text), source);
        }
    }

    /// A project without saved templates gives each mode's instructions as
    /// its default, naming this repository's code and spec locations from its
    /// `piton.config.pi`, without the spec-reading prompt, and the mode reads
    /// back from them. Only Spec and Chain point at the fluency file.
    #[test]
    fn instructions_fill_in_the_template() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/system-prompt-test");
        fs::remove_dir_all(&project_dir).ok();
        fs::create_dir_all(&project_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_FILE_NAME),
            project_dir.join(CONFIG_FILE_NAME),
        )
        .unwrap();

        // Freeform has no instructions at all.
        assert_eq!(
            instructions(SendMode::Freeform, &project_dir).unwrap(),
            None
        );
        // Written already, so no piton is run for it.
        let fluency = crate::piton_fluency::file(&project_dir);
        fs::create_dir_all(fluency.parent().unwrap()).unwrap();
        fs::write(&fluency, "# Fluency\n").unwrap();
        let fluency_file = crate::piton_fluency::relative_file();
        let reading = system_prompts::default_prompt(system_prompts::Prompt::SpecReading);
        let suspense_file = crate::suspense_fluency::relative_file();
        for mode in SendMode::ALL
            .into_iter()
            .filter(|mode| *mode != SendMode::Freeform)
        {
            let given = instructions(mode, &project_dir).unwrap().unwrap();
            // Only the locations its run can see in its container are named:
            // a Spec or Chain run sees no code; a question sees both.
            let (code, spec) = match mode {
                SendMode::Spec | SendMode::Both => ("", "./spec"),
                _ => ("./src", "./spec"),
            };
            let template = system_prompts::fill_instructions(
                system_prompts::default_prompt(mode),
                code,
                spec,
                system_prompts::Fluency {
                    piton: Some(&fluency_file),
                    suspense: Some(&suspense_file),
                },
            );
            // A question ends with the card format, on a paragraph of its
            // own; no task is given it.
            let expected = if mode == SendMode::Ask {
                format!("{template}\n\n{}", system_prompts::ask_cards())
            } else {
                template
            };
            assert_eq!(given, expected);
            assert_eq!(
                given.contains("suspense-prompt"),
                mode == SendMode::Ask,
                "{mode:?}: {given}"
            );
            // Every mode with instructions points at the Suspense fluency.
            assert!(
                given.contains(&format!("read {suspense_file} once")),
                "{mode:?} isn't pointed at the Suspense fluency: {given}"
            );
            if code.is_empty() {
                assert!(!given.contains("./src"), "{mode:?} names the code: {given}");
            }
            if spec.is_empty() {
                assert!(
                    !given.contains("./spec"),
                    "{mode:?} names the spec: {given}"
                );
            }
            assert!(
                !given.contains(&reading[..40]),
                "{mode:?} repeats the spec reading"
            );
            assert_eq!(
                given.contains(&format!("read {fluency_file} once")),
                matches!(mode, SendMode::Spec | SendMode::Both),
                "{mode:?}: {given}"
            );
            assert!(!given.contains("# Fluency"), "{mode:?} holds the fluency");
            assert_eq!(mode_of(&given), Some(mode));
        }
        assert_eq!(mode_of("Something else."), None);

        system_prompts::save(
            SendMode::Code,
            "Code in ${CODE_LOCATION} only.",
            &project_dir,
        )
        .unwrap();
        assert_eq!(
            instructions(SendMode::Code, &project_dir)
                .unwrap()
                .as_deref(),
            Some("Code in ./src only.")
        );
        // A question left with no Ask instructions of its own is still told
        // the card format.
        system_prompts::save(SendMode::Ask, "", &project_dir).unwrap();
        assert_eq!(
            instructions(SendMode::Ask, &project_dir).unwrap().as_deref(),
            Some(system_prompts::ask_cards())
        );
        // A template saved before instructions were sent apart from the
        // system prompt, still naming the spec reading and the fluency, gives
        // neither.
        system_prompts::save(
            SendMode::Spec,
            "Spec only.\n\n${SPEC_READING}\n\n${PITON_FLUENCY}\n\nWrite ${UNDERSTANDING_FILE}.",
            &project_dir,
        )
        .unwrap();
        assert_eq!(
            instructions(SendMode::Spec, &project_dir)
                .unwrap()
                .as_deref(),
            Some("Spec only.\n\nWrite ${UNDERSTANDING_FILE}.")
        );
        system_prompts::save(SendMode::Spec, "${PITON_FLUENCY}", &project_dir).unwrap();
        assert_eq!(instructions(SendMode::Spec, &project_dir).unwrap(), None);
        fs::remove_dir_all(&project_dir).ok();
    }

    /// The project's system prompt is its system template with the locations
    /// and the spec-reading prompt filled in: the same however often it is
    /// built, with nothing in it particular to a prompt, and no fluency, even
    /// with the fluency file written.
    #[test]
    fn the_project_system_prompt_is_the_same_every_time() {
        let project_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/project-system-prompt-test");
        fs::remove_dir_all(&project_dir).ok();
        fs::create_dir_all(&project_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_FILE_NAME),
            project_dir.join(CONFIG_FILE_NAME),
        )
        .unwrap();
        let fluency = crate::piton_fluency::file(&project_dir);
        fs::create_dir_all(fluency.parent().unwrap()).unwrap();
        fs::write(&fluency, "# Fluency\n").unwrap();
        let first = project_system_prompt(&project_dir).unwrap().unwrap();
        let second = project_system_prompt(&project_dir).unwrap().unwrap();
        assert_eq!(first, second);
        assert!(first.contains("./src") && first.contains("./spec"));
        assert!(first.contains("Before executing anything, read the spec"));
        assert!(first.contains("compiled reference under .claude/reference"));
        assert!(
            !first.contains("Fluency") && !first.contains("fluency.md"),
            "the system prompt points at the fluency: {first}"
        );
        assert!(!first.contains("${"), "a placeholder is left: {first}");
        fs::remove_dir_all(&project_dir).ok();
    }

    /// A prompt's imports name the spec files they come from, as
    /// `<module>.pi` or `<module>/index.pi`; packages name none.
    #[test]
    fn imports_name_their_spec_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/spec-files-test");
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("spec/b")).unwrap();
        fs::write(dir.join(CONFIG_FILE_NAME), "root: ./spec\n").unwrap();
        fs::write(dir.join("spec/a.pi"), "").unwrap();
        fs::write(dir.join("spec/b/index.pi"), "").unwrap();
        let mut imports = Imports::default();
        imports.add_from_source(
            "from /a import A\nfrom ./b import B\nfrom /missing import M\nfrom @piton/belay import Skill",
        );
        assert_eq!(
            spec_files(&imports, &dir),
            [dir.join("spec/a.pi"), dir.join("spec/b/index.pi")]
        );
    }

    /// Text a code task might well hold that Piton would read as more than
    /// text: braces, interpolations, a reference to a name the prompt
    /// imports, the placeholders themselves, comments, keys, list items, and
    /// lines of backslashes that would close an escape block.
    const TRICKY: &str = concat!(
        "Made `{a: 1}` and ${x} from @{ApplicationScope}, \\ {1 + 2} \\.\n",
        "\\\\\\\n",
        "\\\\\\\\\\\\\n",
        "${UNDERSTANDING_FILE}, ${CODE_PROMPT}, ${CODE_RESULT}, ${SPEC_LOCATION}\n",
        "\n",
        "  - item: value // not a comment\n",
        "    codeTask:\n",
        "            \\\\\\\n",
        "ends with \\",
    );

    /// A Code task sent to Spec is given Spec's instructions, then, on a
    /// paragraph of its own, the code-to-spec prompt, filled in as a template
    /// is but for CODE_PROMPT and CODE_RESULT, left for when it is sent.
    /// Either one left empty leaves the other alone.
    #[test]
    fn code_tasks_sent_to_spec_are_given_the_handoff() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/code-to-spec-test");
        fs::remove_dir_all(&project_dir).ok();
        fs::create_dir_all(&project_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_FILE_NAME),
            project_dir.join(CONFIG_FILE_NAME),
        )
        .unwrap();

        let spec = instructions(SendMode::Spec, &project_dir).unwrap().unwrap();
        // Given to a spec run, which can't see the code in its container.
        let handoff = system_prompts::fill_instructions(
            system_prompts::default_prompt(system_prompts::Prompt::CodeToSpec),
            "",
            "./spec",
            system_prompts::Fluency::default(),
        );
        assert!(!handoff.contains("./src"), "{handoff}");
        assert!(handoff.starts_with("This prompt was first sent to change the code"));
        assert!(handoff.contains("Change the spec at ./spec so it describes"));
        assert!(handoff.ends_with(&format!(
            "sent:\n\n{}\n\nWhat the code task said it built, its final output:\n\n{}",
            system_prompts::CODE_PROMPT,
            system_prompts::CODE_RESULT
        )));
        let given = code_to_spec_instructions(&project_dir).unwrap().unwrap();
        assert_eq!(given, format!("{spec}\n\n{handoff}"));
        // A Spec task sent from Spec alone is given Spec's alone.
        assert!(!spec.contains("first sent to change the code"));

        system_prompts::save(system_prompts::Prompt::CodeToSpec, "  \n", &project_dir).unwrap();
        assert_eq!(code_to_spec_instructions(&project_dir).unwrap(), Some(spec));
        system_prompts::save(
            system_prompts::Prompt::CodeToSpec,
            "Built: ${CODE_RESULT}",
            &project_dir,
        )
        .unwrap();
        system_prompts::save(SendMode::Spec, "", &project_dir).unwrap();
        assert_eq!(
            code_to_spec_instructions(&project_dir).unwrap().as_deref(),
            Some("Built: ${CODE_RESULT}")
        );
        fs::remove_dir_all(&project_dir).ok();
    }

    /// The code task a Spec task was sent from is saved with its anchor, and
    /// reads back exactly, with a final output or without one; an anchor
    /// saved without one, as every anchor was before, reads back without.
    #[test]
    fn code_tasks_are_saved_with_the_anchor() {
        for result in [Some(TRICKY.to_string()), Some(String::new()), None] {
            let mut anchor = HiddenAnchor::random();
            anchor.mode = Some(SendMode::Spec);
            anchor.attached_text = vec!["note".into()];
            anchor.system_prompt = Some("Spec.\n\nWas: ${CODE_PROMPT}".into());
            anchor.code_task = Some(CodeTask {
                prompt: format!("Change it.\n{TRICKY}"),
                result: result.clone(),
            });
            let source = anchor.source("Change it.");
            let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
            assert_eq!(text, "Change it.");
            assert_eq!(parsed.code_task, anchor.code_task, "{source}");
            assert_eq!(parsed.system_prompt, anchor.system_prompt);
            assert_eq!(parsed.attached_text, anchor.attached_text);
            assert_eq!(parsed.source(&text), source);

            // Without a system prompt, as when both prompts are left empty.
            anchor.system_prompt = None;
            let (parsed, _) = HiddenAnchor::parse(&anchor.source("Change it.")).unwrap();
            assert_eq!(parsed.code_task, anchor.code_task);
            assert_eq!(parsed.system_prompt, None);
        }
        let mut anchor = HiddenAnchor::random();
        anchor.system_prompt = Some("Spec.".into());
        let (parsed, _) = HiddenAnchor::parse(&anchor.source("x")).unwrap();
        assert_eq!(parsed.code_task, None);
    }

    /// A task sent to the other mode, from Code or from Spec, is saved with
    /// the name of the task it was sent from, which reads back whatever else
    /// it was saved with; one saved before this was kept reads back as sent
    /// from none.
    #[test]
    fn tasks_sent_to_the_other_mode_are_saved_with_where_they_were_sent_from() {
        let from = HiddenAnchor::random_name();
        for (mode, code_task, system_prompt, attached) in [
            (
                SendMode::Spec,
                Some(CodeTask {
                    prompt: format!("Change it.\n{TRICKY}"),
                    result: Some("Done.".into()),
                }),
                Some("Spec.".to_string()),
                vec!["note".to_string()],
            ),
            (
                SendMode::Spec,
                Some(CodeTask {
                    prompt: "Change it.".into(),
                    result: None,
                }),
                None,
                Vec::new(),
            ),
            (SendMode::Code, None, Some("Code.".to_string()), Vec::new()),
            (SendMode::Code, None, None, Vec::new()),
        ] {
            let mut anchor = HiddenAnchor::random();
            anchor
                .imports
                .add_from_source("from ./scope/application import ApplicationScope");
            anchor.mode = Some(mode);
            anchor.sliced = true;
            anchor.attached_text = attached;
            anchor.system_prompt = system_prompt;
            anchor.code_task = code_task;
            anchor.sent_from = Some(from.clone());
            let prompt = "Change @{ApplicationScope}.";
            let source = anchor.source(prompt);
            let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
            assert_eq!(text, prompt);
            assert_eq!(parsed.sent_from, anchor.sent_from, "{source}");
            assert_eq!(parsed.code_task, anchor.code_task, "{source}");
            assert_eq!(parsed.system_prompt, anchor.system_prompt, "{source}");
            assert_eq!(parsed.source(&text), source);

            // Saved before it was kept.
            anchor.sent_from = None;
            let (parsed, _) = HiddenAnchor::parse(&anchor.source(prompt)).unwrap();
            assert_eq!(parsed.sent_from, None);
            assert_eq!(parsed.code_task, anchor.code_task);
        }
    }

    /// A chain, and its code step, are saved with whether a post-build spec
    /// update follows, after where the step was sent from; one saved before
    /// this was kept reads back as followed by none.
    #[test]
    fn chains_are_saved_with_their_post_build_spec_update() {
        for (mode, sent_from, code_task) in [
            (SendMode::Both, None, None),
            (
                SendMode::Code,
                Some(HiddenAnchor::random_name()),
                Some(CodeTask {
                    prompt: format!("Change it.\n{TRICKY}"),
                    result: Some("Wrote the spec.".into()),
                }),
            ),
        ] {
            let mut anchor = HiddenAnchor::random();
            anchor.mode = Some(mode);
            anchor.system_prompt = Some("Chain.".into());
            anchor.code_task = code_task;
            anchor.sent_from = sent_from;
            anchor.post_build_update = true;
            let source = anchor.source("Change it.");
            assert!(source.contains(POST_BUILD_UPDATE_LINE), "{source}");
            let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
            assert!(parsed.post_build_update, "{source}");
            assert_eq!(parsed.sent_from, anchor.sent_from);
            assert_eq!(parsed.code_task, anchor.code_task);
            assert_eq!(parsed.source(&text), source);

            anchor.post_build_update = false;
            let (parsed, _) = HiddenAnchor::parse(&anchor.source("Change it.")).unwrap();
            assert!(!parsed.post_build_update);
        }
    }

    /// Where a task was sent from is kept with it, not sent: it compiles as
    /// it would without it, its references resolved.
    #[test]
    fn where_a_task_was_sent_from_is_not_sent() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./scope/application import ApplicationScope");
        anchor.mode = Some(SendMode::Code);
        anchor.system_prompt = Some("Code, see @{ApplicationScope}.".into());
        anchor.sent_from = Some(HiddenAnchor::random_name());
        let dir = project_dir.join("target/hidden-anchor-sent-from-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("compile.pi");
        fs::write(&file, anchor.source("Change @{ApplicationScope}.")).unwrap();
        let compiled = compile(&anchor, &file, project_dir).unwrap();
        assert!(
            compiled
                .user_prompt
                .starts_with("Change [ApplicationScope]("),
            "{}",
            compiled.user_prompt
        );
        assert!(!compiled.user_prompt.contains("sentFrom"));
        let system_prompt = compiled.system_prompt.unwrap();
        assert!(
            system_prompt.starts_with("Code, see [ApplicationScope]("),
            "{system_prompt}"
        );
        assert!(!system_prompt.contains("sentFrom"));
        fs::remove_file(&file).ok();
    }

    /// A Spec task sent from a Code task compiles, whatever the code task
    /// holds, and is sent the code task's prompt and final output exactly as
    /// they were: never compiled, a reference to a name the prompt imports
    /// left as written, and nothing in them filled in, the understanding file
    /// filled in only where the template asks for it. One that left no final
    /// output is sent a line saying so.
    #[test]
    fn code_tasks_reach_the_harness_as_text() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./scope/application import ApplicationScope");
        anchor.mode = Some(SendMode::Spec);
        let template = format!(
            "Spec, see @{{ApplicationScope}}, understood in {}.\n\n{}",
            system_prompts::UNDERSTANDING_FILE,
            system_prompts::default_prompt(system_prompts::Prompt::CodeToSpec)
        );
        anchor.system_prompt = Some(system_prompts::fill_instructions(
            &template, "./src", "./spec", system_prompts::Fluency::default(),
        ));
        let prompt = format!("Change @{{ApplicationScope}}.\n{TRICKY}");
        let result = format!("Done.\n{TRICKY}");
        anchor.code_task = Some(CodeTask {
            prompt: prompt.clone(),
            result: Some(result.clone()),
        });

        let dir = project_dir.join("target/hidden-anchor-code-task-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("compile.pi");
        fs::write(&file, anchor.source("Write the spec.")).unwrap();
        let compiled = compile(&anchor, &file, project_dir).unwrap();
        assert_eq!(compiled.code_task, anchor.code_task);
        let sent = compiled.instructions_as_sent(Some("u.md")).unwrap();
        assert!(
            sent.starts_with("Spec, see [ApplicationScope]("),
            "the template's reference isn't resolved: {sent}"
        );
        assert!(sent.contains("understood in u.md."), "{sent}");
        assert!(
            sent.ends_with(&format!(
                "The prompt the code task was sent:\n\n{prompt}\n\n\
                 What the code task said it built, its final output:\n\n{result}"
            )),
            "{sent}"
        );

        anchor.code_task = Some(CodeTask {
            prompt: "Change it.".into(),
            result: None,
        });
        fs::write(&file, anchor.source("Write the spec.")).unwrap();
        let sent = compile(&anchor, &file, project_dir)
            .unwrap()
            .instructions_as_sent(None)
            .unwrap();
        assert!(
            sent.ends_with(&format!(
                "final output:\n\n{}",
                system_prompts::NO_CODE_RESULT
            )),
            "{sent}"
        );
        fs::remove_file(&file).ok();
    }

    /// A prompt's escape block is fenced beyond any line of backslashes in
    /// it, and reads back exactly, even with nothing in it.
    #[test]
    fn escape_blocks_outlast_the_text() {
        let mut anchor = HiddenAnchor::random();
        for prompt in [
            "",
            "\\\\\\\n  \\\\\\\\  \nend",
            "  lead\ttab  \n\n\n",
            "true",
        ] {
            anchor.system_prompt = Some(prompt.to_string());
            let source = anchor.source(prompt);
            let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
            assert_eq!(text, prompt, "{source}");
            assert_eq!(parsed.system_prompt.as_deref(), Some(prompt));
        }
        assert_eq!(super::fence_length("a\n \\\\\\\\ \nb"), 5);
        assert_eq!(super::fence_length("\\"), 3);
    }

    /// References resolve to the heading Belay gives the anchor, or the
    /// property, a repeated one numbered.
    #[test]
    fn fragments_follow_the_headings() {
        let document = "# Button\n\n## Description\n\n```\n# not a heading\n```\n\n\
                        ## Color\n\n# Save Button\n\n## Description\n\n### Tone\n";
        use super::fragment;
        assert_eq!(fragment(document, "Button").as_deref(), Some("button"));
        assert_eq!(fragment(document, "Button.color").as_deref(), Some("color"));
        assert_eq!(
            fragment(document, "SaveButton.description").as_deref(),
            Some("description-1")
        );
        assert_eq!(
            fragment(document, "SaveButton.description.tone").as_deref(),
            Some("tone")
        );
        assert_eq!(
            fragment(document, "SaveButton.missing").as_deref(),
            Some("save-button")
        );
        assert_eq!(fragment(document, "Other"), None);
        assert_eq!(super::title_case("whatIsAType"), "What Is AType");
        assert_eq!(super::title_case("t1"), "T 1");
    }

    /// Prompts saved as quoted pieces by earlier versions still read back.
    #[test]
    fn quoted_prompts_read_back() {
        let old = "from /a import A\n\nanchor Prompt_old:\n    userPrompt:\n        -\n            - @{A}\n            - \" say \\\"hi\\\"\"\n        -\n            - \"\"\n\n    mode: combined\n\n    attachedText:\n        -\n            - \"x\"\n            - \"y\"\n\n    systemPrompt:\n        -\n            - \"Be brief.\"\n";
        let (anchor, prompt) = HiddenAnchor::parse(old).unwrap();
        assert_eq!(prompt, "@{A} say \"hi\"\n");
        assert_eq!(anchor.mode, Some(SendMode::Both));
        assert_eq!(anchor.attached_text, ["x\ny"]);
        assert_eq!(anchor.system_prompt.as_deref(), Some("Be brief."));
    }

    #[test]
    fn prompt_starts_on_reported_line() {
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./a import A\nfrom ./b import B\n");
        let source = anchor.draft_source("first\nsecond");
        let lines: Vec<&str> = source.lines().collect();
        let first = anchor.prompt_first_line() as usize;
        assert_eq!(lines[first], format!("{PROMPT_INDENT}first"));
        assert_eq!(lines[first + 1], format!("{PROMPT_INDENT}second"));
    }

    /// A prompt is split into text and references, only `@{…}` and `${…}`
    /// around a dotted path of identifiers counting.
    #[test]
    fn prompts_split_into_text_and_references() {
        use super::{Piece, pieces};
        assert_eq!(
            pieces("See @{App-scope.concept} and ${Foo}, not ${1x} or @{a b} or ${"),
            [
                Piece::Text("See "),
                Piece::Reference("@{App-scope.concept}", "App-scope"),
                Piece::Text(" and "),
                Piece::Reference("${Foo}", "Foo"),
                Piece::Text(", not ${1x} or @{a b} or ${"),
            ]
        );
        assert_eq!(pieces(""), []);
        assert_eq!(pieces("héllo 🙂"), [Piece::Text("héllo 🙂")]);
    }

    /// What `piton lsp` is shown keeps references, words, and every position,
    /// blanking whatever Piton would read as more than text.
    #[test]
    fn the_lsp_draft_blanks_everything_but_references_and_words() {
        use super::blank_for_lsp;
        let line = "  key: {\"a\": 1} // see @{ApplicationScope} and @{Appl 🙂 é";
        let blanked = blank_for_lsp(line);
        assert_eq!(
            blanked,
            "__key_ __a__ 1_ __ see @{ApplicationScope} and @{Appl __ é"
        );
        let units = |text: &str| text.encode_utf16().count();
        assert_eq!(units(&blanked), units(line));
    }

    /// Pasted text, however much of it looks like Piton, is saved, read back,
    /// and compiled exactly as it was typed, line breaks and all; only
    /// references to imported names resolve, and old prose prompts still read
    /// back.
    #[test]
    fn pasted_prompts_are_taken_as_they_are() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let prompt = "Update @{ApplicationScope} // not a comment\n\
                      key: value\n\
                      - a dash, \"quotes\", and {braces}\n\
                      \n\
                      \t  indented C:\\Users\\me\\ ${HOME} @{NotImported} ${ApplicationScope}\n\
                      1 + 2\n\
                      true\n\
                      ```\n\
                      ends with a backslash \\";
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from /scope/application import ApplicationScope");
        anchor.system_prompt = Some("Mind { and \" and \\.".into());
        let source = anchor.source(prompt);

        let (parsed, text) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(text, prompt);
        assert_eq!(parsed.system_prompt, anchor.system_prompt);
        assert_eq!(parsed.source(&text), source);

        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = project_dir.join("target/hidden-anchor-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pasted.pi");
        fs::write(&file, &source).unwrap();
        let compiled = compile(&anchor, &file, project_dir).unwrap();
        let link =
            "[ApplicationScope](.claude/reference/scope/application/index.md#application-scope)";
        assert_eq!(
            compiled.user_prompt,
            prompt
                .replace("@{ApplicationScope}", link)
                .replace("${ApplicationScope}", "ApplicationScope"),
        );
        assert_eq!(compiled.system_prompt, anchor.system_prompt);

        let old = "anchor Prompt_old:\n    userPrompt:\n        first\n\n        second\n";
        assert_eq!(HiddenAnchor::parse(old).unwrap().1, "first\n\nsecond");
    }

    /// A preview compiles the prompt as sending it would, and keeps nothing:
    /// not in the history, nor where it was compiled.
    #[test]
    fn previews_compile_and_keep_nothing() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from /scope/application import ApplicationScope");
        anchor.system_prompt = Some("Be brief.".into());
        let history = super::history_dir(project_dir).join(format!("x-{}.pi", anchor.name));
        let compiled = super::preview(&anchor, "Look at @{ApplicationScope}", project_dir).unwrap();
        assert_eq!(
            compiled.user_prompt,
            "Look at [ApplicationScope](.claude/reference/scope/application/index.md#application-scope)"
        );
        assert_eq!(compiled.system_prompt.as_deref(), Some("Be brief."));
        let kept = project_dir.join(super::APP_DIR).join("preview");
        assert!(
            !kept.exists() || fs::read_dir(&kept).unwrap().next().is_none(),
            "the preview's source was kept"
        );
        assert!(!history.exists());
    }

    /// Compiles a hidden anchor, from outside the spec root, against this
    /// repository's own spec with the real `piton` CLI.
    #[test]
    fn compiles_against_this_project() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./scope/application import ApplicationScope");
        anchor.mode = Some(SendMode::Spec);
        anchor.system_prompt = Some(system_prompts::fill(
            system_prompts::default_prompt(SendMode::Spec),
            "./src",
            "./spec",
            system_prompts::default_prompt(system_prompts::Prompt::SpecReading),
        ));

        let dir = project_dir.join("target/hidden-anchor-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("compile.pi");
        fs::write(
            &file,
            anchor.source("See @{ApplicationScope}.\n\n- a list item"),
        )
        .unwrap();

        let compiled = compile(&anchor, &file, project_dir).unwrap();
        let text = compiled.user_prompt;
        assert!(text.contains("[ApplicationScope]("), "{text}");
        assert!(text.contains("- a list item"), "{text}");
        assert!(!text.contains("Attached text"), "{text}");
        assert_eq!(compiled.system_prompt, anchor.system_prompt);
    }

    /// A sliced prompt reads back sliced, and reaches the harness with its
    /// references named rather than linked, followed by their slices.
    #[test]
    fn a_sliced_prompt_is_sent_with_its_slices() {
        let mut anchor = HiddenAnchor::random();
        anchor.mode = Some(SendMode::Code);
        anchor.sliced = true;
        anchor.attached_text = vec!["attached".into()];
        let (read, prompt) = HiddenAnchor::parse(&anchor.source("Hello")).unwrap();
        assert!(read.sliced);
        assert_eq!(read.mode, Some(SendMode::Code));
        assert_eq!(read.attached_text, anchor.attached_text);
        assert_eq!(prompt, "Hello");
        let (read, _) = HiddenAnchor::parse(&HiddenAnchor::random().source("Hello")).unwrap();
        assert!(!read.sliced);

        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut anchor = HiddenAnchor::random();
        anchor
            .imports
            .add_from_source("from ./scope/application import ApplicationScope");
        anchor.sliced = true;
        let dir = project_dir.join("target/hidden-anchor-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sliced.pi");
        fs::write(&file, anchor.source("See @{ApplicationScope}.")).unwrap();
        let text = compile(&anchor, &file, project_dir).unwrap().user_prompt;
        assert!(
            text.starts_with("See ApplicationScope.\n\nSpec slices:\n\n# ApplicationScope"),
            "{text}"
        );
    }

    /// Attached text that looks like Piton, or holds quotes, backslashes, and
    /// fences, is saved and read back as it is, compiles as it is, and reaches
    /// the harness after the prompt, each piece fenced beyond its own
    /// backticks.
    #[test]
    fn attached_text_is_taken_as_it_is() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let tricky = "fn main() { println!(\"${x} @{Y}\"); }\n\n  - item: value // not a comment\nends with \\\n```rust\ninner\n```";
        let mut anchor = HiddenAnchor::random();
        anchor.mode = Some(SendMode::Ask);
        anchor.attached_text = vec![tricky.to_string(), "single line".to_string()];
        anchor.system_prompt = Some("Answer.".into());
        let source = anchor.source("What does it do?");

        let (parsed, prompt) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(prompt, "What does it do?");
        assert_eq!(parsed.attached_text, anchor.attached_text);
        assert_eq!(parsed.mode, Some(SendMode::Ask));
        assert_eq!(parsed.system_prompt.as_deref(), Some("Answer."));

        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = project_dir.join("target/hidden-anchor-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("attached.pi");
        fs::write(&file, &source).unwrap();
        let compiled = compile(&anchor, &file, project_dir).unwrap();
        assert_eq!(
            compiled.user_prompt,
            format!(
                "What does it do?\n\nAttached text:\n\n````\n{tricky}\n````\n\n```\nsingle line\n```"
            )
        );
        assert_eq!(compiled.system_prompt.as_deref(), Some("Answer."));
    }

    /// Attached images are saved as their paths, from the project directory,
    /// each an escape block keyed image1, image2, and so on, after the
    /// attached text; they read back in order, alongside everything else,
    /// and anchors saved before images could be attached read back with
    /// none. The images reach the harness as files, never as text in the
    /// compiled prompt.
    #[test]
    fn attached_images_are_saved_as_their_paths() {
        let mut anchor = HiddenAnchor::random();
        anchor.mode = Some(SendMode::Code);
        anchor.sliced = true;
        anchor.attached_text = vec!["log".into()];
        // Even a path that looks like Piton is kept as it is.
        let images = vec![
            ".suspense/images/1-aaaaaaaaaaaa.png".to_string(),
            ".suspense/images/2-{x} @{Y}: b.jpg".to_string(),
        ];
        anchor.attached_images = images.clone();
        anchor.system_prompt = Some("Be brief.".into());
        anchor.code_task = Some(CodeTask {
            prompt: "Did it".into(),
            result: None,
        });
        anchor.sent_from = Some("Prompt_from".into());
        let source = anchor.source("Look at this");
        assert!(
            source.contains(
                "    attachedImages:\n        image1:\n            \\\\\\\n            .suspense/images/1-aaaaaaaaaaaa.png\n            \\\\\\\n        image2:"
            ),
            "{source}"
        );
        let (parsed, prompt) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(prompt, "Look at this");
        assert_eq!(parsed.attached_images, images);
        assert_eq!(parsed.attached_text, ["log"]);
        assert!(parsed.sliced);
        assert_eq!(parsed.system_prompt.as_deref(), Some("Be brief."));
        assert_eq!(parsed.code_task, anchor.code_task);
        assert_eq!(parsed.sent_from.as_deref(), Some("Prompt_from"));
        assert_eq!(
            parsed.attached(),
            super::Attached {
                text: vec!["log".into()],
                images: images.clone(),
            }
        );

        // Images alone, with nothing else attached.
        let mut alone = HiddenAnchor::random();
        alone.attached_images = vec!["a.png".into()];
        let (parsed, _) = HiddenAnchor::parse(&alone.source("Hi")).unwrap();
        assert_eq!(parsed.attached_images, ["a.png"]);
        assert!(parsed.attached_text.is_empty());

        // Saved before images could be attached: none.
        let old = "anchor Prompt_old:\n    userPrompt:\n        \\\\\\\n        Hi\n        \\\\\\\n\n    mode: code\n\n    attachedText:\n        text1:\n            \\\\\\\n            x\n            \\\\\\\n\n    systemPrompt:\n        \\\\\\\n        Be.\n        \\\\\\\n";
        let (parsed, prompt) = HiddenAnchor::parse(old).unwrap();
        assert_eq!(prompt, "Hi");
        assert!(parsed.attached_images.is_empty());
        assert_eq!(parsed.attached_text, ["x"]);
        assert_eq!(parsed.system_prompt.as_deref(), Some("Be."));
        // Nothing attached, nothing written.
        assert!(
            !HiddenAnchor::random()
                .source("Hi")
                .contains("attachedImages")
        );

        if crate::piton_build::piton_missing() {
            return;
        }
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = project_dir.join("target/hidden-anchor-test");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("images.pi");
        let mut anchor = HiddenAnchor::random();
        anchor.attached_images = images.clone();
        fs::write(&file, anchor.source("Look at this")).unwrap();
        let compiled = compile(&anchor, &file, project_dir).unwrap();
        assert_eq!(compiled.user_prompt, "Look at this");
        assert_eq!(compiled.images, super::image_files(&images, project_dir));
    }

    /// A Freeform prompt is sent just as it was typed, then its attached
    /// text, with no system prompt, and saved with its mode, which reads
    /// back; a prompt saved before Freeform was, with no mode, still does.
    #[test]
    fn freeform_prompts_are_sent_as_typed() {
        let typed = "Fix @{Button.color}: {1 + 2}\n    - as is";
        let compiled = super::freeform(typed, &[]);
        assert_eq!(compiled.user_prompt, typed);
        assert!(compiled.system_prompt.is_none());
        let attached = vec!["context".to_string()];
        assert_eq!(
            super::freeform(typed, &attached).user_prompt,
            super::with_attached_text(typed, &attached)
        );

        let mut anchor = HiddenAnchor::random();
        anchor.mode = Some(SendMode::Freeform);
        anchor.attached_text = attached.clone();
        let source = anchor.source(typed);
        assert!(source.contains("    mode: freeform"), "{source}");
        let (parsed, prompt) = HiddenAnchor::parse(&source).unwrap();
        assert_eq!(prompt, typed);
        assert_eq!(parsed.mode, Some(SendMode::Freeform));
        assert_eq!(parsed.attached_text, attached);
        assert!(parsed.system_prompt.is_none());

        let (old, _) = HiddenAnchor::parse(&HiddenAnchor::random().source("Old")).unwrap();
        assert_eq!(old.mode, None);
    }
}
