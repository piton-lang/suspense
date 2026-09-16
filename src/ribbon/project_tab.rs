//! The ribbon's Project tab, for the project as a whole: New Project, which
//! sets up a new project and opens it, and Open Project, which picks a
//! piton.config.pi whose folder becomes the project.

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Command, CommandPlace, Ribbon};
use crate::new_project::NewProject;
use crate::project_directory::ProjectDirectory;

pub(super) const COMMANDS: &[CommandPlace] = &[
    CommandPlace {
        command: Command::NewProject,
        group: "Project",
        primary: true,
    },
    CommandPlace {
        command: Command::OpenProject,
        group: "Project",
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

/// Open Project: always so, whether or not a project is open; the open
/// project's name is beside the tabs instead.
pub(super) fn open_project(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    button(
        "project-directory",
        IconName::FolderOpen,
        "Open Project…".into(),
    )
    .tooltip("Pick a piton.config.pi file; its folder becomes the project")
    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx)))
    .into_any_element()
}

/// The widest the project's name gets before it is cut off.
const MAX_NAME_WIDTH: Pixels = px(320.);

/// The open project's name, a label beside the tabs, or "No project"; its
/// tooltip gives the project's path.
pub(super) fn project_name(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let project = ProjectDirectory::get(cx);
    let name = project.as_ref().map(|dir| {
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string())
    });
    let label = h_flex()
        .id("ribbon-project-name")
        .min_w_0()
        .max_w(MAX_NAME_WIDTH)
        .gap_1p5()
        .px_3()
        .child(
            Icon::new(IconName::FolderClosed)
                .small()
                .text_color(theme.muted_foreground),
        )
        .map(|label| match name {
            Some(name) => label.child(div().min_w_0().truncate().font_medium().child(name)),
            None => label.child(div().text_color(theme.muted_foreground).child("No project")),
        })
        .when_some(project, |label, dir| {
            let path = SharedString::from(dir.display().to_string());
            label.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
        });
    // Lets UI tests find the name; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(label).into_any_element()
}

impl Ribbon {
    /// Opens the project picker; the folder of the file picked becomes the
    /// project.
    pub fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = ProjectDirectory::pick(cx);
        cx.spawn_in(window, async move |_, cx| {
            let result = picked.await;
            cx.update(|window, cx| match result {
                Ok(Some(dir)) => ProjectDirectory::set(dir, cx),
                Ok(None) => {}
                Err(err) => window.push_notification(Notification::error(err.to_string()), cx),
            })
            .ok();
        })
        .detach();
    }
}
