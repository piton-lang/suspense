//! The subagents a task's run has started, for the right sidebar: each with
//! what it was started to do, what it is doing now, and whether it is still
//! at work. The task counts as running while any is.

use gpui_kit::SharedString;

use crate::harness::{HarnessEvent, SubagentState};

/// Whether a subagent is at work, or how it ended.
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
    /// What it was started to do.
    pub description: SharedString,
    /// Which kind of agent it is, such as Explore or Plan, when known.
    pub kind: Option<SharedString>,
    /// What it last said it was doing.
    pub activity: Option<SharedString>,
    pub state: State,
}

/// A run's subagents, in the order they started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subagents {
    pub list: Vec<Subagent>,
}

impl Subagents {
    /// Follows a harness event. Reports on tasks that didn't start as
    /// subagents, such as shell commands, are left out.
    pub fn apply(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::SubagentStarted {
                id,
                description,
                kind,
            } => {
                if self.find(id).is_none() {
                    self.list.push(Subagent {
                        id: id.clone(),
                        description: description.clone().into(),
                        kind: kind.clone().map(Into::into),
                        activity: None,
                        state: State::Running,
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
                }
            }
            _ => {}
        }
    }

    /// The run is over: any subagent still at work was stopped with it.
    pub fn end(&mut self) {
        for agent in &mut self.list {
            if agent.state == State::Running {
                agent.state = State::Stopped;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_subagents_and_ignores_other_tasks() {
        let mut agents = Subagents::default();
        agents.apply(&HarnessEvent::SubagentStarted {
            id: "a".into(),
            description: "Plan the code".into(),
            kind: Some("Plan".into()),
        });
        // A shell command's reports: never started as a subagent.
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
        agents.apply(&HarnessEvent::SubagentStarted {
            id: "a".into(),
            description: "Explore".into(),
            kind: None,
        });
        agents.end();
        assert_eq!(agents.list[0].state, State::Stopped);
    }
}
