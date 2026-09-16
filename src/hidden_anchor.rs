//! The hidden anchor. Prompts are typed as plain text, but behind the scenes
//! they are the `userPrompt` of a randomly-named Piton anchor whose imports
//! `piton lsp` adds automatically (see [`crate::piton_lsp`]), so they are never
//! written or seen. Each sent prompt is saved as a `.pi` file in the project's
//! prompt history and compiled; the compiled `userPrompt` is what the harness
//! receives.
//!
//! A prompt may hold anything, pasted text included, so it isn't written as
//! Piton prose, where braces, quotes, colons, leading dashes, comments, and
//! backslashes all mean something. Each line is a list of pieces instead: the
//! text between references as a quoted string, taken exactly as it is, and each
//! `@{Name}` or `${Name.path}` whose name is imported as a reference of its own.
//! A reference to a name that isn't imported, such as a pasted `${HOME}`, is
//! only text.

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
        writeln!(source, "\nanchor {}:\n    userPrompt:", self.name).ok();
        self.push_pieces(&mut source, prompt);
        // After the prompt, so the prompt's lines stay where
        // `prompt_first_line` says they are.
        if let Some(mode) = self.mode {
            source.push('\n');
            writeln!(source, "{MODE_PREFIX}{}", mode.key()).ok();
        }
        if !self.attached_text.is_empty() {
            source.push('\n');
            source.push_str(ATTACHED_TEXT_LINE);
            source.push('\n');
            for text in &self.attached_text {
                writeln!(source, "{ATTACHMENT_ITEM}").ok();
                for line in text.split('\n') {
                    writeln!(
                        source,
                        "{ATTACHMENT_LINE_PREFIX}{}",
                        quote(line.trim_end_matches('\r'))
                    )
                    .ok();
                }
            }
        }
        if let Some(system_prompt) = &self.system_prompt {
            source.push('\n');
            source.push_str(SYSTEM_PROMPT_LINE);
            source.push('\n');
            self.push_pieces(&mut source, system_prompt);
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

    /// Adds `text` to `source` as the lines of a property, each a list of its
    /// pieces: quoted text, and references to imported names.
    fn push_pieces(&self, source: &mut String, text: &str) {
        for line in text.split('\n') {
            writeln!(source, "{PROMPT_INDENT}-").ok();
            let mut pending = String::new();
            let mut wrote = false;
            for piece in pieces(line.trim_end_matches('\r')) {
                match piece {
                    Piece::Reference(reference, name) if self.imports.has(name) => {
                        if !pending.is_empty() {
                            writeln!(source, "{PIECE_PREFIX}{}", quote(&pending)).ok();
                            pending.clear();
                        }
                        writeln!(source, "{PIECE_PREFIX}{reference}").ok();
                        wrote = true;
                    }
                    Piece::Reference(text, _) | Piece::Text(text) => pending.push_str(text),
                }
            }
            if !pending.is_empty() || !wrote {
                writeln!(source, "{PIECE_PREFIX}{}", quote(&pending)).ok();
            }
        }
    }

    /// Reads back an anchor saved with [`Self::source`], with the prompt it
    /// holds.
    pub fn parse(source: &str) -> Option<(Self, String)> {
        let mut imports = Imports::default();
        let mut lines = source.lines();
        let name = loop {
            let line = lines.next()?;
            if let Some(name) = line
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
        // Prompt lines are indented deeper, so only the properties themselves
        // match these lines.
        let property = |line: &&str| {
            *line == SYSTEM_PROMPT_LINE
                || *line == ATTACHED_TEXT_LINE
                || line.starts_with(MODE_PREFIX)
        };
        let end = lines.iter().position(property).unwrap_or(lines.len());
        // Drop the blank line separating the prompt from the next property;
        // prompt lines, even empty ones, are always indented.
        let prompt = &lines[..end];
        let prompt = if end < lines.len() {
            prompt.strip_suffix(&[""]).unwrap_or(prompt)
        } else {
            prompt
        };
        let mut rest = &lines[end..];
        let mut mode = None;
        if let Some(key) = rest.first().and_then(|line| line.strip_prefix(MODE_PREFIX)) {
            mode = SendMode::from_key(key.trim());
            rest = &rest[1..];
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        let mut attached_text = Vec::new();
        if rest.first() == Some(&ATTACHED_TEXT_LINE) {
            rest = &rest[1..];
            while let Some(line) = rest.first() {
                if *line == ATTACHMENT_ITEM {
                    attached_text.push(Vec::new());
                } else if let Some(quoted) = line.strip_prefix(ATTACHMENT_LINE_PREFIX) {
                    attached_text.last_mut()?.push(unquote(quoted)?);
                } else {
                    break;
                }
                rest = &rest[1..];
            }
            rest = rest.strip_prefix(&[""]).unwrap_or(rest);
        }
        let attached_text = attached_text
            .into_iter()
            .map(|lines| lines.join("\n"))
            .collect();
        let system_prompt = match rest.first() {
            Some(&SYSTEM_PROMPT_LINE) => Some(read_block(&rest[1..])),
            _ => None,
        };
        Some((
            Self {
                name,
                imports,
                mode,
                attached_text,
                system_prompt,
            },
            read_block(prompt),
        ))
    }

    /// The zero-based line of [`Self::source`] holding the prompt's first line.
    pub fn prompt_first_line(&self) -> u32 {
        self.imports.0.len() as u32 + 3
    }
}

/// The start of the `mode` line of [`HiddenAnchor::source`].
const MODE_PREFIX: &str = "    mode: ";

/// The line opening the `systemPrompt` property of [`HiddenAnchor::source`].
const SYSTEM_PROMPT_LINE: &str = "    systemPrompt:";

/// The line opening the `attachedText` property of [`HiddenAnchor::source`],
/// the line opening each piece of text in it, and the start of each of the
/// piece's quoted lines.
const ATTACHED_TEXT_LINE: &str = "    attachedText:";
const ATTACHMENT_ITEM: &str = "        -";
const ATTACHMENT_LINE_PREFIX: &str = "            - ";

/// `line` as a quoted Piton string, taken as it is: quotes and backslashes
/// escaped, and each `${` too, so nothing in it is interpolated or evaluated.
fn quote(line: &str) -> String {
    let mut quoted = String::with_capacity(line.len() + 2);
    quoted.push('"');
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '"' | '\\') || (c == '$' && chars.peek() == Some(&'{')) {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// The line a string written by [`quote`] holds.
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

/// The start of each piece of a line, written by [`HiddenAnchor::source`].
const PIECE_PREFIX: &str = "            - ";

/// The text of a property's lines: as pieces, written by
/// [`HiddenAnchor::source`], or as the prose earlier versions saved.
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
/// for it (see [`crate::system_prompts`]), naming the code and spec locations
/// set in its `piton.config.pi`. A template left empty gives none.
pub fn system_prompt(mode: SendMode, project_dir: &Path) -> Result<Option<String>> {
    let template = system_prompts::load(mode, project_dir)?;
    if template.trim().is_empty() {
        return Ok(None);
    }
    let code = config_value(project_dir, "codeRoot")?;
    let spec = config_value(project_dir, "root")?;
    Ok(Some(system_prompts::fill(&template, &code, &spec)))
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

/// Compiles `file`, a saved `anchor`, and returns its compiled prompt text.
pub fn compile(anchor: &HiddenAnchor, file: &Path, project_dir: &Path) -> Result<CompiledPrompt> {
    let output = Command::new("piton")
        .args(["compile", "--stdout"])
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
    let user_prompt = compiled
        .get("userPrompt")
        .ok_or_else(|| anyhow!("the compiled anchor has no userPrompt"))?;
    // Each piece of attached text compiles to the list of its lines.
    let attached_text: Vec<String> = compiled
        .get("attachedText")
        .and_then(Value::as_array)
        .map(|pieces| {
            pieces
                .iter()
                .map(|piece| match piece {
                    Value::Array(lines) => lines
                        .iter()
                        .map(|line| line.as_str().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    other => prose(other),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CompiledPrompt {
        user_prompt: with_attached_text(&text_of(user_prompt), &attached_text),
        system_prompt: compiled.get("systemPrompt").map(text_of),
    })
}

/// Reads the spec root (`root:`) from the project's `piton.config.pi`.
fn spec_root(project_dir: &Path) -> Result<PathBuf> {
    config_value(project_dir, "root").map(PathBuf::from)
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

/// A compiled property's text: its lines of pieces joined back together, or,
/// for prose, as [`prose`] reads it.
fn text_of(value: &Value) -> String {
    match value {
        Value::Array(lines) if !lines.is_empty() && lines.iter().all(Value::is_array) => lines
            .iter()
            .map(|line| {
                line.as_array()
                    .into_iter()
                    .flatten()
                    .map(|piece| match piece {
                        Value::String(text) => text.clone(),
                        other => prose(other),
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => prose(other),
    }
}

/// Turns a compiled `userPrompt` back into text: paragraphs are separated by
/// blank lines and lists become `- ` items.
fn prose(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block {
                Value::Array(items) => items
                    .iter()
                    .map(|item| format!("- {}", prose(item)))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => prose(other),
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Null => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        HiddenAnchor, Imports, MODE_PREFIX, PROMPT_INDENT, SYSTEM_PROMPT_LINE, compile, mode_of,
        prose, system_prompt,
    };
    use crate::chat_input::SendMode;
    use crate::project_directory::CONFIG_FILE_NAME;
    use crate::system_prompts;

    #[test]
    fn merges_import_edits_into_absolute_imports() {
        let mut imports = Imports::default();
        assert!(imports.add_from_source("from ./lib import Concept\n\n"));
        assert!(imports.add_from_source("from ./lib import Concept, Scope\n"));
        assert!(imports.add_from_source("from @piton/belay import ClaudeAdapter\n\n"));
        assert!(!imports.add_from_source("from /lib import Scope\nanchor Other:\n"));

        let anchor = HiddenAnchor {
            name: "Prompt_test".into(),
            imports,
            mode: None,
            attached_text: Vec::new(),
            system_prompt: None,
        };
        assert_eq!(
            anchor.source("hi"),
            "from /lib import Concept, Scope\n\
             from @piton/belay import ClaudeAdapter\n\
             \n\
             anchor Prompt_test:\n    userPrompt:\n        -\n            - \"hi\"\n"
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
            let prompt = system_prompt(mode, &project_dir).unwrap().unwrap();
            assert_eq!(
                prompt,
                system_prompts::fill(system_prompts::default_template(mode), "./src", "./spec")
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
            system_prompt(SendMode::Code, &project_dir)
                .unwrap()
                .as_deref(),
            Some("Code in ./src only.")
        );
        system_prompts::save(SendMode::Ask, "", &project_dir).unwrap();
        assert_eq!(system_prompt(SendMode::Ask, &project_dir).unwrap(), None);
    }

    #[test]
    fn turns_compiled_prompt_into_text() {
        let value = serde_json::json!(["First paragraph.", ["one", "two"], "Last."]);
        assert_eq!(prose(&value), "First paragraph.\n\n- one\n- two\n\nLast.");
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
        let link = "[ApplicationScope](.claude/reference/scope/application/ApplicationScope.md)";
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
            system_prompts::default_template(SendMode::Spec),
            "./src",
            "./spec",
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
