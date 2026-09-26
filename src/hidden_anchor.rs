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
use std::process::Command;
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
    /// Text attached to the prompt, written as `attachedText` after the mode:
    /// each piece a list of quoted lines, taken as it is rather than as Piton.
    pub attached_text: Vec<String>,
    /// The `systemPrompt` written after the attached text, if any.
    pub system_prompt: Option<String>,
}

/// A compiled hidden anchor.
pub struct CompiledPrompt {
    pub user_prompt: String,
    pub system_prompt: Option<String>,
}

impl HiddenAnchor {
    /// A freshly named anchor with no imports and no system prompt.
    pub fn random() -> Self {
        Self {
            name: format!(
                "Prompt_{:016x}",
                RandomState::new().hash_one(SystemTime::now())
            ),
            imports: Imports::default(),
            mode: None,
            sliced: false,
            attached_text: Vec::new(),
            system_prompt: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
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
        if !self.attached_text.is_empty() {
            source.push('\n');
            source.push_str(ATTACHED_TEXT_LINE);
            source.push('\n');
            for (index, text) in self.attached_text.iter().enumerate() {
                writeln!(source, "{PROMPT_INDENT}{ATTACHMENT_KEY}{}:", index + 1).ok();
                push_block(&mut source, ATTACHMENT_INDENT, text);
            }
        }
        if let Some(system_prompt) = &self.system_prompt {
            source.push('\n');
            source.push_str(SYSTEM_PROMPT_LINE);
            source.push('\n');
            push_block(&mut source, PROMPT_INDENT, system_prompt);
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
                        || *line == SLICED_LINE
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
        let system_prompt = match rest.first() {
            Some(&SYSTEM_PROMPT_LINE) => Some(match read_escaped(&rest[1..], PROMPT_INDENT) {
                Some((text, _)) => text,
                None => {
                    let end = rest[1..]
                        .iter()
                        .position(|line| *line == REFERENCES_LINE)
                        .map_or(rest.len(), |end| end + 1);
                    read_block(&rest[1..end])
                }
            }),
            _ => None,
        };
        Some((
            Self {
                name,
                imports,
                mode,
                sliced,
                attached_text,
                system_prompt,
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

/// The line opening the `systemPrompt` property of [`HiddenAnchor::source`].
const SYSTEM_PROMPT_LINE: &str = "    systemPrompt:";

/// The line opening the `references` property of [`HiddenAnchor::source`].
const REFERENCES_LINE: &str = "    references:";

/// The line opening the `attachedText` property of [`HiddenAnchor::source`],
/// what each piece of text in it is keyed by, followed by its number, and
/// the indentation of the piece's escape block.
const ATTACHED_TEXT_LINE: &str = "    attachedText:";
const ATTACHMENT_KEY: &str = "text";
const ATTACHMENT_INDENT: &str = "            ";

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

/// The system prompt a prompt sent in `mode` is given: the project's template
/// for it (see [`crate::system_prompts`]), with the project's spec-reading
/// prompt injected, naming the code and spec locations set in its
/// `piton.config.pi` and the harness's directory, and with the project's Piton
/// `fluency`. A template that is empty once filled in gives none.
pub fn system_prompt(mode: SendMode, project_dir: &Path, fluency: &str) -> Result<Option<String>> {
    let template = system_prompts::load(mode, project_dir)?;
    if template.trim().is_empty() {
        return Ok(None);
    }
    let code = config_value(project_dir, "codeRoot")?;
    let spec = config_value(project_dir, "root")?;
    let reading = system_prompts::load(system_prompts::Prompt::SpecReading, project_dir)?;
    let filled = system_prompts::fill(&template, &code, &spec, &reading, fluency);
    Ok((!filled.trim().is_empty()).then_some(filled))
}

/// The mode of a prompt saved before modes were, read from the default system
/// prompt it was given; `None` for one sent without a system prompt, or with
/// an edited one.
pub fn mode_of(system_prompt: &str) -> Option<SendMode> {
    [
        ("We're working on the code ", SendMode::Code),
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
    let output = Command::new("piton")
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
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()))
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
    let canonical = |dir: PathBuf| dir.canonicalize().unwrap_or(dir);
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
    let project = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let module = module.strip_prefix(&project).unwrap_or(&module);
    let output = Command::new("piton")
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
        HiddenAnchor, Imports, MODE_PREFIX, PROMPT_INDENT, SYSTEM_PROMPT_LINE, compile, mode_of,
        spec_files, system_prompt,
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
            attached_text: Vec::new(),
            system_prompt: None,
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
    }

    /// A project without saved templates gives each mode its default, naming
    /// this repository's code and spec locations from its `piton.config.pi`,
    /// and the mode reads back from it.
    #[test]
    fn system_prompt_fills_in_the_template() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/system-prompt-test");
        fs::remove_dir_all(&project_dir).ok();
        fs::create_dir_all(&project_dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_FILE_NAME),
            project_dir.join(CONFIG_FILE_NAME),
        )
        .unwrap();

        for mode in SendMode::ALL {
            let prompt = system_prompt(mode, &project_dir, "# Piton fluency")
                .unwrap()
                .unwrap();
            assert_eq!(
                prompt,
                system_prompts::fill(
                    system_prompts::default_prompt(mode),
                    "./src",
                    "./spec",
                    system_prompts::default_prompt(system_prompts::Prompt::SpecReading),
                    "# Piton fluency"
                )
            );
            assert_eq!(mode_of(&prompt), Some(mode));
        }
        assert_eq!(mode_of("Something else."), None);

        system_prompts::save(
            SendMode::Code,
            "Code in ${CODE_LOCATION} only.",
            &project_dir,
        )
        .unwrap();
        assert_eq!(
            system_prompt(SendMode::Code, &project_dir, "")
                .unwrap()
                .as_deref(),
            Some("Code in ./src only.")
        );
        system_prompts::save(SendMode::Ask, "", &project_dir).unwrap();
        assert_eq!(
            system_prompt(SendMode::Ask, &project_dir, "").unwrap(),
            None
        );
        // Nothing but the fluency, without piton, gives none either.
        system_prompts::save(SendMode::Spec, system_prompts::PITON_FLUENCY, &project_dir).unwrap();
        assert_eq!(
            system_prompt(SendMode::Spec, &project_dir, "").unwrap(),
            None
        );
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
            "",
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
}
