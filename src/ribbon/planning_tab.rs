//! The ribbon's Planning tab, for planning work before it is sent, as the
//! RibbonPlanningTab says: its Plan group holds Planning, which opens the
//! project's plan, New Story, and Send Sprint; its Agent group holds Draft
//! Stories and Research.

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::Button;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize};
use crate::planning::{
    CONTEXT, NewStory, OpenDraftStories, OpenPlanning, OpenResearch, SendSprint, SprintPending,
};
use crate::project_directory::ProjectDirectory;

const GROUP: &str = "Plan";
const AGENT: &str = "Agent";

pub(super) const COMMANDS: &[CommandPlace] = &[
    CommandPlace {
        command: Command::OpenPlanning,
        group: GROUP,
        size: CommandSize::Full,
        primary: true,
    },
    CommandPlace {
        command: Command::NewStory,
        group: GROUP,
        size: CommandSize::Slim,
        primary: true,
    },
    CommandPlace {
        command: Command::SendSprint,
        group: GROUP,
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::DraftStories,
        group: AGENT,
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::Research,
        group: AGENT,
        size: CommandSize::Slim,
        primary: false,
    },
];

/// Planning: opens the project's plan, or selects its tab.
pub(super) fn open_planning(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button("open-planning", IconName::ListChecks, "Planning".into())
        .map(|button| {
            if project.is_none() {
                button.tooltip("Open a project to plan its work")
            } else {
                button.tooltip("Open the project's stories and sprint")
            }
        })
        .disabled(project.is_none())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenPlanning), cx))
        .into_any_element()
}

/// New Story: opens the plan and adds a story to its Next sprint.
pub(super) fn new_story(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button("new-story", IconName::Plus, "New Story".into())
        .map(|button| {
            if project.is_none() {
                button.tooltip("Open a project to plan its work")
            } else {
                button.tooltip("Add a story to Next sprint")
            }
        })
        .disabled(project.is_none())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewStory), cx))
        .into_any_element()
}

/// Send Sprint: sends Next sprint, enabled while it holds a story, no sprint
/// is under way, and planning isn't paused for review.
pub(super) fn send_sprint(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let pending = if ProjectDirectory::get(cx).is_some() {
        SprintPending::get(cx)
    } else {
        SprintPending::default()
    };
    button("send-sprint", IconName::Send, "Send Sprint".into())
        .tooltip({
            let said = match (pending.held(), pending.waiting) {
                (Some(why), _) => why.to_string(),
                (None, 1) => "Send the one story in Next sprint".to_string(),
                (None, n) => format!("Send the {n} stories in Next sprint, in order"),
            };
            if pending.auto_start {
                format!("{said}. The next sprint starts automatically.")
            } else {
                said
            }
        })
        .disabled(pending.held().is_some())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(SendSprint), cx))
        .into_any_element()
}

/// Draft Stories: opens the plan and its drafting panel, the request box
/// focused, by the same route as the Backlog heading's and Ctrl/Cmd+Shift+D.
/// Drafting only changes the plan, so a pause doesn't disable it.
pub(super) fn draft_stories(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button("draft-stories", IconName::Sparkles, "Draft Stories".into())
        .map(|button| {
            if project.is_none() {
                button.tooltip("Open a project to plan its work")
            } else {
                button.tooltip_with_action(
                    "Ask the agent to break a request into proposed stories",
                    &OpenDraftStories,
                    Some(CONTEXT),
                )
            }
        })
        .disabled(project.is_none())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenDraftStories), cx))
        .into_any_element()
}

/// Research: opens the plan and its research panel, its box focused, by the
/// same route as the Backlog heading's and Ctrl/Cmd+Shift+R. A second
/// research while paused keeps the pause, so a pause doesn't disable it.
pub(super) fn research(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button("research", IconName::Search, "Research".into())
        .map(|button| {
            if project.is_none() {
                button.tooltip("Open a project to plan its work")
            } else {
                button.tooltip_with_action(
                    "Ask the agent to look into a topic, reading only, before anything is built",
                    &OpenResearch,
                    Some(CONTEXT),
                )
            }
        })
        .disabled(project.is_none())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenResearch), cx))
        .into_any_element()
}
