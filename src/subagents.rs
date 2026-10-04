//! What goes on apart from a task's run's main thread, for the right
//! sidebar: its subagents, whether the main thread waits on them or runs them
//! in the background, the shell commands it runs in the background, and the
//! commands it waits on once they have run long, each with what it was
//! started to do, what it is doing now, how long it has been running, and
//! whether it is still at work.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::{FontFeatures, SharedString};

pub use crate::harness::BackgroundKind as Kind;
use crate::harness::{HarnessEvent, SubagentState};

/// How long a command the main thread waits on runs before it is shown.
pub const LONG_COMMAND: Duration = Duration::from_secs(15);

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
    /// The harness's id of it: its task id for a background task, or else
    /// the id of the tool call that started it.
    pub id: String,
    /// Its task id, by which the harness can stop it on its own; none for
    /// one the harness gave none, which can't be.
    pub task_id: Option<String>,
    /// The main thread waits on it, its tool call's result ending it.
    pub waited: bool,
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
    /// Whether it is shown: a subagent or a background command always, and
    /// a command the main thread waits on only once it has run long.
    pub fn shown(&self) -> bool {
        self.task == Kind::Subagent || !self.waited || self.elapsed() >= LONG_COMMAND
    }

    /// Whether it can be stopped on its own: at work, with a task id, and
    /// not a command the main thread waits on.
    pub fn stoppable(&self) -> bool {
        self.state == State::Running
            && self.task_id.is_some()
            && !(self.waited && self.task == Kind::Command)
    }

    fn end(&mut self, state: State) {
        if self.state == State::Running {
            self.state = state;
            self.ended.get_or_insert_with(Instant::now);
            self.stopping = false;
        }
    }

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
    /// as a subagent or a command are left out, as are the calls subagents
    /// make, and every other tool call.
    pub fn apply(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::ToolCalled {
                id,
                name,
                input,
                subagent: false,
            } => {
                let background = input
                    .get("run_in_background")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let text = |key: &str| {
                    input
                        .get(key)
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                };
                let task = match name.as_str() {
                    "Bash" => {
                        if let Some(command) = text("command") {
                            self.commands.insert(id.clone(), command);
                        }
                        Kind::Command
                    }
                    "Agent" | "Task" => Kind::Subagent,
                    _ => return,
                };
                // One run in the background is the harness's background
                // task, which reports its own start.
                if background || self.list.iter().any(|agent| agent.id == *id) {
                    return;
                }
                let description = match task {
                    Kind::Command => text("command"),
                    Kind::Subagent => text("description").or_else(|| text("prompt")),
                }
                .unwrap_or_default();
                self.list.push(Subagent {
                    id: id.clone(),
                    task_id: None,
                    waited: true,
                    task,
                    description: description.into(),
                    kind: text("subagent_type").map(Into::into),
                    activity: None,
                    state: State::Running,
                    started: Instant::now(),
                    ended: None,
                    stopping: false,
                });
            }
            // A call the main thread waited on ends when its result comes
            // back, done or failed as that says.
            HarnessEvent::ToolFinished { id, is_error } => {
                if let Some(agent) = self
                    .list
                    .iter_mut()
                    .find(|agent| agent.waited && agent.id == *id)
                {
                    agent.end(if *is_error {
                        State::Failed
                    } else {
                        State::Completed
                    });
                }
            }
            HarnessEvent::SubagentStarted {
                id,
                task,
                description,
                kind,
                tool,
            } => {
                if self.find(id).is_some() {
                    return;
                }
                // A subagent the main thread waits on, reported as a task
                // too: the task id lets it be stopped.
                let waited = self.list.iter_mut().find(|agent| {
                    agent.waited
                        && agent.task_id.is_none()
                        && agent.task == *task
                        && agent.state == State::Running
                        && match tool {
                            Some(tool) => agent.id == *tool,
                            None => agent.description.as_ref() == description.as_str(),
                        }
                });
                if let Some(agent) = waited {
                    agent.task_id = Some(id.clone());
                    return;
                }
                let command = (*task == Kind::Command)
                    .then(|| tool.as_ref().and_then(|tool| self.commands.get(tool)))
                    .flatten();
                let description = command.unwrap_or(description);
                self.list.push(Subagent {
                    id: id.clone(),
                    task_id: Some(id.clone()),
                    waited: false,
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
            HarnessEvent::SubagentProgress { id, activity } => {
                if let Some(agent) = self.find(id) {
                    agent.activity = Some(activity.clone().into());
                }
            }
            HarnessEvent::SubagentEnded { id, state } => {
                if let Some(agent) = self.find(id) {
                    agent.end(match state {
                        SubagentState::Completed => State::Completed,
                        SubagentState::Failed => State::Failed,
                        SubagentState::Stopped => State::Stopped,
                    });
                }
            }
            _ => {}
        }
    }

    /// Those shown, in the order they started.
    pub fn shown(&self) -> impl Iterator<Item = &Subagent> {
        self.list.iter().filter(|agent| agent.shown())
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
            agent.end(State::Stopped);
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

    /// The one the harness knows as `id`, by its own id or its task id.
    fn find(&mut self, id: &str) -> Option<&mut Subagent> {
        self.list
            .iter_mut()
            .find(|agent| agent.id == id || agent.task_id.as_deref() == Some(id))
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

    fn call(id: &str, name: &str, input: serde_json::Value, subagent: bool) -> HarnessEvent {
        HarnessEvent::ToolCalled {
            id: id.into(),
            name: name.into(),
            input,
            subagent,
        }
    }

    /// A command the main thread waits on shows only once it has run long;
    /// a short one never does, nor a subagent's command, nor any other tool
    /// call.
    #[test]
    fn only_long_commands_the_main_thread_waits_on_show() {
        let mut agents = Subagents::default();
        agents.apply(&call("t1", "Bash", serde_json::json!({ "command": "ls" }), false));
        agents.apply(&call("t2", "Bash", serde_json::json!({ "command": "cargo test" }), false));
        agents.apply(&call("t3", "Bash", serde_json::json!({ "command": "rm x" }), true));
        agents.apply(&call("t4", "Read", serde_json::json!({ "file_path": "a" }), false));
        assert_eq!(agents.list.len(), 2);
        assert_eq!(agents.shown().count(), 0);
        // The short one is done within its 15 seconds: never shown.
        agents.apply(&HarnessEvent::ToolFinished {
            id: "t1".into(),
            is_error: false,
        });
        // The other runs long.
        agents.list[1].started -= LONG_COMMAND;
        let shown: Vec<_> = agents.shown().map(|agent| agent.description.as_ref()).collect();
        assert_eq!(shown, ["cargo test"]);
        assert!(agents.list[1].waited);
        assert!(!agents.list[1].stoppable(), "a waited-on command can't be stopped");
        agents.apply(&HarnessEvent::ToolFinished {
            id: "t2".into(),
            is_error: true,
        });
        assert_eq!(agents.list[1].state, State::Failed);
        assert_eq!(agents.shown().count(), 1, "it stays shown once it ran long");
        assert_eq!(agents.list[0].state, State::Completed);
        assert!(!agents.list[0].shown());
    }

    /// A command run in the background shows from the start, by the task
    /// the harness reports, not its call, whose result comes back at once.
    #[test]
    fn background_commands_show_at_once() {
        let mut agents = Subagents::default();
        agents.apply(&call(
            "t1",
            "Bash",
            serde_json::json!({ "command": "cargo build", "run_in_background": true }),
            false,
        ));
        assert!(agents.list.is_empty());
        agents.apply(&started("b", Kind::Command, "Build", Some("t1")));
        agents.apply(&HarnessEvent::ToolFinished {
            id: "t1".into(),
            is_error: false,
        });
        assert_eq!(agents.list[0].state, State::Running);
        assert_eq!(agents.list[0].description.as_ref(), "cargo build");
        assert!(agents.list[0].shown() && agents.list[0].stoppable());
    }

    /// A subagent the main thread waits on shows at once, can be stopped
    /// once the harness gives it a task id, and ends at the harness's
    /// notice or its call's result, whichever comes first.
    #[test]
    fn subagents_the_main_thread_waits_on_show_and_end_on_their_result() {
        let mut agents = Subagents::default();
        agents.apply(&call(
            "t1",
            "Agent",
            serde_json::json!({ "description": "Explore the code", "subagent_type": "Explore" }),
            false,
        ));
        assert_eq!(agents.shown().count(), 1);
        assert!(!agents.list[0].stoppable(), "no task id yet");
        agents.apply(&started("a1", Kind::Subagent, "Explore the code", Some("t1")));
        assert_eq!(agents.list.len(), 1, "its task is the same row");
        assert!(agents.list[0].stoppable());
        agents.stopping("a1");
        assert!(agents.list[0].stopping);
        agents.apply(&HarnessEvent::ToolFinished {
            id: "t1".into(),
            is_error: false,
        });
        assert_eq!(agents.list[0].state, State::Completed);
        agents.apply(&HarnessEvent::SubagentEnded {
            id: "a1".into(),
            state: SubagentState::Stopped,
        });
        assert_eq!(agents.list[0].state, State::Completed, "the first end holds");
    }
}
