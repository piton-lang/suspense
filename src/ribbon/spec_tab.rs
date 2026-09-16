//! The ribbon's Spec tab, for working on the spec: Build Spec, which runs
//! `piton build` in the project and says how it went.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, WindowExt as _};
use gpui_kit::*;

use super::{Command, CommandPlace, Ribbon};
use crate::piton_build;
use crate::project_directory::ProjectDirectory;

pub(super) const COMMANDS: &[CommandPlace] = &[CommandPlace {
    command: Command::BuildSpec,
    group: "Build",
    primary: true,
}];

/// Build Spec: disabled rather than hidden until it can run, so it is always
/// found in the same place, with the tooltip saying why.
pub(super) fn build_spec(
    ribbon: &Ribbon,
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    let tooltip = if project.is_none() {
        "Open a project to build its spec"
    } else if ribbon.building {
        "The spec is building"
    } else {
        "Run piton build to compile the spec"
    };
    button("build", IconName::Hammer, "Build Spec".into())
        .tooltip(tooltip)
        .loading(ribbon.building)
        .disabled(project.is_none() || ribbon.building)
        .on_click(cx.listener(|this, _, window, cx| this.build(window, cx)))
        .into_any_element()
}

impl Ribbon {
    /// Whether a build is running.
    pub fn is_building(&self) -> bool {
        self.building
    }

    /// Whether Build can run: there is a project and no build is running.
    pub fn can_build(&self, cx: &App) -> bool {
        ProjectDirectory::get(cx).is_some() && !self.building
    }

    /// Runs `piton build` in the project, then says how it went.
    pub fn build(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = ProjectDirectory::get(cx).filter(|_| !self.building) else {
            return;
        };
        self.building = true;
        cx.notify();

        let build = piton_build::run(dir, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = build.await;
            this.update_in(cx, |this, window, cx| {
                this.building = false;
                cx.notify();
                let note = match result {
                    Ok(outcome) if outcome.success => {
                        let title = match outcome.files.len() {
                            1 => "Built 1 file".to_string(),
                            count => format!("Built {count} files"),
                        };
                        let files = outcome.files;
                        Notification::success("")
                            .title(title)
                            .content(move |_, _, _| {
                                // One line per written file, never wrapped.
                                gpui_kit::component::v_flex()
                                    .children(files.iter().map(|file| {
                                        gpui_kit::component::label::Label::new(file.clone())
                                            .whitespace_nowrap()
                                            .truncate()
                                    }))
                                    .into_any_element()
                            })
                    }
                    Ok(outcome) => Notification::error(outcome.report).title("Build failed"),
                    Err(err) => {
                        Notification::error(err.to_string()).title("Could not run piton build")
                    }
                };
                window.push_notification(note, cx);
            })
            .ok();
        })
        .detach();
    }
}
