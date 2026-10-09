//! The models a prompt can be sent to, as the ChatInputScope's model says:
//! those the harness in use can be told, and the one the user chose for it,
//! remembered per harness, as the UserPreferencesScope says. None chosen is
//! Default, which tells the harness no model, so it uses its own.

use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::Mutex;

use crate::agent::Agent;

/// A model a harness can be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    /// As the harness is told it, with `--model`.
    pub id: String,
    /// What the picker reads, short.
    pub label: String,
    /// Its full name, muted beside it in the menu.
    pub full: String,
}

/// The models `agent` can be told: Claude Code's aliases, or those Codex's
/// or OpenCode's own configuration lists.
pub fn available(agent: Agent) -> Vec<Model> {
    match agent {
        Agent::Claude => [
            ("opus", "Opus", "Claude Opus, the latest"),
            ("sonnet", "Sonnet", "Claude Sonnet, the latest"),
            ("haiku", "Haiku", "Claude Haiku, the latest"),
        ]
        .into_iter()
        .map(|(id, label, full)| Model {
            id: id.into(),
            label: label.into(),
            full: full.into(),
        })
        .collect(),
        Agent::Codex => configured(codex_models(&codex_config())),
        Agent::OpenCode => configured(opencode_models(&opencode_configs())),
    }
}

/// Models named in a harness's configuration, each labelled by its last
/// part, as `gpt-5` for `openai/gpt-5`, in the order first found.
fn configured(ids: Vec<String>) -> Vec<Model> {
    let mut models: Vec<Model> = Vec::new();
    for id in ids {
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let label = id.rsplit('/').next().unwrap_or(&id).to_string();
        models.push(Model {
            label,
            full: id.clone(),
            id,
        });
    }
    models
}

/// Codex's configuration, as it keeps it.
fn codex_config() -> String {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(dirs::home_dir()?.join(".codex")));
    home.and_then(|home| std::fs::read_to_string(home.join("config.toml")).ok())
        .unwrap_or_default()
}

/// Every model `config`, Codex's `config.toml`, names: its own `model`, and
/// each profile's.
fn codex_models(config: &str) -> Vec<String> {
    config
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "model").then_some(())?;
            let value = value.split('#').next()?.trim();
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            (!value.is_empty()).then(|| value.to_string())
        })
        .collect()
}

/// OpenCode's global configuration files, as it keeps them.
fn opencode_configs() -> Vec<String> {
    let Some(dir) = dirs::config_dir().map(|dir| dir.join("opencode")) else {
        return Vec::new();
    };
    ["opencode.json", "opencode.jsonc"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(dir.join(name)).ok())
        .collect()
}

/// Every model `configs`, OpenCode's, name: each one's `model` and
/// `small_model`, then each provider's models, as `provider/model`.
fn opencode_models(configs: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    for config in configs {
        let Ok(config) = serde_json::from_str::<serde_json::Value>(&without_comments(config))
        else {
            continue;
        };
        for key in ["model", "small_model"] {
            if let Some(model) = config.get(key).and_then(|model| model.as_str()) {
                ids.push(model.to_string());
            }
        }
        if let Some(providers) = config.get("provider").and_then(|p| p.as_object()) {
            for (provider, settings) in providers {
                if let Some(models) = settings.get("models").and_then(|m| m.as_object()) {
                    ids.extend(models.keys().map(|model| format!("{provider}/{model}")));
                }
            }
        }
    }
    ids
}

/// JSONC as JSON: its `//` and `/* */` comments, outside strings, gone.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The models chosen, by harness, once read.
#[cfg(not(test))]
static CHOSEN: Mutex<Option<HashMap<Agent, Option<String>>>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// Each test's own choices, apart from every other's.
    static TEST_CHOSEN: std::cell::RefCell<HashMap<Agent, Option<String>>> = Default::default();
}

/// Does `with` what is chosen, by harness.
fn with_chosen<T>(with: impl FnOnce(&mut HashMap<Agent, Option<String>>) -> T) -> T {
    #[cfg(test)]
    {
        TEST_CHOSEN.with_borrow_mut(with)
    }
    #[cfg(not(test))]
    {
        let mut chosen = CHOSEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with(chosen.get_or_insert_with(HashMap::new))
    }
}

/// The file `agent`'s choice is kept in; none in tests.
fn file(agent: Agent) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(
        dirs::config_dir()?
            .join("suspense")
            .join(format!("model-{}", agent.command())),
    )
}

/// The model chosen for `agent`, by its id; none for Default.
pub fn chosen(agent: Agent) -> Option<String> {
    with_chosen(|chosen| {
        chosen
            .entry(agent)
            .or_insert_with(|| {
                file(agent)
                    .and_then(|file| std::fs::read_to_string(file).ok())
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
            })
            .clone()
    })
}

/// Chooses `model` for `agent`, none for Default, and remembers it.
pub fn choose(agent: Agent, model: Option<String>) {
    if let Some(file) = file(agent) {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        // Only a convenience, so failing to keep it is ignored.
        std::fs::write(&file, model.as_deref().unwrap_or_default()).ok();
    }
    with_chosen(|chosen| chosen.insert(agent, model));
}

/// What the picker reads for `model`, `agent`'s: its label, or "Default".
pub fn label(agent: Agent, model: Option<&str>) -> String {
    let Some(model) = model else {
        return "Default".into();
    };
    available(agent)
        .into_iter()
        .find(|known| known.id == model)
        .map_or_else(
            || model.rsplit('/').next().unwrap_or(model).to_string(),
            |known| known.label,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code is told its aliases; Codex and OpenCode what their own
    /// configuration names.
    #[test]
    fn each_harness_lists_its_models() {
        let ids: Vec<String> = available(Agent::Claude).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["opus", "sonnet", "haiku"]);
        let codex = "model = \"gpt-5\" # the main one\n[profiles.fast]\nmodel = \"o4-mini\"\nmodel_provider = \"openai\"\n";
        assert_eq!(codex_models(codex), ["gpt-5", "o4-mini"]);
        let opencode = r#"{
            // the default
            "model": "anthropic/claude-sonnet-4", /* and */
            "provider": { "zai": { "models": { "glm-5": {} } } }
        }"#;
        let ids = opencode_models(&[opencode.to_string()]);
        assert_eq!(ids, ["anthropic/claude-sonnet-4", "zai/glm-5"]);
        let models = configured(ids);
        assert_eq!(models[0].label, "claude-sonnet-4");
        assert_eq!(models[0].full, "anthropic/claude-sonnet-4");
    }

    /// The choice is per harness, Default until chosen.
    #[test]
    fn choices_are_per_harness() {
        assert_eq!(label(Agent::Claude, None), "Default");
        assert_eq!(label(Agent::Claude, Some("opus")), "Opus");
        choose(Agent::Codex, Some("gpt-5".into()));
        assert_eq!(chosen(Agent::Codex).as_deref(), Some("gpt-5"));
        choose(Agent::Codex, None);
        assert_eq!(chosen(Agent::Codex), None);
    }
}
