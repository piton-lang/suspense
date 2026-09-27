//! The coding harnesses Suspense knows: those Belay writes for, and the one
//! every run goes to, which the user picks in the settings and which is
//! remembered in the platform's per-user config directory.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::RwLock;

use anyhow::{Context as _, Result, anyhow};

/// Where the application keeps its per-user files, inside the config directory.
const APP_DIR: &str = "suspense";
const FILE_NAME: &str = "agent";

/// A coding harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    OpenCode,
    Codex,
}

impl Agent {
    /// Every agent, in the order they are listed and written.
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::OpenCode, Agent::Codex];

    /// Every agent, in the order the settings offer them to run.
    pub const RUNNABLE: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::OpenCode];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::OpenCode => "OpenCode",
            Agent::Codex => "Codex",
        }
    }

    /// The directory it reads its agentic Markdown and reference files from,
    /// and Belay writes into.
    pub fn directory(self) -> &'static str {
        match self {
            Agent::Claude => ".claude",
            Agent::OpenCode => ".opencode",
            Agent::Codex => ".codex",
        }
    }

    /// Its adapter anchor's name, as @piton/belay exports it.
    pub fn adapter(self) -> &'static str {
        match self {
            Agent::Claude => "ClaudeCodeAdapter",
            Agent::OpenCode => "OpenCodeAdapter",
            Agent::Codex => "CodexAdapter",
        }
    }

    /// The command that runs it.
    pub fn command(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::OpenCode => "opencode",
            Agent::Codex => "codex",
        }
    }

    /// Whether its command can be found on the `PATH`.
    pub fn installed(self) -> bool {
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| dir.join(self.command()).is_file())
        })
    }

    /// The conversation `id` this agent reported, told apart from another
    /// agent's so it is only ever carried on by the agent it began with.
    /// Claude Code's are left as they are, as they were before any other
    /// agent could be picked.
    pub fn session(self, id: &str) -> String {
        match self {
            Agent::Claude => id.to_string(),
            _ => format!("{}:{id}", self.command()),
        }
    }

    /// The agent a conversation began with, and its own id for it.
    pub fn of_session(session: &str) -> (Agent, &str) {
        Agent::ALL
            .into_iter()
            .filter(|agent| *agent != Agent::Claude)
            .find_map(|agent| {
                let id = session.strip_prefix(agent.command())?.strip_prefix(':')?;
                Some((agent, id))
            })
            .unwrap_or((Agent::Claude, session))
    }

    /// Whether a task's run can be fed more messages while it works: Claude
    /// Code's can, while `codex exec` and `opencode run` read a single prompt.
    pub fn can_be_fed(self) -> bool {
        self == Agent::Claude
    }
}

/// The agent picked, until it is read from the preference file.
#[cfg(not(test))]
static CURRENT: RwLock<Option<Agent>> = RwLock::new(None);

#[cfg(test)]
thread_local! {
    /// Each test's own pick, so tests running at once never see another's.
    static CURRENT: std::cell::Cell<Option<Agent>> = const { std::cell::Cell::new(None) };
}

/// The agent every run goes to: the one the user picked, or Claude Code until
/// one is. Tests always start with Claude Code, whatever the user picked.
pub fn current() -> Agent {
    #[cfg(test)]
    return CURRENT.get().unwrap_or(Agent::Claude);
    #[cfg(not(test))]
    {
        if let Some(agent) = *CURRENT.read().unwrap() {
            return agent;
        }
        let agent = file()
            .ok()
            .and_then(|file| load(&file))
            .unwrap_or(Agent::Claude);
        *CURRENT.write().unwrap() = Some(agent);
        agent
    }
}

/// Picks `agent` for every run from the next on, and saves it as the user's
/// choice. It holds for the session even when it can't be saved.
pub fn set(agent: Agent) -> Result<()> {
    #[cfg(test)]
    {
        CURRENT.set(Some(agent));
        Ok(())
    }
    #[cfg(not(test))]
    {
        *CURRENT.write().unwrap() = Some(agent);
        save(agent, &file()?)
    }
}

#[cfg_attr(test, allow(dead_code))]
fn file() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| anyhow!("no config directory"))?;
    Ok(config_dir.join(APP_DIR).join(FILE_NAME))
}

fn load(file: &Path) -> Option<Agent> {
    let name = fs::read_to_string(file).ok()?;
    Agent::ALL
        .into_iter()
        .find(|agent| agent.command() == name.trim())
}

fn save(agent: Agent, file: &Path) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::write(file, agent.command()).with_context(|| format!("could not save {}", file.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Agent, load, save};

    #[test]
    fn saved_choice_loads_back() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/agent-preference-test");
        let file = dir.join("agent");
        fs::remove_dir_all(&dir).ok();

        assert_eq!(load(&file), None);
        for agent in Agent::ALL {
            save(agent, &file).unwrap();
            assert_eq!(load(&file), Some(agent));
        }
    }

    /// Each agent's conversations are told apart, and Claude Code's, saved
    /// before others could be picked, are still its own.
    #[test]
    fn sessions_know_their_agent() {
        for agent in Agent::ALL {
            let session = agent.session("abc");
            assert_eq!(Agent::of_session(&session), (agent, "abc"));
        }
        assert_eq!(Agent::of_session("3f2a-11"), (Agent::Claude, "3f2a-11"));
    }
}
