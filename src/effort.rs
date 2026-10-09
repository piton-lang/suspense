//! The reasoning effort a prompt can be sent with, as the ChatInputScope's
//! effort says: only Codex takes one, told with `--model-reasoning-effort`.
//! Claude Code and OpenCode take no such setting. The level chosen is
//! remembered per harness, as the UserPreferencesScope says. None chosen is
//! Default, which tells the harness no effort, so it uses its own.

use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::Mutex;

use crate::agent::Agent;

/// A reasoning effort level a harness can be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effort {
    /// As the harness is told it, with `--model-reasoning-effort`.
    pub id: String,
    /// What the picker reads.
    pub label: String,
}

/// The levels `agent` can be told: Codex's low, medium, and high; none for
/// Claude Code or OpenCode, which take no such setting.
pub fn available(agent: Agent) -> Vec<Effort> {
    match agent {
        Agent::Codex => [("low", "Low"), ("medium", "Medium"), ("high", "High")]
            .into_iter()
            .map(|(id, label)| Effort {
                id: id.into(),
                label: label.into(),
            })
            .collect(),
        Agent::Claude | Agent::OpenCode => Vec::new(),
    }
}

/// The effort levels chosen, by harness, once read.
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
            .join(format!("effort-{}", agent.command())),
    )
}

/// The effort level chosen for `agent`, by its id; none for Default.
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

/// Chooses `effort` for `agent`, none for Default, and remembers it.
pub fn choose(agent: Agent, effort: Option<String>) {
    if let Some(file) = file(agent) {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        // Only a convenience, so failing to keep it is ignored.
        std::fs::write(&file, effort.as_deref().unwrap_or_default()).ok();
    }
    with_chosen(|chosen| chosen.insert(agent, effort));
}

/// What the picker reads for `effort`, `agent`'s: its label, or "Default".
pub fn label(agent: Agent, effort: Option<&str>) -> String {
    let Some(effort) = effort else {
        return "Default".into();
    };
    available(agent)
        .into_iter()
        .find(|known| known.id == effort)
        .map_or_else(|| effort.to_string(), |known| known.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only Codex lists levels; Claude Code and OpenCode take none.
    #[test]
    fn only_codex_has_effort_levels() {
        let ids: Vec<String> = available(Agent::Codex)
            .into_iter()
            .map(|level| level.id)
            .collect();
        assert_eq!(ids, ["low", "medium", "high"]);
        assert!(available(Agent::Claude).is_empty());
        assert!(available(Agent::OpenCode).is_empty());
    }

    /// The choice is per harness, Default until chosen.
    #[test]
    fn choices_are_per_harness() {
        assert_eq!(label(Agent::Codex, None), "Default");
        assert_eq!(label(Agent::Codex, Some("medium")), "Medium");
        choose(Agent::Codex, Some("high".into()));
        assert_eq!(chosen(Agent::Codex).as_deref(), Some("high"));
        choose(Agent::Codex, None);
        assert_eq!(chosen(Agent::Codex), None);
    }
}
