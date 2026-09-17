//! Creating the parts a spec is written with (see the SpecComponentsScope): a
//! scope, with a concept and a shape of its own, in a folder of its own; or a
//! concept or a shape added to a spec file. Either written straight into the
//! spec, or handed with a description to a skill to write.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use gpui_kit::actions;

actions!(suspense, [NewScope, NewConcept, NewShape]);

/// Which component a form creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Scope,
    Concept,
    Shape,
}

impl ComponentKind {
    /// The word its name ends with.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Scope => "Scope",
            Self::Concept => "Concept",
            Self::Shape => "Shape",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Scope => "New Scope",
            Self::Concept => "New Concept",
            Self::Shape => "New Shape",
        }
    }

    /// What the prompt to a skill calls it.
    fn noun(self) -> &'static str {
        match self {
            Self::Scope => "scope",
            Self::Concept => "concept",
            Self::Shape => "shape",
        }
    }
}

/// The keywords a scope is declared with, the first to start with.
pub const SCOPE_KEYWORDS: [&str; 2] = ["scope", "ui-component"];

/// The name as typed, with the kind's word added when it doesn't already end
/// with it.
pub fn full_name(typed: &str, kind: ComponentKind) -> String {
    let typed = typed.trim();
    if typed.is_empty() || typed.ends_with(kind.suffix()) {
        typed.to_string()
    } else {
        format!("{typed}{}", kind.suffix())
    }
}

/// Why a name can't be used, if it can't.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let name = name.trim();
    let first = name.chars().next()?;
    if !first.is_ascii_uppercase() {
        return Some("Start the name with a capital letter");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some("Use only letters and digits in the name");
    }
    None
}

/// The names declared at the top level of a spec file, each with the keyword
/// it is declared with: `export abstract anchor Name extends Base as kw:`
/// gives `("anchor", "Name")`.
pub fn declared_names(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let line = line.split("//").next()?.trim_end();
            let line = line.strip_suffix(':')?;
            let mut words = line
                .split_whitespace()
                .skip_while(|word| matches!(*word, "export" | "abstract"));
            let keyword = words.next()?;
            if matches!(keyword, "use" | "from") {
                return None;
            }
            let name = words.next()?;
            name.chars()
                .all(|c| c.is_alphanumeric() || c == '_')
                .then(|| (keyword.to_string(), name.to_string()))
        })
        .collect()
}

/// `ParserScope` as `parser`, `FileTreeScope` as `file-tree`.
pub fn kebab(name: &str, kind: ComponentKind) -> String {
    let stem = name.strip_suffix(kind.suffix()).unwrap_or(name);
    let mut out = String::new();
    for (ix, c) in stem.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if ix > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The folder a scope goes in until one is typed: `scope/parser` for
/// ParserScope, or `ui/components/parser` as a ui-component.
pub fn default_folder(name: &str, keyword: &str) -> String {
    let stem = kebab(name, ComponentKind::Scope);
    if stem.is_empty() {
        return String::new();
    }
    match keyword {
        "ui-component" => format!("ui/components/{stem}"),
        _ => format!("scope/{stem}"),
    }
}

/// Why a folder, relative to the spec location, can't hold a new scope.
pub fn folder_problem(spec_dir: &Path, folder: &str) -> Option<&'static str> {
    let folder = folder.trim().trim_end_matches('/');
    if folder.is_empty() {
        return Some("Give the scope a folder");
    }
    let path = Path::new(folder);
    if !path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Some("The folder must be within the spec");
    }
    if spec_dir.join(path).join("index.pi").exists() {
        return Some("That folder already holds an index.pi");
    }
    None
}

/// The spec files, relative to the spec location, and the spec location's
/// folder.
pub fn spec_files(project_dir: &Path) -> Result<(PathBuf, Vec<String>)> {
    let (spec_root, files) = crate::divergence::list_spec_files(project_dir)?;
    let prefix = format!("{spec_root}/");
    let files = files
        .into_iter()
        .map(|file| file.strip_prefix(&prefix).unwrap_or(&file).to_string())
        .collect();
    Ok((project_dir.join(spec_root), files))
}

/// `text` as prose indented `indent` spaces beneath its key, escaped so that
/// nothing in it reads as an interpolation, a key, a list item, a comment, or
/// a fence.
pub fn prose(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut lines: Vec<String> = Vec::new();
    for line in text.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            if lines.last().is_some_and(|last| !last.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let mut escaped = String::new();
        if line.starts_with('-')
            || line.starts_with("//")
            || line.starts_with("```")
            || line.starts_with("~~~")
        {
            escaped.push('\\');
        }
        for c in line.chars() {
            if matches!(c, '{' | '}' | ':') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        lines.push(format!("{pad}{escaped}"));
    }
    lines.join("\n")
}

/// The description's first sentence.
pub fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    let line = text.lines().next().unwrap_or_default();
    match line.find(". ") {
        Some(end) => &line[..=end],
        None => line,
    }
}

/// A component as its form was filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub kind: ComponentKind,
    /// The full name.
    pub name: String,
    /// For a scope: its keyword and folder, relative to the spec location.
    pub keyword: String,
    pub folder: String,
    /// For a concept or a shape: the file it goes in, relative to the spec
    /// location.
    pub file: String,
    /// For a concept: the shape it has, if any.
    pub shape: Option<String>,
    pub description: String,
}

impl Request {
    fn stem(&self) -> &str {
        self.name
            .strip_suffix(self.kind.suffix())
            .unwrap_or(&self.name)
    }

    /// The index.pi of a new scope.
    pub fn scope_text(&self) -> String {
        let stem = self.stem();
        let uses = if self.keyword == "ui-component" {
            "use /lib\nuse /ui\n"
        } else {
            "use /lib\n"
        };
        format!(
            "{uses}\nexport {keyword} {name}:\n    concept: {{{stem}Concept}}\n\n\
             concept {stem}Concept:\n    pitch:\n{pitch}\n\n    shape: {{{stem}Shape}}\n\n\
             shape {stem}Shape:\n    description:\n{description}\n",
            keyword = self.keyword,
            name = self.name,
            pitch = prose(&self.description, 8),
            description = prose(first_sentence(&self.description), 8),
        )
    }

    /// The concept or shape, as added to the end of a file.
    pub fn anchor_text(&self) -> String {
        match self.kind {
            ComponentKind::Concept => {
                let shape = self
                    .shape
                    .as_ref()
                    .map(|shape| format!("\n\n    shape: {{{shape}}}"))
                    .unwrap_or_default();
                format!(
                    "concept {}:\n    pitch:\n{}{shape}\n",
                    self.name,
                    prose(&self.description, 8)
                )
            }
            _ => format!(
                "shape {}:\n    description:\n{}\n",
                self.name,
                prose(&self.description, 8)
            ),
        }
    }

    /// `existing` with the concept or shape added to its end, after a blank
    /// line.
    pub fn appended_to(&self, existing: &str) -> String {
        let mut text = existing.trim_end_matches('\n').to_string();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&self.anchor_text());
        text
    }

    /// Writes the component into the spec under `spec_dir`, returning the file
    /// written.
    pub fn write(&self, spec_dir: &Path) -> Result<PathBuf> {
        match self.kind {
            ComponentKind::Scope => {
                if let Some(problem) = folder_problem(spec_dir, &self.folder) {
                    bail!("{problem}");
                }
                let folder = spec_dir.join(self.folder.trim().trim_end_matches('/'));
                std::fs::create_dir_all(&folder)
                    .with_context(|| format!("Couldn't create {}", folder.display()))?;
                let file = folder.join("index.pi");
                std::fs::write(&file, self.scope_text())
                    .with_context(|| format!("Couldn't write {}", file.display()))?;
                Ok(file)
            }
            ComponentKind::Concept | ComponentKind::Shape => {
                let file = spec_dir.join(&self.file);
                let existing = std::fs::read_to_string(&file)
                    .with_context(|| format!("Couldn't read {}", file.display()))?;
                if declared_names(&existing)
                    .iter()
                    .any(|(_, name)| *name == self.name)
                {
                    bail!("{} is already declared in {}", self.name, self.file);
                }
                std::fs::write(&file, self.appended_to(&existing))
                    .with_context(|| format!("Couldn't write {}", file.display()))?;
                Ok(file)
            }
        }
    }

    /// The prompt handing the component to `skill` to create.
    pub fn skill_prompt(&self, skill: &str) -> String {
        let mut fields = vec![format!("Name: {}", self.name)];
        match self.kind {
            ComponentKind::Scope => {
                fields.push(format!("Kind: {}", self.keyword));
                fields.push(format!("Folder: {}", self.folder.trim()));
            }
            ComponentKind::Concept => {
                fields.push(format!("File: {}", self.file));
                fields.push(format!(
                    "Shape: {}",
                    self.shape.as_deref().unwrap_or("None")
                ));
            }
            ComponentKind::Shape => fields.push(format!("File: {}", self.file)),
        }
        let mut prompt = format!(
            "/{skill} Create a new {} named {}.\n\n{}",
            self.kind.noun(),
            self.name,
            fields.join("\n")
        );
        if !self.description.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(self.description.trim());
        }
        prompt
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn request(kind: ComponentKind) -> Request {
        Request {
            kind,
            name: full_name("FileTree", kind),
            keyword: "ui-component".into(),
            folder: "ui/components/file-tree".into(),
            file: "index.pi".into(),
            shape: None,
            description: "Shows the project's files: {all} of them. As a tree.\n- even this".into(),
        }
    }

    #[test]
    fn names_get_their_word_and_must_be_identifiers() {
        assert_eq!(full_name("Parser", ComponentKind::Scope), "ParserScope");
        assert_eq!(
            full_name("ParserScope", ComponentKind::Scope),
            "ParserScope"
        );
        assert_eq!(full_name("Idea", ComponentKind::Concept), "IdeaConcept");
        assert_eq!(name_problem(""), None);
        assert!(name_problem("parser").is_some());
        assert!(name_problem("Par ser").is_some());
        assert!(name_problem("Par-ser").is_some());
        assert_eq!(name_problem("Parser2"), None);
    }

    #[test]
    fn folders_follow_the_name_and_kind() {
        assert_eq!(default_folder("ParserScope", "scope"), "scope/parser");
        assert_eq!(
            default_folder("FileTreeScope", "ui-component"),
            "ui/components/file-tree"
        );
        let dir = std::env::temp_dir();
        assert!(folder_problem(&dir, "../x").is_some());
        assert!(folder_problem(&dir, "/x").is_some());
        assert!(folder_problem(&dir, " ").is_some());
    }

    #[test]
    fn declared_names_are_found() {
        let text = "use /lib\nfrom ./a import B\n\nexport scope AScope:\n    concept: {AConcept}\n\n\
                    concept AConcept: // note\n    pitch: x\nexport abstract anchor Base extends X as kw:\n";
        assert_eq!(
            declared_names(text),
            [
                ("scope".to_string(), "AScope".to_string()),
                ("concept".into(), "AConcept".into()),
                ("anchor".into(), "Base".into()),
            ]
        );
    }

    #[test]
    fn prose_is_escaped() {
        assert_eq!(
            prose("a {b}: c\n\n- d\n// e", 4),
            "    a \\{b\\}\\: c\n\n    \\- d\n    \\// e"
        );
        assert_eq!(first_sentence("One. Two."), "One.");
    }

    #[test]
    fn skill_prompts_start_with_the_skill() {
        let prompt = request(ComponentKind::Scope).skill_prompt("build-scope");
        assert!(
            prompt.starts_with(
                "/build-scope Create a new scope named FileTreeScope.\n\nName: FileTreeScope\nKind: ui-component\nFolder: ui/components/file-tree\n\n"
            ),
            "{prompt}"
        );
        assert!(prompt.ends_with("- even this"));
    }

    /// What each form writes is a spec piton reads, with the description as
    /// typed.
    #[test]
    fn written_components_check() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/components-test");
        let _ = std::fs::remove_dir_all(&project);
        let dir = project.join("spec");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(
            project.join("piton.config.pi"),
            "use @piton/config\n\nexport piton-config Project:\n    root: ./spec\n    entry: ./spec/index.pi\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lib/index.pi"),
            "export abstract anchor Shape as shape:\n    description:: string\n\n\
             export abstract anchor Concept as concept:\n    pitch:: string\n    shape:: null:: extends Shape: null\n\n\
             export abstract anchor Scope as scope:\n    concept:: extends Concept\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ui/index.pi"),
            "from /lib import Scope\n\nexport abstract anchor UiComponent extends Scope as ui-component:\n    theme: themed\n",
        )
        .unwrap();
        std::fs::write(dir.join("index.pi"), "use /lib\n").unwrap();

        let scope = request(ComponentKind::Scope).write(&dir).unwrap();
        assert_eq!(scope, dir.join("ui/components/file-tree/index.pi"));
        assert!(request(ComponentKind::Scope).write(&dir).is_err());

        let shape = request(ComponentKind::Shape);
        shape.write(&dir).unwrap();
        let concept = Request {
            shape: Some(shape.name.clone()),
            ..request(ComponentKind::Concept)
        };
        concept.write(&dir).unwrap();
        assert!(concept.write(&dir).is_err(), "declared twice");

        for file in [scope, dir.join("index.pi")] {
            let out = Command::new("piton")
                .arg("compile")
                .arg(&file)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}:\n{}\n{}",
                file.display(),
                std::fs::read_to_string(&file).unwrap(),
                String::from_utf8_lossy(&out.stderr)
            );
            let json = std::fs::read_to_string(file.with_extension("json")).unwrap();
            assert!(
                json.contains("Shows the project's files: {all} of them. As a tree. - even this"),
                "{json}"
            );
        }
        let _ = std::fs::remove_dir_all(&project);
    }
}
