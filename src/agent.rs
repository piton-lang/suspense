//! The coding harnesses Suspense knows: those Belay writes for, and the one
//! each project's runs go to, which is the project's own, saved with its
//! settings, as the HarnessIntegrationScope says. Runs outside any project
//! go to Claude Code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::Result;

use crate::project_settings::ProjectSettings;

/// A coding harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    /// What every run of it is given in its environment, on the host or in
    /// a container: for Claude Code, the claude.ai account's connectors
    /// turned off, so no run is told one needs authorizing, as the
    /// HarnessIntegrationScope says. MCP servers a project configures stay.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Agent::Claude => &[("ENABLE_CLAUDEAI_MCP_SERVERS", "false")],
            Agent::OpenCode | Agent::Codex => &[],
        }
    }

    /// Whether its command can be found, where the user's terminal would
    /// find it.
    pub fn installed(self) -> bool {
        crate::programs::find(self.command()).is_some()
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

    /// The agent `command` runs, as [`Self::command`] names it.
    pub fn of_command(command: &str) -> Option<Agent> {
        Agent::ALL
            .into_iter()
            .find(|agent| agent.command() == command)
    }

    /// Whether it takes a system prompt of its own, apart from the prompt, as
    /// Claude Code does with `--append-system-prompt`. Codex and OpenCode
    /// don't, so theirs is given ahead of the prompt.
    pub fn takes_system_prompt(self) -> bool {
        self == Agent::Claude
    }

    /// Whether a task's run can be fed more messages while it works: Claude
    /// Code's can, while `codex exec` and `opencode run` read a single prompt.
    pub fn can_be_fed(self) -> bool {
        self == Agent::Claude
    }
}

#[cfg(test)]
thread_local! {
    /// Each test's own pick, so tests running at once never see another's.
    static CURRENT: std::cell::Cell<Option<Agent>> = const { std::cell::Cell::new(None) };
}

/// Each project's harness, once read, by its folder.
static PROJECTS: LazyLock<Mutex<HashMap<PathBuf, Agent>>> = LazyLock::new(Default::default);

fn projects() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Agent>> {
    PROJECTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where `agent`'s installation instructions are.
pub fn install_url(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "https://docs.claude.com/en/docs/claude-code/setup",
        Agent::Codex => "https://github.com/openai/codex",
        Agent::OpenCode => "https://opencode.ai/docs/",
    }
}

/// The harness the runs of the project at `project_dir` go to: the one
/// saved as its own, or, where none is, the first its piton.config.pi builds
/// for, or Claude Code where it builds for none, saved as its own then.
/// Runs outside any project go to Claude Code. A test's own pick, made with
/// [`set`], holds over every project's.
pub fn of_project(project_dir: Option<&Path>) -> Agent {
    #[cfg(test)]
    if let Some(agent) = CURRENT.get() {
        return agent;
    }
    let Some(project_dir) = project_dir else {
        return Agent::Claude;
    };
    if let Some(agent) = projects().get(project_dir) {
        return *agent;
    }
    let mut settings = ProjectSettings::load(project_dir);
    let saved = settings.harness.as_deref().and_then(Agent::of_command);
    let agent = saved.unwrap_or_else(|| {
        let config = std::fs::read_to_string(project_dir.join(crate::project_directory::CONFIG_FILE_NAME));
        let agent = config.ok().and_then(|config| first_built_for(&config)).unwrap_or(Agent::Claude);
        // A test's project is left as it is.
        if !cfg!(test) {
            settings.harness = Some(agent.command().to_string());
            settings.save(project_dir).ok();
        }
        agent
    });
    projects().insert(project_dir.to_path_buf(), agent);
    agent
}

/// The harness the project on screen runs with.
pub fn current(cx: &gpui_kit::App) -> Agent {
    of_project(crate::project_directory::ProjectDirectory::get(cx).as_deref())
}

/// The first agent `config`, a piton.config.pi, has an adapter for.
pub fn first_built_for(config: &str) -> Option<Agent> {
    Agent::ALL
        .into_iter()
        .filter_map(|agent| config.find(agent.adapter()).map(|at| (at, agent)))
        .min_by_key(|(at, _)| *at)
        .map(|(_, agent)| agent)
}

/// Picks `agent` for the runs of the project at `project_dir` from the next
/// on, saving it with the project. It holds for the session even when it
/// can't be saved.
pub fn set_for_project(project_dir: &Path, agent: Agent) -> Result<()> {
    projects().insert(project_dir.to_path_buf(), agent);
    let mut settings = ProjectSettings::load(project_dir);
    settings.harness = Some(agent.command().to_string());
    settings.save(project_dir)
}

/// Picks `agent` for every run of this test, whatever its project.
#[cfg(test)]
pub fn set(agent: Agent) -> Result<()> {
    CURRENT.set(Some(agent));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Agent, first_built_for, of_project, set_for_project};

    /// A project's harness is the first its config builds for, until one
    /// is picked for it, and each project keeps its own.
    #[test]
    fn each_project_has_its_own_harness() {
        let config = "BelayConfiguration { adapters: [CodexAdapter {}, ClaudeCodeAdapter {}] }";
        assert_eq!(first_built_for(config), Some(Agent::Codex));
        assert_eq!(first_built_for("nothing"), None);
        let root = std::env::temp_dir().join(format!("suspense-harness-{}", std::process::id()));
        let (one, two) = (root.join("one"), root.join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        std::fs::write(one.join("piton.config.pi"), config).unwrap();
        assert_eq!(of_project(Some(&one)), Agent::Codex);
        assert_eq!(of_project(Some(&two)), Agent::Claude);
        assert_eq!(of_project(None), Agent::Claude);
        set_for_project(&two, Agent::OpenCode).unwrap();
        assert_eq!(of_project(Some(&two)), Agent::OpenCode);
        assert_eq!(of_project(Some(&one)), Agent::Codex);
        let saved = crate::project_settings::ProjectSettings::load(&two);
        assert_eq!(saved.harness.as_deref(), Some("opencode"));
        std::fs::remove_dir_all(&root).ok();
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
