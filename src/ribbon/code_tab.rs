//! The ribbon's Code tab, for working on the code: its Run group holds Find
//! How to Run until the project's run targets are found, then a button for
//! each target and Find Again.

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::Button;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize, Ribbon, RunCommand, RunPrimary, RunRelease};
use crate::project_directory::ProjectDirectory;
use crate::run_targets::ProjectTargets;

const GROUP: &str = "Run";

/// The tab's commands: those of the open project's run targets.
pub(super) fn commands(cx: &App) -> Vec<CommandPlace> {
    let targets = ProjectTargets::get(cx).targets;
    if targets.is_empty() || ProjectDirectory::get(cx).is_none() {
        return vec![CommandPlace {
            command: Command::FindHowToRun,
            group: GROUP,
            size: CommandSize::Full,
            primary: true,
        }];
    }
    let mut places: Vec<CommandPlace> = (0..targets.len())
        .map(|ix| CommandPlace {
            command: Command::RunTarget(ix),
            group: GROUP,
            // The way to run the project leads, large; the rest are slim.
            size: if ix == 0 {
                CommandSize::Full
            } else {
                CommandSize::Slim
            },
            primary: ix == 0,
        })
        .collect();
    places.push(CommandPlace {
        command: Command::FindRunAgain,
        group: GROUP,
        size: CommandSize::Slim,
        primary: false,
    });
    places
}

/// Find How to Run: has the harness look through the project.
pub(super) fn find_how_to_run(
    button: &impl Fn(ElementId, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button(
        "find-how-to-run".into(),
        IconName::Search,
        "Find How to Run".into(),
    )
    .map(|button| {
        if project.is_none() {
            button.tooltip("Open a project to run it")
        } else {
            button.tooltip_with_action(
                "Have the harness look through the project to find how it is built, run, and tested",
                &RunPrimary,
                None,
            )
        }
    })
    .disabled(project.is_none())
    .on_click(cx.listener(|_, _, _, cx| cx.emit(RunCommand::Find)))
    .into_any_element()
}

/// Find Again: looks for the project's targets afresh.
pub(super) fn find_again(
    button: &impl Fn(ElementId, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    button(
        "find-run-again".into(),
        IconName::RefreshCw,
        "Find Again".into(),
    )
    .tooltip("Have the harness look through the project for how to run it afresh")
    .on_click(cx.listener(|_, _, _, cx| cx.emit(RunCommand::Find)))
    .into_any_element()
}

/// A run target's button, its tooltip giving its command, with a spinner
/// while it runs.
pub(super) fn run_target(
    ix: usize,
    button: &impl Fn(ElementId, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectTargets::get(cx);
    let Some(target) = project.targets.get(ix) else {
        return div().into_any_element();
    };
    let running = project.running == Some(ix);
    button(
        ("run-target", ix).into(),
        target.kind.icon(),
        target.name.clone().into(),
    )
    .map(|button| {
        let tooltip = if running {
            format!("{} is running: show it", target.command)
        } else {
            target.command.clone()
        };
        // The primary target is the one its shortcut runs.
        if ix == 0 {
            button.tooltip_with_action(tooltip, &RunPrimary, None)
        } else if target.release {
            button.tooltip_with_action(tooltip, &RunRelease, None)
        } else {
            button.tooltip(tooltip)
        }
    })
    .loading(running)
    .on_click(cx.listener(move |_, _, _, cx| cx.emit(RunCommand::Target(ix))))
    .into_any_element()
}
