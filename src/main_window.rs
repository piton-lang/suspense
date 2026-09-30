//! The main window of the application, from which all main functionality is
//! reached: a ribbon on top, and below it the project tree in a sidebar on the
//! left, then prompt mode. A file's diff floats over all of it.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme, Root, WindowExt as _};
use gpui_kit::*;

use crate::activity::{Job, JobKind, RevealJob};
use crate::animations::rise_in::Leaving;
use crate::app::{APP_TITLE, Quit};
use crate::diff_view::{CloseDiff, DiffView, OpenInEditor};
use crate::divergence_view::{
    AnalyzeDivergence, CloseDivergence, DivergenceView, MinimizeDivergence, Opening,
    ViewDivergenceReports,
};
use crate::fs_browser::{CancelBrowse, ChoosePath, FsBrowser};
use crate::generate_skills_view::{CloseGenerateSkills, GenerateSkills, GenerateSkillsView};
use crate::git_panel::GitPanel;
use crate::inset_panel::{closing_panel, inset_panel};
use crate::new_instruction::{CloseInstruction, NewInstruction, NewInstructionForm};
use crate::new_project::{CloseNewProject, NewProject, NewProjectForm, ProjectCreated};
use crate::palette::{Palette, Picked, SystemCommand, SystemState};
use crate::project::open_project::{self, OpenProject};
use crate::project_directory::ProjectDirectory;
use crate::project_tree::{EntryMoved, OpenDiff, OpenFile, ProjectTree};
use crate::prompt_mode::PromptMode;
use crate::rescope_view::{CloseRescope, MinimizeRescope, RefactorConcepts, Rescope, RescopeView};
use crate::ribbon::RunCommand;
use crate::ribbon::{self, Ribbon};
use crate::run_targets::{self, ProjectTargets};
use crate::run_view::{CloseRun, MinimizeRun, RunView, TargetsFound};
use crate::settings_window::{CloseSettings, OpenSettings, SettingsWindow};
use crate::spec_component_form::{
    CloseSpecComponent, ComponentCreated, RunComponentSkill, SpecComponentForm,
};
use crate::spec_components::{ComponentKind, NewConcept, NewScope, NewShape};
use crate::theme_editor::{CloseThemeEditor, OpenThemeEditor, ThemeEditor};
use crate::theme_preference;

/// Size the window restores to when it is un-maximized.
const RESTORE_SIZE: Size<Pixels> = size(px(1280.), px(800.));

/// The narrowest a split can be dragged.
const MIN_SPLIT_WIDTH: Pixels = px(240.);

/// The sidebar's width until it is dragged, and the narrowest it can be.
const SIDEBAR_WIDTH: Pixels = px(260.);
const MIN_SIDEBAR_WIDTH: Pixels = px(160.);

/// The shortest the file tree can be dragged, above the git panel.
const MIN_FILE_TREE_HEIGHT: Pixels = px(80.);

actions!(suspense, [Dismiss, TogglePalette]);

/// Esc closes what the window has open to dismiss, and otherwise goes on to
/// wherever the keyboard is; it is bound without a context, so it matches
/// ahead of any other binding and passes on what it doesn't take. Ctrl/Cmd+P
/// opens or closes the palette. Ctrl/Cmd+Enter, which moves focus to the chat
/// input, isn't bound here: see [`MainWindow::intercept_focus_chat`].
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-p", TogglePalette, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-p", TogglePalette, None),
    ]);
    crate::palette::bind_keys(cx);
    ribbon::bind_keys(cx);
    crate::diff_view::bind_keys(cx);
    crate::chat_input::bind_keys(cx);
    crate::fs_browser::bind_keys(cx);
    crate::project_indicator::bind_keys(cx);
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
    /// The tallest the git panel grows, beneath the file tree, once the edge
    /// between them has been dragged; until then, half the sidebar. It is
    /// never taller than its contents need.
    git_height: Option<Pixels>,
    /// A changed file's diff, or the new project form, floating over the
    /// window in an inset panel.
    diff: Option<Entity<DiffView>>,
    new_project: Option<Entity<NewProjectForm>>,
    /// The form creating a scope, concept, or shape.
    spec_component: Option<Entity<SpecComponentForm>>,
    /// The panel writing a new instruction.
    new_instruction: Option<Entity<NewInstructionForm>>,
    settings: Option<Entity<SettingsWindow>>,
    /// The theme editor.
    theme_editor: Option<Entity<ThemeEditor>>,
    /// The Generate Skills panel.
    generate_skills: Option<Entity<GenerateSkillsView>>,
    /// The file browser for opening a project.
    project_picker: Option<Entity<FsBrowser>>,
    /// The divergence panel, which can be minimized while its analysis
    /// carries on.
    divergence: Option<Entity<DivergenceView>>,
    divergence_minimized: bool,
    _divergence_subscriptions: Vec<Subscription>,
    /// The Rescope panel, which can likewise be minimized while it looks.
    rescope: Option<Entity<RescopeView>>,
    rescope_minimized: bool,
    _rescope_subscriptions: Vec<Subscription>,
    /// The Run panel, finding how the project runs or running one of its
    /// targets, which can be minimized while it goes on.
    run: Option<Entity<RunView>>,
    run_minimized: bool,
    _run_subscriptions: Vec<Subscription>,
    /// Tracks the inset panel, which keeps focus within it while open.
    panel_focus: FocusHandle,
    /// What was last focused within the panel, to go back to when something
    /// takes focus beneath it.
    panel_last_focus: Option<FocusHandle>,
    _panel_subscriptions: Vec<Subscription>,
    /// The project last on screen, to tell a switch from setting it again.
    shown_project: Option<PathBuf>,
    /// The minimizable panels of each project switched away from, their work
    /// carrying on, for when it's back.
    parked: HashMap<PathBuf, ParkedPanels>,
    /// What the inset panel shows, to know when it comes in and goes away.
    panel_motion: Leaving<AnyView>,
    _subscriptions: Vec<Subscription>,
}

/// A project's minimizable panels while another project is on screen.
#[derive(Default)]
struct ParkedPanels {
    divergence: Option<Entity<DivergenceView>>,
    rescope: Option<Entity<RescopeView>>,
    run: Option<Entity<RunView>>,
    subscriptions: [Vec<Subscription>; 3],
}

impl ParkedPanels {
    fn is_empty(&self) -> bool {
        self.divergence.is_none() && self.rescope.is_none() && self.run.is_none()
    }
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
            // Matches the desktop entry `cargo xtask install` makes, so the
            // window gets its icon and is grouped with it.
            app_id: Some("com.piton-lang.suspense".into()),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            // Start in the saved light or dark mode, else the system's; the
            // ribbon can switch; each mode at its saved brightness.
            theme_preference::restore_colors(cx);
            theme_preference::restore_brightness(cx);
            theme_preference::apply(window, cx);
            let view = cx.new(|cx| MainWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
    }

    /// Ctrl/Cmd+Enter moves focus to the chat input from anywhere in the
    /// window, with its cursor where it was, and sends nothing. It is caught
    /// before any binding is matched, since the inputs, lists, and editors it
    /// would otherwise reach bind it too, and while nothing has focus, no
    /// binding would reach the window at all. It goes on to mean what it
    /// means where it means something already: in the chat input, the
    /// palette, a dialog, the list of recent projects, and an inset panel.
    fn intercept_focus_chat(window: &Window, cx: &mut Context<Self>) -> Subscription {
        let this = cx.weak_entity();
        let handle = window.window_handle();
        cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key != "enter"
                || keystroke.modifiers != Modifiers::secondary_key()
                || window.window_handle() != handle
            {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let busy_elsewhere = this
                    .palette
                    .as_ref()
                    .is_some_and(|palette| palette.read(cx).is_open(window, cx))
                    || window.has_active_dialog(cx)
                    || this.ribbon.read(cx).project_indicator().read(cx).is_open()
                    || this.panel_open();
                let chat = this.prompt_mode.read(cx).chat_input_view();
                if busy_elsewhere || chat.read(cx).has_keyboard(window, cx) {
                    return;
                }
                this.prompt_mode
                    .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
                cx.stop_propagation();
            });
        })
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ribbon = cx.new(Ribbon::new);
        let sidebar = cx.new(ProjectTree::new);
        let prompt_mode = cx.new(|cx| PromptMode::new(window, cx));
        let mut subscriptions = vec![
            // The ribbon's activity spinner follows whatever is running.
            cx.observe(&prompt_mode, |this, _, cx| this.refresh_jobs(cx)),
            // A file a task changed, between its snapshots.
            cx.subscribe_in(
                &prompt_mode,
                window,
                |this, _, open: &crate::prompt_mode::OpenSnapshotDiff, window, cx| {
                    this.open_snapshot_diff(open, window, cx)
                },
            ),
            cx.observe(&ribbon, |this, _, cx| this.refresh_jobs(cx)),
            // The project indicator's "Open Project…".
            cx.subscribe_in(
                &ribbon.read(cx).project_indicator().clone(),
                window,
                |this, _, _: &OpenProject, window, cx| this.open_project_picker(window, cx),
            ),
            cx.subscribe_in(
                &ribbon,
                window,
                |this, _, RevealJob(kind, project), window, cx| {
                    this.reveal_job(*kind, project.clone(), window, cx)
                },
            ),
            cx.subscribe_in(
                &ribbon,
                window,
                |this, _, command: &RunCommand, window, cx| this.run_command(*command, window, cx),
            ),
            // Switching projects closes what belongs to the screen.
            cx.observe_global_in::<ProjectDirectory>(window, |this, window, cx| {
                this.project_changed(window, cx)
            }),
            cx.subscribe_in(&sidebar, window, |this, _, OpenFile(path), window, cx| {
                this.prompt_mode.update(cx, |prompt_mode, cx| {
                    prompt_mode.open_file(path.clone(), window, cx)
                })
            }),
            // A file or folder renamed or deleted in the tree takes the file
            // open in the editor with it.
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, moved: &EntryMoved, window, cx| {
                    this.prompt_mode.update(cx, |prompt_mode, cx| {
                        prompt_mode.file_moved(&moved.from, moved.to.as_deref(), window, cx)
                    })
                },
            ),
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
        subscriptions.push(Self::intercept_focus_chat(window, cx));
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
        // The run targets of the project open at launch, for the Code tab.
        ProjectTargets::set(
            ProjectTargets {
                targets: ProjectDirectory::get(cx)
                    .map(|dir| run_targets::load(&dir))
                    .unwrap_or_default(),
                running: None,
            },
            cx,
        );

        Self {
            ribbon,
            sidebar,
            git_panel: cx.new(|cx| GitPanel::new(window, cx)),
            prompt_mode,
            palette: None,
            _palette_subscription: None,
            sidebar_split: cx.new(|_| ResizableState::default()),
            git_height: None,
            diff: None,
            new_project: None,
            spec_component: None,
            new_instruction: None,
            settings: None,
            theme_editor: None,
            generate_skills: None,
            project_picker: None,
            divergence: None,
            divergence_minimized: false,
            _divergence_subscriptions: Vec::new(),
            rescope: None,
            rescope_minimized: false,
            _rescope_subscriptions: Vec::new(),
            run: None,
            run_minimized: false,
            _run_subscriptions: Vec::new(),
            parked: HashMap::new(),
            panel_focus,
            panel_last_focus: None,
            _panel_subscriptions: Vec::new(),
            shown_project: ProjectDirectory::get(cx),
            panel_motion: Leaving::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Opens a file browser for a project's piton.config.pi in the inset
    /// panel, in place of anything else there, starting in the folder holding
    /// the open project; the folder of the file chosen becomes the project.
    pub fn open_project_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_picker.is_some() {
            return;
        }
        let picker = open_project::picker(cx);
        self.clear_panel();
        self._panel_subscriptions = vec![
            cx.subscribe_in(
                &picker,
                window,
                |this, _, ChoosePath(config), window, cx| {
                    open_project::open(config, cx);
                    this.close_panel(window, cx)
                },
            ),
            cx.subscribe_in(&picker, window, |this, _, _: &CancelBrowse, window, cx| {
                this.close_panel(window, cx)
            }),
        ];
        picker.read(cx).focus_handle(cx).focus(window, cx);
        self.project_picker = Some(picker);
        cx.notify();
    }

    /// Takes away whatever is in the inset panel to make room for something
    /// else, minimizing a divergence analysis.
    fn clear_panel(&mut self) {
        self.diff = None;
        self.new_project = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self.divergence_minimized = self.divergence.is_some();
        self.rescope_minimized = self.rescope.is_some();
        self.run_minimized = self.run.is_some();
    }

    /// Opens the Generate Skills panel for the open project, ranking its
    /// scopes afresh, in place of anything else in the inset panel.
    pub fn open_generate_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.generate_skills.is_some() {
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let view = cx.new(|cx| GenerateSkillsView::new(project_dir, cx));
        self.show_generate_skills(view, window, cx);
    }

    /// Shows `view` in the inset panel, in place of anything else there.
    pub fn show_generate_skills(
        &mut self,
        view: Entity<GenerateSkillsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_panel();
        self._panel_subscriptions = vec![cx.subscribe_in(
            &view,
            window,
            |this, _, _: &CloseGenerateSkills, window, cx| this.close_panel(window, cx),
        )];
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.generate_skills = Some(view);
        cx.notify();
    }

    /// Opens the settings in the inset panel, in place of anything else
    /// there, reading the prompts afresh; already open, they stay as they are.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        let settings = cx.new(|cx| SettingsWindow::new(window, cx));
        self.clear_panel();
        self._panel_subscriptions = vec![cx.subscribe_in(
            &settings,
            window,
            |this, _, _: &CloseSettings, window, cx| this.close_panel(window, cx),
        )];
        settings.read(cx).focus_handle(cx).focus(window, cx);
        self.settings = Some(settings);
        cx.notify();
    }

    /// Opens the theme editor in the inset panel, in place of anything else
    /// there; already open, it stays as it is.
    pub fn open_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_editor.is_some() {
            return;
        }
        let editor = cx.new(|cx| ThemeEditor::new(window, cx));
        self.clear_panel();
        self._panel_subscriptions = vec![cx.subscribe_in(
            &editor,
            window,
            |this, _, _: &CloseThemeEditor, window, cx| this.close_panel(window, cx),
        )];
        editor.read(cx).focus_handle(cx).focus(window, cx);
        self.theme_editor = Some(editor);
        cx.notify();
    }

    /// Opens a changed file's diff in the floating panel, replacing any diff
    /// already there. Any file open in a tab is left as it is.
    pub fn open_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let diff = cx.new(|cx| DiffView::new(path, cx));
        self.show_diff(diff, window, cx);
    }

    /// Opens a file's changes while a task ran, between the snapshots taken
    /// as it started and ended, as [`Self::open_diff`] opens its uncommitted
    /// ones.
    fn open_snapshot_diff(
        &mut self,
        open: &crate::prompt_mode::OpenSnapshotDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let diff = cx.new(|cx| {
            DiffView::between(
                open.top.clone(),
                open.path.clone(),
                open.from.clone(),
                open.before.clone(),
                open.after.clone(),
                cx,
            )
        });
        self.show_diff(diff, window, cx);
    }

    fn show_diff(&mut self, diff: Entity<DiffView>, window: &mut Window, cx: &mut Context<Self>) {
        self.new_project = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self.divergence_minimized = self.divergence.is_some();
        self.rescope_minimized = self.rescope.is_some();
        self.run_minimized = self.run.is_some();
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
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self._panel_subscriptions.clear();
        self.divergence_minimized = false;
        self.rescope_minimized = self.rescope.is_some();
        self.run_minimized = self.run.is_some();
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

    /// Opens the Rescope panel for the open project, looking through its spec,
    /// or brings back one that was minimized.
    pub fn open_rescope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.rescope.clone() {
            self.restore_rescope(&view, window, cx);
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let view = cx.new(|cx| RescopeView::new(project_dir, cx));
        self.show_rescope(view, window, cx);
    }

    /// Shows `view` in the panel, in place of anything else there.
    pub fn show_rescope(
        &mut self,
        view: Entity<RescopeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._rescope_subscriptions = vec![
            cx.subscribe_in(&view, window, |this, _, _: &CloseRescope, window, cx| {
                this.close_rescope(window, cx)
            }),
            cx.subscribe_in(&view, window, |this, _, _: &MinimizeRescope, window, cx| {
                this.minimize_rescope(window, cx)
            }),
            // Refactoring closes the panel and sends the prompt from the Spec
            // tab, as if written there.
            cx.subscribe_in(
                &view,
                window,
                |this, _, RefactorConcepts(prompt), window, cx| {
                    let prompt = prompt.clone();
                    this.close_rescope(window, cx);
                    this.prompt_mode.update(cx, |prompt_mode, cx| {
                        prompt_mode.send(
                            prompt,
                            crate::chat_input::SendMode::Spec,
                            Vec::new(),
                            window,
                            cx,
                        )
                    });
                },
            ),
            cx.observe(&view, |this, _, cx| this.refresh_jobs(cx)),
        ];
        self.rescope = Some(view.clone());
        self.restore_rescope(&view, window, cx);
    }

    /// Shows the Rescope panel, in place of anything else in the panel.
    fn restore_rescope(
        &mut self,
        view: &Entity<RescopeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff = None;
        self.new_project = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self._panel_subscriptions.clear();
        self.divergence_minimized = self.divergence.is_some();
        self.rescope_minimized = false;
        self.run_minimized = self.run.is_some();
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.refresh_jobs(cx);
        cx.notify();
    }

    /// Hides the Rescope panel, its search carrying on.
    fn minimize_rescope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rescope_minimized = true;
        self.panel_last_focus = None;
        self.prompt_mode
            .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        cx.notify();
    }

    /// Opens the ribbon's tab at `ix`, in the order shown, from Alt+1 on. The
    /// ribbon is beneath the inset panel while it is open.
    fn show_ribbon_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.panel_open() {
            self.ribbon
                .update(cx, |ribbon, cx| ribbon.show_tab_at(ix, cx));
        }
    }

    /// Closes the Rescope panel, stopping any search still running.
    fn close_rescope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.showing_rescope();
        self.rescope = None;
        self.rescope_minimized = false;
        self._rescope_subscriptions.clear();
        self.refresh_jobs(cx);
        if showing {
            self.panel_last_focus = None;
            self.prompt_mode
                .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        }
        cx.notify();
    }

    /// Whether the Rescope panel is showing, rather than minimized.
    fn showing_rescope(&self) -> bool {
        self.rescope.is_some() && !self.rescope_minimized
    }

    /// Whether the divergence panel is showing, rather than minimized.
    fn showing_divergence(&self) -> bool {
        self.divergence.is_some() && !self.divergence_minimized
    }

    /// Handles the Code tab's run commands: finding how the project runs, or
    /// running one of its targets. The target running already only comes
    /// back into view.
    fn run_command(&mut self, command: RunCommand, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        match command {
            RunCommand::Find => match self.run.clone() {
                Some(view) => {
                    view.update(cx, |view, cx| view.find(cx));
                    self.restore_run(&view, window, cx);
                }
                None => {
                    let view = cx.new(|cx| RunView::finding(project_dir, cx));
                    self.show_run(view, window, cx);
                }
            },
            RunCommand::Target(ix) => {
                let Some(target) = ProjectTargets::get(cx).targets.get(ix).cloned() else {
                    return;
                };
                match self.run.clone() {
                    Some(view) => {
                        if view.read(cx).running_target() != Some(ix) {
                            view.update(cx, |view, cx| view.run(ix, target, cx));
                        }
                        self.restore_run(&view, window, cx);
                    }
                    None => {
                        let view = cx.new(|cx| RunView::running(project_dir, ix, target, cx));
                        self.show_run(view, window, cx);
                    }
                }
            }
        }
    }

    /// Runs the project's first run target, its primary one, or finds how to
    /// run the project while it has none.
    fn run_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = if ProjectTargets::get(cx).targets.is_empty() {
            RunCommand::Find
        } else {
            RunCommand::Target(0)
        };
        self.run_command(command, window, cx);
    }

    /// Shows `view` in the panel, in place of anything else there.
    pub fn show_run(&mut self, view: Entity<RunView>, window: &mut Window, cx: &mut Context<Self>) {
        self._run_subscriptions = vec![
            cx.subscribe_in(&view, window, |this, _, _: &CloseRun, window, cx| {
                this.close_run(window, cx)
            }),
            cx.subscribe_in(&view, window, |this, _, _: &MinimizeRun, window, cx| {
                this.minimize_run(window, cx)
            }),
            // Once found and saved, the Code tab has a button for each.
            cx.subscribe(&view, |this, view, TargetsFound(targets), cx| {
                // A project switched away from reads them when it's back.
                if ProjectDirectory::get(cx).as_deref() != Some(view.read(cx).project_dir()) {
                    return;
                }
                let targets = targets.clone();
                ProjectTargets::set(
                    ProjectTargets {
                        targets,
                        running: None,
                    },
                    cx,
                );
                this.refresh_running_target(cx);
            }),
            cx.observe(&view, |this, _, cx| {
                this.refresh_running_target(cx);
                this.refresh_jobs(cx);
            }),
        ];
        self.run = Some(view.clone());
        self.restore_run(&view, window, cx);
    }

    /// Shows the Run panel, in place of anything else in the panel.
    fn restore_run(&mut self, view: &Entity<RunView>, window: &mut Window, cx: &mut Context<Self>) {
        self.diff = None;
        self.new_project = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self._panel_subscriptions.clear();
        self.divergence_minimized = self.divergence.is_some();
        self.rescope_minimized = self.rescope.is_some();
        self.run_minimized = false;
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.refresh_running_target(cx);
        self.refresh_jobs(cx);
        cx.notify();
    }

    /// Hides the Run panel, what it holds carrying on.
    fn minimize_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_minimized = true;
        self.panel_last_focus = None;
        self.prompt_mode
            .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        cx.notify();
    }

    /// Closes the Run panel, stopping whatever it holds.
    fn close_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.showing_run();
        self.run = None;
        self.run_minimized = false;
        self._run_subscriptions.clear();
        self.refresh_running_target(cx);
        self.refresh_jobs(cx);
        if showing {
            self.panel_last_focus = None;
            self.prompt_mode
                .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        }
        cx.notify();
    }

    /// Whether the Run panel is showing, rather than minimized.
    fn showing_run(&self) -> bool {
        self.run.is_some() && !self.run_minimized
    }

    /// Tells the ribbon which target runs, if any.
    fn refresh_running_target(&self, cx: &mut Context<Self>) {
        let mut targets = ProjectTargets::get(cx);
        targets.running = self
            .run
            .as_ref()
            .and_then(|view| view.read(cx).running_target());
        ProjectTargets::set(targets, cx);
    }

    /// The open project's run targets, read afresh from its data.
    fn load_run_targets(&self, cx: &mut Context<Self>) {
        let targets = ProjectDirectory::get(cx)
            .map(|dir| run_targets::load(&dir))
            .unwrap_or_default();
        ProjectTargets::set(
            ProjectTargets {
                targets,
                running: None,
            },
            cx,
        );
    }

    /// Switched to another project: a divergence analysis or a rescope
    /// search stops, and any inset panel closes, since they belong to the
    /// screen rather than a project; what runs in the project left carries on.
    fn project_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = ProjectDirectory::get(cx);
        if project == self.shown_project {
            return;
        }
        let left = std::mem::replace(&mut self.shown_project, project.clone());
        // The project left keeps its minimizable panels, minimized, their
        // work carrying on; the one switched to gets back its own.
        let parked = ParkedPanels {
            divergence: self.divergence.take(),
            rescope: self.rescope.take(),
            run: self.run.take(),
            subscriptions: [
                std::mem::take(&mut self._divergence_subscriptions),
                std::mem::take(&mut self._rescope_subscriptions),
                std::mem::take(&mut self._run_subscriptions),
            ],
        };
        if let Some(left) = left
            && !parked.is_empty()
        {
            self.parked.insert(left, parked);
        }
        let back = project
            .as_ref()
            .and_then(|project| self.parked.remove(project))
            .unwrap_or_default();
        let [
            divergence_subscriptions,
            rescope_subscriptions,
            run_subscriptions,
        ] = back.subscriptions;
        self.divergence_minimized = back.divergence.is_some();
        self.divergence = back.divergence;
        self._divergence_subscriptions = divergence_subscriptions;
        self.rescope_minimized = back.rescope.is_some();
        self.rescope = back.rescope;
        self._rescope_subscriptions = rescope_subscriptions;
        self.run_minimized = back.run.is_some();
        self.run = back.run;
        self._run_subscriptions = run_subscriptions;
        self.panel_last_focus = None;
        self.load_run_targets(cx);
        self.refresh_running_target(cx);
        self.close_panel(window, cx);
        // A queued prompt of the project left is no longer edited.
        self.prompt_mode.update(cx, |prompt_mode, cx| {
            prompt_mode.cancel_queued_edit(window, cx)
        });
        self.refresh_jobs(cx);
        // The chat input takes the keyboard in the project switched to, once
        // whatever switched it, such as the project list, has closed.
        let prompt_mode = self.prompt_mode.clone();
        window.defer(cx, move |window, cx| {
            prompt_mode.update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx))
        });
    }

    /// Tells the ribbon everything running: in the project on screen, the
    /// task and questions, a spec build, a divergence analysis, and a rescope
    /// search; then in each other open project, its task, questions, and spec
    /// build. The project indicator is told which projects are busy.
    fn refresh_jobs(&mut self, cx: &mut Context<Self>) {
        let here = ProjectDirectory::get(cx);
        let building: Vec<PathBuf> = self.ribbon.read(cx).building_projects().to_vec();
        let prompt_mode = self.prompt_mode.read(cx);
        let running = prompt_mode.running_jobs();
        let mut busy: Vec<(PathBuf, Vec<String>)> = prompt_mode
            .busy_projects()
            .into_iter()
            .map(|busy| {
                let mut what = Vec::new();
                match busy.tasks.len() {
                    0 => {}
                    1 => what.push("Task running".to_string()),
                    count => what.push(format!("{count} tasks running")),
                }
                match busy.questions.len() {
                    0 => {}
                    1 => what.push("1 question running".to_string()),
                    count => what.push(format!("{count} questions running")),
                }
                (busy.project_dir, what)
            })
            .collect();
        for dir in &building {
            match busy.iter_mut().find(|(busy, _)| busy == dir) {
                Some((_, what)) => what.push("Building the spec".to_string()),
                None => busy.push((dir.clone(), vec!["Building the spec".to_string()])),
            }
        }
        let mut jobs: Vec<Job> = running
            .iter()
            .filter(|job| job.project.is_none())
            .cloned()
            .collect();
        if here.as_ref().is_some_and(|here| building.contains(here)) {
            jobs.push(Job {
                kind: JobKind::Build,
                title: "Building the spec".into(),
                detail: None,
                project: None,
            });
        }
        if self
            .divergence
            .as_ref()
            .is_some_and(|view| view.read(cx).is_running())
        {
            jobs.push(Job {
                kind: JobKind::Divergence,
                title: "Analyzing divergence".into(),
                detail: None,
                project: None,
            });
        }
        if self
            .rescope
            .as_ref()
            .is_some_and(|view| view.read(cx).is_running())
        {
            jobs.push(Job {
                kind: JobKind::Rescope,
                title: "Finding scopes to extract".into(),
                detail: None,
                project: None,
            });
        }
        if let Some(title) = self.run.as_ref().and_then(|view| view.read(cx).job_title()) {
            jobs.push(Job {
                kind: JobKind::Run,
                title,
                detail: None,
                project: None,
            });
        }
        // The work of the panels of projects switched away from.
        let mut parked_jobs = Vec::new();
        for (dir, parked) in &self.parked {
            let mut push = |kind, title: &str| {
                parked_jobs.push(Job {
                    kind,
                    title: crate::activity::title_in(title, Some(dir)),
                    detail: None,
                    project: Some(dir.to_path_buf()),
                })
            };
            if parked
                .divergence
                .as_ref()
                .is_some_and(|view| view.read(cx).is_running())
            {
                push(JobKind::Divergence, "Analyzing divergence");
            }
            if parked
                .rescope
                .as_ref()
                .is_some_and(|view| view.read(cx).is_running())
            {
                push(JobKind::Rescope, "Finding scopes to extract");
            }
            if let Some(title) = parked
                .run
                .as_ref()
                .and_then(|view| view.read(cx).job_title())
            {
                push(JobKind::Run, title.as_ref());
            }
        }
        // Then each other project's, in the order the projects are listed.
        let mut others: Vec<PathBuf> = running
            .iter()
            .chain(&parked_jobs)
            .filter_map(|job| job.project.clone())
            .chain(
                building
                    .iter()
                    .filter(|dir| Some(*dir) != here.as_ref())
                    .cloned(),
            )
            .collect();
        others.sort();
        others.dedup();
        for project in others {
            jobs.extend(
                running
                    .iter()
                    .filter(|job| job.project.as_ref() == Some(&project))
                    .cloned(),
            );
            if building.contains(&project) {
                jobs.push(Job {
                    kind: JobKind::Build,
                    title: crate::activity::title_in("Building the spec", Some(&project)),
                    detail: None,
                    project: Some(project.clone()),
                });
            }
            jobs.extend(
                parked_jobs
                    .iter()
                    .filter(|job| job.project.as_ref() == Some(&project))
                    .cloned(),
            );
        }
        self.ribbon.update(cx, |ribbon, cx| {
            ribbon.set_jobs(jobs, cx);
            ribbon
                .project_indicator()
                .clone()
                .update(cx, |indicator, cx| indicator.set_busy(busy, cx));
        });
    }

    /// Reveals a running job: the task or question behind any inset panel,
    /// or the divergence panel.
    pub fn reveal_job(
        &mut self,
        kind: JobKind,
        project: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A job of another project is revealed there.
        if let Some(project) = project
            && ProjectDirectory::get(cx).as_ref() != Some(&project)
        {
            ProjectDirectory::set(project, cx);
            self.project_changed(window, cx);
        }
        match kind {
            JobKind::Build => {}
            JobKind::Divergence => {
                if let Some(view) = self.divergence.clone() {
                    self.restore_divergence(&view, window, cx);
                }
            }
            JobKind::Rescope => {
                if let Some(view) = self.rescope.clone() {
                    self.restore_rescope(&view, window, cx);
                }
            }
            JobKind::Run => {
                if let Some(view) = self.run.clone() {
                    self.restore_run(&view, window, cx);
                }
            }
            JobKind::Task(_) | JobKind::Question(_) => {
                if self.showing_divergence() {
                    self.minimize_divergence(window, cx);
                } else if self.showing_rescope() {
                    self.minimize_rescope(window, cx);
                } else if self.showing_run() {
                    self.minimize_run(window, cx);
                } else if self.panel_open() {
                    self.close_panel(window, cx);
                }
                self.prompt_mode.update(cx, |prompt_mode, cx| match kind {
                    JobKind::Question(id) => prompt_mode.reveal_question(id, window, cx),
                    JobKind::Task(ix) => prompt_mode.reveal_running_task(ix, cx),
                    _ => {}
                });
            }
        }
    }

    pub fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| NewProjectForm::new(window, cx));
        self.diff = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self.divergence_minimized = self.divergence.is_some();
        self.rescope_minimized = self.rescope.is_some();
        self.run_minimized = self.run.is_some();
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

    /// Opens the panel for writing a new instruction, fresh, in place of
    /// anything else there.
    pub fn open_new_instruction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_instruction.is_some() {
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let form = cx.new(|cx| NewInstructionForm::new(project_dir, window, cx));
        self.clear_panel();
        self._panel_subscriptions = vec![cx.subscribe_in(
            &form,
            window,
            |this, _, CloseInstruction(written), window, cx| {
                let written = written.clone();
                // The editor has already asked about anything not saved.
                this.new_instruction = None;
                if this.panel_open() {
                    this.close_panel(window, cx);
                } else {
                    this.panel_last_focus = None;
                    this._panel_subscriptions.clear();
                    this.prompt_mode
                        .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
                    cx.notify();
                }
                // Once written, it opens in a tab.
                if let Some(file) = written {
                    this.prompt_mode.update(cx, |prompt_mode, cx| {
                        prompt_mode.open_file(file, window, cx)
                    });
                }
            },
        )];
        self.new_instruction = Some(form);
        cx.notify();
    }

    /// Opens the form creating a `kind` of spec component in the panel, fresh,
    /// in place of anything else there.
    pub fn open_spec_component(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .spec_component
            .as_ref()
            .is_some_and(|form| form.read(cx).kind() == kind)
        {
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let open_file = self
            .prompt_mode
            .read(cx)
            .open_file_view()
            .map(|file| file.read(cx).path().to_path_buf());
        let form = cx.new(|cx| SpecComponentForm::new(kind, project_dir, open_file, window, cx));
        self.clear_panel();
        self._panel_subscriptions = vec![
            cx.subscribe_in(
                &form,
                window,
                |this, _, _: &CloseSpecComponent, window, cx| this.close_panel(window, cx),
            ),
            // Once written, the panel closes and the file opens.
            cx.subscribe_in(
                &form,
                window,
                |this, _, ComponentCreated(file), window, cx| {
                    let file = file.clone();
                    this.close_panel(window, cx);
                    this.prompt_mode.update(cx, |prompt_mode, cx| {
                        prompt_mode.open_file(file, window, cx)
                    });
                },
            ),
            // Handing it to a skill closes the panel and sends the prompt from
            // the Spec tab, as if written there.
            cx.subscribe_in(
                &form,
                window,
                |this, _, RunComponentSkill(prompt), window, cx| {
                    let prompt = prompt.clone();
                    this.close_panel(window, cx);
                    this.prompt_mode.update(cx, |prompt_mode, cx| {
                        prompt_mode.send(
                            prompt,
                            crate::chat_input::SendMode::Spec,
                            Vec::new(),
                            window,
                            cx,
                        )
                    });
                },
            ),
        ];
        self.spec_component = Some(form);
        cx.notify();
    }

    /// Sends focus back into the open inset panel: to what was last focused
    /// there, or what it holds.
    fn refocus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = if let Some(diff) = &self.diff {
            diff.read(cx).focus_handle(cx)
        } else if let Some(form) = &self.new_project {
            form.read(cx).focus_handle(cx)
        } else if let Some(form) = &self.spec_component {
            form.read(cx).focus_handle(cx)
        } else if let Some(form) = &self.new_instruction {
            form.read(cx).focus_handle(cx)
        } else if let Some(settings) = &self.settings {
            settings.read(cx).focus_handle(cx)
        } else if let Some(editor) = &self.theme_editor {
            editor.read(cx).focus_handle(cx)
        } else if let Some(view) = &self.generate_skills {
            view.read(cx).focus_handle(cx)
        } else if let Some(picker) = &self.project_picker {
            picker.read(cx).focus_handle(cx)
        } else if let Some(view) = self
            .divergence
            .as_ref()
            .filter(|_| !self.divergence_minimized)
        {
            view.read(cx).focus_handle(cx)
        } else if let Some(view) = self.rescope.as_ref().filter(|_| !self.rescope_minimized) {
            view.read(cx).focus_handle(cx)
        } else if let Some(view) = self.run.as_ref().filter(|_| !self.run_minimized) {
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
        self.diff.is_some()
            || self.new_project.is_some()
            || self.spec_component.is_some()
            || self.new_instruction.is_some()
            || self.settings.is_some()
            || self.theme_editor.is_some()
            || self.generate_skills.is_some()
            || self.project_picker.is_some()
            || self.showing_divergence()
            || self.showing_rescope()
            || self.showing_run()
    }

    /// Closes the inset panel, handing focus back to the chat input.
    fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel_open() {
            return;
        }
        // An instruction being written asks before its changes go.
        if let Some(form) = self.new_instruction.clone()
            && form.read(cx).is_dirty(cx)
        {
            let this = cx.entity().downgrade();
            let title = form.read(cx).title(cx);
            crate::file_view::FileView::confirm_discard(title, window, cx, move |window, cx| {
                this.update(cx, |this, cx| {
                    if this.new_instruction.as_ref() == Some(&form) {
                        this.new_instruction = None;
                        this.close_panel(window, cx);
                    }
                })
                .ok();
            });
            return;
        }
        if self.diff.is_none()
            && self.new_project.is_none()
            && self.spec_component.is_none()
            && self.new_instruction.is_none()
            && self.settings.is_none()
            && self.theme_editor.is_none()
            && self.generate_skills.is_none()
            && self.project_picker.is_none()
        {
            // Only the divergence or Rescope panel is showing: closing it
            // stops it. What can be stopped is minimized instead.
            if self.showing_rescope() {
                self.close_rescope(window, cx);
            } else if self.showing_run() {
                if self.run.as_ref().is_some_and(|run| run.read(cx).can_stop()) {
                    self.minimize_run(window, cx);
                } else {
                    self.close_run(window, cx);
                }
            } else {
                self.close_divergence(window, cx);
            }
            return;
        }
        self.diff = None;
        self.new_project = None;
        self.spec_component = None;
        self.new_instruction = None;
        self.settings = None;
        self.theme_editor = None;
        self.generate_skills = None;
        self.project_picker = None;
        self.panel_last_focus = None;
        self._panel_subscriptions.clear();
        self.prompt_mode
            .update(cx, |prompt_mode, cx| prompt_mode.focus_chat(window, cx));
        cx.notify();
    }

    /// The diff or the new project form, in an inset panel over the window;
    /// clicking the dimmed window around it closes it.
    /// What the inset panel shows, if it is open: its name and its view.
    fn panel_content(&self) -> Option<(&'static str, AnyView)> {
        if let Some(view) = self.rescope.as_ref().filter(|_| self.showing_rescope()) {
            return Some(("rescope", view.clone().into()));
        }
        if let Some(view) = self.run.as_ref().filter(|_| self.showing_run()) {
            return Some(("run", view.clone().into()));
        }
        if let Some(view) = self
            .divergence
            .as_ref()
            .filter(|_| self.showing_divergence())
        {
            return Some(("divergence", view.clone().into()));
        }
        if let Some(diff) = &self.diff {
            return Some(("diff", diff.clone().into()));
        }
        if let Some(picker) = &self.project_picker {
            return Some(("open-project", picker.clone().into()));
        }
        if let Some(view) = &self.generate_skills {
            return Some(("generate-skills", view.clone().into()));
        }
        if let Some(settings) = &self.settings {
            return Some(("settings", settings.clone().into()));
        }
        if let Some(editor) = &self.theme_editor {
            return Some(("theme-editor", editor.clone().into()));
        }
        if let Some(form) = &self.spec_component {
            return Some(("spec-component", form.clone().into()));
        }
        if let Some(form) = &self.new_instruction {
            return Some(("new-instruction", form.clone().into()));
        }
        let form = self.new_project.clone()?;
        Some(("new-project", form.into()))
    }

    /// The inset panel, animating in each time it opens, or the one just
    /// closed, animating out.
    fn render_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let content = self.panel_content();
        let leaving = self
            .panel_motion
            .frame(content.as_ref().map(|(_, view)| view.clone()));
        if let Some((name, view)) = content {
            let close = cx.listener(|this, _, window, cx| this.close_panel(window, cx));
            return Some(inset_panel(
                name,
                &self.panel_focus,
                view,
                close,
                self.panel_motion.came_in,
                cx,
            ));
        }
        let view = leaving?;
        window.request_animation_frame();
        Some(closing_panel(view, self.panel_motion.went_away, cx))
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
        let prompt_mode = self.prompt_mode.read(cx);
        let working = prompt_mode.is_working();
        let ribbon = self.ribbon.read(cx);
        let building = ribbon.is_building();
        let mut projects: Vec<PathBuf> = prompt_mode
            .busy_projects()
            .into_iter()
            .map(|busy| busy.project_dir)
            .chain(ribbon.building_projects().iter().cloned())
            .collect();
        projects.sort();
        projects.dedup();
        let description = match (working, building) {
            (false, false) => return true,
            (true, false) => "The harness is still working on a prompt",
            (false, true) => "piton build is still running",
            (true, true) => {
                "The harness is still working on a prompt and piton build is still running"
            }
        };
        let description = match projects.len() {
            0 | 1 => format!("{description}."),
            count => format!("{description}, in {count} projects."),
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
                    .description(description.clone())
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
                SystemCommand::OpenProject => self.open_project_picker(window, cx),
                SystemCommand::Build => self
                    .ribbon
                    .update(cx, |ribbon, cx| ribbon.build(window, cx)),
                SystemCommand::ToggleDarkMode => {
                    ribbon::set_dark_mode(!cx.theme().is_dark(), window, cx)
                }
                SystemCommand::Settings => self.open_settings(window, cx),
                SystemCommand::Quit => self.quit(window, cx),
            },
        }
    }
}

impl MainWindow {
    /// The file tree, with the git panel beneath it while it shows, the tree
    /// taking what it leaves. The git panel shrinks to fit its contents,
    /// growing up to half the sidebar, or, once the edge between them is
    /// dragged, up to where it was dragged. The git panel's header's change of colour is that edge, so no
    /// line is drawn there, but one shows while it is hovered or dragged.
    fn render_project_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.git_panel.read(cx).is_shown() {
            return div()
                .size_full()
                .child(self.sidebar.clone())
                .into_any_element();
        }
        let ring = cx.theme().ring;
        use gpui_kit::base::ElementExt as _;
        let edge = div()
            .id("git-resize")
            .group("git-resize")
            .absolute()
            .top(-GIT_EDGE_REACH)
            .left_0()
            .right_0()
            .h(GIT_EDGE_REACH * 2.)
            .flex()
            .items_center()
            .cursor_row_resize()
            .on_prepaint(|bounds, _, cx| {
                crate::hit_areas::register_resize("git-resize".into(), bounds, cx)
            })
            .child(
                div()
                    .w_full()
                    .h(px(1.))
                    .group_hover("git-resize", |line| line.bg(ring)),
            )
            .on_drag(GitResize, |_, _, _, cx| cx.new(|_| gpui_kit::EmptyView));
        // Lets UI tests find the sidebar; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(gpui_kit::component::v_flex().id("project-sidebar"))
            .size_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<GitResize>, _, cx| {
                    let bounds = event.bounds;
                    // The tree keeps its least height, the git panel its header.
                    let most = (bounds.size.height - MIN_FILE_TREE_HEIGHT)
                        .max(crate::git_panel::MIN_HEIGHT);
                    let height = (bounds.bottom() - event.event.position.y)
                        .clamp(crate::git_panel::MIN_HEIGHT, most);
                    if this.git_height != Some(height) {
                        this.git_height = Some(height);
                        cx.notify();
                    }
                }),
            )
            .child(div().flex_1().min_h_0().child(self.sidebar.clone()))
            .children(crate::sidebar::between(
                true,
                crate::git_panel::COLOUR_IS_EDGE,
                cx,
            ))
            .child(
                gpui_kit::component::v_flex()
                    .relative()
                    .w_full()
                    // As tall as its contents, up to its most, giving way to
                    // the tree however the window is resized.
                    .flex_shrink(1.)
                    .min_h(crate::git_panel::MIN_HEIGHT)
                    .max_h(match self.git_height {
                        Some(most) => most.into(),
                        None => relative(crate::git_panel::MAX_SHARE),
                    })
                    .child(self.git_panel.clone())
                    .child(gpui_kit::TestSupportExt::test_support(edge)),
            )
            .into_any_element()
    }
}

/// Dragging the edge between the file tree and the git panel.
struct GitResize;

/// How far either side of the edge above the git panel reaches for a drag.
const GIT_EDGE_REACH: Pixels = px(3.);

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Painted first: what overlapping hit areas are, afresh.
            .child(crate::hit_areas::frame_start())
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
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
                // Then the list of recent projects.
                let indicator = this.ribbon.read(cx).project_indicator().clone();
                if indicator.read(cx).is_open() {
                    indicator.update(cx, |indicator, cx| indicator.close(cx));
                    return;
                }
                // Then the inset panel.
                if this.panel_open() {
                    this.close_panel(window, cx);
                    return;
                }
                // Nothing to dismiss: Esc is for wherever the keyboard is.
                cx.propagate();
            }))
            .on_action(cx.listener(
                |this, _: &crate::project_indicator::ToggleProjectList, window, cx| {
                    // Like the palette, it acts on what is beneath the inset
                    // panel.
                    if !this.panel_open() {
                        let indicator = this.ribbon.read(cx).project_indicator().clone();
                        indicator.update(cx, |indicator, cx| indicator.toggle(window, cx));
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &ribbon::RunPrimary, window, cx| {
                // Beneath any inset panel but the Run panel itself.
                if !this.panel_open() || this.showing_run() {
                    this.run_primary(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ribbon::RunRelease, window, cx| {
                if this.panel_open() && !this.showing_run() {
                    return;
                }
                if ProjectDirectory::get(cx).is_none() {
                    return;
                }
                match run_targets::release(&ProjectTargets::get(cx).targets) {
                    Some(ix) => this.run_command(RunCommand::Target(ix), window, cx),
                    // Nothing to run: say so, rather than doing nothing.
                    None => window.push_notification(
                        gpui_kit::component::notification::Notification::warning(
                            "This project has no run target marked as the release run. \
                             Find Again in the Code tab looks for one, or mark one in \
                             .suspense/run.json with \"release\": true.",
                        )
                        .title("No release run target"),
                        cx,
                    ),
                }
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
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenThemeEditor, window, cx| {
                this.open_theme_editor(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Rescope, window, cx| this.open_rescope(window, cx)))
            .on_action(cx.listener(|this, _: &NewScope, window, cx| {
                this.open_spec_component(ComponentKind::Scope, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewConcept, window, cx| {
                this.open_spec_component(ComponentKind::Concept, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewShape, window, cx| {
                this.open_spec_component(ComponentKind::Shape, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewInstruction, window, cx| {
                this.open_new_instruction(window, cx)
            }))
            .on_action(cx.listener(|this, _: &GenerateSkills, window, cx| {
                this.open_generate_skills(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenProject, window, cx| {
                    this.open_project_picker(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &AnalyzeDivergence, window, cx| {
                this.open_divergence(Opening::Analyze, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ViewDivergenceReports, window, cx| {
                this.open_divergence(Opening::ViewReports, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ribbon::ShowTab1, _, cx| this.show_ribbon_tab(0, cx)))
            .on_action(cx.listener(|this, _: &ribbon::ShowTab2, _, cx| this.show_ribbon_tab(1, cx)))
            .on_action(cx.listener(|this, _: &ribbon::ShowTab3, _, cx| this.show_ribbon_tab(2, cx)))
            .on_action(cx.listener(|this, _: &ribbon::ShowTab4, _, cx| this.show_ribbon_tab(3, cx)))
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
                        .with_handle_appearance(crate::hit_areas::resize_edges("sidebar-split"))
                        .children([
                            resizable_panel()
                                .size(SIDEBAR_WIDTH)
                                .size_range(MIN_SIDEBAR_WIDTH..Pixels::MAX)
                                .child(self.render_project_sidebar(cx)),
                            resizable_panel()
                                .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                                .child(self.prompt_mode.clone()),
                        ]),
                ),
            )
            .children(self.render_panel(window, cx))
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

    /// The ribbon's body is only as tall as its commands need, and each
    /// command only as tall as it needs: the body is the tallest column on the
    /// open tab, padded 8 pixels above and below, and never taller than two
    /// stacked 27px slim buttons 4px apart, padded, 74px; a tab with no
    /// commands is shorter than any with them. A full button is always two
    /// slim buttons tall, 58px.
    #[gpui_kit::test]
    async fn the_ribbon_body_fits_its_commands(cx: &mut TestAppContext) {
        use crate::ribbon::RibbonTab;

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
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
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        let handle: gpui_kit::AnyWindowHandle = window.into();
        let padding = gpui_kit::px(8.);
        let body_of = |tab: RibbonTab, commands: &[&'static str], cx: &mut TestAppContext| {
            ribbon.update(cx, |ribbon, cx| ribbon.select_tab(tab, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let body = window.find("ribbon-controls").bounds();
                let (mut top, mut bottom) = (body.bottom(), body.top());
                for command in commands {
                    let bounds = window.find(*command).bounds();
                    top = top.min(bounds.top());
                    bottom = bottom.max(bounds.bottom());
                }
                if !commands.is_empty() {
                    assert_eq!(top - body.top(), padding, "{tab:?}: above the commands");
                    assert_eq!(body.bottom() - bottom, padding, "{tab:?}: below them");
                }
                body.size.height
            })
            .unwrap()
        };
        // Two slim buttons stacked, many slim buttons and a full one, and no
        // commands at all.
        let project = body_of(
            RibbonTab::Project,
            &["new-project", "project-directory"],
            cx,
        );
        let spec = body_of(
            RibbonTab::Spec,
            &[
                "build",
                "analyze-divergence",
                "view-divergence-reports",
                "generate-skills",
                "rescope",
                "new-scope",
                "new-concept",
                "new-shape",
                "new-instruction",
            ],
            cx,
        );
        // Brightness, beside a full button.
        let application = body_of(RibbonTab::Application, &["brightness", "settings"], cx);
        // Two slim buttons stacked, padded, is as tall as the body gets.
        let most = gpui_kit::px(27.) * 2. + gpui_kit::px(4.) + padding * 2.;
        assert_eq!(most, gpui_kit::px(74.));
        assert!(
            project == most && spec == most && application == most,
            "the tabs' bodies don't fit their commands: \
             two stacked {project:?}, many {spec:?}, \
             Application {application:?}"
        );

        // On Spec and on Application alike, a full button is two slim buttons
        // tall, and fills the buttons' room.
        for (tab, full, stacked) in [
            (RibbonTab::Spec, "build", 2.),
            (RibbonTab::Application, "settings", 2.),
        ] {
            ribbon.update(cx, |ribbon, cx| ribbon.select_tab(tab, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let body = window.find("ribbon-controls").bounds();
                let button = window.find(full).bounds();
                let slims = gpui_kit::px(27.) * stacked + gpui_kit::px(4.) * (stacked - 1.);
                assert_eq!(slims, gpui_kit::px(58.));
                assert_eq!(
                    button.size.height, slims,
                    "{tab:?}: the full button isn't {stacked} slim buttons tall"
                );
                assert_eq!(
                    button.size.height,
                    body.size.height - padding * 2.,
                    "{tab:?}: it doesn't fill the buttons' room"
                );
                assert_eq!(
                    button.top() - body.top(),
                    padding,
                    "{tab:?}: not at the top"
                );
            })
            .unwrap();
        }
    }

    /// Reset Brightness sits beneath the Brightness slider, in its column:
    /// disabled while the mode showing is at its starting brightness, and
    /// otherwise putting it back there, keeping the mode.
    #[gpui_kit::test]
    async fn reset_brightness_sits_beneath_the_slider(cx: &mut TestAppContext) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};

        use crate::ribbon::RibbonTab;
        use crate::theme;

        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            theme::set_brightness(true, 0, cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        ribbon.update(cx, |r, cx| r.select_tab(RibbonTab::Application, cx));
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let (slider, reset) = (
                window.find("brightness").bounds(),
                window.find("reset-brightness").bounds(),
            );
            assert!(
                reset.top() >= slider.bottom() && reset.left() == slider.left(),
                "{reset:?} isn't beneath {slider:?}"
            );
            // Already at its start: clicking does nothing.
            window.click("reset-brightness", cx);
        })
        .unwrap();
        cx.update(|cx| {
            let step = *theme::brightness_range(true).end();
            theme::set_brightness(true, step, cx);
        });
        ribbon.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("reset-brightness", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(theme::brightness(cx), 0);
            assert!(cx.theme().is_dark(), "resetting changed the mode");
        });
    }

    /// The Brightness slider is the Appearance group's one control, with no
    /// Dark mode switch: as tall as a slim button, its track 120px wide, with
    /// no background of its own. Its one track runs from the darkest
    /// appearance to the lightest, dark mode's steps then light mode's, so
    /// dragging it all the way right brings light mode at its lightest, and
    /// all the way left dark mode at its darkest. The thumb follows the
    /// appearance showing, however it was set, and double-clicking it puts
    /// the mode showing back to 0. At the brightest dark mode allows, the
    /// ribbon's buttons still show lighter than its command area. Being
    /// primary, the collapsed ribbon keeps it.
    #[gpui_kit::test]
    async fn the_brightness_slider_brightens_the_interface(cx: &mut TestAppContext) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
        use gpui_kit::{Bounds, Hsla, Pixels, point, px};

        use crate::ribbon::RibbonTab;
        use crate::theme;

        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        ribbon.update(cx, |r, cx| r.select_tab(RibbonTab::Application, cx));
        cx.run_until_parked();

        // A solid, visible quad painted exactly at `bounds`, if any is.
        fn painted_at(window: &gpui_kit::Window, bounds: Bounds<Pixels>) -> Option<Hsla> {
            let scale = window.scale_factor();
            window
                .painted_quads()
                .into_iter()
                .find(|q| {
                    (q.bounds.origin.x.0 - bounds.left().as_f32() * scale).abs() < 1.
                        && (q.bounds.origin.y.0 - bounds.top().as_f32() * scale).abs() < 1.
                        && (q.bounds.size.width.0 - bounds.size.width.as_f32() * scale).abs() < 1.
                        && (q.bounds.size.height.0 - bounds.size.height.as_f32() * scale).abs() < 1.
                        && q.background.as_solid().is_some_and(|c| c.a > 0.)
                })
                .and_then(|q| q.background.as_solid())
        }

        // The group's one control, with nothing behind it.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("dark-mode").is_none(),
                "Dark mode is still there"
            );
            let group = window.find("Appearance").bounds();
            let brightness = window.find("brightness").bounds();
            let slider = window.find("brightness-slider").bounds();
            assert_eq!(
                brightness.size.height,
                px(27.),
                "not a slim button's height"
            );
            assert_eq!(
                (brightness.left(), brightness.top()),
                (group.left(), group.top()),
                "it doesn't lead the group"
            );
            let darkest = window.find("brightness-darkest").bounds();
            let lightest = window.find("brightness-lightest").bounds();
            assert!(darkest.right() <= slider.left() && slider.right() <= lightest.left());
            assert_eq!(slider.size.width, px(120.), "the track isn't 120px wide");
            assert!(
                painted_at(window, brightness).is_none(),
                "it has a background of its own"
            );
        })
        .unwrap();

        let (dark_low, dark_high) = {
            let range = theme::mode_brightness_range(true);
            (*range.start(), *range.end())
        };
        let light_high = *theme::mode_brightness_range(false).end();
        let last = (theme::appearances().len() - 1) as f32;
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(theme::appearance_index(true, 0) as f32),
            "the thumb isn't on dark mode at 0"
        );
        let drag = |cx: &mut TestAppContext, to_right: bool| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let slider = window.find("brightness-slider").bounds();
                let end = if to_right {
                    slider.right() + px(40.)
                } else {
                    slider.left() - px(40.)
                };
                window.drag(slider.center(), point(end, slider.center().y), cx);
            })
            .unwrap();
            cx.run_until_parked();
        };

        // All the way right: light mode at its lightest, its text as it was.
        drag(cx, true);
        cx.update(|cx| {
            assert!(!cx.theme().is_dark(), "it didn't reach light mode");
            assert_eq!(theme::brightness(cx), light_high);
            assert_eq!(*theme::palette(cx), theme::brightened(false, light_high));
            assert_eq!(
                Theme::global(cx).foreground,
                theme::color(theme::LIGHT.text),
                "the text changed"
            );
        });
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(last)
        );

        // All the way left: dark mode at its darkest.
        drag(cx, false);
        cx.update(|cx| {
            assert!(cx.theme().is_dark(), "it didn't reach dark mode");
            assert_eq!(theme::brightness(cx), dark_low);
            assert_eq!(*theme::palette(cx), theme::brightened(true, dark_low));
        });
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(0.)
        );

        // Set some other way, the thumb follows. At the brightest dark mode
        // allows, the command area has moved, and a button on it still shows
        // lighter than it, as far as it did at 0.
        cx.update(|cx| theme::set_brightness(true, dark_high, cx));
        ribbon.update(cx, |r, cx| r.select_tab(RibbonTab::Project, cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let area = painted_at(window, window.find("ribbon-controls").bounds())
                .expect("the command area isn't painted");
            let moved = theme::brightened(true, dark_high).ribbon;
            assert_eq!(area, theme::color(moved), "the command area didn't move");
            let button = painted_at(window, window.find("new-project").bounds())
                .expect("the button isn't painted");
            let shown: gpui_kit::Rgba = area.blend(button).into();
            let area_rgb: gpui_kit::Rgba = area.into();
            assert!(
                shown.r > area_rgb.r + 0.05,
                "the button {shown:?} isn't lighter than the command area {area_rgb:?}"
            );
            // Its base, moved as the base is.
            let base = (theme::brightened(true, dark_high).base & 0xff) as f32;
            assert!(
                (shown.r * 255. - base).abs() < 1.,
                "the button is {shown:?}, not the moved base"
            );
        })
        .unwrap();
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(theme::appearance_index(true, dark_high) as f32),
            "the thumb didn't follow"
        );

        // Double-clicking it puts the mode showing back to 0, the slider
        // with it.
        ribbon.update(cx, |r, cx| r.select_tab(RibbonTab::Application, cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.double_click("brightness-darkest", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.theme().is_dark());
            assert_eq!(theme::brightness(cx), 0, "a double-click didn't reset it");
            assert_eq!(*theme::palette(cx), theme::DARK);
        });
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(theme::appearance_index(true, 0) as f32)
        );
        // Double-clicking the track near its end resets it too, rather than
        // the second click moving it there.
        drag(cx, true);
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::{InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent};
            window.render_frame(cx);
            let slider = window.find("brightness-slider").bounds();
            let position = point(slider.right() - px(4.), slider.center().y);
            for click_count in [1, 2] {
                let down = MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count,
                    first_mouse: false,
                };
                window.dispatch_event(down.to_platform_input(), cx);
                let up = MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count,
                };
                window.dispatch_event(up.to_platform_input(), cx);
                window.render_frame(cx);
            }
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!cx.theme().is_dark());
            assert_eq!(theme::brightness(cx), 0, "the second click moved it");
        });
        assert_eq!(
            ribbon.read_with(cx, |r, cx| r.brightness_slider(cx)),
            Some(theme::appearance_index(false, 0) as f32)
        );

        // Collapsed, it shows, being primary.
        ribbon.update(cx, |r, cx| r.toggle_collapsed(cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("dark-mode").is_none());
            assert!(
                window.try_find("brightness").is_some(),
                "the collapsed ribbon leaves out Brightness"
            );
        })
        .unwrap();
    }

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

    /// Drawn as the window would draw it, the chat input's chain has no line
    /// beneath it, joined over Code and Spec or apart, since its tab bar
    /// draws no lines; apart, beneath it looks just as beneath Ask, wherever
    /// the window's size and display scale put the chat input, in dark and
    /// light mode alike.
    #[gpui_kit::test]
    async fn the_chain_never_has_a_line_beneath_it_at_any_size(cx: &mut TestAppContext) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let chat_input = main
            .unwrap()
            .read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        let handle: gpui_kit::AnyWindowHandle = window.into();
        let mut wrong = Vec::new();
        for (tab, joined) in [(1, true), (0, false)] {
            cx.update_window(handle, |_, window, cx| {
                chat_input.update(cx, |input, cx| input.select_tab(tab, window, cx))
            })
            .unwrap();
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                cx.update(|cx| Theme::change(mode, None, cx));
                for scale in [1., 1.25, 1.5, 1.75, 2.] {
                    gpui_kit::VisualTestContext::from_window(handle, cx)
                        .simulate_scale_factor_change(scale);
                    for height in 600..612 {
                        let size =
                            gpui_kit::size(gpui_kit::px(1000.), gpui_kit::px(height as f32 + 0.3));
                        gpui_kit::VisualTestContext::from_window(handle, cx).simulate_resize(size);
                        cx.run_until_parked();
                        // Lets the chain finish sliding into place.
                        std::thread::sleep(Duration::from_millis(if height == 600 {
                            400
                        } else {
                            5
                        }));
                        cx.update_window(handle, |_, window, cx| {
                            window.render_frame(cx);
                            window.render_frame(cx);
                            let scale = window.scale_factor();
                            let chain = window.within("both-tab").find(0usize).bounds();
                            let border: gpui_kit::Rgba = cx.theme().border.into();
                            let (width, rows, pixels) = crate::frame_image::pixels(window);
                            // Clear of Code's and Spec's own edges.
                            let columns = ((chain.left().as_f32() + 8.) * scale) as usize
                                ..((chain.right().as_f32() - 8.) * scale) as usize;
                            let bottom = (chain.bottom().as_f32() * scale).round() as usize;
                            // A row under the chain mostly in the line's colour.
                            let line =
                                (bottom.saturating_sub(3)..(bottom + 1).min(rows)).any(|y| {
                                    let lined = columns
                                        .clone()
                                        .filter(|x| {
                                            let p = pixels[y * width + x.min(&(width - 1))];
                                            (p[0] - border.r).abs() < 0.01
                                                && (p[1] - border.g).abs() < 0.01
                                                && (p[2] - border.b).abs() < 0.01
                                        })
                                        .count();
                                    lined > columns.len() / 2
                                });
                            // Apart, the line beneath the chain is Ask's.
                            if !joined {
                                let ask = window.within("ask-tab").find(0usize).bounds();
                                let (chain_x, ask_x) = (
                                    (chain.center().x.as_f32() * scale) as usize,
                                    (ask.center().x.as_f32() * scale) as usize,
                                );
                                let differs = (bottom.saturating_sub(4)..(bottom + 2).min(rows))
                                    .any(|y| pixels[y * width + chain_x] != pixels[y * width + ask_x]);
                                if differs {
                                    wrong.push(format!(
                                        "apart, a line unlike Ask's at {scale}x, {mode:?}, {height}px tall"
                                    ));
                                }
                            }
                            if line {
                                wrong.push(format!(
                                    "a line beneath the chain ({}) at {scale}x, {mode:?}, {height}px tall: {chain:?}",
                                    if joined { "joined" } else { "apart" }
                                ));
                            }
                        })
                        .unwrap();
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// In the theme, dark and light, the ribbon's project indicator leads the
    /// tab row in its own colour, with no border, then the
    /// Project tab, open when the window opens, the tab row's full height;
    /// nothing in the tab row has a border.
    #[gpui_kit::test]
    async fn ribbon_tabs_are_drawn_beside_the_project_indicator(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let scale = window.scale_factor();
                let prefix = window.find("ribbon-prefix").bounds();
                let (left, right) = (
                    prefix.left().as_f32() * scale,
                    prefix.right().as_f32() * scale,
                );
                let quads = window.painted_quads();
                let area = crate::project_indicator::block(cx);
                // Pure black in dark mode; a text input's background in light.
                let expected = match mode {
                    ThemeMode::Dark => gpui_kit::black(),
                    _ => crate::theme::color(crate::theme::palette(cx).well),
                };
                assert_eq!(area, expected, "{mode:?}: the indicator's colour");
                let background = quads.iter().find(|quad| {
                    (quad.bounds.origin.x.0 - left).abs() < 1.
                        && (quad.bounds.size.width.0 - (right - left)).abs() < 1.
                        && quad.background.as_solid() == Some(area)
                });
                let background = background
                    .unwrap_or_else(|| panic!("{mode:?}: the indicator isn't in its own colour"));
                assert!(
                    background.border_widths.right.0 == 0. && background.border_widths.left.0 == 0.,
                    "{mode:?}: the indicator's area has a border"
                );
                // The left container holds only the indicator, vertically
                // centred, shrunk to fit; with nothing running, the right
                // container holds nothing, so isn't there. There is no
                // collapse chevron.
                let left = window.find("ribbon-left").bounds();
                assert!(
                    (left.right() - prefix.right()).abs() < gpui_kit::px(1.)
                        && (left.left() - prefix.left()).abs() < gpui_kit::px(1.),
                    "{mode:?}: the left container {left:?} doesn't fit its contents"
                );
                assert!(window.try_find("ribbon-collapse").is_none());
                assert!(window.try_find("ribbon-right").is_none());
                let indicator_right = right;
                // The Project tab, open, starts straight after the indicator,
                // the full height of the tab row.
                let row = window.find("ribbon-tabs-row").bounds();
                let project_tab = window.find(("ribbon-tab", 0usize)).bounds();
                assert!(
                    (project_tab.left().as_f32() * scale - indicator_right).abs() < 1.,
                    "{mode:?}: the Project tab {project_tab:?} isn't right after the indicator"
                );
                assert_eq!(
                    (project_tab.top(), project_tab.size.height),
                    (row.top(), gpui_kit::px(32.)),
                    "{mode:?}: the Project tab isn't the tab row's height"
                );
                // Nothing in the tab row has a border: the tabs are flat.
                // (The ribbon's own line along its bottom, beneath the
                // commands, is no part of it.)
                let bordered = quads.iter().find(|q| {
                    let bw = &q.border_widths;
                    q.bounds.origin.y.0 + q.bounds.size.height.0
                        <= row.bottom().as_f32() * scale + 1.
                        && q.border_color.a > 0.
                        && (bw.top.0 > 0. || bw.bottom.0 > 0. || bw.left.0 > 0. || bw.right.0 > 0.)
                });
                assert!(
                    bordered.is_none(),
                    "{mode:?}: something in the tab row has a border: {bordered:?}"
                );
            })
            .unwrap();
        }
    }

    /// The tab row has a background of its own, #333333 in dark mode, a step
    /// lighter than the command area's #222222, and in light mode a step
    /// darker; a closed tab shows the row's colour, having none of its own,
    /// and the open tab the command area's, meeting the commands beneath it
    /// with no line between them.
    #[gpui_kit::test]
    async fn ribbon_tab_row_and_open_tab_take_their_own_colours(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let palette = crate::theme::palette(cx);
                let (area, tab_row) = (
                    crate::theme::color(palette.ribbon),
                    crate::theme::color(palette.ribbon_tabs),
                );
                if mode == ThemeMode::Dark {
                    assert_eq!(
                        (palette.ribbon, palette.ribbon_tabs),
                        (0x222222, 0x333333),
                        "the ribbon's dark colours"
                    );
                    assert!(tab_row.l > area.l, "dark: the tab row isn't lighter");
                } else {
                    assert!(tab_row.l < area.l, "light: the tab row isn't darker");
                }
                let scale = window.scale_factor();
                let (width, rows, pixels) = crate::frame_image::pixels(window);
                let at = |x: gpui_kit::Pixels, y: gpui_kit::Pixels| {
                    let (x, y) = ((x.as_f32() * scale) as usize, (y.as_f32() * scale) as usize);
                    assert!(x < width && y < rows, "({x}, {y}) is outside the frame");
                    let p = pixels[y * width + x];
                    gpui_kit::Rgba {
                        r: p[0],
                        g: p[1],
                        b: p[2],
                        a: 1.,
                    }
                };
                let same = |actual: gpui_kit::Rgba, expected: gpui_kit::Hsla, what: &str| {
                    let expected: gpui_kit::Rgba = expected.into();
                    assert!(
                        (actual.r - expected.r).abs() < 0.01
                            && (actual.g - expected.g).abs() < 0.01
                            && (actual.b - expected.b).abs() < 0.01,
                        "{mode:?}: {what} is {actual:?}, not {expected:?}"
                    );
                };
                let row = window.find("ribbon-tabs-row").bounds();
                let body = window.find("ribbon-controls").bounds();
                assert_eq!(row.size.height, gpui_kit::px(32.), "{mode:?}: the tab row");
                assert_eq!(body.top(), row.bottom(), "{mode:?}: a gap under the tabs");
                // Project is open; Application, closed, has no background.
                let open = window.find(("ribbon-tab", 0usize)).bounds();
                let closed = window.find(("ribbon-tab", 3usize)).bounds();
                let middle = row.top() + row.size.height / 2.;
                same(
                    at(open.left() + gpui_kit::px(2.), middle),
                    area,
                    "the open tab",
                );
                same(
                    at(closed.left() + gpui_kit::px(2.), middle),
                    tab_row,
                    "a closed tab",
                );
                same(
                    at(row.right() - gpui_kit::px(40.), middle),
                    tab_row,
                    "the tab row past the tabs",
                );
                // Down the open tab into the commands, not a pixel of anything
                // else: no line between them.
                let x = open.left() + open.size.width / 2.;
                let mut y = open.top() + gpui_kit::px(1.);
                while y < body.top() + gpui_kit::px(6.) {
                    same(
                        at(x, y),
                        area,
                        &format!("the open tab and commands at {y:?}"),
                    );
                    y += gpui_kit::px(1.) / scale;
                }
                // Beside the open tab, the row's colour runs right down to the
                // commands.
                same(
                    at(
                        closed.left() + gpui_kit::px(2.),
                        row.bottom() - gpui_kit::px(1.),
                    ),
                    tab_row,
                    "the bottom of a closed tab",
                );
                same(
                    at(
                        closed.left() + gpui_kit::px(2.),
                        body.top() + gpui_kit::px(1.),
                    ),
                    area,
                    "the commands beneath a closed tab",
                );
            })
            .unwrap();
        }
    }

    /// The Code and Spec tabs, each open alone, show their mode only in their
    /// label: neither the tab nor its body has a coloured background, in dark
    /// or light mode, and opening one moves no other tab.
    #[gpui_kit::test]
    async fn ribbon_mode_tabs_have_no_coloured_background(cx: &mut TestAppContext) {
        use crate::ribbon::RibbonTab;
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        // Every tab's bounds, in order.
        let tab_bounds = |window: &mut gpui_kit::Window| {
            (0..crate::ribbon::RibbonTab::ALL.len())
                .map(|ix| window.find(("ribbon-tab", ix)).bounds())
                .collect::<Vec<_>>()
        };
        for (tab, mode_of_tab, after) in [
            (
                RibbonTab::Code,
                crate::chat_input::SendMode::Code,
                RibbonTab::Spec,
            ),
            (
                RibbonTab::Spec,
                crate::chat_input::SendMode::Spec,
                RibbonTab::Application,
            ),
        ] {
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                cx.update(|cx| Theme::change(mode, None, cx));
                let open = |tabs: &[RibbonTab], cx: &mut TestAppContext| {
                    ribbon.update(cx, |r, cx| {
                        r.tab_clicked(tabs[0], 1, false, cx);
                        for &tab in &tabs[1..] {
                            r.tab_clicked(tab, 1, true, cx);
                        }
                    });
                    cx.run_until_parked();
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        tab_bounds(window)
                    })
                    .unwrap()
                };
                let closed = open(&[RibbonTab::Project], cx);
                let beside_plain = open(&[RibbonTab::Project, after], cx);
                let beside_tinted = open(&[tab, after], cx);
                assert_eq!(
                    beside_plain, beside_tinted,
                    "{tab:?} {mode:?}: a tab moved as {tab:?} was tinted"
                );
                assert_eq!(
                    closed, beside_tinted,
                    "{tab:?} {mode:?}: a tab moved as tabs opened"
                );

                open(&[tab], cx);
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let tint = crate::chat_input::mode_tint(mode_of_tab, cx);
                    let area = crate::theme::color(crate::theme::palette(cx).ribbon);
                    let body = area.blend(tint);
                    let quads = window.painted_quads();
                    assert!(
                        !quads.iter().any(|q| q.background.as_solid() == Some(tint)),
                        "{tab:?} {mode:?}: the tab is tinted"
                    );
                    assert!(
                        !quads.iter().any(|q| q.background.as_solid() == Some(body)),
                        "{tab:?} {mode:?}: the tab's body is tinted"
                    );
                })
                .unwrap();
            }
        }
    }

    /// Ctrl+R (Cmd+R on macOS) opens the recent projects flush beneath the
    /// project indicator, square, in its dark colour, over the dimmed window,
    /// its rows reaching its edges; typing filters them fuzzily, Down and Enter
    /// open the one highlighted, and Escape closes the list.
    #[gpui_kit::test]
    async fn project_list_opens_from_the_keyboard_and_filters(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        let base =
            std::env::temp_dir().join(format!("suspense-project-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["alpha", "beta-tools", "gamma"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
            std::fs::write(base.join(name).join("piton.config.pi"), "").unwrap();
        }
        let file = base.join("recent.json");
        let mut recent = crate::recent_projects::RecentProjects::default();
        for name in ["alpha", "beta-tools", "gamma"] {
            recent.note(&base.join(name));
        }
        recent.save(&file).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            let last = crate::recent_projects::init(Some(file.clone()), cx);
            ProjectDirectory::set(last.unwrap(), cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        #[cfg(target_os = "macos")]
        let toggle = "cmd-r";
        #[cfg(not(target_os = "macos"))]
        let toggle = "ctrl-r";

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.press(toggle, cx);
            window.render_frame(cx);
            window.render_frame(cx);
            let indicator = window.find("ribbon-project-name").bounds();
            let list = window.find("recent-projects").bounds();
            assert!(
                (list.top() - indicator.bottom()).abs() < gpui_kit::px(1.)
                    && (list.left() - indicator.left()).abs() < gpui_kit::px(1.),
                "the list {list:?} isn't flush beneath the indicator {indicator:?}"
            );
            let scale = window.scale_factor();
            let quads = window.painted_quads();
            let well = crate::project_indicator::block(cx);
            let list_quad = quads
                .iter()
                .find(|q| {
                    (q.bounds.origin.y.0 - list.top().as_f32() * scale).abs() < 1.
                        && (q.bounds.size.width.0 - list.size.width.as_f32() * scale).abs() < 1.
                        && q.background.as_solid() == Some(well)
                })
                .expect("the list isn't in the indicator's colour");
            assert_eq!(list_quad.corner_radii.top_left.0, 0., "the list is rounded");
            assert_eq!(
                list_quad.corner_radii.bottom_right.0, 0.,
                "the list is rounded"
            );
            let dim = crate::theme::dimming(cx);
            assert!(
                quads.iter().any(|q| q.background.as_solid() == Some(dim)
                    && q.bounds.size.width.0 >= window.viewport_size().width.as_f32() * scale - 1.),
                "the window isn't dimmed"
            );
            for ix in 0..3usize {
                let row = window.find(("recent-project", ix)).bounds();
                assert!(
                    (row.left() - list.left()).abs() <= gpui_kit::px(1.)
                        && (row.right() - list.right()).abs() <= gpui_kit::px(1.),
                    "row {ix} {row:?} doesn't reach the list's edges {list:?}"
                );
            }

            window.input("bta", cx);
            window.render_frame(cx);
            assert!(window.try_find(("recent-project", 0usize)).is_some());
            assert!(
                window.try_find(("recent-project", 1usize)).is_none(),
                "the filter left more than beta-tools"
            );
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("recent-projects").is_none(),
                "Escape left it open"
            );

            // Newest first: gamma, beta-tools, alpha. Down twice to alpha.
            window.press(toggle, cx);
            window.render_frame(cx);
            window.press("down", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| ProjectDirectory::get(cx)),
            Some(base.join("alpha"))
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("recent-projects").is_none(),
                "picking left it open"
            );
        })
        .unwrap();
    }

    /// Alt+1 to Alt+5 open the ribbon's tabs in the order they are shown,
    /// each alone, expanding a collapsed ribbon; Alt+6 does nothing.
    #[gpui_kit::test]
    async fn alt_number_opens_the_ribbon_tabs_in_order(cx: &mut TestAppContext) {
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
        let handle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        let press = |key: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.press(key, cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        for (ix, tab) in RibbonTab::ALL.into_iter().enumerate() {
            press(&format!("alt-{}", ix + 1), cx);
            assert_eq!(ribbon.read_with(cx, |r, _| r.open_tabs().to_vec()), [tab]);
        }
        press("alt-6", cx);
        assert_eq!(
            ribbon.read_with(cx, |r, _| r.open_tabs().to_vec()),
            [RibbonTab::Application]
        );
        ribbon.update(cx, |r, cx| {
            if !r.is_collapsed() {
                r.toggle_collapsed(cx)
            }
        });
        press("alt-3", cx);
        ribbon.read_with(cx, |r, _| {
            assert!(!r.is_collapsed(), "the ribbon stayed collapsed");
            assert_eq!(r.open_tabs(), [RibbonTab::Spec]);
        });
    }

    /// The ribbon's buttons: full ones the height of the body inside 8px of
    /// padding, slim ones 27px tall at the top of their column, 4px apart,
    /// square and borderless, each on a background of its own apart from the
    /// body's, #444444 on its #222222 in dark mode; the first group starts
    /// 8px in, with no title strip.
    #[gpui_kit::test]
    async fn ribbon_buttons_are_full_or_slim_and_square(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx);
        });
        // Each button is square, with a background of its own laid over the
        // body, clearly apart from it, though not starkly.
        fn square_and_apart(
            window: &gpui_kit::Window,
            button: gpui_kit::Bounds<gpui_kit::Pixels>,
            mode: ThemeMode,
            cx: &gpui_kit::App,
        ) {
            let scale = window.scale_factor();
            let quad = window
                .painted_quads()
                .into_iter()
                .find(|q| {
                    (q.bounds.origin.x.0 - button.left().as_f32() * scale).abs() < 1.
                        && (q.bounds.origin.y.0 - button.top().as_f32() * scale).abs() < 1.
                        && (q.bounds.size.height.0 - button.size.height.as_f32() * scale).abs() < 1.
                        && q.background.as_solid().is_some_and(|c| c.a > 0.)
                })
                .unwrap_or_else(|| panic!("{mode:?}: {button:?} has no background of its own"));
            let widths = quad.border_widths;
            assert!(
                widths.top.0 == 0.
                    && widths.bottom.0 == 0.
                    && widths.left.0 == 0.
                    && widths.right.0 == 0.,
                "{mode:?}: {button:?} has a border"
            );
            let radii = quad.corner_radii;
            assert!(
                radii.top_left.0 == 0.
                    && radii.top_right.0 == 0.
                    && radii.bottom_left.0 == 0.
                    && radii.bottom_right.0 == 0.,
                "{mode:?}: {button:?} has rounded corners"
            );
            let color = quad.background.as_solid().unwrap();
            assert!(
                color.a < 0.9,
                "{mode:?}: the background isn't laid over the body"
            );
            // Clearly apart from the body, though not starkly.
            let body_color = crate::theme::color(crate::theme::palette(cx).ribbon);
            let apart = (body_color.blend(color).l - body_color.l).abs();
            if mode == ThemeMode::Dark {
                let shown: gpui_kit::Rgba = body_color.blend(color).into();
                assert!(
                    (shown.r * 255. - 68.).abs() < 1. && (shown.g - shown.r).abs() < 0.001,
                    "dark: {button:?} is {shown:?}, not #444444"
                );
            }
            assert!(
                (0.06..0.16).contains(&apart),
                "{mode:?}: {button:?} is {apart} apart from the body"
            );
        }
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            ribbon.update(cx, |r, cx| {
                r.select_tab(crate::ribbon::RibbonTab::Project, cx)
            });
            cx.update(|cx| Theme::change(mode, None, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.simulate_mouse_move(
                    gpui_kit::point(gpui_kit::px(900.), gpui_kit::px(600.)),
                    cx,
                );
                window.render_frame(cx);
                let px = gpui_kit::px;
                let body = window.find("ribbon-controls").bounds();
                let group = window.find("Project").bounds();
                // No title strip: the group starts with its buttons, 8px in
                // and 8px down, and ends 8px above the bottom.
                assert_eq!(
                    (group.left() - body.left(), group.top() - body.top()),
                    (px(8.), px(8.)),
                    "{mode:?}: the first group isn't 8px in"
                );
                assert_eq!(
                    body.bottom() - group.bottom(),
                    px(8.),
                    "{mode:?}: the group isn't 8px above the bottom"
                );
                // Project's two commands are slim, stacked from the top.
                let slim = window.find("new-project").bounds();
                let below = window.find("project-directory").bounds();
                assert_eq!(
                    slim.top() - body.top(),
                    px(8.),
                    "{mode:?}: no 8px padding above the buttons"
                );
                assert_eq!(
                    slim.left(),
                    group.left(),
                    "{mode:?}: space before New Project"
                );
                for button in [slim, below] {
                    assert_eq!(
                        button.size.height,
                        px(27.),
                        "{mode:?}: {button:?} isn't slim"
                    );
                }
                assert_eq!(
                    (below.left(), below.top() - slim.bottom()),
                    (slim.left(), px(4.)),
                    "{mode:?}: Open Project isn't stacked 4px under New Project"
                );
                square_and_apart(window, slim, mode, cx);
            })
            .unwrap();

            // Settings, on the Application tab, is a full button.
            ribbon.update(cx, |r, cx| {
                r.select_tab(crate::ribbon::RibbonTab::Application, cx)
            });
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let px = gpui_kit::px;
                let body = window.find("ribbon-controls").bounds();
                let full = window.find("settings").bounds();
                assert_eq!(full.size.height, px(58.), "{mode:?}: Settings isn't full");
                assert_eq!(
                    full.top() - body.top(),
                    px(8.),
                    "{mode:?}: no 8px padding above the buttons"
                );
                assert_eq!(
                    body.bottom() - full.bottom(),
                    px(8.),
                    "{mode:?}: no 8px padding below the buttons"
                );

                square_and_apart(window, full, mode, cx);
            })
            .unwrap();
        }
    }

    /// On the Spec tab, whose five groups are Build, Components, Analysis,
    /// Skills, and Refactor, a divider stands between each group and the
    /// next, and none before the first: 4px wide, 4px from the buttons either
    /// side, as tall as the buttons, from the top of the first to the bottom
    /// of the last, its ends softly rounded, in the tab row's colour, which it
    /// keeps on the Spec tab's tinted body, in dark and light mode alike.
    #[gpui_kit::test]
    async fn ribbon_groups_are_parted_by_dividers(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle = window.into();
        let ribbon = main.unwrap().read_with(cx, |main, _| main.ribbon.clone());
        ribbon.update(cx, |r, cx| r.select_tab(crate::ribbon::RibbonTab::Spec, cx));
        let groups = ["Build", "Components", "Analysis", "Skills", "Refactor"];
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let px = gpui_kit::px;
                let scale = window.scale_factor();
                let body = window.find("ribbon-controls").bounds();
                let tab_row = crate::theme::color(crate::theme::palette(cx).ribbon_tabs);
                let quads = window.painted_quads();
                assert!(window.try_find(("ribbon-divider", 0usize)).is_none());
                assert!(window.try_find(("ribbon-divider", groups.len())).is_none());
                for (ix, pair) in groups.windows(2).enumerate() {
                    let (before, after) =
                        (window.find(pair[0]).bounds(), window.find(pair[1]).bounds());
                    let divider = window.find(("ribbon-divider", ix + 1)).bounds();
                    assert_eq!(divider.size.width, px(4.), "{mode:?}: {pair:?}'s divider");
                    assert_eq!(
                        (
                            divider.left() - before.right(),
                            after.left() - divider.right()
                        ),
                        (px(4.), px(4.)),
                        "{mode:?}: {pair:?}'s divider isn't 4px from their buttons"
                    );
                    // As tall as the buttons: the tab's full buttons, from 8px
                    // down to 8px above the bottom.
                    assert_eq!(
                        (divider.top() - body.top(), divider.size.height),
                        (px(8.), px(58.)),
                        "{mode:?}: {pair:?}'s divider isn't as tall as the buttons"
                    );
                    let quad = quads
                        .iter()
                        .find(|q| {
                            (q.bounds.origin.x.0 - divider.left().as_f32() * scale).abs() < 1.
                                && (q.bounds.size.width.0 - 4. * scale).abs() < 1.
                                && q.background.as_solid().is_some()
                        })
                        .unwrap_or_else(|| panic!("{mode:?}: {pair:?}'s divider isn't drawn"));
                    assert_eq!(
                        quad.background.as_solid(),
                        Some(tab_row),
                        "{mode:?}: {pair:?}'s divider isn't the tab row's colour"
                    );
                    let radii = quad.corner_radii;
                    assert!(
                        radii.top_left.0 > 0. && radii.bottom_right.0 > 0.,
                        "{mode:?}: {pair:?}'s divider's ends aren't rounded"
                    );
                }
            })
            .unwrap();
        }
    }

    /// A file's tab has its close button just after its name, and can be
    /// dragged along the tab bar: dropped on another file's tab it takes its
    /// place, selected, while Chat stays first.
    #[gpui_kit::test]
    async fn file_tabs_are_tight_and_can_be_dragged(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-drag-tabs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.md", "b.md", "c.md"] {
            std::fs::write(dir.join(name), "# x\n").unwrap();
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let prompt_mode = main.read_with(cx, |main, _| main.prompt_mode.clone());
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        // Each opens just after the one selected, so they open in order.
        for name in ["a.md", "b.md", "c.md"] {
            cx.update_window(handle, |_, window, cx| {
                prompt_mode.update(cx, |p, cx| p.open_file(dir.join(name), window, cx))
            })
            .unwrap();
            cx.run_until_parked();
        }
        let order = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |p, cx| {
                p.open_file_views()
                    .iter()
                    .map(|file| {
                        file.read(cx)
                            .path()
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>()
            })
        };
        let shown = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |p, cx| {
                p.open_file_view().map(|file| {
                    file.read(cx)
                        .path()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
            })
        };
        assert_eq!(order(cx), ["a.md", "b.md", "c.md"]);

        // Each tab is the bar's height, one after another from its left
        // edge; a file's has 12px of padding at its left, and its 16px close
        // button leaves 6px at its right.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let bar = window.find("body-tabs-row").bounds();
            let chat = window.within("body-tabs").find(0usize).bounds();
            let tab = window.within("body-tabs").find(1usize).bounds();
            let name = window.find(("file-tab-name", 0usize)).bounds();
            let close = window.find(("close-file-tab-box", 0usize)).bounds();
            assert_eq!(chat.left(), bar.left(), "Chat isn't at the bar's left edge");
            assert_eq!(
                tab.left(),
                chat.right(),
                "a gap between Chat and the first file"
            );
            for t in [chat, tab] {
                assert_eq!((t.top(), t.size.height), (bar.top(), bar.size.height));
            }
            let left = name.left() - tab.left();
            let right = tab.right() - (close.left() + gpui_kit::px(16.));
            assert!(
                (left - gpui_kit::px(12.)).abs() <= gpui_kit::px(1.)
                    && (right - gpui_kit::px(6.)).abs() <= gpui_kit::px(1.),
                "the tab's padding is {left:?} and {right:?}"
            );
            assert_eq!(close.size.height, gpui_kit::px(16.));
            assert!(
                (close.center().y - tab.center().y).abs() <= gpui_kit::px(1.)
                    && (name.center().y - tab.center().y).abs() <= gpui_kit::px(1.),
                "the tab's contents aren't centred up and down"
            );
        })
        .unwrap();
        // The close button sits 4px after the name.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let name = window.find(("file-tab-name", 0usize)).bounds();
            let close = window.find(("close-file-tab-box", 0usize)).bounds();
            let gap = close.left() - name.right();
            assert!(
                gap >= gpui_kit::px(0.) && gap <= gpui_kit::px(4.5),
                "the close button is {gap:?} from the name"
            );
        })
        .unwrap();

        // Drags the name of tab `from` into the gap past tab `to`, over its
        // far half, so it takes that tab's place.
        let drag = |cx: &mut TestAppContext, from: usize, to: usize| {
            let (start, end) = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let tab = window.within("body-tabs").find(to + 1).bounds();
                    let share = if to > from { 0.85 } else { 0.15 };
                    (
                        window.find(("file-tab-name", from)).bounds().center(),
                        gpui_kit::point(tab.left() + tab.size.width * share, tab.center().y),
                    )
                })
                .unwrap();
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_mouse_move(start, None, Default::default());
            visual.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
            let halfway = start + (end - start) / 2.;
            visual.simulate_mouse_move(halfway, gpui_kit::MouseButton::Left, Default::default());
            visual.simulate_mouse_move(end, gpui_kit::MouseButton::Left, Default::default());
            visual.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
            cx.run_until_parked();
        };
        drag(cx, 0, 2);
        assert_eq!(order(cx), ["b.md", "c.md", "a.md"]);
        assert_eq!(
            shown(cx).as_deref(),
            Some("a.md"),
            "the dragged tab isn't selected"
        );
        drag(cx, 2, 1);
        assert_eq!(order(cx), ["b.md", "a.md", "c.md"]);
        assert_eq!(shown(cx).as_deref(), Some("a.md"));
        // Chat stays first: a drop on it moves nothing.
        let chat = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let first = window.find(("file-tab", 0usize)).bounds();
                gpui_kit::point(
                    window.find("body-tabs-row").bounds().left() + gpui_kit::px(12.),
                    first.center().y,
                )
            })
            .unwrap();
        let start = cx
            .update_window(handle, |_, window, _| {
                window.find(("file-tab-name", 2usize)).bounds().center()
            })
            .unwrap();
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_mouse_move(start, None, Default::default());
        visual.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(chat, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(chat, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        assert_eq!(order(cx), ["b.md", "a.md", "c.md"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// In the window, the chat input's buttons end 8 pixels in from its
    /// right edge, and that edge is the window's, with no sidebar beside it.
    #[gpui_kit::test]
    async fn the_chat_input_is_padded_at_its_right_in_the_window(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let body = window.find("body-tint").bounds();
            let rightmost = ["send", "send-options", "queue", "preview"]
                .map(|id| window.find(id).bounds().right())
                .into_iter()
                .fold(gpui_kit::px(0.), gpui_kit::Pixels::max);
            let width = window.viewport_size().width;
            assert!(
                body.right() <= width,
                "the chat input {body:?} runs past the window {width:?}"
            );
            assert_eq!(
                width - rightmost,
                gpui_kit::px(8.),
                "the buttons' right padding"
            );
        })
        .unwrap();
    }

    /// Clicking files in the project tree opens each in a tab of its own
    /// beneath the body's tab bar, after Chat, as wide as the bar, with the
    /// chat input beneath it and nothing sliding. Clicking an open file again
    /// selects its tab. Closing the selected tab selects the one to its
    /// right, else its left, and Chat once no file is left.
    #[gpui_kit::test]
    async fn files_open_in_tabs_of_the_body(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-open-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "# A\n").unwrap();
        std::fs::write(dir.join("b.md"), "# B\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let prompt_mode = main.read_with(cx, |main, _| main.prompt_mode.clone());
        let open = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |p, cx| {
                p.open_file_views()
                    .iter()
                    .map(|file| file.read(cx).path().file_name().unwrap().to_owned())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
        };
        let shown = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |p, cx| {
                p.open_file_view().map(|file| {
                    file.read(cx)
                        .path()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
            })
        };

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 1usize)).is_some()
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

        // Straight away, with nothing sliding, the file fills the space
        // between the tab bar and the chat input, as wide as the bar, in
        // place of the chat history.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let tabs = window.find("body-tabs-row").bounds();
            let file = window.find("file-view").bounds();
            let editor = window.find("prompt-editor").bounds();
            assert!(
                window.try_find("history").is_none(),
                "the history shows too"
            );
            assert!(window.try_find(("pane-slide", 1usize)).is_none());
            assert!(
                (file.top() - tabs.bottom()).abs() <= gpui_kit::px(1.)
                    && (file.left() - tabs.left()).abs() <= gpui_kit::px(1.)
                    && (file.right() - tabs.right()).abs() <= gpui_kit::px(1.),
                "the file {file:?} isn't beneath the tab bar {tabs:?}, as wide"
            );
            assert!(
                editor.top() >= file.bottom(),
                "the chat input {editor:?} isn't beneath the file {file:?}"
            );
        })
        .unwrap();
        assert_eq!(open(cx), ["a.md"]);

        // Another file opens in a tab of its own just after the one selected,
        // leaving the first open; clicking the first again selects its tab.
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 1usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(open(cx), ["a.md", "b.md"]);
        assert_eq!(shown(cx).as_deref(), Some("b.md"));
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(open(cx), ["a.md", "b.md"]);
        assert_eq!(shown(cx).as_deref(), Some("a.md"));

        // Closing the selected tab selects the one to its right; closing
        // the last, Chat, where the history shows again.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("close-file-tab", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(open(cx), ["b.md"]);
        assert_eq!(shown(cx).as_deref(), Some("b.md"));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("close-file-tab", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(open(cx).is_empty());
        assert_eq!(shown(cx), None);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("file-view").is_none());
            assert!(window.try_find("history").is_some());
        })
        .unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file's diff floats over the whole window, inset 32px from each edge,
    /// leaving any file open in a tab alone; Esc closes it, as does
    /// clicking the dimmed window around it, and opening the file from it
    /// closes it and opens the file in a tab.
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
                "the diff closed the file in its tab"
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

        // Opening the file from it closes it, with the file in a tab.
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
            crate::double_borders::assert_none(window);
            assert!(window.try_find("file-view").is_some());
        })
        .unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Open Project opens a file browser for a piton.config.pi in the inset
    /// panel rather than the platform's dialog; choosing one makes its folder
    /// the project and closes the panel.
    #[gpui_kit::test]
    async fn open_project_browses_for_its_config(cx: &mut TestAppContext) {
        let base =
            std::env::temp_dir().join(format!("suspense-open-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("demo")).unwrap();
        std::fs::write(base.join("demo/piton.config.pi"), "").unwrap();
        std::fs::write(base.join("demo/notes.md"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            crate::recent_projects::init(Some(base.join("recent.json")), cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let windows = cx.update(|cx| cx.windows().len());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("project-directory", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let picker = main.read_with(cx, |main, _| {
            main.project_picker.clone().expect("no picker")
        });
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            windows,
            "a window opened"
        );
        picker.update(cx, |picker, cx| picker.go(base.join("demo"), cx));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("open-project-panel").is_some());
            // Only the config is listed, so it is the first row.
            assert!(
                window.try_find(("folder-row", 1usize)).is_none(),
                "notes.md is listed"
            );
            window.double_click(("folder-row", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| ProjectDirectory::get(cx)),
            Some(base.join("demo"))
        );
        assert!(
            !main.read_with(cx, |main, _| main.panel_open()),
            "the panel stayed open"
        );
        // Opened again, it starts in the folder the project was chosen in.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("project-directory", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let picker = main.read_with(cx, |main, _| main.project_picker.clone().unwrap());
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.dir().to_path_buf()),
            base.join("demo")
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Clicking the project indicator lists the recent projects that can
    /// still be opened, newest first, the one open marked; clicking one opens
    /// it, and the last row opens the project browser. Escape closes the list.
    #[gpui_kit::test]
    async fn project_indicator_lists_recent_projects(cx: &mut TestAppContext) {
        let base = std::env::temp_dir().join(format!("suspense-indicator-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["first", "second", "gone"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
        }
        for name in ["first", "second"] {
            std::fs::write(base.join(name).join("piton.config.pi"), "").unwrap();
        }
        let file = base.join("recent.json");
        let mut recent = crate::recent_projects::RecentProjects::default();
        for name in ["first", "gone", "second"] {
            recent.note(&base.join(name));
        }
        recent.save(&file).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            // The last project opened opens again.
            let last = crate::recent_projects::init(Some(file.clone()), cx);
            assert_eq!(last, Some(base.join("second")));
            ProjectDirectory::set(last.unwrap(), cx);
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
            window.click("ribbon-project-name", cx);
            window.render_frame(cx);
            assert!(window.try_find(("recent-project", 0usize)).is_some());
            assert!(window.try_find(("recent-project", 1usize)).is_some());
            assert!(
                window.try_find(("recent-project", 2usize)).is_none(),
                "a project without its config is listed"
            );
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("recent-projects").is_none(),
                "Escape left it open"
            );

            window.click("ribbon-project-name", cx);
            window.render_frame(cx);
            window.click(("recent-project", 1usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| ProjectDirectory::get(cx)),
            Some(base.join("first"))
        );
        // Opening it noted it, newest first, and saved that.
        assert_eq!(
            crate::recent_projects::RecentProjects::load(&file).projects[0],
            base.join("first")
        );

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("ribbon-project-name", cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            window.click("open-project-from-recent", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.project_picker.is_some()));
        std::fs::remove_dir_all(&base).ok();
    }

    /// The Code tab offers Find How to Run until the project's targets are
    /// found; finding saves them, and each becomes a button, the way to run
    /// the project leading. Pressing one runs it in the Run panel, its button
    /// spinning, and listed as running; pressing it again only brings the
    /// panel back.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn projects_are_found_runnable_and_run_from_the_code_tab(cx: &mut TestAppContext) {
        use crate::ribbon::RibbonTab;
        use crate::run_targets::{self, ProjectTargets};
        let (dir, _) = crate::generate_skills::fixture("run-main");
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            ProjectDirectory::set(dir.clone(), cx);
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
        ribbon.update(cx, |ribbon, cx| ribbon.select_tab(RibbonTab::Code, cx));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("find-how-to-run").is_some());
            assert!(window.try_find(("run-target", 0usize)).is_none());
        })
        .unwrap();

        // Found, the targets are saved, and the tab has a button for each.
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                let view = cx.new(|cx| {
                    crate::run_view::RunView::finding_with(
                        dir.clone(),
                        crate::run_view::tests::agent,
                        cx,
                    )
                });
                main.show_run(view, window, cx);
            });
        })
        .unwrap();
        for _ in 0..100 {
            cx.run_until_parked();
            if cx.update(|cx| !ProjectTargets::get(cx).targets.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(run_targets::load(&dir).len(), 2);
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_run(window, cx));
            window.render_frame(cx);
            assert!(window.try_find("find-how-to-run").is_none());
            assert!(window.try_find(("run-target", 0usize)).is_some());
            assert!(window.try_find(("run-target", 1usize)).is_some());
            assert!(window.try_find("find-run-again").is_some());
        })
        .unwrap();

        // Pressing a target runs it, here something that runs on.
        run_targets::save(
            &dir,
            &[run_targets::Target {
                name: "Serve".into(),
                command: "sleep 30".into(),
                kind: run_targets::Kind::Run,
                release: false,
            }],
        )
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.load_run_targets(cx);
                main.run_command(crate::ribbon::RunCommand::Target(0), window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        let panel = main.read_with(cx, |main, _| main.run.clone().unwrap());
        assert!(main.read_with(cx, |main, _| main.showing_run()));
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(0));
        assert_eq!(
            panel.read_with(cx, |view, _| view.job_title()).as_deref(),
            Some("Running Serve")
        );
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.minimize_run(window, cx);
                main.run_command(crate::ribbon::RunCommand::Target(0), window, cx);
            });
        })
        .unwrap();
        assert!(main.read_with(cx, |main, _| main.showing_run()));
        assert_eq!(
            panel.read_with(cx, |view, _| view.running_target()),
            Some(0)
        );
        // While it can be stopped, it can't be closed: it has no close
        // button, and Esc or clicking the dimmed window minimizes it, the
        // target running on.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("run-stop").is_some());
            assert!(window.try_find("run-minimize").is_some());
            assert!(window.try_find("run-close").is_none());
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.run.is_some() && !main.showing_run()));
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(0));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.run_command(crate::ribbon::RunCommand::Target(0), window, cx)
            });
            window.render_frame(cx);
            window.click_at(
                "run-backdrop",
                gpui_kit::point(gpui_kit::px(8.), gpui_kit::px(8.)),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.run.is_some() && !main.showing_run()));
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(0));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.run_command(crate::ribbon::RunCommand::Target(0), window, cx)
            });
        })
        .unwrap();
        // Once stopped, it can be closed.
        panel.update(cx, |view, cx| view.stop(cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("run-close").is_some());
        })
        .unwrap();
        // Closing stops it.
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_run(window, cx))
        })
        .unwrap();
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), None);

        // Ctrl/Cmd+Shift+F5 runs the release target, when there is one.
        let release = if cfg!(target_os = "macos") {
            "cmd-shift-f5"
        } else {
            "ctrl-shift-f5"
        };
        cx.update_window(handle, |_, window, cx| window.press(release, cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), None);
        cx.update_window(handle, |_, window, cx| {
            let notes = Root::read(window, cx).notification.read(cx).notifications();
            assert_eq!(
                notes.len(),
                1,
                "no notification said there is no release target"
            );
        })
        .unwrap();
        run_targets::save(
            &dir,
            &[
                run_targets::Target {
                    name: "Serve".into(),
                    command: "sleep 30".into(),
                    kind: run_targets::Kind::Run,
                    release: false,
                },
                run_targets::Target {
                    name: "Serve Release".into(),
                    command: "sleep 30".into(),
                    kind: run_targets::Kind::Run,
                    release: true,
                },
            ],
        )
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.load_run_targets(cx));
            window.press(release, cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(1));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_run(window, cx))
        })
        .unwrap();

        // Ctrl/Cmd+F5 runs the primary target.
        let run = if cfg!(target_os = "macos") {
            "cmd-f5"
        } else {
            "ctrl-f5"
        };
        cx.update_window(handle, |_, window, cx| window.press(run, cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(0));
        assert!(main.read_with(cx, |main, _| main.showing_run()));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_run(window, cx))
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Each project comes back as it was left: its file, its output's lock,
    /// its commit message, and its Run panel, minimized, still running.
    #[gpui_kit::test]
    async fn projects_come_back_as_they_were_left(cx: &mut TestAppContext) {
        use crate::activity::JobKind;
        use crate::run_targets::{self, ProjectTargets};
        let (a, _) = crate::generate_skills::fixture("left-alpha");
        let (b, _) = crate::generate_skills::fixture("left-beta");
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            ProjectDirectory::set(a.clone(), cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        let (prompt_mode, git_panel) = main.read_with(cx, |main, _| {
            (main.prompt_mode.clone(), main.git_panel.clone())
        });

        // In alpha: a file open, the output locked, a message begun, and a
        // target running in the Run panel.
        std::fs::write(a.join("notes.md"), "notes").unwrap();
        run_targets::save(
            &a,
            &[run_targets::Target {
                name: "Serve".into(),
                command: "sleep 30".into(),
                kind: run_targets::Kind::Run,
                release: false,
            }],
        )
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.open_file(a.join("notes.md"), window, cx);
                prompt_mode.set_output_lock(true, cx);
            });
            git_panel.update(cx, |panel, cx| {
                panel.set_message("Half a thought", window, cx)
            });
            main.update(cx, |main, cx| {
                main.load_run_targets(cx);
                main.run_command(crate::ribbon::RunCommand::Target(0), window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        let panel = main.read_with(cx, |main, _| main.run.clone().unwrap());

        // In beta, none of it shows, and alpha's target runs on, listed.
        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        assert!(prompt_mode.read_with(cx, |p, _| p.open_file_view().is_none()));
        assert!(!prompt_mode.read_with(cx, |p, _| p.output_locked()));
        assert_eq!(git_panel.read_with(cx, |p, cx| p.message(cx)), "");
        assert!(main.read_with(cx, |main, _| main.run.is_none()));
        assert_eq!(
            panel.read_with(cx, |view, _| view.running_target()),
            Some(0)
        );
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), None);
        let jobs = main.read_with(cx, |main, cx| main.ribbon.read(cx).jobs().to_vec());
        assert!(
            jobs.iter()
                .any(|job| job.kind == JobKind::Run && job.project.as_ref() == Some(&a)),
            "{jobs:?}"
        );

        // Back in alpha, everything is as it was, the panel minimized.
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        assert!(prompt_mode.read_with(cx, |p, cx| {
            p.open_file_view()
                .is_some_and(|file| file.read(cx).path() == a.join("notes.md"))
        }));
        assert!(prompt_mode.read_with(cx, |p, _| p.output_locked()));
        assert_eq!(
            git_panel.read_with(cx, |p, cx| p.message(cx)),
            "Half a thought"
        );
        assert!(main.read_with(cx, |main, _| main.run.as_ref() == Some(&panel)));
        assert!(!main.read_with(cx, |main, _| main.showing_run()));
        assert_eq!(cx.update(|cx| ProjectTargets::get(cx).running), Some(0));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_run(window, cx))
        })
        .unwrap();
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    /// Rescope opens its panel, which minimizes while it looks, and comes back
    /// from the activity list or the command; its
    /// Refactor closes it and sends the prompt from the Spec tab, queued while
    /// the harness is busy.
    #[gpui_kit::test]
    async fn rescope_minimizes_and_refactors(cx: &mut TestAppContext) {
        use crate::activity::JobKind;
        let (dir, _) = crate::generate_skills::fixture("rescope-main");
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            ProjectDirectory::set(dir.clone(), cx);
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
            main.update(cx, |main, cx| {
                let view = cx.new(|cx| {
                    crate::rescope_view::RescopeView::with_agent(
                        dir.clone(),
                        crate::rescope_view::tests::agent,
                        cx,
                    )
                });
                main.show_rescope(view, window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("rescope-panel").is_some());
            window.click("rescope-minimize", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            !main.read_with(cx, |main, _| main.panel_open()),
            "minimizing left it open"
        );
        assert!(main.read_with(cx, |main, _| main.rescope.is_some()));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.reveal_job(JobKind::Rescope, None, window, cx)
            });
        })
        .unwrap();
        assert!(main.read_with(cx, |main, _| main.showing_rescope()));
        // The command brings back the same panel rather than another.
        let first = main.read_with(cx, |main, _| main.rescope.clone().unwrap());
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.minimize_rescope(window, cx);
                main.open_rescope(window, cx);
            });
        })
        .unwrap();
        assert_eq!(
            main.read_with(cx, |main, _| main.rescope.clone()),
            Some(first)
        );

        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("rescope-refactor").is_some()
        })
        .await;
        let prompt_mode = main.read_with(cx, |main, _| main.prompt_mode.clone());
        prompt_mode.update(cx, |prompt_mode, _| prompt_mode.set_working(true));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            window.click("rescope-refactor", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| main.rescope.is_none()),
            "the panel stayed"
        );
        let queued = prompt_mode.read_with(cx, |prompt_mode, _| prompt_mode.queued_texts());
        assert_eq!(queued.len(), 1);
        assert!(queued[0].contains("TooltipScope"), "{queued:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The Spec tab's Components group, after Build, opens a form for each
    /// component: Create writes it into the spec and opens the file, and Run
    /// Skill hands it to the chosen skill from the Spec tab instead.
    #[gpui_kit::test]
    async fn spec_components_are_created_or_handed_to_a_skill(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-spec-components-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "use @piton/config\n\nexport piton-config Project:\n    root: ./spec\n    entry: ./spec/index.pi\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("spec/index.pi"),
            "shape BaseShape:\n    description: base\n",
        )
        .unwrap();

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
        let ribbon = main.read_with(cx, |main, _| main.ribbon.clone());
        ribbon.update(cx, |r, cx| r.select_tab(crate::ribbon::RibbonTab::Spec, cx));
        cx.run_until_parked();

        // New Scope, full, then New Concept and New Shape stacked, after Build.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let build = window.find("Build").bounds();
            let group = window.find("Components").bounds();
            let analysis = window.find("Analysis").bounds();
            assert!(group.left() > build.right() && analysis.left() > group.right());
            let scope = window.find("new-scope").bounds();
            let concept = window.find("new-concept").bounds();
            let shape = window.find("new-shape").bounds();
            assert!(concept.left() > scope.right());
            assert_eq!(concept.left(), shape.left());
            assert!(shape.top() > concept.bottom());
            window.click("new-scope", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let form = main.read_with(cx, |main, _| main.spec_component.clone().expect("no form"));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("spec-component-panel");
            crate::double_borders::assert_none(window);
            // Nothing is written without a description.
            form.update(cx, |form, cx| {
                form.fill("Parser", "", window, cx);
                form.create(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        let written = dir.join("spec/scope/parser/index.pi");
        assert!(!written.exists());
        cx.update_window(handle, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.fill("Parser", "Reads {things}: all of them.", window, cx)
            });
            assert_eq!(form.read(cx).creating_problem(cx), None);
            window.render_frame(cx);
            window.click("spec-component-create", cx);
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx).spec_component.is_none()
        })
        .await;
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("export scope ParserScope:"), "{text}");
        assert!(
            text.contains("        \\\\\\\n        Reads {things}: all of them.\n        \\\\\\\n"),
            "{text}"
        );
        let open = main.read_with(cx, |main, cx| {
            main.prompt_mode
                .read(cx)
                .open_file_view()
                .map(|file| file.read(cx).path().to_path_buf())
        });
        assert_eq!(open, Some(written.clone()));

        // A shape, added to the end of a file.
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.open_spec_component(crate::spec_components::ComponentKind::Shape, window, cx)
            });
        })
        .unwrap();
        let form = main.read_with(cx, |main, _| main.spec_component.clone().expect("no form"));
        cx.update_window(handle, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.choose_file("index.pi", window, cx);
                form.fill("Base", "Taken.", window, cx);
                form.create(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| main.spec_component.is_some()),
            "a name already declared was created"
        );
        cx.update_window(handle, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.fill("Leaf", "A leaf.", window, cx);
                form.create(window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            main.read(cx).spec_component.is_none()
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(dir.join("spec/index.pi")).unwrap(),
            "shape BaseShape:\n    description: base\n\nshape LeafShape:\n    description:\n        A leaf.\n"
        );

        // A concept, handed to a skill.
        let prompt_mode = main.read_with(cx, |main, _| main.prompt_mode.clone());
        prompt_mode.update(cx, |prompt_mode, _| prompt_mode.set_working(true));
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.open_spec_component(crate::spec_components::ComponentKind::Concept, window, cx)
            });
        })
        .unwrap();
        let form = main.read_with(cx, |main, _| main.spec_component.clone().expect("no form"));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.choose_file("index.pi", window, cx);
                form.fill("Idea", "", window, cx);
                form.set_skills(vec!["build-scope".into()], window, cx);
            });
            assert_eq!(form.read(cx).running_problem(cx), None);
            window.render_frame(cx);
            let run = window.find("spec-component-run-skill").bounds();
            let panel = window.find("spec-component-panel").bounds();
            assert!(panel.contains(&run.center()), "{run:?} outside {panel:?}");
            window.click("spec-component-run-skill", cx);
        })
        .unwrap();
        // Read as soon as it is sent, before anything more runs: this bare
        // project can't compile the prompt's hidden anchor, so it leaves the
        // queue again once that is tried.
        assert!(main.read_with(cx, |main, _| main.spec_component.is_none()));
        let queued = prompt_mode.read_with(cx, |prompt_mode, _| prompt_mode.queued_texts());
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert!(
            queued[0].starts_with(
                "/build-scope Create a new concept named IdeaConcept.\n\nName: IdeaConcept\nFile: index.pi\nShape: None"
            ),
            "{queued:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// New Instruction, a slim button in Components, asks which folder or file
    /// of the code, names the instruction after it, and shows the file it
    /// will be, mirrored under the shape location, in the editor before it is
    /// on disk. Closing the panel with it unsaved asks first; saving writes
    /// it, and closing the editor then opens it in a tab.
    #[gpui_kit::test]
    async fn new_instruction_is_written_in_the_editor(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-instruction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec/shape")).unwrap();
        std::fs::create_dir_all(dir.join("src/ribbon")).unwrap();
        std::fs::write(dir.join("src/ribbon/spec_tab.rs"), "").unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "use @piton/config\nuse @piton/belay\n\nexport piton-config Project:\n    root: ./spec\n    entry: ./spec/index.pi\n\n    frameworks:\n        - {Belay}\n\nbelay-config Belay:\n    codeRoot: ./src\n    shapeRoot: ./spec/shape\n",
        )
        .unwrap();
        std::fs::write(dir.join("spec/index.pi"), "").unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            ProjectDirectory::set(dir.clone(), cx);
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
        ribbon.update(cx, |r, cx| r.select_tab(crate::ribbon::RibbonTab::Spec, cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let shape = window.find("new-shape").bounds();
            let instruction = window.find("new-instruction").bounds();
            assert!(instruction.size.height < shape.size.height + gpui_kit::px(1.));
            window.click("new-instruction", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let form = main.read_with(cx, |main, _| {
            main.new_instruction.clone().expect("no panel")
        });
        cx.update_window(handle, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.choose("src/ribbon/spec_tab.rs", window, cx)
            });
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            window.find("new-instruction-location");
            assert_eq!(
                form.read(cx).file(cx).as_deref(),
                Some("spec/shape/ribbon/SpecTab.pi")
            );
            window.click("new-instruction-continue", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let file = form.read_with(cx, |form, _| form.file_view().expect("not writing"));
        let written = dir.join("spec/shape/ribbon/SpecTab.pi");
        file.read_with(cx, |file, _| {
            assert_eq!(file.path(), written.as_path());
            assert!(file.is_dirty());
        });
        assert!(!written.exists(), "written before it was saved");

        // Closing the panel with it unsaved asks first, and keeps it.
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| main.close_panel(window, cx));
            window.render_frame(cx);
        })
        .unwrap();
        assert!(main.read_with(cx, |main, _| main.new_instruction.is_some()));
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            assert!(window.has_active_dialog(cx));
            window.close_dialog(cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("save-file", cx);
        })
        .unwrap();
        let start = std::time::Instant::now();
        while !written.exists() {
            assert!(start.elapsed() < TIMEOUT, "never saved");
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.run_until_parked();
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("export instruction SpecTab:"), "{text}");
        assert!(
            text.contains("Instructions for src/ribbon/spec_tab.rs"),
            "{text}"
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("close-file", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.new_instruction.is_none()));
        let open = main.read_with(cx, |main, cx| {
            main.prompt_mode
                .read(cx)
                .open_file_view()
                .map(|file| file.read(cx).path().to_path_buf())
        });
        assert_eq!(open, Some(written));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Theme, on the Application tab, opens the theme editor in the inset
    /// panel; a colour chosen shows in the mode's colour at once, and its
    /// Reset puts the theme's own back; the close button closes it.
    #[gpui_kit::test]
    async fn the_theme_editor_opens_in_the_inset_panel(cx: &mut TestAppContext) {
        use crate::chat_input::{SendMode, mode_color};
        use crate::ribbon::RibbonTab;
        use gpui_kit::component::ActiveTheme as _;
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
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
        ribbon.update(cx, |ribbon, cx| {
            ribbon.select_tab(RibbonTab::Application, cx)
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("theme", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.theme_editor.is_some()));
        let own = cx.update(|cx| mode_color(SendMode::Code, cx));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("theme-editor-panel").is_some());
            assert!(window.try_find("theme-base").is_some());
            assert!(window.try_find("theme-ask").is_some());
        })
        .unwrap();

        // Chosen, Code takes it everywhere, and its Reset puts it back.
        cx.update(|cx| {
            let dark = cx.theme().is_dark();
            let mut colors = crate::theme::custom_colors();
            colors.of_mut(dark).code = Some(0x00aa00);
            crate::theme::set_custom_colors(colors, cx);
            assert_eq!(
                mode_color(SendMode::Code, cx),
                crate::theme::color(0x00aa00)
            );
        });
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("theme-code-reset", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(crate::theme::custom_colors(), Default::default());
            assert_eq!(mode_color(SendMode::Code, cx), own);
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("theme-editor-close", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(main.read_with(cx, |main, _| main.theme_editor.is_none()));
    }

    /// Ctrl/Cmd+, opens the settings in the inset panel, in the main window
    /// rather than a window of their own; opening them again keeps the one
    /// panel, and the close button closes it.
    #[gpui_kit::test]
    async fn settings_open_in_the_inset_panel(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            crate::settings_window::bind_keys(cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        #[cfg(target_os = "macos")]
        let open = "cmd-,";
        #[cfg(not(target_os = "macos"))]
        let open = "ctrl-,";

        let windows = cx.update(|cx| cx.windows().len());
        cx.update_window(handle, |_, window, cx| window.press(open, cx))
            .unwrap();
        cx.run_until_parked();
        let first = main.read_with(cx, |main, _| main.settings.clone().expect("no settings"));
        assert_eq!(
            cx.update(|cx| cx.windows().len()),
            windows,
            "a window opened"
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let backdrop = window.find("settings-backdrop").bounds();
            let panel = window.find("settings-panel").bounds();
            assert_eq!(panel.left() - backdrop.left(), gpui_kit::px(32.));
            assert!(window.try_find("settings-close").is_some());
            window.press(open, cx);
        })
        .unwrap();
        cx.run_until_parked();
        main.read_with(cx, |main, _| {
            assert_eq!(main.settings.as_ref(), Some(&first), "opened another")
        });

        // It animates in: the panel starts below where it settles, and comes
        // to rest there.
        let surface_offset = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let place = window.find("settings-panel").bounds();
                let surface = window.find("settings-panel-surface").bounds();
                surface.top() - place.top()
            })
            .unwrap()
        };
        assert!(
            surface_offset(cx) > gpui_kit::px(0.),
            "the panel didn't start below its place"
        );
        let mut settled = false;
        for _ in 0..60 {
            if surface_offset(cx).abs() < gpui_kit::px(0.5) {
                settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(settled, "the panel never settled into place");

        cx.update_window(handle, |_, window, cx| window.click("settings-close", cx))
            .unwrap();
        cx.run_until_parked();
        main.read_with(cx, |main, _| assert!(main.settings.is_none()));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("settings-panel").is_none());
        })
        .unwrap();
        // It animates out, then is gone; while it goes, it doesn't block the
        // window beneath.
        assert!(main.read_with(cx, |main, _| main.panel_motion.is_leaving()));
        cx.update_window(handle, |_, window, cx| window.click("new-project", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| main.new_project.is_some()),
            "the closing panel took the click"
        );
        cx.update_window(handle, |_, window, cx| {
            window.click("new-project-close", cx)
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        std::thread::sleep(crate::animations::rise_in::LEAVE_TIME + Duration::from_millis(50));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
        })
        .unwrap();
        assert!(main.read_with(cx, |main, _| !main.panel_motion.is_leaving()));
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
            // Templates and agents sit in a second column, beside the rest.
            let main = window.find("new-project-main-column").bounds();
            let side = window.find("new-project-side-column").bounds();
            let templates = window.find("new-project-templates").bounds();
            let agents = window.find("new-project-agents").bounds();
            assert!(
                side.left() >= main.right(),
                "{side:?} isn't right of {main:?}"
            );
            assert!((side.top() - main.top()).abs() < gpui_kit::px(1.));
            for group in [templates, agents] {
                assert!(
                    group.left() >= side.left()
                        && group.right() <= side.right() + gpui_kit::px(0.5)
                );
            }
            assert!(agents.top() > templates.bottom());
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
            for ix in 0..crate::new_project::Agent::ALL.len() {
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
            assert_eq!(browser.read(cx).choice(), Some(base.join("projects/apps")));
            window.click("folder-choose", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let settings = form.read_with(cx, |form, cx| {
            assert!(form.browser().is_none(), "choosing left the browser open");
            form.settings(cx)
        });
        assert_eq!(settings.folder(), base.join("projects/apps/demo"));
        assert_eq!(settings.template, "base", "the first template isn't chosen");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // The second template: scope, concept, and shape.
            window.click(("new-project-template", 1usize), cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
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
        assert!(
            base.join("projects/apps/demo/spec/lib/Scope.pi").exists(),
            "the chosen template's files weren't written"
        );
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
            // It sits in the right container, at the far right of the bar.
            let (spinner, right, row) = (
                window.find("ribbon-activity").bounds(),
                window.find("ribbon-right").bounds(),
                window.find("ribbon-tabs-row").bounds(),
            );
            assert!(
                (right.right() - row.right()).abs() < gpui_kit::px(1.)
                    && (spinner.right() - right.right()).abs() < gpui_kit::px(1.),
                "the spinner {spinner:?} isn't at the far right of {row:?}"
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
                main.reveal_job(JobKind::Question(1), None, window, cx)
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
            prompt_mode.update(cx, |prompt_mode, cx| prompt_mode.stop_test_question(1, cx));
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
            crate::double_borders::assert_none(window);
            assert!(
                window.try_find("ribbon-activity").is_none(),
                "the spinner stayed"
            );
        })
        .unwrap();
    }

    /// Esc in an open file leaves the keyboard there; Ctrl/Cmd+Enter moves it
    /// to the chat input, where typing then lands, and sends nothing.
    #[gpui_kit::test]
    async fn ctrl_enter_in_a_file_focuses_the_chat_input(cx: &mut TestAppContext) {
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
        cx.update_window(handle, |_, window, cx| {
            assert!(
                !chat.read(cx).is_focused(window, cx),
                "Esc focused the chat"
            );
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-enter"
                } else {
                    "ctrl-enter"
                },
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| window.input("hi", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(chat.read(cx).value(cx).as_ref(), "hi"));
        // The file got no new line from it.
        cx.update(|cx| assert_eq!(file.read(cx).text(cx).as_ref(), "# Notes\n"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Ctrl/Cmd+Enter focuses the chat input from inside the tree's name
    /// field, whose own Enter would otherwise take it, and while nothing has
    /// focus at all.
    #[gpui_kit::test]
    async fn ctrl_enter_focuses_the_chat_input_from_anywhere(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-focus-chat-{}", std::process::id()));
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
        let chat = main.read_with(cx, |main, cx| main.prompt_mode.read(cx).chat_input_view());
        let tree = main.read_with(cx, |main, _| main.sidebar.clone());
        let send = if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        };

        // In the name field of a new file in the tree.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.start_naming(
                    crate::project_tree::Naming::NewFile(dir.clone()),
                    window,
                    cx,
                )
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(!chat.read(cx).has_keyboard(window, cx));
            window.input("new.md", cx);
            window.press(send, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(
                chat.read(cx).has_keyboard(window, cx),
                "Ctrl+Enter in the tree's name field left the chat unfocused"
            );
        })
        .unwrap();
        assert!(!dir.join("new.md").exists(), "Ctrl+Enter named the file");

        // With nothing focused.
        cx.update_window(handle, |_, window, cx| {
            window.blur(cx);
            assert!(!chat.read(cx).has_keyboard(window, cx));
            window.press(send, cx);
            assert!(
                chat.read(cx).has_keyboard(window, cx),
                "Ctrl+Enter with nothing focused left the chat unfocused"
            );
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
    /// The ribbon's tabs are Project, Code, Spec, and Application,
    /// each showing only its own controls: opening a project under Project,
    /// Build Spec under Spec, dark mode and settings under Application, and
    /// none of those under Code.
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
        let controls = ["project-directory", "build", "brightness", "settings"];

        for (tab, shown) in [
            (RibbonTab::Project, Some("project-directory")),
            (RibbonTab::Code, None),
            (RibbonTab::Spec, Some("build")),
            (RibbonTab::Application, Some("brightness")),
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
                // Brightness is the Appearance group's one column.
                if tab == RibbonTab::Application {
                    let appearance = window.find("Appearance").bounds();
                    let brightness = window.find("brightness").bounds();
                    assert_eq!(
                        (brightness.left(), brightness.right()),
                        (appearance.left(), appearance.right()),
                        "Appearance holds more than one column"
                    );
                    assert!(window.try_find("dark-mode").is_none());
                }
                // Analysis follows Build: Analyze Divergence, then View
                // Divergence Reports.
                if tab == RibbonTab::Spec {
                    let build = window.find("build").bounds();
                    let analyze = window.find("analyze-divergence").bounds();
                    let view = window.find("view-divergence-reports").bounds();
                    assert!(build.right() <= analyze.left() && analyze.right() <= view.left());
                    // Then Skills: Generate Skills.
                    let skills = window.find("generate-skills").bounds();
                    assert!(view.right() <= skills.left());
                    // Then Refactor: Rescope.
                    let rescope = window.find("rescope").bounds();
                    assert!(skills.right() <= rescope.left());
                }
            })
            .unwrap();
        }

        // Ctrl+click opens tabs alongside each other: their groups sit side by
        // side in the order of the tabs, a divider between one tab's and the
        // next's as between any groups; a plain click opens one alone again.
        ribbon.update(cx, |ribbon, cx| {
            ribbon.tab_clicked(RibbonTab::Application, 1, false, cx);
            ribbon.tab_clicked(RibbonTab::Project, 1, true, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            ribbon.read_with(cx, |ribbon, _| ribbon.open_tabs().to_vec()),
            [RibbonTab::Project, RibbonTab::Application]
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for control in controls.into_iter().filter(|control| *control != "build") {
                assert!(window.try_find(control).is_some(), "{control} isn't shown");
            }
            assert!(window.try_find("build").is_none());
            // Project's groups come before Application's, a divider between
            // them 4px from each.
            let project = window.find("Project").bounds();
            let appearance = window.find("Appearance").bounds();
            let divider = window.find(("ribbon-divider", 1usize)).bounds();
            assert_eq!(
                (
                    divider.left() - project.right(),
                    appearance.left() - divider.right()
                ),
                (gpui_kit::px(4.), gpui_kit::px(4.)),
                "the divider {divider:?} isn't 4px from Project's group {project:?} and Application's {appearance:?}"
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

        // Groups sit side by side, a divider between them 4px from each, and
        // none before the first.
        cx.update_window(handle, |_, window, _| {
            let appearance = window.find("Appearance").bounds();
            let colours = window.find("Colours").bounds();
            let preferences = window.find("Preferences").bounds();
            let divider = window.find(("ribbon-divider", 1usize)).bounds();
            assert_eq!(
                (
                    divider.left() - appearance.right(),
                    colours.left() - divider.right()
                ),
                (gpui_kit::px(4.), gpui_kit::px(4.)),
                "the divider {divider:?} isn't 4px from {appearance:?} and {colours:?}"
            );
            let divider = window.find(("ribbon-divider", 2usize)).bounds();
            assert_eq!(
                (
                    divider.left() - colours.right(),
                    preferences.left() - divider.right()
                ),
                (gpui_kit::px(4.), gpui_kit::px(4.)),
                "the divider {divider:?} isn't 4px from {colours:?} and {preferences:?}"
            );
            assert!(window.try_find(("ribbon-divider", 0usize)).is_none());
        })
        .unwrap();

        // The tab row's height, which the collapsed row keeps; the left
        // container runs its full height.
        let expanded_row = cx
            .update_window(handle, |_, window, _| {
                let left = window.find("ribbon-left").bounds();
                let row = window.find("ribbon-tabs-row").bounds();
                assert_eq!(
                    (left.top(), left.bottom()),
                    (row.top(), row.bottom()),
                    "the left container isn't the tab row's height"
                );
                (left.top(), row.bottom(), left.size.height)
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
            // As tall as the tab row was, its bottom line where the tab row's
            // was, and the left container as tall.
            let (top, line_bottom, left_height) = expanded_row;
            let row = window.find("ribbon-primary").bounds();
            let left = window.find("ribbon-left").bounds();
            assert_eq!(row.top(), top, "the collapsed row moved");
            assert_eq!(
                row.bottom(),
                line_bottom,
                "the collapsed row {row:?} isn't the tab row's height"
            );
            // The indicator, and the rest of the left container, keep their
            // height and place, ending on the line in both.
            assert_eq!(
                left.size.height, left_height,
                "the left container changed height"
            );
            assert_eq!(left.top(), top, "the left container moved");
            let prefix = window.find("ribbon-prefix").bounds();
            assert_eq!(
                prefix.size.height, left_height,
                "the indicator changed height"
            );
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
            // Every control is vertically centred in the row.
            let row = window.find("ribbon-primary").bounds();
            let row_middle = row.origin.y + row.size.height / 2.;
            for control in controls {
                let bounds = window.find(control).bounds();
                let middle = bounds.origin.y + bounds.size.height / 2.;
                assert!(
                    (middle - row_middle).abs() <= gpui_kit::px(1.),
                    "{control} is not centred: {bounds:?} in {row:?}"
                );
            }
            // Each command is only as wide as its icon and label, in the
            // order of the tabs, rather than the first filling the row.
            let small = [
                "new-project",
                "project-directory",
                "build",
                "brightness",
                "settings",
            ];
            for pair in small.windows(2) {
                let (a, b) = (window.find(pair[0]).bounds(), window.find(pair[1]).bounds());
                assert!(
                    b.left() - a.right() >= gpui_kit::px(8.),
                    "{pair:?} overlap or touch"
                );
            }
            // Brightness is as wide as its icons and 120px track.
            for control in small.into_iter().filter(|&c| c != "brightness") {
                let bounds = window.find(control).bounds();
                assert!(
                    bounds.size.width < gpui_kit::px(160.),
                    "{control} stretches: {bounds:?} in {row:?}"
                );
            }
            // Small buttons stay 22px tall, whatever the tabs make them.
            for control in ["new-project", "project-directory", "build", "settings"] {
                assert_eq!(
                    window.find(control).bounds().size.height,
                    gpui_kit::px(22.),
                    "{control} isn't small"
                );
            }
        })
        .unwrap();

        // Too narrow for its commands, the row scrolls them sideways: the
        // project indicator stays whole and in the window, left of the
        // commands.
        let wide = cx
            .update_window(handle, |_, window, _| window.bounds().size)
            .unwrap();
        cx.simulate_window_resize(handle, gpui_kit::size(gpui_kit::px(360.), wide.height));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let row = window.find("ribbon-primary").bounds();
            let commands = window.find("ribbon-primary-commands").bounds();
            let left = window.find("ribbon-left").bounds();
            let name = window.find("ribbon-project-name").bounds();
            let settings = window.find("settings").bounds();
            assert!(
                row.right() <= gpui_kit::px(360.),
                "the row overflows: {row:?}"
            );
            assert!(name.left() >= row.left() && name.right() <= left.right());
            assert!(left.right() <= commands.left());
            assert!(
                settings.right() > commands.right(),
                "the commands squeeze to fit rather than scroll: {settings:?} in {commands:?}"
            );
            assert!(window.find("new-project").bounds().size.width < gpui_kit::px(160.));
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "ribbon-primary-commands",
                gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                    gpui_kit::px(-2000.),
                    gpui_kit::px(0.),
                )),
                cx,
            )
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let commands = window.find("ribbon-primary-commands").bounds();
            let settings = window.find("settings").bounds();
            assert!(
                settings.right() <= commands.right(),
                "scrolling doesn't reach the last command: {settings:?} in {commands:?}"
            );
        })
        .unwrap();
        cx.simulate_window_resize(handle, wide);
        cx.run_until_parked();

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

    /// The git panel shrinks to fit its contents beneath the file tree. With
    /// more than fits, it grows to half the sidebar, its body scrolling
    /// beneath its header; once the edge above it is dragged, it grows up to
    /// where it was dragged instead, but never past what its contents need.
    #[gpui_kit::test]
    async fn the_git_panel_fits_its_contents(cx: &mut TestAppContext) {
        use gpui_kit::{point, px};
        let dir = std::env::temp_dir().join(format!("suspense-git-resize-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(dir.join("piton.config.pi"), "root: ./spec\n").unwrap();
        std::fs::write(dir.join("spec/index.pi"), "a: 1\n").unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("spec/index.pi"), "a: 2\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find("git-header").is_some()
        })
        .await;
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(px(1000.), px(800.)));
        let frame = |cx: &mut TestAppContext| {
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
        };
        let bounds = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, _| window.find(id).bounds())
                .unwrap()
        };
        let notes = |count: usize, cx: &mut TestAppContext| {
            for ix in 0..count {
                crate::commit_notes::add(&dir, &format!("Note {ix}")).unwrap();
            }
            cx.update(crate::commit_notes::changed);
            frame(cx);
        };
        let fits = |cx: &mut TestAppContext| {
            let (panel, body) = (bounds("git-panel", cx), bounds("git-body", cx));
            (panel.size.height - crate::git_panel::HEADER_HEIGHT - body.size.height).abs() < px(1.)
        };
        frame(cx);
        let sidebar = bounds("project-sidebar", cx);
        let half = sidebar.size.height * crate::git_panel::MAX_SHARE;

        // With little in it, it is only as tall as that, along the bottom.
        let start = bounds("git-panel", cx);
        assert!(fits(cx), "the git panel {start:?} doesn't fit its contents");
        assert!(start.size.height < half, "the git panel starts {start:?}");
        assert_eq!(start.bottom(), sidebar.bottom());

        // Dragged up, it grows no taller than its contents need.
        cx.update_window(handle, |_, window, cx| {
            let edge = point(start.center().x, start.top());
            window.drag(edge, edge - point(px(0.), px(100.)), cx);
        })
        .unwrap();
        frame(cx);
        assert_eq!(
            bounds("git-panel", cx),
            start,
            "dragged up past its contents"
        );

        // With more in it than fits there, it grows as far as it was dragged,
        // only its notes scrolling: the message and buttons stay fixed along
        // its bottom.
        notes(40, cx);
        let dragged = bounds("git-panel", cx);
        assert!(
            (dragged.size.height - start.size.height - px(100.)).abs() < px(2.),
            "with more notes than fit, the git panel is {dragged:?}"
        );
        let first_note = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, _| {
                (0..200usize)
                    .filter_map(|id| window.try_find(("commit-note", id)))
                    .map(|note| note.bounds())
                    .min_by(|a, b| a.top().partial_cmp(&b.top()).unwrap())
                    .unwrap()
            })
            .unwrap()
        };
        let (footer, note) = (bounds("git-footer", cx), first_note(cx));
        assert!(
            footer.bottom() <= dragged.bottom() && footer.top() >= dragged.top(),
            "the message and buttons {footer:?} aren't within the panel {dragged:?}"
        );
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "git-scroll",
                gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                cx,
            );
        })
        .unwrap();
        frame(cx);
        assert!(
            first_note(cx).top() < note.top(),
            "the notes didn't scroll: {note:?} then {:?}",
            first_note(cx)
        );
        assert_eq!(
            bounds("git-footer", cx),
            footer,
            "the message and buttons moved as the notes scrolled"
        );
        let summary = bounds("git-summary", cx);
        assert!(summary.top() >= dragged.top(), "the summary scrolled away");

        // Dragged past half the sidebar, it grows past half too.
        cx.update_window(handle, |_, window, cx| {
            let edge = point(dragged.center().x, dragged.top());
            window.drag(edge, point(edge.x, sidebar.top() + half - px(50.)), cx);
        })
        .unwrap();
        frame(cx);
        let tall = bounds("git-panel", cx);
        assert!(tall.size.height > half, "dragged past half, it is {tall:?}");

        // Once its notes are gone, it shrinks to fit again.
        std::fs::remove_file(crate::commit_notes::file(&dir)).unwrap();
        cx.update(crate::commit_notes::changed);
        frame(cx);
        assert_eq!(bounds("git-panel", cx), start, "without its notes");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Until the edge above it is dragged, the git panel grows to half the
    /// sidebar at most.
    #[gpui_kit::test]
    async fn the_git_panel_grows_to_half_the_sidebar(cx: &mut TestAppContext) {
        use gpui_kit::px;
        let dir = std::env::temp_dir().join(format!("suspense-git-half-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(dir.join("piton.config.pi"), "root: ./spec\n").unwrap();
        std::fs::write(dir.join("spec/index.pi"), "a: 1\n").unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("spec/index.pi"), "a: 2\n").unwrap();
        for ix in 0..40 {
            crate::commit_notes::add(&dir, &format!("Note {ix}")).unwrap();
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find("git-header").is_some()
        })
        .await;
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(px(1000.), px(800.)));
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
        let (sidebar, panel, scroll, body) = cx
            .update_window(handle, |_, window, _| {
                (
                    window.find("project-sidebar").bounds(),
                    window.find("git-panel").bounds(),
                    window.find("git-scroll").bounds(),
                    window.find("git-body").bounds(),
                )
            })
            .unwrap();
        let half = sidebar.size.height * crate::git_panel::MAX_SHARE;
        assert!(
            (panel.size.height - half).abs() < px(1.),
            "the git panel is {panel:?} in {sidebar:?}"
        );
        assert_eq!(panel.bottom(), sidebar.bottom());
        assert!(
            body.size.height > scroll.size.height,
            "its body {body:?} fits in {scroll:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// In a git repository, the git panel sits beneath the file tree with no
    /// line between them: the tree on the darkest surface runs straight into
    /// the panel's header on the ribbon's tab row colour, whose change of
    /// colour is the edge, in dark and light mode alike.
    #[gpui_kit::test]
    async fn no_line_lies_between_the_tree_and_the_git_panel(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        let dir =
            std::env::temp_dir().join(format!("suspense-sidebar-edge-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for folder in [".claude", "assets", "spec/lib", "src", "targets"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for (file, text) in [
            ("piton.config.pi", "root: ./spec\ncodeRoot: ./src\n"),
            (".gitignore", "targets/\n"),
            ("spec/index.pi", "a: 1\n"),
            ("spec/lib/index.pi", "b: 2\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("targets/out", "\n"),
        ] {
            std::fs::write(dir.join(file), text).unwrap();
        }
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("src/main.rs"), "fn main() { go() }\n").unwrap();
        std::fs::write(dir.join("assets/new.txt"), "new\n").unwrap();
        crate::commit_notes::add(
            &dir,
            "Rewrite Editor spec with marquee selection and push/pull",
        )
        .unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find("git-header").is_some()
                && window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(1000.), gpui_kit::px(974.)));
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                crate::double_borders::assert_none(window);
                let palette = crate::theme::palette(cx);
                let tree = window.find("project-tree").bounds();
                let header = window.find("git-header").bounds();
                assert!(
                    (header.top() - tree.bottom()).abs() < gpui_kit::px(0.5),
                    "{mode:?}: something lies between the tree, ending at {:?}, and the \
                     header, starting at {:?}",
                    tree.bottom(),
                    header.top()
                );
                let frame = crate::frame_image::Frame::of(window);
                // Straight down across the edge, clear of the scroll column:
                // the tree's colour, then the header's, and nothing between.
                let x = tree.left() + gpui_kit::px(100.);
                let edge = header.top().as_f32().round() as i32;
                for dy in -3..3 {
                    let y = gpui_kit::px((edge + dy) as f32 + 0.5);
                    let expected = if dy < 0 {
                        palette.darkest
                    } else {
                        palette.ribbon_tabs
                    };
                    assert_eq!(
                        frame.at(gpui_kit::point(x, y)),
                        expected,
                        "{mode:?}: {dy}px from the edge between the tree and the git panel"
                    );
                }
            })
            .unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
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
            crate::double_borders::assert_none(window);
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

    /// Switching projects stops a rescope search and closes its panel; the
    /// project indicator marks the busy project left, and quitting still asks
    /// while only it is working. Its task is listed with its name, and
    /// revealing it switches back to it; a build in it leaves Build free in
    /// the project on screen.
    #[gpui_kit::test]
    async fn switching_projects_keeps_the_one_left_running(cx: &mut TestAppContext) {
        let base = std::env::temp_dir().join(format!("suspense-switching-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(base.join(name).join("spec")).unwrap();
            std::fs::write(base.join(name).join("piton.config.pi"), "").unwrap();
        }
        let (a, b) = (
            std::fs::canonicalize(base.join("alpha")).unwrap(),
            std::fs::canonicalize(base.join("beta")).unwrap(),
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            super::bind_keys(cx);
            crate::project_lsp::ProjectLsp::init(cx);
            crate::file_view::bind_keys(cx);
            ProjectDirectory::set(a.clone(), cx);
        });
        let mut main = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            main = Some(view.clone());
            Root::new(view, window, cx)
        });
        let main = main.unwrap();
        let handle = window.into();
        cx.run_until_parked();
        let (prompt_mode, ribbon) = main.read_with(cx, |main, _| {
            (main.prompt_mode.clone(), main.ribbon.clone())
        });
        prompt_mode.update(cx, |prompt_mode, cx| {
            prompt_mode.start_test_question("Still thinking in alpha", cx);
        });
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                let view = cx.new(|cx| {
                    crate::rescope_view::RescopeView::with_agent(
                        a.clone(),
                        |_, _, _, _| Ok(r#"{"concepts": []}"#.into()),
                        cx,
                    )
                });
                main.show_rescope(view, window, cx)
            });
        })
        .unwrap();
        assert!(main.read_with(cx, |main, _| main.rescope.is_some()));

        // The rescope search stays with alpha, out of sight in beta.
        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        assert!(
            main.read_with(cx, |main, _| main.rescope.is_none()
                && !main.panel_open()
                && main
                    .parked
                    .get(&a)
                    .is_some_and(|parked| parked.rescope.is_some())),
            "the rescope search wasn't kept for alpha"
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("project-busy-elsewhere").is_some());
            // Alpha is listed, busy, though it was never among the recent
            // projects kept.
            let indicator = ribbon.read(cx).project_indicator().clone();
            indicator.update(cx, |indicator, cx| indicator.open(window, cx));
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(window.try_find(("recent-project-busy", 0usize)).is_some());
            indicator.update(cx, |indicator, cx| indicator.close(cx));
        })
        .unwrap();
        let jobs = ribbon.read_with(cx, |ribbon, _| ribbon.jobs().to_vec());
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        assert_eq!(jobs[0].title.as_ref(), "Question · alpha");
        assert_eq!(jobs[0].project.as_ref(), Some(&a));

        // A build in alpha doesn't hold up Build in beta.
        ribbon.update(cx, |ribbon, cx| ribbon.set_building_in(a.clone(), cx));
        assert!(ribbon.read_with(cx, |ribbon, cx| ribbon.can_build(cx)));

        // Quitting asks, though nothing runs in beta.
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_action(Box::new(crate::app::Quit), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("cancel").is_some()
        })
        .await;
        // Once the dialog has risen into place, so the button stays under
        // the pointer from press to release, however slow the machine.
        cx.update_window(handle, |_, window, cx| {
            std::thread::sleep(Duration::from_millis(400));
            window.render_frame(cx);
            window.click("cancel", cx)
        })
        .unwrap();
        cx.run_until_parked();

        // Clicking alpha in the project list switches to it, and the chat
        // input takes the keyboard from the list's filter.
        let chat = prompt_mode.read_with(cx, |prompt_mode, _| prompt_mode.chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            let indicator = ribbon.read(cx).project_indicator().clone();
            indicator.update(cx, |indicator, cx| indicator.open(window, cx));
            window.render_frame(cx);
            assert!(!chat.read(cx).is_focused(window, cx));
            // Once the list has risen into place, so the row stays under
            // the pointer from press to release, however slow the machine.
            std::thread::sleep(Duration::from_millis(400));
            window.render_frame(cx);
            window.click(("recent-project", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| ProjectDirectory::get(cx)), Some(a.clone()));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                chat.read(cx).is_focused(window, cx),
                "the chat input isn't focused"
            );
        })
        .unwrap();
        // Switched some other way, with focus in the editor, the chat input
        // takes it all the same.
        std::fs::write(a.join("notes.md"), "notes").unwrap();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |prompt_mode, cx| {
                prompt_mode.open_file(a.join("notes.md"), window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            let file = prompt_mode.read(cx).open_file_view().unwrap();
            file.update(cx, |file, cx| file.focus_editor(window, cx));
            window.render_frame(cx);
            assert!(!chat.read(cx).is_focused(window, cx));
        })
        .unwrap();
        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                chat.read(cx).is_focused(window, cx),
                "the editor kept focus"
            );
        })
        .unwrap();

        // Revealing alpha's question switches back to alpha.
        cx.update_window(handle, |_, window, cx| {
            main.update(cx, |main, cx| {
                main.reveal_job(jobs[0].kind, jobs[0].project.clone(), window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| ProjectDirectory::get(cx)), Some(a.clone()));
        prompt_mode.read_with(cx, |prompt_mode, _| {
            assert_eq!(prompt_mode.running_jobs().len(), 1);
            assert!(prompt_mode.running_jobs()[0].project.is_none());
        });
        std::fs::remove_dir_all(&base).ok();
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
        // Nothing matches, and it says Enter searches with the harness.
        palette.read_with(cx, |palette, _| {
            assert!(palette.result_labels().is_empty());
            assert_eq!(
                palette.empty_text(),
                "No results. Press Enter to perform an agentic search."
            );
        });
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

    /// In the window as the app lays it out, in the sidebar above the git
    /// panel, at each mode's brightness, the file tree's scroll column is laid
    /// straight over the tree's darkest surface: the track is that surface, a
    /// pixel in from either side; the column's line down both sides, the
    /// buttons, and the thumb at rest are all the same colour laid over it;
    /// the thumb's end is half way between the two; and each
    /// arrow, solid and faint, is laid over its button. The tree has more rows
    /// than fit, so the thumb and the track both show.
    #[gpui_kit::test]
    async fn the_trees_scroll_column_is_on_the_trees_surface_in_the_window(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
        use gpui_kit::{Bounds, Pixels, point, px};
        let dir = std::env::temp_dir().join(format!("suspense-tree-column-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(dir.join("piton.config.pi"), "root: ./spec\n").unwrap();
        for ix in 0..80 {
            std::fs::write(dir.join(format!("file-{ix:02}.txt")), "\n").unwrap();
        }
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| MainWindow::new(window, cx));
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find("git-header").is_some()
                && window.try_find(("project-entry", 0usize)).is_some()
                && window.try_find("project-tree-scroll-column").is_some()
        })
        .await;
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(px(1000.), px(700.)));

        // `color` laid over `surface`, as 0xRRGGBB.
        let over = |surface: u32, color: gpui_kit::Hsla| {
            let c: gpui_kit::Rgba = color.into();
            [(16, c.r), (8, c.g), (0, c.b)]
                .into_iter()
                .fold(0u32, |rgb, (shift, channel)| {
                    let s = ((surface >> shift) & 0xff) as f32 / 255.;
                    let v = s * (1. - c.a) + channel * c.a;
                    rgb | ((v * 255.).round() as u32) << shift
                })
        };
        // The same, to a step in each channel.
        let same = |a: u32, b: u32| {
            (0..3).all(|ix| {
                let channel = |c: u32| ((c >> (ix * 8)) & 0xff) as i32;
                (channel(a) - channel(b)).abs() <= 1
            })
        };

        for (mode, brightness) in [
            (ThemeMode::Dark, 0),
            (ThemeMode::Dark, 1),
            (ThemeMode::Dark, -2),
            (ThemeMode::Light, 0),
        ] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                crate::theme::set_brightness(mode == ThemeMode::Dark, brightness, cx);
            });
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let case = format!("{mode:?} at {brightness}");
                let surface = crate::theme::palette(cx).darkest;
                let colors = crate::scrollbar::scroll_colors(cx.theme().is_dark());
                let raised = over(surface, colors.raised);
                let (end, arrow) = (over(surface, colors.thumb_end), over(raised, colors.arrow));
                let frame = crate::frame_image::Frame::of(window);
                let tree = window.find("project-tree").bounds();
                let column = window.find("project-tree-scroll-column").bounds();
                let track = window.find("project-tree-scroll-track").bounds();
                let up = window.find("project-tree-scroll-up").bounds();
                let down = window.find("project-tree-scroll-down").bounds();
                let at = |x: f32, y: f32, from: Bounds<Pixels>| {
                    frame.at(point(from.left() + px(x + 0.5), from.top() + px(y + 0.5)))
                };
                assert_eq!(
                    column.right(),
                    tree.right(),
                    "{case}: the column isn't at the tree's right"
                );
                let height = track.size.height.as_f32() as i32;

                // The thumb, at the top with the tree unscrolled, and the
                // track below it, read down the column's middle: the thumb's
                // inside is raised, with a row at either end.
                let inside: Vec<i32> = (0..height)
                    .filter(|y| same(at(8., *y as f32, track), raised))
                    .collect();
                let (first, last) = (inside[0] - 1, *inside.last().unwrap() + 1);
                assert_eq!(first, 0, "{case}: the thumb isn't at the top");
                assert!(
                    last > 10 && last < height - 10,
                    "{case}: the thumb, rows {first} to {last} of {height}, leaves no track"
                );

                // The column's line down both sides, from the top of the up
                // button to the bottom of the down button, and the tree's
                // surface beside it.
                let mut y = 0.;
                while y < column.size.height.as_f32() {
                    let c = at(-4., y, column);
                    assert!(
                        same(c, surface),
                        "{case}: {c:06x} beside the column at {y}, not the tree's {surface:06x}"
                    );
                    for x in [0., 17.] {
                        let c = at(x, y, column);
                        assert!(
                            same(c, raised),
                            "{case}: {c:06x} at ({x}, {y}) of the column, not {raised:06x}"
                        );
                    }
                    y += 1.;
                }
                // The track, inset a pixel either side.
                for y in [last + 1, (last + height) / 2, height - 1] {
                    for x in [1., 8., 16.] {
                        let c = at(x, y as f32, track);
                        assert!(
                            same(c, surface),
                            "{case}: the track is {c:06x} at ({x}, {y}), not {surface:06x}"
                        );
                    }
                }
                // The thumb: raised, its full width, and its bottom end half
                // way between it and the surface.
                for x in [1., 8., 16.] {
                    let c = at(x, (last / 2) as f32, track);
                    assert!(same(c, raised), "{case}: the thumb is {c:06x} at {x}");
                    let c = at(x, last as f32, track);
                    assert!(same(c, end), "{case}: the thumb's end is {c:06x} at {x}");
                }
                // Each button: raised, and a faint solid arrow on it.
                for button in [up, down] {
                    let mut lit = 0;
                    for y in 0..17 {
                        for x in 0..18 {
                            let c = at(x as f32, y as f32, button);
                            assert!(
                                same(c, raised) || same(c, arrow),
                                "{case}: {c:06x} at ({x}, {y}) in a button"
                            );
                            lit += same(c, arrow) as usize;
                        }
                    }
                    assert_eq!(lit, 18, "{case}: the arrow isn't 6, 6, 4 and 2 pixels");
                }
            })
            .unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
