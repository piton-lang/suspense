//! The ribbon's Project tab, for the project as a whole: Open Project, which
//! picks a piton.config.pi whose folder becomes the project.

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use super::{Command, CommandPlace, Ribbon};
use crate::project_directory::ProjectDirectory;

pub(super) const COMMANDS: &[CommandPlace] = &[CommandPlace {
    command: Command::OpenProject,
    group: "Project",
    primary: true,
}];

/// Open Project: the project's name once one is open.
pub(super) fn open_project(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let (label, tooltip): (SharedString, SharedString) = match ProjectDirectory::get(cx) {
        Some(dir) => (
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
            format!("{}\nOpen a different piton.config.pi", dir.display()).into(),
        ),
        None => (
            "Open Project…".into(),
            "Pick a piton.config.pi file; its folder becomes the project".into(),
        ),
    };
    button("project-directory", IconName::FolderOpen, label)
        .tooltip(tooltip)
        .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx)))
        .into_any_element()
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
