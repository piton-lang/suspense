//! Harness mentions: `/` for the harness's skills and commands and `@` for its
//! agents. The chat input completes them; the prompt carries them to the
//! harness as typed, which runs a leading `/name` and uses a mentioned
//! `@agent-name`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, Range, TextEdit,
};
use serde_json::{Value, json};

use crate::hidden_anchor::APP_DIR;
use crate::piton_lsp::lsp_position;

/// The skills and agents the last harness run reported, relative to the
/// project directory.
const HARNESS_REPORT: &str = "harness.json";

/// Agents every harness has, whether or not a run has reported them yet.
const BUILT_IN_AGENTS: [(&str, &str); 3] = [
    (
        "general-purpose",
        "Researches complex questions, searches code, and carries out multi-step tasks.",
    ),
    (
        "Explore",
        "Searches the codebase read-only and reports what it finds.",
    ),
    ("Plan", "Designs an implementation plan for a task."),
];

/// The prefix an agent is inserted with.
const AGENT_PREFIX: &str = "agent-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    Skill,
    Command,
    Agent,
}

impl MentionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Agent => "agent",
        }
    }
}

/// A skill, command, or agent the harness can be asked to use.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocable {
    pub kind: MentionKind,
    pub name: String,
    pub description: Option<String>,
}

impl Invocable {
    /// The mention as inserted, after its sigil: `name ` for a skill or
    /// command, and `agent-name ` for an agent.
    fn mention_name(&self) -> String {
        match self.kind {
            MentionKind::Agent => format!("{AGENT_PREFIX}{} ", self.name),
            _ => format!("{} ", self.name),
        }
    }

    /// The whole mention as inserted: `/name ` or `@agent-name `.
    pub fn mention(&self) -> String {
        let sigil = if self.kind == MentionKind::Agent {
            '@'
        } else {
            '/'
        };
        format!("{sigil}{}", self.mention_name())
    }
}

/// A mention being typed at the cursor.
#[derive(Debug, PartialEq)]
pub struct Mention<'a> {
    /// `/` or `@`.
    pub sigil: char,
    /// Byte offset of the name, just after the sigil.
    pub name_start: usize,
    /// What has been typed of the name.
    pub typed: &'a str,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':')
}

/// The mention the cursor at byte `offset` is in, if any: a `/` or `@` at the
/// start of the text or after whitespace, followed by what has been typed of
/// a name.
pub fn mention_at(text: &str, offset: usize) -> Option<Mention<'_>> {
    let before = text.get(..offset)?;
    let name_len: usize = before
        .chars()
        .rev()
        .take_while(|&c| is_name_char(c))
        .map(char::len_utf8)
        .sum();
    let name_start = before.len() - name_len;
    let sigil = before[..name_start]
        .chars()
        .next_back()
        .filter(|c| matches!(c, '/' | '@'))?;
    let starts_word = before[..name_start - 1]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    starts_word.then(|| Mention {
        sigil,
        name_start,
        typed: &before[name_start..],
    })
}

/// Completions for the mention at byte `offset` of `text`, or `None` when
/// the cursor is not in one. Ranges are in the text's own coordinates.
pub fn complete(
    text: &str,
    offset: usize,
    invocables: &[Invocable],
) -> Option<Vec<CompletionItem>> {
    let mention = mention_at(text, offset)?;
    let agents = mention.sigil == '@';
    let typed = if agents {
        mention
            .typed
            .strip_prefix(AGENT_PREFIX)
            .unwrap_or(mention.typed)
    } else {
        mention.typed
    }
    .to_lowercase();
    let range = Range::new(
        lsp_position(text, mention.name_start),
        lsp_position(text, offset),
    );

    let mut matches: Vec<(u8, &Invocable)> = invocables
        .iter()
        .filter(|invocable| (invocable.kind == MentionKind::Agent) == agents)
        .filter_map(|invocable| {
            let name = invocable.name.to_lowercase();
            if name.starts_with(&typed) {
                Some((0, invocable))
            } else if name.contains(&typed) {
                Some((1, invocable))
            } else {
                None
            }
        })
        .collect();
    matches.sort_by(|(a_rank, a), (b_rank, b)| {
        a_rank
            .cmp(b_rank)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Some(
        matches
            .into_iter()
            .map(|(_, invocable)| {
                let new_text = invocable.mention_name();
                CompletionItem {
                    label: invocable.name.clone(),
                    kind: Some(match invocable.kind {
                        MentionKind::Skill => CompletionItemKind::FUNCTION,
                        MentionKind::Command => CompletionItemKind::METHOD,
                        MentionKind::Agent => CompletionItemKind::CLASS,
                    }),
                    detail: Some(invocable.kind.label().into()),
                    documentation: invocable.description.clone().map(Documentation::String),
                    filter_text: Some(invocable.name.clone()),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, new_text))),
                    ..CompletionItem::default()
                }
            })
            .collect(),
    )
}

/// The harness's configuration directory.
fn claude_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
}

/// Every skill, command, and agent the harness has for `project_dir`: the
/// project's, the user's, and enabled plugins' from disk, the built-in
/// agents, and whatever the last harness run reported. Blocking.
pub fn discover(project_dir: Option<&Path>) -> Vec<Invocable> {
    discover_in(project_dir, claude_dir().as_deref())
}

fn discover_in(project_dir: Option<&Path>, claude_dir: Option<&Path>) -> Vec<Invocable> {
    let mut found = Vec::new();
    if let Some(project_dir) = project_dir {
        scan(&project_dir.join(".claude"), None, &mut found);
    }
    if let Some(claude_dir) = claude_dir {
        scan(claude_dir, None, &mut found);
        for (plugin, install_path) in enabled_plugins(claude_dir, project_dir) {
            scan(&install_path, Some(&plugin), &mut found);
        }
    }
    found.extend(BUILT_IN_AGENTS.iter().map(|(name, description)| Invocable {
        kind: MentionKind::Agent,
        name: name.to_string(),
        description: Some(description.to_string()),
    }));
    if let Some(report) = project_dir.and_then(read_report) {
        found.extend(report);
    }

    // The first of each name wins: those found on disk carry descriptions.
    let mut unique: Vec<Invocable> = Vec::new();
    for invocable in found {
        let same_group = |other: &Invocable| {
            (other.kind == MentionKind::Agent) == (invocable.kind == MentionKind::Agent)
                && other.name == invocable.name
        };
        if !unique.iter().any(same_group) {
            unique.push(invocable);
        }
    }
    unique
}

/// Adds the skills, commands, and agents under a `.claude` directory or a
/// plugin, naming a plugin's own `plugin:name`.
fn scan(root: &Path, plugin: Option<&str>, found: &mut Vec<Invocable>) {
    let named = |name: String| match plugin {
        Some(plugin) => format!("{plugin}:{name}"),
        None => name,
    };

    for dir in sorted_entries(&root.join("skills")) {
        let Ok(source) = fs::read_to_string(dir.join("SKILL.md")) else {
            continue;
        };
        let (name, description) = frontmatter(&source);
        let Some(name) = name.or_else(|| file_name(&dir)) else {
            continue;
        };
        found.push(Invocable {
            kind: MentionKind::Skill,
            name: named(name),
            description,
        });
    }

    // Commands in subdirectories are namespaced by them: `dir:name`.
    let commands_dir = root.join("commands");
    for file in markdown_files(&commands_dir) {
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        let name = file
            .with_extension("")
            .strip_prefix(&commands_dir)
            .map(|path| {
                path.components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        found.push(Invocable {
            kind: MentionKind::Command,
            name: named(name),
            description: frontmatter(&source).1,
        });
    }

    for file in markdown_files(&root.join("agents")) {
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        let (name, description) = frontmatter(&source);
        let Some(name) = name.or_else(|| file_name(&file.with_extension(""))) else {
            continue;
        };
        found.push(Invocable {
            kind: MentionKind::Agent,
            name: named(name),
            description,
        });
    }
}

fn file_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    entries.sort();
    entries
}

/// The `.md` files under `dir`, at any depth, in name order.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in sorted_entries(dir) {
        if path.is_dir() {
            files.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files
}

/// The `name` and `description` in a Markdown file's YAML frontmatter.
fn frontmatter(source: &str) -> (Option<String>, Option<String>) {
    let mut lines = source.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut fields: BTreeMap<&str, String> = BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        // An indented line continues a block value (`description: |`).
        if line.starts_with(char::is_whitespace) {
            if let Some(value) = current.and_then(|key| fields.get_mut(key)) {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
            }
            continue;
        }
        current = None;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let value = if matches!(value, "|" | ">" | "|-" | ">-") {
            ""
        } else {
            value
        };
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value);
        fields.insert(key, value.to_string());
        current = Some(key);
    }
    let field = |key| fields.get(key).filter(|value| !value.is_empty()).cloned();
    (field("name"), field("description"))
}

/// The enabled plugins, by name, with where each is installed.
fn enabled_plugins(claude_dir: &Path, project_dir: Option<&Path>) -> Vec<(String, PathBuf)> {
    let read_json = |path: PathBuf| -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    };

    // Project settings override the user's.
    let mut settings = vec![claude_dir.join("settings.json")];
    if let Some(project_dir) = project_dir {
        settings.push(project_dir.join(".claude/settings.json"));
        settings.push(project_dir.join(".claude/settings.local.json"));
    }
    let mut enabled = BTreeMap::new();
    for settings in settings.into_iter().filter_map(read_json) {
        if let Some(plugins) = settings.get("enabledPlugins").and_then(Value::as_object) {
            for (id, on) in plugins {
                enabled.insert(id.clone(), on.as_bool().unwrap_or(false));
            }
        }
    }

    let installed = read_json(claude_dir.join("plugins/installed_plugins.json"));
    let installs = installed
        .as_ref()
        .and_then(|installed| installed.get("plugins"))
        .and_then(Value::as_object);
    let project = project_dir.map(|dir| dir.to_string_lossy().into_owned());
    enabled
        .into_iter()
        .filter(|(_, on)| *on)
        .filter_map(|(id, _)| {
            // An install scoped to a project only counts in that project.
            let install = installs?.get(&id)?.as_array()?.iter().find(|install| {
                install
                    .get("projectPath")
                    .and_then(Value::as_str)
                    .is_none_or(|path| Some(path) == project.as_deref())
            })?;
            let install_path = install.get("installPath")?.as_str()?;
            let name = id.split('@').next().unwrap_or(&id).to_string();
            Some((name, PathBuf::from(install_path)))
        })
        .collect()
}

/// Saves the skills and agents a harness run reports in its `init` event, so
/// they are offered before the next run. Other events are ignored.
pub fn remember(event: &Value, project_dir: &Path) {
    if event.get("type").and_then(Value::as_str) != Some("system")
        || event.get("subtype").and_then(Value::as_str) != Some("init")
    {
        return;
    }
    let report = json!({
        "skills": event.get("skills").cloned().unwrap_or_else(|| json!([])),
        "agents": event.get("agents").cloned().unwrap_or_else(|| json!([])),
    });
    let dir = project_dir.join(APP_DIR);
    if fs::create_dir_all(&dir).is_ok() {
        fs::write(dir.join(HARNESS_REPORT), report.to_string()).ok();
    }
}

fn read_report(project_dir: &Path) -> Option<Vec<Invocable>> {
    let report: Value = serde_json::from_str(
        &fs::read_to_string(project_dir.join(APP_DIR).join(HARNESS_REPORT)).ok()?,
    )
    .ok()?;
    let names = |key: &str, kind: MentionKind| {
        report
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(move |name| Invocable {
                kind,
                name: name.to_string(),
                description: None,
            })
            .collect::<Vec<_>>()
    };
    let mut invocables = names("skills", MentionKind::Skill);
    invocables.extend(names("agents", MentionKind::Agent));
    Some(invocables)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use lsp_types::{CompletionTextEdit, Position, Range};
    use serde_json::json;

    use super::{
        Invocable, Mention, MentionKind, complete, discover_in, frontmatter, mention_at, remember,
    };

    #[test]
    fn finds_mentions_only_where_a_word_starts() {
        fn at(text: &str) -> Option<Mention<'_>> {
            mention_at(text, text.len())
        }
        assert_eq!(
            at("/code-rev"),
            Some(Mention {
                sigil: '/',
                name_start: 1,
                typed: "code-rev"
            })
        );
        assert_eq!(
            at("ask @agent-Ex"),
            Some(Mention {
                sigil: '@',
                name_start: 5,
                typed: "agent-Ex"
            })
        );
        assert_eq!(
            at("see\n@"),
            Some(Mention {
                sigil: '@',
                name_start: 5,
                typed: ""
            })
        );
        assert_eq!(at("src/main"), None);
        assert_eq!(at("me@example"), None);
        assert_eq!(at("See @{Appl"), None);
        assert_eq!(at("plain words"), None);
    }

    fn invocable(kind: MentionKind, name: &str) -> Invocable {
        Invocable {
            kind,
            name: name.into(),
            description: None,
        }
    }

    #[test]
    fn completes_skills_and_commands_after_a_slash_and_agents_after_an_at() {
        let invocables = [
            invocable(MentionKind::Skill, "simplify"),
            invocable(MentionKind::Command, "review-pr"),
            invocable(MentionKind::Skill, "code-review"),
            invocable(MentionKind::Agent, "code-reviewer"),
            invocable(MentionKind::Agent, "Explore"),
        ];
        let labels = |text: &str| -> Vec<String> {
            complete(text, text.len(), &invocables)
                .unwrap()
                .into_iter()
                .map(|item| item.label)
                .collect()
        };

        // Matches at the start of a name come first.
        assert_eq!(labels("/rev"), ["review-pr", "code-review"]);
        assert_eq!(labels("Try @agent-rev"), ["code-reviewer"]);
        assert_eq!(labels("@ex"), ["Explore"]);
        assert_eq!(complete("a/b", 3, &invocables), None);

        let text = "Use @Exp";
        let item = complete(text, text.len(), &invocables).unwrap().remove(0);
        let Some(CompletionTextEdit::Edit(edit)) = item.text_edit else {
            panic!("{item:?}");
        };
        assert_eq!(
            edit.range,
            Range::new(Position::new(0, 5), Position::new(0, 8))
        );
        assert_eq!(edit.new_text, "agent-Explore ");
    }

    #[test]
    fn reads_frontmatter() {
        assert_eq!(
            frontmatter("---\nname: dataviz\ndescription: \"Charts, done well\"\n---\n# Body"),
            (Some("dataviz".into()), Some("Charts, done well".into()))
        );
        assert_eq!(
            frontmatter("---\ndescription: |\n  Two\n  lines\nmodel: sonnet\n---\n"),
            (None, Some("Two lines".into()))
        );
        assert_eq!(frontmatter("# No frontmatter"), (None, None));
    }

    #[test]
    fn discovers_project_user_and_plugin_invocables() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/harness-mentions-test");
        fs::remove_dir_all(&root).ok();
        let project = root.join("project");
        let user = root.join("user");
        let plugin = root.join("plugins/cache/market/tools/1.0.0");
        let write = |path: &Path, text: &str| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };

        write(
            &project.join(".claude/skills/build/SKILL.md"),
            "---\nname: build-app\ndescription: Builds the app\n---\n",
        );
        write(
            &project.join(".claude/commands/git/sync.md"),
            "---\ndescription: Syncs\n---\n",
        );
        write(
            &user.join("agents/reviewer.md"),
            "---\nname: reviewer\n---\n",
        );
        write(
            &plugin.join("skills/lint/SKILL.md"),
            "---\nname: lint\n---\n",
        );
        write(
            &root.join("plugins/cache/market/off/skills/nope/SKILL.md"),
            "---\nname: nope\n---\n",
        );
        write(
            &user.join("settings.json"),
            &json!({ "enabledPlugins": { "tools@market": true, "off@market": false } }).to_string(),
        );
        write(
            &user.join("plugins/installed_plugins.json"),
            &json!({ "plugins": {
                "tools@market": [{ "scope": "user", "installPath": plugin }],
                "off@market": [{ "scope": "user", "installPath": root.join("plugins/cache/market/off") }],
            } })
            .to_string(),
        );
        remember(
            &json!({ "type": "system", "subtype": "init", "skills": ["build-app", "simplify"], "agents": ["Explore", "claude"] }),
            &project,
        );

        let found = discover_in(Some(&project), Some(&user));
        let names = |kind: MentionKind| -> Vec<&str> {
            found
                .iter()
                .filter(|invocable| invocable.kind == kind)
                .map(|invocable| invocable.name.as_str())
                .collect()
        };
        assert_eq!(
            names(MentionKind::Skill),
            ["build-app", "tools:lint", "simplify"]
        );
        assert_eq!(names(MentionKind::Command), ["git:sync"]);
        assert_eq!(
            names(MentionKind::Agent),
            ["reviewer", "general-purpose", "Explore", "Plan", "claude"]
        );
        let build = found
            .iter()
            .find(|invocable| invocable.name == "build-app")
            .unwrap();
        assert_eq!(build.description.as_deref(), Some("Builds the app"));
    }
}
