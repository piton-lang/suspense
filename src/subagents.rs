//! The background tasks a task's run has started, for the right sidebar:
//! its subagents and the shell commands it ran in the background, each with
//! what it was started to do, what it is doing now, how long it has been
//! running, and whether it is still at work. The task counts as running
//! while any is.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::{FontFeatures, SharedString};

pub use crate::harness::BackgroundKind as Kind;
use crate::harness::{HarnessEvent, SubagentState};

/// Whether a background task is at work, or how it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Completed,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    pub id: String,
    /// A subagent or a command.
    pub task: Kind,
    /// What it was started to do: a subagent's description, or the command
    /// itself.
    pub description: SharedString,
    /// Which kind of agent it is, such as Explore or Plan, when known.
    pub kind: Option<SharedString>,
    /// What it last said it was doing.
    pub activity: Option<SharedString>,
    pub state: State,
    /// When the harness said it started.
    pub started: Instant,
    /// When it ended, once it has.
    pub ended: Option<Instant>,
    /// It was asked to stop, and hasn't been notified ending yet.
    pub stopping: bool,
}

impl Subagent {
    /// How long it has been running, or ran.
    pub fn elapsed(&self) -> Duration {
        self.ended
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started)
    }
}

/// A run's background tasks, in the order they started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subagents {
    pub list: Vec<Subagent>,
    /// The command each shell tool call of the run's own ran, by the call,
    /// so a command run in the background is shown as itself.
    commands: HashMap<String, String>,
}

impl Subagents {
    /// Follows a harness event. Reports on tasks that weren't seen starting
    /// as a subagent or a command are left out.
    pub fn apply(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::ToolCalled {
                id,
                name,
                input,
                subagent: false,
            } if name == "Bash" => {
                if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
                    self.commands.insert(id.clone(), command.to_string());
                }
            }
            HarnessEvent::SubagentStarted {
                id,
                task,
                description,
                kind,
                tool,
            } => {
                if self.find(id).is_none() {
                    let command = (*task == Kind::Command)
                        .then(|| tool.as_ref().and_then(|tool| self.commands.get(tool)))
                        .flatten();
                    let description = command.unwrap_or(description);
                    self.list.push(Subagent {
                        id: id.clone(),
                        task: *task,
                        description: description.clone().into(),
                        kind: kind.clone().map(Into::into),
                        activity: None,
                        state: State::Running,
                        started: Instant::now(),
                        ended: None,
                        stopping: false,
                    });
                }
            }
            HarnessEvent::SubagentProgress { id, activity } => {
                if let Some(agent) = self.find(id) {
                    agent.activity = Some(activity.clone().into());
                }
            }
            HarnessEvent::SubagentEnded { id, state } => {
                if let Some(agent) = self.find(id) {
                    agent.state = match state {
                        SubagentState::Completed => State::Completed,
                        SubagentState::Failed => State::Failed,
                        SubagentState::Stopped => State::Stopped,
                    };
                    agent.ended.get_or_insert_with(Instant::now);
                    agent.stopping = false;
                }
            }
            _ => {}
        }
    }

    /// The one `id` was asked to stop: it stays at work until the harness
    /// notifies its end.
    pub fn stopping(&mut self, id: &str) {
        if let Some(agent) = self.find(id).filter(|agent| agent.state == State::Running) {
            agent.stopping = true;
        }
    }

    /// The run is over: any still at work was stopped with it.
    pub fn end(&mut self) {
        for agent in &mut self.list {
            if agent.state == State::Running {
                agent.state = State::Stopped;
                agent.ended.get_or_insert_with(Instant::now);
                agent.stopping = false;
            }
        }
    }

    /// Whether any is still at work.
    pub fn any_running(&self) -> bool {
        self.list.iter().any(|agent| agent.state == State::Running)
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.list
            .iter()
            .filter(|agent| agent.state == State::Running)
            .count()
    }

    fn find(&mut self, id: &str) -> Option<&mut Subagent> {
        self.list.iter_mut().find(|agent| agent.id == id)
    }
}

/// Tabular figures, so a running time doesn't shift as it counts.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

/// How long something has run, as "0:42", "12:05", or "1:02:33".
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: &str, task: Kind, description: &str, tool: Option<&str>) -> HarnessEvent {
        HarnessEvent::SubagentStarted {
            id: id.into(),
            task,
            description: description.into(),
            kind: None,
            tool: tool.map(Into::into),
        }
    }

    #[test]
    fn follows_subagents_and_ignores_other_tasks() {
        let mut agents = Subagents::default();
        agents.apply(&HarnessEvent::SubagentStarted {
            id: "a".into(),
            task: Kind::Subagent,
            description: "Plan the code".into(),
            kind: Some("Plan".into()),
            tool: None,
        });
        // Reports on a task never seen starting.
        agents.apply(&HarnessEvent::SubagentProgress {
            id: "b".into(),
            activity: "cargo test".into(),
        });
        agents.apply(&HarnessEvent::SubagentProgress {
            id: "a".into(),
            activity: "Running Read files".into(),
        });
        assert_eq!(agents.list.len(), 1);
        assert_eq!(agents.running(), 1);
        assert_eq!(
            agents.list[0].activity.as_deref(),
            Some("Running Read files")
        );
        agents.apply(&HarnessEvent::SubagentEnded {
            id: "a".into(),
            state: SubagentState::Completed,
        });
        assert_eq!(agents.running(), 0);
        assert_eq!(agents.list[0].state, State::Completed);
    }

    #[test]
    fn those_at_work_when_the_run_ends_were_stopped() {
        let mut agents = Subagents::default();
        agents.apply(&started("a", Kind::Subagent, "Explore", None));
        agents.end();
        assert_eq!(agents.list[0].state, State::Stopped);
        assert!(agents.list[0].ended.is_some());
    }

    /// A command run in the background is shown as the command its tool
    /// call ran, alongside subagents, in the order they started.
    #[test]
    fn commands_are_shown_as_themselves() {
        let mut agents = Subagents::default();
        agents.apply(&started("a", Kind::Subagent, "Explore", None));
        agents.apply(&HarnessEvent::ToolCalled {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "cargo build", "description": "Build" }),
            subagent: false,
        });
        agents.apply(&started("b", Kind::Command, "Build", Some("t1")));
        agents.apply(&started("c", Kind::Command, "Test", None));
        let shown: Vec<_> = agents.list.iter().map(|agent| agent.description.as_ref()).collect();
        assert_eq!(shown, ["Explore", "cargo build", "Test"]);
        assert_eq!(agents.list[1].task, Kind::Command);
    }

    /// One asked to stop stays at work until the harness notifies its end.
    #[test]
    fn one_asked_to_stop_runs_until_it_ends() {
        let mut agents = Subagents::default();
        agents.apply(&started("a", Kind::Command, "sleep 100", None));
        agents.stopping("a");
        assert!(agents.list[0].stopping);
        assert_eq!(agents.list[0].state, State::Running);
        agents.apply(&HarnessEvent::SubagentEnded {
            id: "a".into(),
            state: SubagentState::Stopped,
        });
        assert!(!agents.list[0].stopping);
        assert_eq!(agents.list[0].state, State::Stopped);
        assert!(agents.list[0].ended.is_some());
    }

    #[test]
    fn elapsed_is_minutes_and_seconds_then_hours() {
        assert_eq!(format_elapsed(Duration::from_secs(42)), "0:42");
        assert_eq!(format_elapsed(Duration::from_secs(12 * 60 + 5)), "12:05");
        assert_eq!(format_elapsed(Duration::from_secs(3600 + 2 * 60 + 33)), "1:02:33");
    }
}
