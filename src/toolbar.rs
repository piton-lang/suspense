//! Toolbar along the top of the main window: pick the project directory and
//! run `piton build` in it, and switch between light and dark mode at the far
//! right.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Disableable, WindowExt};
use gpui_kit::*;

use crate::piton_build;
use crate::project_directory::ProjectDirectory;
use crate::theme_preference;

pub struct Toolbar {
    building: bool,
}

impl Toolbar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify())
            .detach();
        Self { building: false }
    }

    /// Whether a build is running.
    pub fn is_building(&self) -> bool {
        self.building
    }

    /// Whether Build can run: there is a project and no build is running.
    pub fn can_build(&self, cx: &App) -> bool {
        ProjectDirectory::get(cx).is_some() && !self.building
    }

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

impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let project = ProjectDirectory::get(cx);
        let project_label: SharedString = project
            .as_ref()
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| "Open Project…".into());
        let project_tooltip: SharedString = project
            .as_ref()
            .map(|dir| dir.display().to_string().into())
            .unwrap_or_else(|| "Select a piton.config.pi file".into());

        let side = || {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                side()
                    .child(
                        Button::new("project-directory")
                            .ghost()
                            .label(project_label)
                            .tooltip(project_tooltip)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_project(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("build")
                            .primary()
                            .label("Build")
                            .tooltip("Run piton build")
                            .loading(self.building)
                            .disabled(project.is_none() || self.building)
                            .on_click(cx.listener(|this, _, window, cx| this.build(window, cx))),
                    ),
            )
            .child(
                side()
                    .justify_end()
                    .child(
                        gpui_kit::component::switch::Switch::new("dark-mode")
                            .label("Dark mode")
                            .checked(cx.theme().is_dark())
                            .on_click(|dark, window, cx| set_dark_mode(*dark, window, cx)),
                    )
                    .child(
                        Button::new("settings")
                            .ghost()
                            .icon(IconName::Settings)
                            .tooltip("Settings")
                            .on_click(|_, _, cx| crate::settings_window::open(cx)),
                    ),
            )
    }
}

/// Switches between light and dark mode, saving the choice.
pub fn set_dark_mode(dark: bool, window: &mut Window, cx: &mut App) {
    let mode = if dark {
        gpui_kit::component::ThemeMode::Dark
    } else {
        gpui_kit::component::ThemeMode::Light
    };
    if let Err(err) = theme_preference::set(mode, window, cx) {
        let note = Notification::error(format!("{err:#}")).title("Could not save dark mode");
        window.push_notification(note, cx);
    }
}
