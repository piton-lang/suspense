//! The ribbon's Project tab, for the project as a whole: New Project, which
//! sets up a new project and opens it, and Open Project, which picks a
//! piton.config.pi whose folder becomes the project.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize};
use crate::new_project::NewProject;
use crate::project::open_project::OpenProject;

pub(super) const COMMANDS: &[CommandPlace] = &[
    CommandPlace {
        command: Command::NewProject,
        group: "Project",
        size: CommandSize::Slim,
        primary: true,
    },
    CommandPlace {
        command: Command::OpenProject,
        group: "Project",
        size: CommandSize::Slim,
        primary: true,
    },
];

/// New Project: opens the form for a new project.
pub(super) fn new_project(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button("new-project", IconName::FolderPlus, "New Project".into())
        .tooltip("Create a new project and open it")
        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewProject), cx))
        .into_any_element()
}

/// Open Project: always so, whether or not a project is open; the project
/// indicator beside the tabs shows which is. It opens a file browser for the
/// project's piton.config.pi in the main window's inset panel.
pub(super) fn open_project(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button(
        "project-directory",
        IconName::FolderOpen,
        "Open Project…".into(),
    )
    .tooltip("Pick a piton.config.pi file; its folder becomes the project")
    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenProject), cx))
    .into_any_element()
}
