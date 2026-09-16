//! The main window of the application, from which all main functionality is
//! reached: a ribbon on top, and below it the project tree in a sidebar on the
//! left, then prompt mode. A file's diff floats over all of it.

use std::path::PathBuf;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme, Root, WindowExt as _};
use gpui_kit::*;

use crate::activity::{Job, JobKind, RevealJob};
use crate::app::{APP_TITLE, Quit};
use crate::diff_view::{CloseDiff, DiffView, OpenInEditor};
use crate::divergence_view::{
    AnalyzeDivergence, CloseDivergence, DivergenceView, MinimizeDivergence, Opening,
    ViewDivergenceReports,
};
use crate::git_panel::GitPanel;
use crate::inset_panel::inset_panel;
use crate::new_project::{CloseNewProject, NewProject, NewProjectForm, ProjectCreated};
use crate::palette::{Palette, Picked, SystemCommand, SystemState};
use crate::project_directory::ProjectDirectory;
use crate::project_tree::{OpenDiff, OpenFile, ProjectTree};
use crate::prompt_mode::PromptMode;
use crate::ribbon::{self, Ribbon};
use crate::settings_window;
use crate::theme_preference;

/// Size the window restores to when it is un-maximized.
const RESTORE_SIZE: Size<Pixels> = size(px(1280.), px(800.));

/// The narrowest a split can be dragged.
const MIN_SPLIT_WIDTH: Pixels = px(240.);

/// The sidebar's width until it is dragged, and the narrowest it can be.
const SIDEBAR_WIDTH: Pixels = px(260.);
const MIN_SIDEBAR_WIDTH: Pixels = px(160.);

actions!(suspense, [FocusChat, TogglePalette]);

/// Esc anywhere in the window moves focus to the chat input. It is bound
/// without a context, so an editor or popup that has something to cancel
/// (a completion menu, extra cursors) takes Esc first. Ctrl/Cmd+P opens or
/// closes the palette.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", FocusChat, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-p", TogglePalette, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-p", TogglePalette, None),
    ]);
    crate::palette::bind_keys(cx);
    ribbon::bind_keys(cx);
    crate::diff_view::bind_keys(cx);
    crate::chat_input::bind_keys(cx);
    crate::folder_browser::bind_keys(cx);
}

pub struct MainWindow {
    ribbon: Entity<Ribbon>,
    sidebar: Entity<ProjectTree>,
    git_panel: Entity<GitPanel>,
    prompt_mode: Entity<PromptMode>,
    /// The palette last opened, which may since have closed.
    palette: Option<Entity<Palette>>,
    _palette_subscription: Option<Subscription>,
    sidebar_split: Entity<ResizableState>,
    /// A changed file's diff, or the new project form, floating over the
    /// window in an inset panel.
    diff: Option<Entity<DiffView>>,
    new_project: Option<Entity<NewProjectForm>>,
    /// The divergence panel, which can be minimized while its analysis
    /// carries on.
    divergence: Option<Entity<DivergenceView>>,
    divergence_minimized: bool,
    _divergence_subscriptions: Vec<Subscription>,
    /// Tracks the inset panel, which keeps focus within it while open.
    panel_focus: FocusHandle,
    /// What was last focused within the panel, to go back to when something
    /// takes focus beneath it.
    panel_last_focus: Option<FocusHandle>,
    _panel_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    pub fn open(cx: &mut App) -> Result<WindowHandle<Root>> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                None,
                RESTORE_SIZE,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_TITLE.into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            // Start in the saved light or dark mode, else the system's; the
            // ribbon can switch.
            theme_preference::apply(window, cx);
            let view = cx.new(|cx| MainWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ribbon = cx.new(Ribbon::new);
        let sidebar = cx.new(ProjectTree::new);
        let prompt_mode = cx.new(|cx| PromptMode::new(window, cx));
        let mut subscriptions = vec![
            // The ribbon's activity spinner follows whatever is running.
            cx.observe(&prompt_mode, |this, _, cx| this.refresh_jobs(cx)),
            cx.observe(&ribbon, |this, _, cx| this.refresh_jobs(cx)),
            cx.subscribe_in(&ribbon, window, |this, _, RevealJob(kind), window, cx| {
                this.reveal_job(*kind, window, cx)
            }),
            cx.subscribe_in(&sidebar, window, |this, _, OpenFile(path), window, cx| {
                this.prompt_mode.update(cx, |prompt_mode, cx| {
                    prompt_mode.open_file(path.clone(), window, cx)
                })
            }),
            cx.subscribe_in(&sidebar, window, |this, _, OpenDiff(path), window, cx| {
                this.open_diff(path.clone(), window, cx)
            }),
            // Until light or dark mode is chosen, the window keeps following
            // the system's appearance as it changes.
            cx.observe_window_appearance(window, |_, window, cx| {
                theme_preference::apply(window, cx)
            }),
            // Focus left on nothing, as when a focused file is closed, would
            // put Esc out of the window's reach; the chat input takes it, or
            // the inset panel while it is open.
            cx.on_focus_lost(window, |this, window, cx| {
                if this.panel_open() {
                    this.refocus_panel(window, cx);
                } else {
                    this.prompt_mode
                        .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx))
                }
            }),
        ];
        // While the inset panel is open, nothing beneath it can take focus:
        // focus that leaves it goes back to where it was within it.
        let panel_focus = cx.focus_handle();
        subscriptions.push(cx.on_focus_in(&panel_focus, window, |this, window, cx| {
            this.panel_last_focus = window.focused(cx);
        }));
        subscriptions.push(
            cx.on_focus_out(&panel_focus, window, |this, _, window, cx| {
                if this.panel_open() {
                    this.refocus_panel(window, cx);
                }
            }),
        );
        // Closing the window ends the application, so it asks first too.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let close = this
                .update(cx, |this, cx| this.confirm_quit(window, cx))
                .unwrap_or(true);
            // The settings window does not keep the application running.
            if close {
                cx.defer(|cx| cx.quit());
            }
            close
        });

        Self {
            ribbon,
            sidebar,
            git_panel: cx.new(|cx| GitPanel::new(window, cx)),
            prompt_mode,
            palette: None,
            _palette_subscription: None,
            sidebar_split: cx.new(|_| ResizableState::default()),
            diff: None,
            new_project: None,
            divergence: None,
            divergence_minimized: false,
            _divergence_subscriptions: Vec::new(),
            panel_focus,
            panel_last_focus: None,
            _panel_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Opens a changed file's diff in the floating panel, replacing any diff
    /// already there. Any file open in the split is left as it is.
    pub fn open_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let diff = cx.new(|cx| DiffView::new(path, cx));
        self.new_project = None;
        self.divergence_minimized = self.divergence.is_some();
        self._panel_subscriptions = vec![
            cx.subscribe_in(&diff, window, |this, _, _: &CloseDiff, window, cx| {
                this.close_panel(window, cx)
            }),
            // Opening the file closes the diff, and opens the file in the
            // split as clicking it in the tree would.
            cx.subscribe_in(&diff, window, |this, _, OpenInEditor(path), window, cx| {
                this.close_panel(window, cx);
                this.prompt_mode.update(cx, |prompt_mode, cx| {
                    prompt_mode.open_file(path.clone(), window, cx)
                })
            }),
        ];
        diff.read(cx).focus_handle(cx).focus(window, cx);
        self.diff = Some(diff);
        cx.notify();
    }

    /// Opens the new project form in the panel, fresh, in place of any diff.
    /// Opens the divergence panel for the open project, to analyze it or to
    /// view its reports, in place of anything else in the panel.
    pub fn open_divergence(
        &mut self,
        opening: Opening,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A minimized panel comes back, rather than another opening.
        if let Some(view) = self.divergence.clone() {
            view.update(cx, |view, cx| view.open_to(opening, cx));
            self.restore_divergence(&view, window, cx);
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        self.show_divergence(
            cx.new(|cx| DivergenceView::new(project_dir, opening, cx)),
            window,
            cx,
        );
    }

    /// Shows `view` in the panel, in place of anything else there.
    pub fn show_divergence(
        &mut self,
        view: Entity<DivergenceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._divergence_subscriptions = vec![
            cx.subscribe_in(&view, window, |this, _, _: &CloseDivergence, window, cx| {
                this.close_divergence(window, cx)
            }),
            cx.subscribe_in(
                &view,
                window,
                |this, _, _: &MinimizeDivergence, window, cx| this.minimize_divergence(window, cx),
            ),
            cx.observe(&view, |this, _, cx| this.refresh_jobs(cx)),
        ];
        self.divergence = Some(view.clone());
        self.restore_divergence(&view, window, cx);
    }

    /// Shows the divergence panel, in place of anything else in the panel.
    fn restore_divergence(
        &mut self,
        view: &Entity<DivergenceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff = None;
        self.new_project = None;
        self._panel_subscriptions.clear();
        self.divergence_minimized = false;
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.refresh_jobs(cx);
        cx.notify();
    }

    /// Hides the divergence panel, its analysis carrying on.
    fn minimize_divergence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.divergence_minimized = true;
        self.panel_last_focus = None;
        self.prompt_mode
            .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        cx.notify();
    }

    /// Closes the divergence panel, stopping any analysis still running.
    fn close_divergence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.showing_divergence();
        self.divergence = None;
        self.divergence_minimized = false;
        self._divergence_subscriptions.clear();
        self.refresh_jobs(cx);
        if showing {
            self.panel_last_focus = None;
            self.prompt_mode
                .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        }
        cx.notify();
    }

    /// Whether the divergence panel is showing, rather than minimized.
    fn showing_divergence(&self) -> bool {
        self.divergence.is_some() && !self.divergence_minimized
    }

    /// Tells the ribbon everything running: a spec build, the task and
    /// questions, and a divergence analysis.
    fn refresh_jobs(&mut self, cx: &mut Context<Self>) {
        let mut jobs = Vec::new();
        if self.ribbon.read(cx).is_building() {
            jobs.push(Job {
                kind: JobKind::Build,
                title: "Building the spec".into(),
                detail: None,
            });
        }
        jobs.extend(self.prompt_mode.read(cx).running_jobs());
        if self
            .divergence
            .as_ref()
            .is_some_and(|view| view.read(cx).is_running())
        {
            jobs.push(Job {
                kind: JobKind::Divergence,
                title: "Analyzing divergence".into(),
                detail: None,
            });
        }
        self.ribbon
            .update(cx, |ribbon, cx| ribbon.set_jobs(jobs, cx));
    }

    /// Reveals a running job: the task or question behind any inset panel,
    /// or the divergence panel.
    pub fn reveal_job(&mut self, kind: JobKind, window: &mut Window, cx: &mut Context<Self>) {
        match kind {
            JobKind::Build => {}
            JobKind::Divergence => {
                if let Some(view) = self.divergence.clone() {
                    self.restore_divergence(&view, window, cx);
                }
            }
            JobKind::Task | JobKind::Question(_) => {
                if self.showing_divergence() {
                    self.minimize_divergence(window, cx);
                } else if self.panel_open() {
                    self.close_panel(window, cx);
                }
                self.prompt_mode.update(cx, |prompt_mode, cx| match kind {
                    JobKind::Question(id) => prompt_mode.reveal_question(id, cx),
                    _ => prompt_mode.reveal_task(cx),
                });
            }
        }
    }

    pub fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| NewProjectForm::new(window, cx));
        self.diff = None;
        self.divergence_minimized = self.divergence.is_some();
        self._panel_subscriptions = vec![
            cx.subscribe_in(&form, window, |this, _, _: &CloseNewProject, window, cx| {
                this.close_panel(window, cx)
            }),
            // Once created, the project opens and the panel closes.
            cx.subscribe_in(
                &form,
                window,
                |this, _, ProjectCreated(folder, warning), window, cx| {
                    ProjectDirectory::set(folder.clone(), cx);
                    this.close_panel(window, cx);
                    // Something that didn't stop the project opening, like
                    // Git, still failed.
                    if let Some(warning) = warning {
                        window.push_notification(
                            gpui_kit::component::notification::Notification::warning(
                                warning.clone(),
                            )
                            .title("Project created"),
                            cx,
                        );
                    }
                },
            ),
        ];
        self.new_project = Some(form);
        cx.notify();
    }

    /// Sends focus back into the open inset panel: to what was last focused
    /// there, or what it holds.
    fn refocus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = if let Some(diff) = &self.diff {
            diff.read(cx).focus_handle(cx)
        } else if let Some(form) = &self.new_project {
            form.read(cx).focus_handle(cx)
        } else if let Some(view) = self
            .divergence
            .as_ref()
            .filter(|_| !self.divergence_minimized)
        {
            view.read(cx).focus_handle(cx)
        } else {
            return;
        };
        let target = self
            .panel_last_focus
            .clone()
            .filter(|last| last.contains(&content, window) || content.contains(last, window))
            .unwrap_or(content);
        // After this round of focus changes settles.
        window.defer(cx, move |window, cx| target.focus(window, cx));
    }

    /// Whether the inset panel is open.
    fn panel_open(&self) -> bool {
        self.diff.is_some() || self.new_project.is_some() || self.showing_divergence()
    }

    /// Closes the inset panel, handing focus back to the chat input.
    fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel_open() {
            return;
        }
        if self.diff.is_none() && self.new_project.is_none() {
            // Only the divergence panel is showing: closing it stops it.
            self.close_divergence(window, cx);
            return;
        }
        self.diff = None;
        self.new_project = None;
        self.panel_last_focus = None;
        self._panel_subscriptions.clear();
        self.prompt_mode
            .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        cx.notify();
    }

    /// The diff or the new project form, in an inset panel over the window;
    /// clicking the dimmed window around it closes it.
    fn render_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let close = cx.listener(|this, _, window, cx| this.close_panel(window, cx));
        if let Some(view) = self
            .divergence
            .as_ref()
            .filter(|_| self.showing_divergence())
        {
            return Some(inset_panel(
                "divergence",
                &self.panel_focus,
                view.clone(),
                close,
                cx,
            ));
        }
        if let Some(diff) = &self.diff {
            return Some(inset_panel(
                "diff",
                &self.panel_focus,
                diff.clone(),
                close,
                cx,
            ));
        }
        let form = self.new_project.clone()?;
        Some(inset_panel(
            "new-project",
            &self.panel_focus,
            form,
            close,
            cx,
        ))
    }

    /// Quits, once confirmed if a task is running.
    fn quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_quit(window, cx) {
            cx.quit();
        }
    }

    /// Whether the application can end now: nothing is running. While a
    /// prompt or a build is, it asks instead, quitting once confirmed.
    fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let working = self.prompt_mode.read(cx).is_working();
        let building = self.ribbon.read(cx).is_building();
        let description = match (working, building) {
            (false, false) => return true,
            (true, false) => "The harness is still working on a prompt.",
            (false, true) => "piton build is still running.",
            (true, true) => {
                "The harness is still working on a prompt and piton build is still running."
            }
        };
        // An open palette makes way; a confirmation already open stays the
        // only one.
        if let Some(palette) = &self.palette
            && palette.read(cx).is_open(window, cx)
        {
            window.close_dialog(cx);
        }
        if !window.has_active_dialog(cx) {
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title("Quit while a task is running?")
                    .description(description)
                    .button_props(
                        DialogButtonProps::default()
                            .show_cancel(true)
                            .ok_text("Quit")
                            .ok_variant(ButtonVariant::Danger)
                            .cancel_text("Keep Running"),
                    )
                    .on_ok(|_, _, cx| {
                        cx.quit();
                        true
                    })
            });
        }
        false
    }

    /// Opens a fresh palette on the Files tab, or closes the one open.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = &self.palette
            && palette.read(cx).is_open(window, cx)
        {
            window.close_dialog(cx);
            return;
        }
        let ribbon = self.ribbon.read(cx);
        let system = SystemState {
            can_build: ribbon.can_build(cx),
            dark: cx.theme().is_dark(),
        };
        let palette = cx.new(|cx| Palette::new(system, window, cx));
        self._palette_subscription = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _, picked: &Picked, window, cx| this.run_picked(picked, window, cx),
        ));
        Palette::open(&palette, window, cx);
        self.palette = Some(palette);
    }

    fn run_picked(&mut self, picked: &Picked, window: &mut Window, cx: &mut Context<Self>) {
        match picked {
            Picked::File(path) => self.prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.open_file(path.clone(), window, cx)
            }),
            Picked::Mention(mention) => self.prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.insert_mention(mention, window, cx)
            }),
            Picked::System(command) => match command {
                SystemCommand::OpenProject => self
                    .ribbon
                    .update(cx, |ribbon, cx| ribbon.pick_project(window, cx)),
                SystemCommand::Build => self
                    .ribbon
                    .update(cx, |ribbon, cx| ribbon.build(window, cx)),
                SystemCommand::ToggleDarkMode => {
                    ribbon::set_dark_mode(!cx.theme().is_dark(), window, cx)
                }
                SystemCommand::Settings => settings_window::open(cx),
                SystemCommand::Quit => self.quit(window, cx),
            },
        }
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &FocusChat, window, cx| {
                // The context-less Esc binding outranks the palette's own, so
                // the palette's Esc is handled here.
                if let Some(palette) = &this.palette
                    && palette.read(cx).is_open(window, cx)
                {
                    palette.update(cx, |palette, cx| palette.cancel(window, cx));
                    return;
                }
                // Likewise any other dialog, such as the quit confirmation.
                if window.has_active_dialog(cx) {
                    window.close_dialog(cx);
                    return;
                }
                // Then the ribbon's list of what's running.
                if this.ribbon.read(cx).jobs_open() {
                    this.ribbon.update(cx, |ribbon, cx| ribbon.close_jobs(cx));
                    return;
                }
                // Then the inset panel.
                if this.panel_open() {
                    this.close_panel(window, cx);
                    return;
                }
                this.prompt_mode
                    .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx))
            }))
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| {
                // The palette acts on what is beneath the inset panel.
                if !this.panel_open() {
                    this.toggle_palette(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewProject, window, cx| this.open_new_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AnalyzeDivergence, window, cx| {
                this.open_divergence(Opening::Analyze, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ViewDivergenceReports, window, cx| {
                this.open_divergence(Opening::ViewReports, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ribbon::ToggleRibbon, _, cx| {
                // The ribbon is beneath the inset panel while it is open.
                if !this.panel_open() {
                    this.ribbon
                        .update(cx, |ribbon, cx| ribbon.toggle_collapsed(cx))
                }
            }))
            .child(self.ribbon.clone())
            .child(
                div().flex_1().min_h_0().child(
                    // One `children` call for every panel: the group's
                    // `children` replaces panels added before it.
                    h_resizable("sidebar-split")
                        .with_state(&self.sidebar_split)
                        .children([
                            resizable_panel()
                                .size(SIDEBAR_WIDTH)
                                .size_range(MIN_SIDEBAR_WIDTH..Pixels::MAX)
                                .child(
                                    // The file tree, with the git panel beneath it.
                                    gpui_kit::component::v_flex()
                                        .size_full()
                                        .child(div().flex_1().min_h_0().child(self.sidebar.clone()))
                                        .child(self.git_panel.clone()),
                                ),
                            resizable_panel()
                                .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                                .child(self.prompt_mode.clone()),
                        ]),
                ),
            )
            .children(self.render_panel(cx))
            // Inside the window's element tree, so actions such as
            // TogglePalette reach it from a focused dialog.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::MainWindow;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;

    const TIMEOUT: Duration = Duration::from_secs(2);

    #[cfg(target_os = "macos")]
    const TOGGLE_RIBBON: &str = "cmd-f1";
    #[cfg(not(target_os = "macos"))]
    const TOGGLE_RIBBON: &str = "ctrl-f1";

    /// The chat input has focus as soon as the window opens: a prompt typed
    /// and sent without clicking anywhere is sent, which without a project
    /// open shows a notification saying so.
    #[gpui_kit::test]
    async fn chat_input_is_focused_when_the_window_opens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        for key in "hello".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        #[cfg(target_os = "macos")]
        let send = "cmd-enter";
        #[cfg(not(target_os = "macos"))]
        let send = "ctrl-enter";
        cx.update_window(handle, |_, window, cx| window.press(send, cx))
            .unwrap();

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("notification").is_some()
        })
        .await;
    }

    /// Clicking a file in the project tree opens it beside the chat history,
    /// taking 50% of the width; the chat input stays unsplit below both.
    #[gpui_kit::test]
    async fn clicking_a_file_opens_it_beside_the_chat_history(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-open-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_some()
        })
        .await;

        // The file slides in from the sidebar: its pane grows from the
        // sidebar's edge, through widths in between, and the file sits at the
        // pane's right edge as it does.
        let sidebar = cx
            .update_window(handle, |_, window, _| window.find("project-tree").bounds())
            .unwrap();
        let mut widths = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(350) {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window
                        .try_find(("pane-slide", 1usize))
                        .map(|pane| pane.bounds())
                })
                .unwrap();
            if let Some(pane) = found {
                assert!(
                    (pane.left() - sidebar.right()).abs() <= gpui_kit::px(2.),
                    "the pane {pane:?} doesn't grow from the sidebar {sidebar:?}"
                );
                widths.push(pane.size.width);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        let widest = widths
            .iter()
            .copied()
            .fold(gpui_kit::px(0.), gpui_kit::Pixels::max);
        assert!(
            widths
                .iter()
                .any(|width| *width > gpui_kit::px(1.) && *width < widest - gpui_kit::px(1.)),
            "the pane {widths:?} appeared without sliding in"
        );

        // Once it has, the split settles at its share of the width.
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let file = window.find("file-view").bounds();
            let history = window.find("history").bounds();
            let editor = window.find("prompt-editor").bounds();
            let send = window.find("send").bounds();
            let share = file.size.width / (file.size.width + history.size.width);
            assert!(
                (share - 0.5).abs() < 0.02,
                "file takes {share} of the width: {file:?} beside {history:?}"
            );
            assert!(
                history.left() >= file.right() - gpui_kit::px(1.),
                "history is not right of the file: {file:?} then {history:?}"
            );
            assert!(
                editor.top() >= file.bottom()
                    && editor.left() < file.right()
                    && send.right() > history.left(),
                "chat input (editor {editor:?}, Send {send:?}) is split with the file {file:?}"
            );
        })
        .unwrap();

        // Closing it slides it back into the sidebar: the pane shrinks at the
        // sidebar's edge, through widths in between, then is gone.
        cx.update_window(handle, |_, window, cx| window.click("close-file", cx))
            .unwrap();
        let mut widths = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(350) {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window
                        .try_find(("pane-slide-out", 1usize))
                        .map(|pane| pane.bounds())
                })
                .unwrap();
            if let Some(pane) = found {
                assert!(
                    (pane.left() - sidebar.right()).abs() <= gpui_kit::px(2.),
                    "the pane {pane:?} doesn't shrink into the sidebar {sidebar:?}"
                );
                widths.push(pane.size.width);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        assert!(
            widths.windows(2).all(|pair| pair[1] <= pair[0] + gpui_kit::px(1.))
                && widths
                    .iter()
                    .any(|width| *width > gpui_kit::px(1.) && *width < widths[0] - gpui_kit::px(1.)),
            "the pane {widths:?} closed without sliding out"
        );
        std::thread::sleep(Duration::from_millis(200));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_none()
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file's diff floats over the whole window, inset 32px from each edge,
    /// leaving any file open in the split alone; Esc closes it, as does
    /// clicking the dimmed window around it, and opening the file from it
    /// closes it and opens the file in the split.
    #[gpui_kit::test]
    async fn diff_floats_over_the_window(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-diff-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_some()
        })
        .await;

        let open = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                main.update(cx, |main, cx| {
                    main.open_diff(dir.join("notes.md"), window, cx)
                });
            })
            .unwrap();
        };
        open(cx);
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("diff-view").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let backdrop = window.find("diff-backdrop").bounds();
            let panel = window.find("diff-panel").bounds();
            let inset = gpui_kit::px(32.);
            assert_eq!(
                backdrop.origin,
                gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.))
            );
            assert_eq!(
                panel.left() - backdrop.left(),
                inset,
                "{panel:?} in {backdrop:?}"
            );
            assert_eq!(
                panel.top() - backdrop.top(),
                inset,
                "{panel:?} in {backdrop:?}"
            );
            assert_eq!(
                backdrop.right() - panel.right(),
                inset,
                "{panel:?} in {backdrop:?}"
            );
            assert_eq!(
                backdrop.bottom() - panel.bottom(),
                inset,
                "{panel:?} in {backdrop:?}"
            );
            assert!(
                window.try_find("file-view").is_some(),
                "the diff closed the file in the split"
            );
        })
        .unwrap();

        // Nothing beneath takes focus or input while it is open: Tab stays
        // within it, focus taken beneath comes back, and the palette and
        // ribbon shortcuts do nothing.
        // Focus moving out of the panel is only seen in an active window.
        cx.update_window(handle, |_, window, _| window.activate_window())
            .unwrap();
        // Focus listeners run as frames are drawn.
        let in_panel = |cx: &mut TestAppContext| {
            for _ in 0..2 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
                cx.run_until_parked();
            }
            cx.update_window(handle, |_, window, cx| {
                main.read(cx).panel_focus.contains_focused(window, cx)
            })
            .unwrap()
        };
        cx.run_until_parked();
        assert!(in_panel(cx), "opening the diff didn't focus it");
        for _ in 0..6 {
            cx.update_window(handle, |_, window, cx| window.press("tab", cx))
                .unwrap();
            cx.run_until_parked();
            assert!(in_panel(cx), "Tab left the panel");
        }
        cx.update_window(handle, |_, window, cx| window.press("shift-tab", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(in_panel(cx), "Shift+Tab left the panel");
        cx.update_window(handle, |_, window, cx| {
            let prompt_mode = main.read(cx).prompt_mode.clone();
            prompt_mode.update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert!(in_panel(cx), "focus taken beneath stayed there");
        #[cfg(target_os = "macos")]
        const PALETTE: &str = "cmd-p";
        #[cfg(not(target_os = "macos"))]
        const PALETTE: &str = "ctrl-p";
        cx.update_window(handle, |_, window, cx| {
            window.press(PALETTE, cx);
            window.press(TOGGLE_RIBBON, cx);
        })
        .unwrap();
        cx.run_until_parked();
        main.read_with(cx, |main, cx| {
            assert!(
                main.palette.is_none(),
                "the palette opened beneath the panel"
            );
            assert!(
                !main.ribbon.read(cx).is_collapsed(),
                "the ribbon collapsed beneath the panel"
            );
        });
        assert!(main.read_with(cx, |main, _| main.panel_open()));

        // Esc closes it.
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| !main.panel_open()),
            "Esc left the diff open"
        );

        // So does clicking the dimmed window around it, but not the panel.
        open(cx);
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("diff-panel", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| main.panel_open()),
            "clicking the panel closed it"
        );
        cx.update_window(handle, |_, window, cx| {
            window.click_at(
                "diff-backdrop",
                gpui_kit::point(gpui_kit::px(8.), gpui_kit::px(8.)),
                cx,
            )
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| !main.panel_open()),
            "clicking the dimmed window left the diff open"
        );

        // Opening the file from it closes it, with the file in the split.
        open(cx);
        cx.run_until_parked();
        let diff = main.read_with(cx, |main, _| main.diff.clone().unwrap());
        cx.update(|cx| {
            diff.update(cx, |_, cx| {
                cx.emit(crate::diff_view::OpenInEditor(dir.join("notes.md")))
            })
        });
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| !main.panel_open()));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("file-view").is_some());
        })
        .unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    /// New Project opens the form in the inset panel; browsing for a location
    /// goes through the folder browser and back; and New creates the project,
    /// opens it, and closes the panel.
    #[gpui_kit::test]
    async fn new_project_creates_and_opens_a_project(cx: &mut TestAppContext) {
        let base =
            std::env::temp_dir().join(format!("suspense-new-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("projects")).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("new-project", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let form = main.read_with(cx, |main, _| main.new_project.clone().expect("no form"));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let backdrop = window.find("new-project-backdrop").bounds();
            let panel = window.find("new-project-panel").bounds();
            assert_eq!(panel.left() - backdrop.left(), gpui_kit::px(32.));
            assert_eq!(backdrop.bottom() - panel.bottom(), gpui_kit::px(32.));
        })
        .unwrap();
        // In a window too short for the whole form, it scrolls rather than
        // squeezing: each agent's checkbox sits wholly inside the group.
        cx.simulate_window_resize(
            handle,
            gpui_kit::size(gpui_kit::px(1000.), gpui_kit::px(560.)),
        );
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let agents = window.find("new-project-agents").bounds();
            for ix in 0..5usize {
                let row = window.find(("new-project-agent-row", ix)).bounds();
                assert!(
                    row.top() >= agents.top()
                        && row.bottom() <= agents.bottom() + gpui_kit::px(0.5),
                    "agent {ix} {row:?} spills out of the group {agents:?}"
                );
                assert!(
                    row.size.height >= gpui_kit::px(16.),
                    "agent {ix} {row:?} is squeezed"
                );
                // Its label has room below its baseline for p, y, and
                // brackets, rather than being clipped to its font size.
                let label = window
                    .find(format!("new-project-agent-{ix}-label"))
                    .bounds();
                assert!(
                    label.size.height >= gpui_kit::px(14. * 1.3),
                    "agent {ix}'s label {label:?} is clipped to a single tight line"
                );
                assert!(
                    label.bottom() <= row.bottom() + gpui_kit::px(0.5),
                    "{label:?} spills out of {row:?}"
                );
            }
            form.update(cx, |form, cx| {
                form.set_name("demo", window, cx);
                form.set_location(base.clone(), cx);
            });
        })
        .unwrap();
        cx.run_until_parked();

        // Browsing: going into "projects" and choosing it.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("new-project-browse", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let browser = form.read_with(cx, |form, _| form.browser().expect("no browser"));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(browser.read(cx).dir(), base.as_path());
            window.double_click(("folder-row", 0usize), cx);
            window.render_frame(cx);
            assert_eq!(browser.read(cx).dir(), base.join("projects"));

            // A new folder: Escape in its row closes only the row.
            window.click("folder-new", cx);
            window.render_frame(cx);
            assert!(browser.read(cx).new_folder_open());
            window.press("escape", cx);
            assert!(
                !browser.read(cx).new_folder_open(),
                "Escape left the row open"
            );
            assert!(
                window.try_find("folder-browser").is_some(),
                "Escape closed the panel"
            );

            // Named, created, and selected, so Choose chooses it.
            window.click("folder-new", cx);
            window.render_frame(cx);
            browser.update(cx, |browser, cx| {
                browser.set_new_folder_name("apps", window, cx)
            });
            window.render_frame(cx);
            window.click("folder-new-create", cx);
            window.render_frame(cx);
            assert!(
                base.join("projects/apps").is_dir(),
                "the folder wasn't created"
            );
            assert!(!browser.read(cx).new_folder_open());
            assert_eq!(browser.read(cx).choice(), base.join("projects/apps"));
            window.click("folder-choose", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let settings = form.read_with(cx, |form, cx| {
            assert!(form.browser().is_none(), "choosing left the browser open");
            form.settings(cx)
        });
        assert_eq!(settings.folder(), base.join("projects/apps/demo"));

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("new-project-create", cx);
        })
        .unwrap();
        let start = std::time::Instant::now();
        while cx.update(|cx| ProjectDirectory::get(cx)).is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the project never opened"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            cx.update(|cx| ProjectDirectory::get(cx)),
            Some(base.join("projects/apps/demo"))
        );
        assert!(base.join("projects/apps/demo/piton.config.pi").exists());
        // Git is initialized unless unchecked.
        assert!(
            base.join("projects/apps/demo/.git").is_dir(),
            "the new project isn't a Git repository"
        );
        assert!(
            !main.read_with(cx, |main, _| main.panel_open()),
            "the panel stayed open"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// While anything runs, a spinner beside the project's name lists it, and
    /// picking something reveals it. The divergence panel can be minimized,
    /// its analysis carrying on and listed there, and comes back from the
    /// list; a running question is listed too, and revealed by closing the
    /// panel over it. Once nothing runs, the spinner goes.
    #[gpui_kit::test]
    async fn running_jobs_are_listed_and_revealed(cx: &mut TestAppContext) {
        use std::path::Path;

        use crate::activity::JobKind;
        use crate::divergence::Cancel;
        use crate::divergence_view::DivergenceView;
        use crate::piton_build::BuildOutcome;

        fn built(_: &Path) -> anyhow::Result<BuildOutcome> {
            Ok(BuildOutcome {
                success: true,
                files: Vec::new(),
                report: String::new(),
            })
        }
        fn answered(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> anyhow::Result<String> {
            Ok(r#"{"files": []}"#.to_string())
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let jobs = |cx: &mut TestAppContext| {
            main.read_with(cx, |main, cx| {
                main.ribbon
                    .read(cx)
                    .jobs()
                    .iter()
                    .map(|job| job.kind)
                    .collect::<Vec<_>>()
            })
        };
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("ribbon-activity").is_none(),
                "a spinner with nothing running"
            );
            main.update(cx, |main, cx| {
                let view = cx.new(|cx| {
                    DivergenceView::with_runners(
                        env!("CARGO_MANIFEST_DIR").into(),
                        std::env::temp_dir()
                            .join(format!("suspense-jobs-divergence-{}", std::process::id())),
                        built,
                        answered,
                        crate::divergence_view::Opening::Analyze,
                        cx,
                    )
                });
                main.show_divergence(view, window, cx);
            });
        })
        .unwrap();
        // Its agents answer at once, so it is held running.
        cx.run_until_parked();
        let view = main.read_with(cx, |main, _| main.divergence.clone().unwrap());
        view.update(cx, |view, cx| view.hold_running_for_test(true, cx));
        cx.run_until_parked();
        assert_eq!(jobs(cx), [JobKind::Divergence]);

        // Minimized, the analysis carries on, listed beside the project name.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("divergence-panel").is_some());
            window.click("divergence-minimize", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("divergence-panel").is_none(),
                "minimizing left the panel up"
            );
            assert!(
                window.try_find("ribbon-activity").is_some(),
                "no spinner while it runs"
            );
        })
        .unwrap();
        assert!(
            main.read_with(cx, |main, _| main.divergence.is_some()),
            "minimizing stopped it"
        );

        // A running question is listed after it; picking the analysis brings
        // its panel back, and picking the question closes the panel again.
        cx.update_window(handle, |_, _, cx| {
            let prompt_mode = main.read(cx).prompt_mode.clone();
            prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.start_test_question("Still thinking?", cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(jobs(cx), [JobKind::Question(1), JobKind::Divergence]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("ribbon-activity", cx);
            window.render_frame(cx);
            window.click(("ribbon-job", 1usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("divergence-panel").is_some(),
                "the analysis wasn't revealed"
            );
            assert!(
                window.try_find("ribbon-jobs").is_none(),
                "the list stayed open"
            );
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.reveal_job(JobKind::Question(1), window, cx)
            });
            window.render_frame(cx);
            assert!(
                window.try_find("divergence-panel").is_none(),
                "the panel stayed over the question"
            );
        })
        .unwrap();

        // Once the analysis is over and the question closed, the spinner goes.
        view.update(cx, |view, cx| view.hold_running_for_test(false, cx));
        cx.update_window(handle, |_, _, cx| {
            let prompt_mode = main.read(cx).prompt_mode.clone();
            prompt_mode.update(cx, |prompt_mode, cx| prompt_mode.close_test_question(1, cx));
        })
        .unwrap();
        let start = std::time::Instant::now();
        while !jobs(cx).is_empty() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "still running: {:?}",
                jobs(cx)
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("ribbon-activity").is_none(),
                "the spinner stayed"
            );
        })
        .unwrap();
    }

    /// Esc in an open file moves focus back to the chat input, where typing
    /// then lands.
    #[gpui_kit::test]
    async fn escape_in_a_file_focuses_the_chat_input(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("file-view").is_some()
        })
        .await;

        let file = main.read_with(cx, |main, cx| {
            main.prompt_mode.read(cx).open_file_view().unwrap()
        });
        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            file.update(cx, |file, cx| file.focus_editor(window, cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| window.input("hi", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(chat.read(cx).value(cx).as_ref(), "hi"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Esc in the chat input takes focus out of it, and the window's own Esc
    /// does not hand focus straight back: typing no longer lands in it.
    /// The ribbon's tabs are Project, Code, Spec, Research, and Application,
    /// each showing only its own controls: opening a project under Project,
    /// Build Spec under Spec, dark mode and settings under Application, and
    /// nothing under Code or Research.
    #[gpui_kit::test]
    async fn ribbon_tabs_hold_their_controls(cx: &mut TestAppContext) {
        use crate::ribbon::RibbonTab;

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let ribbon = main.read_with(cx, |main, _| main.ribbon.clone());
        let controls = ["project-directory", "build", "dark-mode", "settings"];

        for (tab, shown) in [
            (RibbonTab::Project, Some("project-directory")),
            (RibbonTab::Code, None),
            (RibbonTab::Spec, Some("build")),
            (RibbonTab::Research, None),
            (RibbonTab::Application, Some("dark-mode")),
        ] {
            ribbon.update(cx, |ribbon, cx| ribbon.select_tab(tab, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, _| {
                for control in controls {
                    let expected = shown == Some(control)
                        || (tab == RibbonTab::Application && control == "settings");
                    assert_eq!(
                        window.try_find(control).is_some(),
                        expected,
                        "{control} under {tab:?}"
                    );
                }
                // Analysis follows Build: Analyze Divergence, then View
                // Divergence Reports.
                if tab == RibbonTab::Spec {
                    let build = window.find("build").bounds();
                    let analyze = window.find("analyze-divergence").bounds();
                    let view = window.find("view-divergence-reports").bounds();
                    assert!(build.right() <= analyze.left() && analyze.right() <= view.left());
                }
            })
            .unwrap();
        }

        // Ctrl+click opens tabs alongside each other: their groups sit side by
        // side in the order of the tabs, with no divider between; a plain click
        // opens one alone again.
        ribbon.update(cx, |ribbon, cx| {
            ribbon.tab_clicked(RibbonTab::Application, 1, false, cx);
            ribbon.tab_clicked(RibbonTab::Project, 1, true, cx);
            ribbon.tab_clicked(RibbonTab::Code, 1, true, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            ribbon.read_with(cx, |ribbon, _| ribbon.open_tabs().to_vec()),
            [RibbonTab::Project, RibbonTab::Code, RibbonTab::Application]
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for control in controls.into_iter().filter(|control| *control != "build") {
                assert!(window.try_find(control).is_some(), "{control} isn't shown");
            }
            assert!(window.try_find("build").is_none());
            // Project's groups come before Application's, with nothing between
            // them but the gap: no divider.
            let project = window.find("Project").bounds();
            let appearance = window.find("Appearance").bounds();
            assert_eq!(
                appearance.left() - project.right(),
                gpui_kit::px(12.),
                "something sits between Project's group {project:?} and Application's {appearance:?}"
            );
        })
        .unwrap();
        ribbon.update(cx, |ribbon, cx| {
            ribbon.tab_clicked(RibbonTab::Application, 1, false, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            ribbon.read_with(cx, |ribbon, _| ribbon.open_tabs().to_vec()),
            [RibbonTab::Application]
        );

        // The project's name sits at the left end of the tabs' bar.
        cx.update_window(handle, |_, window, _| {
            let bar = window.find("ribbon-tabs-row").bounds();
            let name = window.find("ribbon-project-name").bounds();
            assert!(
                (name.left() - bar.left()).abs() <= gpui_kit::px(1.)
                    && name.top() >= bar.top()
                    && name.bottom() <= bar.bottom(),
                "the project name {name:?} isn't at the left of the tabs {bar:?}"
            );
        })
        .unwrap();

        // Groups sit side by side with nothing between them but the gap: the
        // title strip down each one's left edge marks where it starts.
        cx.update_window(handle, |_, window, _| {
            let appearance = window.find("Appearance").bounds();
            let preferences = window.find("Preferences").bounds();
            assert_eq!(
                preferences.left() - appearance.right(),
                gpui_kit::px(12.),
                "something sits between {appearance:?} and {preferences:?}"
            );
        })
        .unwrap();

        // Ctrl+F1 collapses the ribbon: the tabs go, and every primary
        // command shows in a single row, whichever tab was selected.
        cx.update_window(handle, |_, window, cx| window.press(TOGGLE_RIBBON, cx))
            .unwrap();
        cx.run_until_parked();
        assert!(ribbon.read_with(cx, |ribbon, _| ribbon.is_collapsed()));
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("ribbon-controls").is_none());
            assert!(
                window.try_find("ribbon-tabs-row").is_none(),
                "the tabs still show"
            );
            assert!(window.try_find("ribbon-primary").is_some());
            // Led by the project's name.
            let row = window.find("ribbon-primary").bounds();
            let name = window.find("ribbon-project-name").bounds();
            for control in controls {
                assert!(
                    window.find(control).bounds().left() > name.right(),
                    "{control} comes before the project name"
                );
            }
            assert!(name.left() >= row.left());
            for control in controls {
                assert!(
                    window.try_find(control).is_some(),
                    "{control} is not in the row"
                );
            }
            // Every control, the chevron included, is vertically centred in
            // the row.
            let row = window.find("ribbon-primary").bounds();
            let row_middle = row.origin.y + row.size.height / 2.;
            for control in controls.into_iter().chain(["ribbon-collapse"]) {
                let bounds = window.find(control).bounds();
                let middle = bounds.origin.y + bounds.size.height / 2.;
                assert!(
                    (middle - row_middle).abs() <= gpui_kit::px(1.),
                    "{control} is not centred: {bounds:?} in {row:?}"
                );
            }
        })
        .unwrap();

        // Ctrl+F1 again brings the tabs back; double-clicking one collapses it.
        cx.update_window(handle, |_, window, cx| window.press(TOGGLE_RIBBON, cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("ribbon-tabs-row").is_some());
            assert!(window.try_find("ribbon-primary").is_none());
        })
        .unwrap();
        ribbon.update(cx, |ribbon, cx| {
            ribbon.tab_clicked(RibbonTab::Project, 1, false, cx);
            ribbon.tab_clicked(RibbonTab::Project, 2, false, cx);
        });
        assert!(ribbon.read_with(cx, |ribbon, _| ribbon.is_collapsed()));
    }

    #[gpui_kit::test]
    async fn escape_in_the_chat_input_takes_focus_out(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            assert!(chat.read(cx).is_focused(window, cx));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            assert!(
                !chat.read(cx).is_focused(window, cx),
                "Esc left focus in the chat input"
            );
            window.input("hi", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(chat.read(cx).value(cx).as_ref(), ""));
    }

    /// The project tree sits in a sidebar along the left, before prompt mode.
    #[gpui_kit::test]
    async fn sidebar_is_left_of_prompt_mode(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("project-tree").is_some() && window.try_find("send").is_some()
        })
        .await;
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let sidebar = window.find("project-tree").bounds();
            let send = window.find("send").bounds();
            assert!(
                sidebar.left() <= gpui_kit::px(1.) && sidebar.size.width > gpui_kit::px(0.),
                "sidebar is not along the left: {sidebar:?}"
            );
            assert!(
                send.left() >= sidebar.right(),
                "prompt mode (Send at {send:?}) overlaps the sidebar: {sidebar:?}"
            );
        })
        .unwrap();
    }

    /// While a prompt runs, quitting or closing the window asks first: Keep
    /// Running or Esc dismisses the question and leaves the window open. With
    /// nothing running the window closes straight away.
    #[gpui_kit::test]
    async fn quitting_while_a_task_runs_asks_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        main.update(cx, |main, cx| {
            main.prompt_mode
                .update(cx, |prompt_mode, _| prompt_mode.set_working(true))
        });

        // Asked twice, it still asks once: one Keep Running dismisses it.
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| {
                window.dispatch_action(Box::new(crate::app::Quit), cx)
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.click("cancel", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_none()
        })
        .await;

        assert!(
            !gpui_kit::VisualTestContext::from_window(handle, cx).simulate_close(),
            "the window closed while a prompt was running"
        );
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_none()
        })
        .await;

        main.update(cx, |main, cx| {
            main.prompt_mode
                .update(cx, |prompt_mode, _| prompt_mode.set_working(false))
        });
        assert!(gpui_kit::VisualTestContext::from_window(handle, cx).simulate_close());
    }

    #[cfg(target_os = "macos")]
    const PALETTE: &str = "cmd-p";
    #[cfg(not(target_os = "macos"))]
    const PALETTE: &str = "ctrl-p";

    /// Ctrl/Cmd+P opens the palette on the Files tab, where typing narrows the
    /// project's files and Enter opens the highlighted one. Opened again,
    /// Shift+Tab wraps round to Agents, where Enter inserts the agent into
    /// the chat input.
    #[gpui_kit::test]
    async fn palette_opens_files_and_inserts_agents(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-palette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels() == ["notes.md", "src/lib.rs"]
        })
        .await;
        for key in "notes".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels() == ["notes.md"]
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("file-view").is_some() && !palette.read(cx).is_open(window, cx)
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx)
                .palette
                .as_ref()
                .is_some_and(|open| open != &palette)
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.update_window(handle, |_, window, cx| window.press("shift-tab", cx))
            .unwrap();
        for key in "Explore".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette
                .read(cx)
                .result_labels()
                .first()
                .is_some_and(|label| label == "Explore")
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();

        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            chat.read(cx).value(cx).as_ref() == "@agent-Explore "
                && chat.read(cx).is_focused(window, cx)
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A harness that finds `notes.md`.
    fn finds_notes(
        _: String,
        system_prompt: Option<String>,
        _: Option<crate::harness::Resume>,
        _: std::path::PathBuf,
    ) -> futures::channel::mpsc::UnboundedReceiver<crate::harness::HarnessEvent> {
        assert!(system_prompt.is_some_and(|prompt| prompt.contains("one file")));
        let (tx, rx) = futures::channel::mpsc::unbounded();
        for event in [
            crate::harness::HarnessEvent::ToolStarted {
                id: "t1".into(),
                name: "Glob".into(),
            },
            crate::harness::HarnessEvent::Finished {
                is_error: false,
                result: r#"{"files": ["notes.md"]}"#.into(),
            },
        ] {
            tx.unbounded_send(event).unwrap();
        }
        rx
    }

    /// A harness that finds two files.
    fn finds_both(
        _: String,
        _: Option<String>,
        _: Option<crate::harness::Resume>,
        _: std::path::PathBuf,
    ) -> futures::channel::mpsc::UnboundedReceiver<crate::harness::HarnessEvent> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        tx.unbounded_send(crate::harness::HarnessEvent::Finished {
            is_error: false,
            result: r#"{"files": ["notes.md", "src/lib.rs"]}"#.into(),
        })
        .unwrap();
        rx
    }

    /// On the Files tab, Enter with nothing matched hands the search to the
    /// harness, and the one file it finds opens. Ctrl/Cmd+Enter hands it over
    /// even while files match; finding several leaves the palette showing the
    /// search under an alert.
    #[gpui_kit::test]
    async fn palette_hands_file_searches_to_the_harness(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-palette-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        palette.update(cx, |palette, _| palette.set_send_to_harness(finds_notes));
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            palette.read(cx).result_labels().len() == 2
        })
        .await;
        for key in "the meeting jottings".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("file-view").is_some() && !palette.read(cx).is_open(window, cx)
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx)
                .palette
                .as_ref()
                .is_some_and(|open| open != &palette)
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        palette.update(cx, |palette, _| palette.set_send_to_harness(finds_both));
        cx.update_window(handle, |_, window, cx| window.input("s", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            !palette.read(cx).result_labels().is_empty()
        })
        .await;
        #[cfg(target_os = "macos")]
        let search = "cmd-enter";
        #[cfg(not(target_os = "macos"))]
        let search = "ctrl-enter";
        cx.update_window(handle, |_, window, cx| window.press(search, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            let palette = palette.read(cx);
            palette.is_searching_files()
                && window.try_find("palette-search").is_some()
                && !palette.is_open(window, cx)
        })
        .await;

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Esc in the palette clears the search, then closes the palette and puts
    /// focus back in the chat input.
    #[gpui_kit::test]
    async fn escape_closes_the_palette(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press(PALETTE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("palette").is_some()
        })
        .await;
        let palette = main.read_with(cx, |main, _| main.palette.clone().unwrap());
        cx.update_window(handle, |_, window, cx| window.input("x", cx))
            .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(
                palette.read(cx).is_open(window, cx),
                "Esc closed the palette before clearing the search"
            );
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            window.try_find("palette").is_none() && chat.read(cx).is_focused(window, cx)
        })
        .await;
    }
}
