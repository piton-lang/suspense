//! The settings, shown in the main window's inset panel, in sections picked
//! from a sidebar of vertical tabs: the system prompt each tab gives a prompt,
//! the spec-reading prompt injected into them, and the code-to-spec prompt
//! added to a Code task sent to Spec, for the open project, and
//! the harness every run goes to, for the user (see [`crate::agent`]), and
//! the containers runs go in: whether Podman can run them, and whether each
//! harness is logged in in its container (see [`crate::container`]). Every
//! edit is saved straight away to the project's `.suspense/system-prompts`
//! (see [`crate::system_prompts`]), and the prompts are read from there each
//! time the settings open, so an edit made by hand shows up.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::{self, Agent};
use crate::growing_input::GrowToFit;
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::chat_input::{self, SendMode};
use crate::sandbox::{Classification, ProfileKind, Refused};
use crate::system_prompts::{
    self, CODE_LOCATION, CODE_PROMPT, CODE_RESULT, HARNESS_DIRECTORY, PITON_FLUENCY_FILE, Prompt,
    SUSPENSE_FLUENCY_FILE,
    SPEC_LOCATION, SPEC_PROMPT, SPEC_READING, SPEC_RESULT,
};

actions!(suspense, [OpenSettings]);

/// Emitted when the settings are closed.
pub struct CloseSettings;

/// How wide the sidebar of sections is.
const SIDEBAR_WIDTH: Pixels = px(180.);

/// A section of the settings, picked from the sidebar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Section {
    #[default]
    SystemPrompts,
    InjectedPrompts,
    Agent,
    Sandbox,
    Updates,
}

impl Section {
    const ALL: [Section; 5] = [
        Section::SystemPrompts,
        Section::InjectedPrompts,
        Section::Agent,
        Section::Sandbox,
        Section::Updates,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::SystemPrompts => "System prompts",
            Section::InjectedPrompts => "Injected prompts",
            Section::Agent => "Agent",
            Section::Sandbox => "Sandbox",
            Section::Updates => "Updates",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Section::SystemPrompts => "system-prompts",
            Section::InjectedPrompts => "injected-prompts",
            Section::Agent => "agent",
            Section::Sandbox => "sandbox",
            Section::Updates => "updates",
        }
    }

    /// The prompts it edits.
    fn holds(self, prompt: Prompt) -> bool {
        matches!(
            (self, prompt),
            (Section::SystemPrompts, Prompt::System | Prompt::Mode(_))
                | (
                    Section::InjectedPrompts,
                    Prompt::SpecReading | Prompt::CodeToSpec | Prompt::SpecToCode | Prompt::Resume
                )
        )
    }
}

/// The running version, and where it can update itself, whether it checks
/// automatically, a check on demand, and where things stand.
fn render_updates(cx: &mut App) -> impl IntoElement {
    use crate::self_update::{Status, Updates, cant_update};
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let version = div()
        .font_medium()
        .child(format!("Suspense {}", crate::version::VERSION));
    let details = render_update_details(cx);
    let section = v_flex().gap_3().child(version);
    if let Some(why) = cant_update() {
        return section
            .child(div().text_color(muted).child(why))
            .child(details);
    }
    let Some(updates) = Updates::get(cx) else {
        return section.child(details);
    };
    let busy = Updates::busy(cx);
    let restart = |id: &'static str| {
        Button::new(id)
            .small()
            .primary()
            .label("Restart")
            .on_click(|_, _, cx| Updates::restart(cx))
    };
    let status: AnyElement = match &updates.status {
        Status::Idle => div().into_any_element(),
        Status::Checking => div().text_color(muted).child("Checking for updates…").into_any_element(),
        Status::UpToDate => {
            let checked = updates
                .last_checked
                .map(|then| {
                    format!(
                        " Checked {}.",
                        crate::divergence::ago(then, crate::self_update::now())
                    )
                })
                .unwrap_or_default();
            div()
                .child(format!("Suspense is up to date.{checked}"))
                .into_any_element()
        }
        Status::Downloading { version, got, total } => {
            let percent = (*got * 100).checked_div(*total).unwrap_or(0).min(100);
            div()
                .text_color(muted)
                .child(format!(
                    "Downloading {version}… {percent}% ({:.1} of {:.1} MB)",
                    *got as f64 / 1_000_000.,
                    *total as f64 / 1_000_000.
                ))
                .into_any_element()
        }
        Status::Ready { version } => h_flex()
            .gap_2()
            .child(format!("Suspense {version} is ready"))
            .child(restart("settings-update-restart"))
            .into_any_element(),
        Status::NotWritable { dir, page } => {
            let page = page.clone();
            v_flex()
                .gap_1()
                .child(div().text_color(danger).child(format!(
                    "Suspense can't update itself in {}, as it can't write there.",
                    dir.display()
                )))
                .child(
                    Button::new("settings-update-page")
                        .small()
                        .label("Open the release's page")
                        .on_click(move |_, _, cx| cx.open_url(&page)),
                )
                .into_any_element()
        }
        Status::Failed(why) => div().text_color(danger).child(why.clone()).into_any_element(),
        Status::Updated { version } => div()
            .child(format!("Updated to Suspense {version}."))
            .into_any_element(),
    };
    section
        .child(
            crate::checkbox::checkbox("settings-check-updates", "Check for updates automatically")
                .checked(updates.automatic)
                .on_click(|checked, _, cx| Updates::set_automatic(*checked, cx)),
        )
        .child(
            Button::new("settings-check-now")
                .small()
                .label("Check for updates")
                .disabled(busy)
                .on_click(|_, _, cx| Updates::check(true, cx)),
        )
        .child(status)
        .when_some(
            updates
                .finished_at_launch
                .clone()
                .filter(|_| !matches!(updates.status, Status::Ready { .. })),
            |section, version| {
                section.child(
                    h_flex()
                        .gap_2()
                        .child(if version.is_empty() {
                            "An update was put in place as Suspense started; a restart finishes it."
                                .to_string()
                        } else {
                            format!(
                                "Suspense {version} was put in place as Suspense started; a restart finishes it."
                            )
                        })
                        .child(restart("settings-update-finish")),
                )
            },
        )
        .child(details)
}

/// The Updates section's Details, collapsed to start with: what updating
/// rests on, with Show log, which opens `updates.log`, and Copy, which puts
/// it all, with the log's last lines, on the clipboard.
fn render_update_details(cx: &mut App) -> AnyElement {
    use crate::self_update::{ShowUpdatesLog, Updates};
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let open = Updates::get(cx).is_some_and(|updates| updates.details_open);
    let toggle = Button::new("settings-update-details")
        .ghost()
        .xsmall()
        .icon(if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .label("Details")
        .on_click(|_, _, cx| Updates::toggle_details(cx));
    let section = v_flex().gap_2().child(div().child(toggle));
    if !open {
        return section.into_any_element();
    }
    let rows = crate::self_update::details(cx).into_iter().map(|(label, value)| {
        h_flex()
            .gap_4()
            .items_start()
            .text_sm()
            .child(div().w(px(96.)).flex_none().text_color(muted).child(label))
            .child(div().flex_1().min_w_0().whitespace_normal().child(value))
    });
    section
        .child(
            gpui_kit::TestSupportExt::test_support(
                v_flex().id("settings-update-details-rows").gap_1().children(rows),
            ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("settings-update-show-log")
                        .small()
                        .label("Show log")
                        .disabled(crate::self_update::log::file().is_none_or(|file| !file.exists()))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ShowUpdatesLog), cx)
                        }),
                )
                .child(
                    Button::new("settings-update-copy")
                        .small()
                        .icon(IconName::Copy)
                        .label("Copy")
                        .on_click(|_, _, cx| {
                            let text = crate::self_update::details_text(cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }),
                ),
        )
        .into_any_element()
}

/// The section picked, kept while the settings are closed and opened again.
#[derive(Default)]
struct PickedSection(Section);

impl Global for PickedSection {}

/// Picks the Agent section, for the settings to open on it, as choosing
/// another harness from the welcome page does.
pub fn pick_agent_section(cx: &mut App) {
    cx.set_global(PickedSection(Section::Agent));
}

/// Ctrl+, (Cmd+, on macOS) opens the settings from anywhere in the main
/// window, which handles the action.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-,", OpenSettings, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
}

/// `text` with its first letter a capital, as a sentence begins.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One prompt, as edited.
struct PromptEditor {
    prompt: Prompt,
    editor: Entity<EditorState>,
    /// How the editor grows to fit the whole prompt, which it never scrolls.
    fit: GrowToFit,
    /// Why the prompt could not be read or saved, until it can be.
    error: Option<SharedString>,
}

pub struct SettingsWindow {
    prompts: Vec<PromptEditor>,
    /// Whether the sandbox can run here, and what it enforces, once read.
    sandbox: Option<Result<String, Refused>>,
    _sandbox_read: Task<()>,
    /// What Test sandbox found, once it has run, and whether it is running.
    sandbox_test: Option<Result<Vec<(String, bool)>, Refused>>,
    testing: bool,
    _sandbox_test: Task<()>,
    /// Whether Test's results are listed, check by check.
    test_open: bool,
    /// The list a pattern is being typed into, the spec's or the code's,
    /// and what is typed.
    adding_pattern: Option<bool>,
    pattern: Entity<InputState>,
    /// An area being named: a new one, or the added one at its place.
    naming: Option<Option<usize>>,
    area_name: Entity<InputState>,
    /// The added area whose folders and patterns are open beneath its row.
    editing_area: Option<usize>,
    /// The added area a pattern is being typed into, and what is typed.
    area_pattern_for: Option<usize>,
    area_pattern: Entity<InputState>,
    /// Why the classification couldn't be saved, when it couldn't.
    classification_error: Option<SharedString>,
    /// Why the project's settings couldn't be saved, when they couldn't.
    project_settings_error: Option<SharedString>,
    /// Set while the editors are filled from disk, so doing so saves nothing.
    loading: bool,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseSettings> for SettingsWindow {}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![
            cx.observe_global_in::<ProjectDirectory>(window, |this, window, cx| {
                this.load(window, cx)
            }),
        ];
        let prompts = Prompt::ALL
            .into_iter()
            .map(|prompt| {
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(piton_syntax::LANGUAGE_NAME)
                        .line_number(false)
                        .folding(false)
                        .soft_wrap(true)
                        // No empty rows below the last line: it is sized to
                        // its text.
                        .scroll_beyond_last_line(Some(0))
                });
                subscriptions.push(cx.subscribe_in(
                    &editor,
                    window,
                    move |this, _, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) && !this.loading {
                            this.save(prompt, cx);
                        }
                    },
                ));
                PromptEditor {
                    prompt,
                    editor,
                    fit: GrowToFit::new(usize::MAX),
                    error: None,
                }
            })
            .collect();

        // A pattern typed is saved once Enter is pressed or it loses focus.
        let pattern = cx.new(|cx| InputState::new(window, cx).placeholder("**/*.pi"));
        subscriptions.push(cx.subscribe_in(
            &pattern,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_pattern(window, cx);
                }
            },
        ));
        // An area's name, and a pattern of an added area, are saved once
        // Enter is pressed or they lose focus.
        let area_name = cx.new(|cx| InputState::new(window, cx).placeholder("Docs"));
        subscriptions.push(cx.subscribe_in(
            &area_name,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_naming(window, cx);
                }
            },
        ));
        let area_pattern = cx.new(|cx| InputState::new(window, cx).placeholder("docs/**/*.md"));
        subscriptions.push(cx.subscribe_in(
            &area_pattern,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_area_pattern(window, cx);
                }
            },
        ));
        let mut this = Self {
            prompts,
            project_settings_error: None,
            _sandbox_read: Task::ready(()),
            sandbox: None,
            sandbox_test: None,
            testing: false,
            _sandbox_test: Task::ready(()),
            test_open: false,
            adding_pattern: None,
            pattern,
            naming: None,
            area_name,
            editing_area: None,
            area_pattern_for: None,
            area_pattern,
            classification_error: None,
            loading: false,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load(window, cx);
        this
    }

    fn section(cx: &App) -> Section {
        cx.try_global::<PickedSection>()
            .map(|picked| picked.0)
            .unwrap_or_default()
    }

    /// Shows `section`, from its top.
    fn pick(&mut self, section: Section, cx: &mut Context<Self>) {
        cx.set_global(PickedSection(section));
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Fills each editor with the open project's prompt, leaving alone any
    /// that already shows it so its cursor stays put.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project_dir = ProjectDirectory::get(cx);
        if let Some(project_dir) = &project_dir {
            // So each can be found and edited by hand too. The defaults still
            // show if they cannot be saved.
            system_prompts::save_missing(project_dir).ok();
        }
        self.loading = true;
        for prompt in &mut self.prompts {
            let (text, error) = match &project_dir {
                Some(project_dir) => match system_prompts::load(prompt.prompt, project_dir) {
                    Ok(text) => (text, None),
                    Err(err) => (String::new(), Some(format!("{err:#}").into())),
                },
                None => (String::new(), None),
            };
            prompt.error = error;
            if prompt.editor.read(cx).value().as_ref() != text {
                prompt
                    .editor
                    .update(cx, |editor, cx| editor.set_value(text, window, cx));
            }
        }
        self.loading = false;
        let _ = project_dir;
        self.read_sandbox(cx);
        cx.notify();
    }

    /// Reads, in the background, whether the sandbox can run here.
    fn read_sandbox(&mut self, cx: &mut Context<Self>) {
        let read = cx.background_spawn(async { crate::sandbox::status() });
        self._sandbox_read = cx.spawn(async move |this, cx| {
            let status = read.await;
            this.update(cx, |this, cx| {
                this.sandbox = Some(status);
                cx.notify();
            })
            .ok();
        });
    }

    /// Runs Test: a short set of the hostile checks, in a temporary project,
    /// each saying whether the system refused it.
    fn test_sandbox(&mut self, cx: &mut Context<Self>) {
        self.testing = true;
        let run = cx.background_spawn(async { crate::sandbox::self_test() });
        self._sandbox_test = cx.spawn(async move |this, cx| {
            let found = run.await;
            this.update(cx, |this, cx| {
                this.sandbox_test = Some(found);
                this.testing = false;
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    /// Changes the open project's classification by `change`, saved at once.
    fn classify(&mut self, change: impl FnOnce(&mut Classification), cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let mut classification = Classification::load(&project_dir);
        change(&mut classification);
        self.classification_error = classification
            .save(&project_dir)
            .err()
            .map(|err| format!("{err}").into());
        cx.notify();
    }

    /// Adds folders to the code or the spec, chosen with the folder browser,
    /// opened in the project.
    fn add_folder(&mut self, spec: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let roots: Vec<String> = paths
                    .iter()
                    .filter_map(|path| path.strip_prefix(&project_dir).ok())
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .filter(|root| !root.is_empty())
                    .collect();
                this.classify(
                    |classification| {
                        let list = if spec {
                            &mut classification.spec_roots
                        } else {
                            &mut classification.code_roots
                        };
                        for root in roots {
                            if !list.contains(&root) {
                                list.push(root);
                            }
                        }
                    },
                    cx,
                )
            })
            .ok();
        })
        .detach();
    }

    /// Adds a row to the code's or the spec's list to type a pattern into.
    fn start_pattern(&mut self, spec: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.adding_pattern = Some(spec);
        self.pattern.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Saves the pattern typed, once Enter is pressed or it loses focus; an
    /// empty one is let go.
    fn commit_pattern(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.adding_pattern.take() else {
            return;
        };
        let pattern = self.pattern.read(cx).value().trim().to_string();
        self.pattern.update(cx, |input, cx| input.set_value("", window, cx));
        if pattern.is_empty() {
            cx.notify();
            return;
        }
        if globset::Glob::new(&pattern).is_err() {
            self.classification_error = Some(format!("`{pattern}` isn't a pattern").into());
            cx.notify();
            return;
        }
        self.classify(
            |classification| {
                let list = if spec {
                    &mut classification.spec_patterns
                } else {
                    &mut classification.code_patterns
                };
                if !list.contains(&pattern) {
                    list.push(pattern.clone());
                }
            },
            cx,
        );
    }

    /// A card of the section: a box on the raised surface, headed by `title`.
    fn card(title: &'static str, cx: &App) -> Div {
        let palette = crate::theme::palette(cx);
        v_flex()
            .w_full()
            .gap_2()
            .p(px(12.))
            .rounded(px(6.))
            .bg(crate::theme::color(palette.raised))
            .child(div().text_sm().font_semibold().child(title))
    }

    /// The Status card: whether the sandbox can enforce every profile here,
    /// the backend and what it enforces, and Test.
    fn render_sandbox_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
        let backend = crate::sandbox::backend_name();
        let runs = matches!(self.sandbox, Some(Ok(_)));
        let dot = |color: Hsla| div().flex_none().size(px(8.)).rounded_full().bg(color);
        let line: AnyElement = match &self.sandbox {
            None => h_flex()
                .gap_2()
                .child(dot(muted))
                .child(div().text_color(muted).child("Checking the sandbox…"))
                .into_any_element(),
            Some(Ok(enforces)) => {
                // What it enforces, in plain words, after the backend's name.
                let plain = enforces
                    .split_once(": ")
                    .map_or(enforces.as_str(), |(_, plain)| plain)
                    .to_string();
                h_flex()
                    .gap_2()
                    .child(dot(success))
                    .child(div().font_medium().child(backend))
                    .child(div().text_color(muted).child(plain))
                    .into_any_element()
            }
            Some(Err(why)) => h_flex()
                .gap_2()
                .child(dot(danger))
                .child(div().flex_1().min_w_0().text_color(danger).child(format!(
                    "{}. No task or question will run until it can.",
                    capitalized(&why.message)
                )))
                .child(
                    Button::new("settings-sandbox-docs")
                        .ghost()
                        .small()
                        .label("Documentation")
                        .on_click(|_, _, cx| cx.open_url(crate::sandbox::DOCUMENTATION)),
                )
                .into_any_element(),
        };
        let test = Button::new("settings-test-sandbox")
            .ghost()
            .small()
            .label("Test")
            .loading(self.testing)
            .disabled(self.testing || !runs)
            .on_click(cx.listener(|this, _, _, cx| this.test_sandbox(cx)));
        let results = self.sandbox_test.as_ref().map(|found| match found {
            Err(why) => div().text_sm().text_color(danger).child(why.message.clone()).into_any_element(),
            Ok(checks) => {
                let through = checks.iter().filter(|(_, refused)| !refused).count();
                let summary = if through == 0 {
                    format!("{} of {} refused", checks.len(), checks.len())
                } else {
                    format!("{through} of {} got through", checks.len())
                };
                let open = self.test_open;
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .id("settings-sandbox-test-summary")
                            .gap_1()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(if through == 0 { success } else { danger })
                            .child(
                                Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .xsmall(),
                            )
                            .child(summary)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.test_open = !this.test_open;
                                cx.notify();
                            })),
                    )
                    .when(open, |list| {
                        list.children(checks.iter().map(|(check, refused)| {
                            h_flex()
                                .gap_2()
                                .pl_4()
                                .text_sm()
                                .child(
                                    Icon::new(if *refused { IconName::Check } else { IconName::X })
                                        .xsmall()
                                        .text_color(if *refused { success } else { danger }),
                                )
                                .child(div().text_color(muted).child(check.clone()))
                        }))
                    })
                    .into_any_element()
            }
        });
        Self::card("Status", cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(line))
                    .child(test),
            )
            .children(results)
            .into_any_element()
    }

    /// Sets `kind`'s level in `area` for the open project, or with none, puts
    /// back the profile's default, as the grid's menus do. The Spec column's
    /// Code row is the Read the code switch, and its Other files row at
    /// Read the Read the whole project switch, so the two always agree.
    fn set_level(
        &mut self,
        kind: ProfileKind,
        area: crate::sandbox::reach::Area,
        level: Option<crate::sandbox::reach::Level>,
        cx: &mut Context<Self>,
    ) {
        use crate::sandbox::reach::{Area, Level, mode_key};
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let store = |this: &mut Self, level: Option<Level>, cx: &mut Context<Self>| {
            this.classify(
                |classification| {
                    let mode = mode_key(kind).to_string();
                    // An added area keeps its levels with it.
                    if let Area::Added(index) = area {
                        if let Some(added) = classification.areas.get_mut(index) {
                            match level {
                                Some(level) => {
                                    added.levels.insert(mode, level);
                                }
                                None => {
                                    added.levels.remove(&mode);
                                }
                            }
                        }
                        return;
                    }
                    match level {
                        Some(level) => {
                            classification
                                .levels
                                .entry(mode)
                                .or_default()
                                .insert(area.key().to_string(), level);
                        }
                        None => {
                            if let Some(areas) = classification.levels.get_mut(&mode) {
                                areas.remove(area.key());
                                if areas.is_empty() {
                                    classification.levels.remove(&mode);
                                }
                            }
                        }
                    }
                },
                cx,
            )
        };
        let reads_code = crate::project_settings::spec_reads_code(&project_dir);
        match (kind, area, level) {
            (ProfileKind::Spec, Area::Code, level) => {
                store(self, None, cx);
                self.set_spec_reads_code(level == Some(Level::Read), cx);
            }
            (ProfileKind::Spec, Area::EverythingElse, level) if reads_code => match level {
                Some(Level::Edit) => {
                    self.set_spec_reads_project(false, cx);
                    store(self, Some(Level::Edit), cx);
                }
                level => {
                    store(self, None, cx);
                    self.set_spec_reads_project(level == Some(Level::Read), cx);
                }
            },
            (_, _, level) => store(self, level, cx),
        }
    }

    /// Starts naming an area: a new one, with `target` none, or renaming
    /// the added area at `target`.
    fn start_naming(&mut self, target: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let current = target
            .and_then(|index| {
                let project_dir = ProjectDirectory::get(cx)?;
                Classification::load(&project_dir)
                    .areas
                    .get(index)
                    .map(|area| area.name.clone())
            })
            .unwrap_or_default();
        self.naming = Some(target);
        self.area_name.update(cx, |input, cx| {
            input.set_value(current, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Saves the name typed, once Enter is pressed or it loses focus: a new
    /// area, opened to add its folders and patterns, or a renamed one. An
    /// empty name is let go.
    fn commit_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.naming.take() else {
            return;
        };
        let name = self.area_name.read(cx).value().trim().to_string();
        self.area_name.update(cx, |input, cx| input.set_value("", window, cx));
        if name.is_empty() {
            cx.notify();
            return;
        }
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let added = Classification::load(&project_dir).areas.len();
        self.classify(
            |classification| match target {
                Some(index) => {
                    if let Some(area) = classification.areas.get_mut(index) {
                        area.name = name;
                    }
                }
                None => classification.areas.push(crate::sandbox::AddedArea {
                    name,
                    ..crate::sandbox::AddedArea::default()
                }),
            },
            cx,
        );
        if target.is_none() {
            self.editing_area = Some(added);
        }
    }

    /// Removes the added area at `index`, its files going back into Other
    /// files.
    fn remove_area(&mut self, index: usize, cx: &mut Context<Self>) {
        self.classify(
            |classification| {
                if index < classification.areas.len() {
                    classification.areas.remove(index);
                }
            },
            cx,
        );
        self.editing_area = None;
        self.area_pattern_for = None;
    }

    /// Adds folders to the added area at `index`, chosen with the folder
    /// browser.
    fn add_area_folder(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let roots: Vec<String> = paths
                    .iter()
                    .filter_map(|path| path.strip_prefix(&project_dir).ok())
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .filter(|root| !root.is_empty())
                    .collect();
                this.classify(
                    |classification| {
                        if let Some(area) = classification.areas.get_mut(index) {
                            for root in roots {
                                if !area.roots.contains(&root) {
                                    area.roots.push(root);
                                }
                            }
                        }
                    },
                    cx,
                )
            })
            .ok();
        })
        .detach();
    }

    /// Adds a row to the added area at `index` to type a pattern into.
    fn start_area_pattern(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.area_pattern_for = Some(index);
        self.area_pattern.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Saves the pattern typed into an added area, once Enter is pressed or
    /// it loses focus; an empty one is let go.
    fn commit_area_pattern(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.area_pattern_for.take() else {
            return;
        };
        let pattern = self.area_pattern.read(cx).value().trim().to_string();
        self.area_pattern.update(cx, |input, cx| input.set_value("", window, cx));
        if pattern.is_empty() {
            cx.notify();
            return;
        }
        if globset::Glob::new(&pattern).is_err() {
            self.classification_error = Some(format!("`{pattern}` isn't a pattern").into());
            cx.notify();
            return;
        }
        self.classify(
            |classification| {
                if let Some(area) = classification.areas.get_mut(index)
                    && !area.patterns.contains(&pattern)
                {
                    area.patterns.push(pattern);
                }
            },
            cx,
        );
    }

    /// An area's name in the grid, and beneath it, muted and cut off, what
    /// it covers, the whole list in its tooltip. An added area's name renames
    /// it, its covered line opens its folders and patterns, and a button
    /// shown on hover removes it. Suspense data notes the files every run is
    /// given there on its own.
    fn render_area_name(
        &self,
        row: usize,
        area: crate::sandbox::reach::Area,
        name: String,
        covered: Vec<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::sandbox::reach::{Area, OWN_FILES};
        let muted = cx.theme().muted_foreground;
        let whole: SharedString = if covered.is_empty() {
            "Nothing yet".into()
        } else {
            covered.join(", ").into()
        };
        let tip = whole.clone();
        let added = match area {
            Area::Added(index) => Some(index),
            _ => None,
        };
        let renaming = added.is_some() && self.naming == Some(added);
        let name: AnyElement = match added {
            _ if renaming => Input::new(&self.area_name).small().into_any_element(),
            Some(index) => div()
                .id(SharedString::from(format!("settings-area-name-{row}")))
                .text_sm()
                .cursor_pointer()
                .truncate()
                .child(name)
                .on_click(cx.listener(move |this, _, window, cx| this.start_naming(Some(index), window, cx)))
                .into_any_element(),
            None => div().text_sm().truncate().child(name).into_any_element(),
        };
        // Lets UI tests find it; inert in normal builds.
        let covered_line = gpui_kit::TestSupportExt::test_support(
            div().id(SharedString::from(format!("settings-area-covered-{row}"))),
        )
            .text_xs()
            .text_color(muted)
            .truncate()
            .child(whole)
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .when_some(added, |line, index| {
                line.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    this.editing_area = (this.editing_area != Some(index)).then_some(index);
                    cx.notify();
                }))
            });
        let group = SharedString::from(format!("settings-area-row-{row}"));
        v_flex()
            .id(group.clone())
            .group(group.clone())
            .w(px(150.))
            .min_w_0()
            .child(
                h_flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(name))
                    .children(added.map(|index| {
                        div()
                            .opacity(0.)
                            .group_hover(group.clone(), |style| style.opacity(1.))
                            .child(
                                Button::new(SharedString::from(format!("settings-area-remove-{row}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::X)
                                    .tooltip("Remove this area; its files go back into Other files")
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_area(index, cx))),
                            )
                    })),
            )
            .child(covered_line)
            .when(area == Area::ProjectData, |column| {
                column.child(div().text_xs().text_color(muted).child(OWN_FILES))
            })
            .into_any_element()
    }

    /// An added area's folders and patterns, as its covered line opens them:
    /// each in monospace with a remove button, one another area already has
    /// in the error colour saying which, then Add folder and Add pattern.
    fn render_area_editor(
        &self,
        index: usize,
        classification: &Classification,
        project_dir: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, mono) = (theme.muted_foreground, theme.danger, theme.mono_font_family.clone());
        let Some(area) = classification.areas.get(index) else {
            return div().into_any_element();
        };
        let agent = crate::agent::of_project(Some(project_dir));
        let reference = project_dir.join(agent.directory()).join("reference");
        let entries: Vec<(String, bool)> = area
            .roots
            .iter()
            .map(|root| (root.clone(), false))
            .chain(area.patterns.iter().map(|pattern| (pattern.clone(), true)))
            .collect();
        let mut list = v_flex().gap_0p5().pl(px(12.)).py_1();
        for (n, (entry, pattern)) in entries.into_iter().enumerate() {
            let taken = classification.taken_by(index, &entry, pattern, project_dir, &reference);
            let removed = entry.clone();
            list = list.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .when(taken.is_some(), |entry| entry.text_color(danger))
                            .child(entry),
                    )
                    .children(taken.map(|taken| {
                        div().text_xs().text_color(danger).child(format!("already in {taken}"))
                    }))
                    .child(
                        Button::new(SharedString::from(format!("settings-area-{index}-remove-{n}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let removed = removed.clone();
                                this.classify(
                                    |classification| {
                                        if let Some(area) = classification.areas.get_mut(index) {
                                            area.roots.retain(|root| *root != removed);
                                            area.patterns.retain(|pattern| *pattern != removed);
                                        }
                                    },
                                    cx,
                                )
                            })),
                    ),
            );
        }
        if self.area_pattern_for == Some(index) {
            list = list.child(Input::new(&self.area_pattern).small());
        } else if area.roots.is_empty() && area.patterns.is_empty() {
            list = list.child(div().text_sm().text_color(muted).child("Nothing yet"));
        }
        list.child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("settings-area-{index}-add-folder")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::FolderPlus)
                        .label("Add folder")
                        .on_click(cx.listener(move |this, _, window, cx| this.add_area_folder(index, window, cx))),
                )
                .child(
                    Button::new(SharedString::from(format!("settings-area-{index}-add-pattern")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label("Add pattern")
                        .on_click(cx.listener(move |this, _, window, cx| this.start_area_pattern(index, window, cx))),
                ),
        )
        .into_any_element()
    }

    /// The Who can see what card: each mode's reach into each part of the
    /// project, as the profiles compile now.
    fn render_sandbox_profiles(&self, project_dir: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        use crate::sandbox::Access;
        use crate::sandbox::reach::{Area, Level, OUTSIDE, allowed, level, reach};
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
        let theme = cx.theme();
        let (muted, success, accent, danger) =
            (theme.muted_foreground, theme.success, theme.primary, theme.danger);
        let modes = [
            (ProfileKind::Code, SendMode::Code, "Code"),
            (ProfileKind::Spec, SendMode::Spec, "Spec"),
            (ProfileKind::Question, SendMode::Ask, "Ask"),
            (ProfileKind::Freeform, SendMode::Freeform, "Freeform"),
        ];
        let agent = crate::agent::of_project(Some(project_dir));
        let kinds: Vec<ProfileKind> = modes.iter().map(|(kind, ..)| *kind).collect();
        let rows = reach(&kinds, agent, project_dir);
        let names = crate::sandbox::reach::rows(agent, project_dir);
        let classification = Classification::load(project_dir);
        let this = cx.entity().downgrade();
        const AREA_WIDTH: f32 = 150.;
        const CELL_WIDTH: f32 = 84.;
        let header = h_flex()
            .gap_1()
            .child(div().w(px(AREA_WIDTH)))
            .children(modes.iter().map(|(_, mode, label)| {
                div()
                    .w(px(CELL_WIDTH))
                    .text_sm()
                    .font_medium()
                    .text_color(chat_input::mode_color(*mode, cx))
                    .child(*label)
            }));
        let mut grid = v_flex().gap_1().child(header);
        for (row, (area, cells)) in rows.into_iter().enumerate() {
            let (name, covered) = names
                .get(row)
                .map(|(_, name, covered)| (name.clone(), covered.clone()))
                .unwrap_or_default();
            let mut line = h_flex()
                .items_start()
                .gap_1()
                .child(self.render_area_name(row, area, name, covered, cx));
            for (column, cell) in cells.into_iter().enumerate() {
                let kind = kinds[column];
                let (current, tint, color, tip): (Option<Level>, Option<Hsla>, Hsla, String) = match cell {
                    Err(why) => (None, None, danger, why),
                    Ok(reach) => {
                        let paths = if reach.paths.is_empty() {
                            "Nothing".to_string()
                        } else {
                            reach.paths.join("\n")
                        };
                        match reach.access {
                            Some(Access::ReadWrite) => (Some(Level::Edit), Some(success.opacity(0.16)), success, paths),
                            Some(Access::ReadOnly) => (Some(Level::Read), Some(accent.opacity(0.16)), accent, paths),
                            None => (Some(Level::NoAccess), None, muted, paths),
                        }
                    }
                };
                let label = current.map_or("Refused", Level::label);
                // Set by the user: the Spec column's Code row is the Read the
                // code switch, never a level of its own.
                let linked = kind == ProfileKind::Spec && area == Area::Code;
                let set = !linked && level(&classification, kind, area).is_some();
                let reasons: Vec<&'static str> = Level::ALL
                    .into_iter()
                    .filter_map(|choice| allowed(kind, area, choice).err())
                    .collect();
                let this = this.clone();
                let menu = Button::new(SharedString::from(format!("settings-reach-{row}-{column}")))
                    .ghost()
                    .xsmall()
                    .label(label)
                    .tooltip(SharedString::from(tip))
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        for choice in Level::ALL {
                            let this = this.clone();
                            menu = menu.item(
                                PopupMenuItem::new(choice.label())
                                    .checked(current == Some(choice))
                                    .disabled(allowed(kind, area, choice).is_err())
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |this, cx| this.set_level(kind, area, Some(choice), cx))
                                            .ok();
                                    }),
                            );
                        }
                        // Why what is disabled is, as the sandbox never allows it.
                        if !reasons.is_empty() {
                            menu = menu.separator();
                            for reason in &reasons {
                                menu = menu.item(PopupMenuItem::label(*reason));
                            }
                        }
                        if set {
                            let this = this.clone();
                            menu = menu.separator().item(PopupMenuItem::new("Use default").on_click(
                                move |_, _, cx| {
                                    this.update(cx, |this, cx| this.set_level(kind, area, None, cx)).ok();
                                },
                            ));
                        }
                        menu
                    });
                line = line.child(
                    div()
                        .relative()
                        .w(px(CELL_WIDTH))
                        .rounded(px(4.))
                        .text_color(color)
                        .when_some(tint, |cell, tint| cell.bg(tint))
                        .child(menu)
                        // Set by the user, a small dot in its corner.
                        .when(set, |cell| {
                            cell.child(
                                gpui_kit::TestSupportExt::test_support(
                                    div().id(SharedString::from(format!("settings-reach-set-{row}-{column}"))),
                                )
                                .absolute()
                                .top(px(2.))
                                .right(px(2.))
                                .size(px(5.))
                                .rounded_full()
                                .bg(accent),
                            )
                        }),
                );
            }
            grid = grid.child(line);
            // An added area being edited: its folders and patterns, beneath
            // its row.
            if let Area::Added(index) = area
                && self.editing_area == Some(index)
            {
                grid = grid.child(self.render_area_editor(index, &classification, project_dir, cx));
            }
        }
        // A new area being named, at the end of the added ones.
        if self.naming == Some(None) {
            grid = grid.child(
                h_flex()
                    .gap_1()
                    .child(div().w(px(AREA_WIDTH)).child(Input::new(&self.area_name).small())),
            );
        }
        let any_set = classification.levels.values().any(|areas| !areas.is_empty())
            || classification.areas.iter().any(|area| !area.levels.is_empty());
        Self::card("Who can see what", cx)
            .child(grid)
            .child(div().text_sm().text_color(muted).child(OUTSIDE))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("settings-add-area")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add area")
                            .on_click(cx.listener(|this, _, window, cx| this.start_naming(None, window, cx))),
                    )
                    .child(
                        Button::new("settings-reset-grid")
                            .ghost()
                            .small()
                            .label("Reset grid")
                            .disabled(!any_set)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.classify(
                                    |classification| {
                                        classification.levels.clear();
                                        for area in &mut classification.areas {
                                            area.levels.clear();
                                        }
                                    },
                                    cx,
                                )
                            })),
                    ),
            )
            .into_any_element()
    }

    /// One of the Code and spec card's lists: its folders and patterns, in
    /// monospace, each removed from the button shown on hover, those in
    /// both lists in the error colour, then Add folder and Add pattern.
    fn render_classified(
        &self,
        spec: bool,
        classification: &Classification,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, mono) = (theme.muted_foreground, theme.danger, theme.mono_font_family.clone());
        let (folders, patterns, other_folders, other_patterns) = if spec {
            (
                &classification.spec_roots,
                &classification.spec_patterns,
                &classification.code_roots,
                &classification.code_patterns,
            )
        } else {
            (
                &classification.code_roots,
                &classification.code_patterns,
                &classification.spec_roots,
                &classification.spec_patterns,
            )
        };
        let (title, mode) = if spec { ("Spec", SendMode::Spec) } else { ("Code", SendMode::Code) };
        let side = if spec { "spec" } else { "code" };
        let rows: Vec<(String, bool, bool)> = folders
            .iter()
            .map(|folder| (folder.clone(), false, other_folders.contains(folder)))
            .chain(
                patterns
                    .iter()
                    .map(|pattern| (pattern.clone(), true, other_patterns.contains(pattern))),
            )
            .collect();
        let mut list = v_flex()
            .gap_0p5()
            .child(div().text_sm().font_medium().text_color(chat_input::mode_color(mode, cx)).child(title));
        for (n, (row, is_pattern, in_both)) in rows.into_iter().enumerate() {
            let group = SharedString::from(format!("settings-{side}-row-{n}"));
            let removed = row.clone();
            list = list.child(
                h_flex()
                    .id(group.clone())
                    .group(group.clone())
                    .gap_2()
                    .text_sm()
                    .font_family(mono.clone())
                    .when(in_both, |row| row.text_color(danger))
                    .child(div().flex_1().min_w_0().truncate().child(row))
                    .child(
                        div()
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                            .child(
                                Button::new(SharedString::from(format!("settings-{side}-remove-{n}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::X)
                                    .tooltip("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let removed = removed.clone();
                                        this.classify(
                                            |classification| {
                                                let list = match (spec, is_pattern) {
                                                    (false, false) => &mut classification.code_roots,
                                                    (false, true) => &mut classification.code_patterns,
                                                    (true, false) => &mut classification.spec_roots,
                                                    (true, true) => &mut classification.spec_patterns,
                                                };
                                                list.retain(|item| *item != removed);
                                            },
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            );
        }
        if self.adding_pattern == Some(spec) {
            list = list.child(Input::new(&self.pattern).small());
        } else if folders.is_empty() && patterns.is_empty() {
            list = list.child(div().text_sm().text_color(muted).child("Nothing"));
        }
        list.child(
            h_flex()
                .gap_1()
                .pt_1()
                .child(
                    Button::new(SharedString::from(format!("settings-{side}-add-folder")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::FolderPlus)
                        .label("Add folder")
                        .on_click(cx.listener(move |this, _, window, cx| this.add_folder(spec, window, cx))),
                )
                .child(
                    Button::new(SharedString::from(format!("settings-{side}-add-pattern")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label("Add pattern")
                        .on_click(cx.listener(move |this, _, window, cx| this.start_pattern(spec, window, cx))),
                ),
        )
        .into_any_element()
    }

    /// The Code and spec card: the project's classification, as two lists.
    fn render_sandbox_classification(&self, project_dir: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let classification = Classification::load(project_dir);
        let in_both = classification
            .code_roots
            .iter()
            .any(|root| classification.spec_roots.contains(root))
            || classification
                .code_patterns
                .iter()
                .any(|pattern| classification.spec_patterns.contains(pattern));
        let conflict = classification.conflict(project_dir);
        // The lists alone; the grid's levels are its own.
        let from_locations = Classification {
            levels: classification.levels.clone(),
            areas: classification.areas.clone(),
            ..Classification::from_locations(project_dir)
        };
        let differs = classification != from_locations;
        Self::card("Code and spec", cx)
            .child(div().text_sm().text_color(muted).child(
                "Which parts of the project are code and which are spec. A Code run can't open \
                 spec, and a Spec run can't open code.",
            ))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(div().flex_1().min_w_0().child(self.render_classified(false, &classification, cx)))
                    .child(div().flex_1().min_w_0().child(self.render_classified(true, &classification, cx))),
            )
            .when(in_both || conflict.is_some(), |card| {
                card.child(div().text_sm().text_color(danger).child(
                    "Something is in both lists. Runs won't start until it is in one only.",
                ))
            })
            .children(
                self.classification_error
                    .clone()
                    .map(|why| div().text_sm().text_color(danger).child(why)),
            )
            .child(
                div().child(
                    Button::new("settings-reset-classification")
                        .ghost()
                        .small()
                        .label("Reset to project locations")
                        .disabled(!differs)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let Some(project_dir) = ProjectDirectory::get(cx) else {
                                return;
                            };
                            this.classify(
                                |classification| {
                                    *classification = Classification {
                                        levels: std::mem::take(&mut classification.levels),
                                        areas: std::mem::take(&mut classification.areas),
                                        ..Classification::from_locations(&project_dir)
                                    }
                                },
                                cx,
                            )
                        })),
                ),
            )
            .into_any_element()
    }

    /// The Spec tasks card: whether Spec tasks may read the code, and the
    /// whole project, each a switch.
    fn render_sandbox_spec_tasks(&self, project_dir: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let settings = crate::project_settings::ProjectSettings::load(project_dir);
        let row = |label: &'static str, says: &'static str, switch: AnyElement| {
            h_flex()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(label))
                        .child(div().text_sm().text_color(muted).child(says)),
                )
                .child(switch)
        };
        Self::card("Spec tasks", cx)
            .child(row(
                "Read the code",
                "Spec tasks can read the code, but never change it.",
                Switch::new("settings-spec-reads-code")
                    .checked(settings.spec_reads_code)
                    .on_click(cx.listener(|this, on: &bool, _, cx| this.set_spec_reads_code(*on, cx)))
                    .into_any_element(),
            ))
            .when(settings.spec_reads_code, |card| {
                card.child(
                    div().pl_6().child(row(
                        "Read the whole project",
                        "They can also read the rest of the project, read only.",
                        Switch::new("settings-spec-reads-project")
                            .checked(settings.spec_reads_project)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.set_spec_reads_project(*on, cx)
                            }))
                            .into_any_element(),
                    )),
                )
            })
            .children(
                self.project_settings_error
                    .clone()
                    .map(|error| div().text_sm().text_color(cx.theme().danger).child(error)),
            )
            .into_any_element()
    }

    /// The Outside the sandbox card: what it doesn't cover, so it is never
    /// taken for protection.
    fn render_sandbox_outside(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let line = |icon: IconName, says: &'static str| {
            h_flex()
                .items_start()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(div().pt_0p5().child(Icon::new(icon).xsmall()))
                .child(div().flex_1().min_w_0().child(says))
        };
        Self::card("Outside the sandbox", cx)
            .child(line(
                IconName::Globe,
                "The network: a run can still fetch the project from anywhere it is published.",
            ))
            .child(line(
                IconName::Bot,
                "The harness's own service: it keeps what earlier runs of a conversation sent it.",
            ))
            .child(line(
                IconName::ClipboardPaste,
                "The clipboard: a run can read whatever was last copied.",
            ))
            .into_any_element()
    }

    /// The "Sandbox" section, as the SettingsWindowScope says: a column of
    /// cards, Status first and Outside the sandbox last, the project's own
    /// between them while one is open.
    fn render_sandbox(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Lets UI tests find each card; inert in normal builds.
        let tagged = |id: &'static str, card: AnyElement| {
            gpui_kit::TestSupportExt::test_support(div().id(id).w_full().child(card)).into_any_element()
        };
        let mut column = v_flex()
            .gap_4()
            .child(tagged("settings-sandbox-status", self.render_sandbox_status(cx)));
        match ProjectDirectory::get(cx) {
            Some(project_dir) => {
                column = column
                    .child(tagged("settings-sandbox-reach", self.render_sandbox_profiles(&project_dir, cx)))
                    .child(tagged(
                        "settings-sandbox-classification",
                        self.render_sandbox_classification(&project_dir, cx),
                    ))
                    .child(tagged("settings-sandbox-spec-tasks", self.render_sandbox_spec_tasks(&project_dir, cx)));
            }
            None => {
                column = column.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Who can see what, code and spec, and Spec tasks are for a project, once one is open."),
                );
            }
        }
        column.child(tagged("settings-sandbox-outside", self.render_sandbox_outside(cx)))
    }

    /// Lets the open project's Spec runs read the code, or not, saved with
    /// the project at once; from its next run.
    fn set_spec_reads_code(&mut self, reads: bool, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let mut settings = crate::project_settings::ProjectSettings::load(&project_dir);
        settings.spec_reads_code = reads;
        // Not reading the code, they read nothing else of it either.
        if !reads {
            settings.spec_reads_project = false;
        }
        self.project_settings_error = settings
            .save(&project_dir)
            .err()
            .map(|err| format!("{err:#}").into());
        crate::debug_log::log(
            Some(&project_dir),
            format!("Spec runs may read the code: {reads}"),
        );
        cx.notify();
    }

    /// Lets the open project's Spec runs read the whole project too, or not,
    /// saved with the project at once; from its next run.
    fn set_spec_reads_project(&mut self, reads: bool, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let mut settings = crate::project_settings::ProjectSettings::load(&project_dir);
        settings.spec_reads_project = reads && settings.spec_reads_code;
        self.project_settings_error = settings
            .save(&project_dir)
            .err()
            .map(|err| format!("{err:#}").into());
        crate::debug_log::log(
            Some(&project_dir),
            format!("Spec runs may read the whole project: {}", settings.spec_reads_project),
        );
        cx.notify();
    }

    /// Saves `which` as edited.
    fn save(&mut self, which: Prompt, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let Some(prompt) = self
            .prompts
            .iter_mut()
            .find(|prompt| prompt.prompt == which)
        else {
            return;
        };
        let text = prompt.editor.read(cx).value();
        prompt.error = system_prompts::save(which, &text, &project_dir)
            .err()
            .map(|err| format!("{err:#}").into());
        cx.notify();
    }

    /// Puts `which`'s default back, and saves it.
    fn reset(&mut self, which: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompts.iter().find(|prompt| prompt.prompt == which) else {
            return;
        };
        prompt.editor.update(cx, |editor, cx| {
            editor.set_value(system_prompts::default_prompt(which), window, cx)
        });
        self.save(which, cx);
    }

    fn render_prompt(
        &self,
        ix: usize,
        prompt: &PromptEditor,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let which = prompt.prompt;
        let is_default =
            prompt.editor.read(cx).value().as_ref() == system_prompts::default_prompt(which);
        let file = format!(".suspense/system-prompts/{}.md", which.key());
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().font_semibold().child(which.label()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(file),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "reset-{}-system-prompt",
                            which.key()
                        )))
                        .ghost()
                        .xsmall()
                        .label("Reset to default")
                        .disabled(is_default)
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.reset(which, window, cx)),
                        ),
                    ),
            )
            .when(which == Prompt::CodeToSpec, |column| {
                // Lets UI tests find it; inert in normal builds.
                column.child(gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("code-to-spec-note")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Added to a Code task sent to Spec, after Spec's system \
                             prompt. {CODE_PROMPT} and {CODE_RESULT} stand for the code \
                             task's prompt and its final output."
                        )),
                ))
            })
            .when(which == Prompt::SpecToCode, |column| {
                // Lets UI tests find it; inert in normal builds.
                column.child(gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("spec-to-code-note")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "Added to the Code task a Chain task hands on to once the \
                             spec is written and built, after Code's system prompt. \
                             {SPEC_PROMPT} and {SPEC_RESULT} stand for the prompt and the \
                             spec task's final output."
                        )),
                ))
            })
            .child({
                let (height, _) = prompt.fit.heights(&prompt.editor, window, cx);
                div()
                    .relative()
                    .child(Editor::new(&prompt.editor).h(height))
                    .child(GrowToFit::tracker(
                        &prompt.editor,
                        cx.entity().downgrade(),
                        move |this: &mut Self| &mut this.prompts[ix].fit,
                    ))
            })
            .children(
                prompt
                    .error
                    .clone()
                    .map(|error| div().text_sm().text_color(theme.danger).child(error)),
            )
    }

    /// Sends every run of the open project from the next on to `agent`,
    /// saved with the project.
    fn pick_agent(&mut self, agent: Agent, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        // A pick that can't be saved still holds for the session.
        agent::set_for_project(&project_dir, agent).ok();
        cx.notify();
    }

    /// A row for each harness, the open project's picked, each with the
    /// command it runs and whether that is installed. With no project open,
    /// none is offered: the harness is chosen for each project.
    fn render_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if ProjectDirectory::get(cx).is_none() {
            return div()
                .text_sm()
                .text_color(muted)
                .child("The harness is chosen for each project. Open a project to pick its harness.")
                .into_any_element();
        }
        let current = agent::current(cx);
        v_flex()
            .gap_3()
            .children(Agent::RUNNABLE.into_iter().map(|agent| {
                h_flex()
                    .gap_2()
                    .child(
                        Radio::new(SharedString::from(format!(
                            "settings-agent-{}",
                            agent.command()
                        )))
                        .label(agent.label())
                        .checked(agent == current)
                        .on_click(
                            cx.listener(move |this, _: &bool, _, cx| this.pick_agent(agent, cx)),
                        ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(agent.command()),
                    )
                    .when(!agent.installed(), |row| {
                        row.child(div().text_sm().text_color(muted).child("Not installed"))
                    })
            }))
            .into_any_element()
    }

    /// The sidebar of sections, the one picked marked with the accent.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let picked = Self::section(cx);
        v_flex()
            .flex_none()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .py_2()
            .border_r_1()
            .border_color(theme.border)
            .children(Section::ALL.into_iter().map(|section| {
                let is_picked = section == picked;
                div()
                    .id(SharedString::from(format!(
                        "settings-section-{}",
                        section.key()
                    )))
                    .px_4()
                    .py_2()
                    .border_l_2()
                    .border_color(if is_picked {
                        theme.accent
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(is_picked, |row| row.bg(theme.list_active).font_semibold())
                    .when(!is_picked, |row| {
                        row.text_color(theme.muted_foreground)
                            .hover(|row| row.bg(theme.list_hover))
                    })
                    .cursor_pointer()
                    .child(section.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(section, cx)))
            }))
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = Self::section(cx);
        let theme = cx.theme();
        let (background, foreground, muted, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
        );
        let (title, blurb, without_project) = match section {
            Section::SystemPrompts => (
                "System prompts",
                format!(
                    "Each tab gives a prompt sent from it this system prompt. \
                     They are saved with the project as they are edited. \
                     {CODE_LOCATION} and {SPEC_LOCATION} stand for codeRoot and \
                     root in piton.config.pi, {SPEC_READING} for the injected \
                     spec reading, and {PITON_FLUENCY_FILE} for the file \
                     `piton agent --print-fluency` is written to at each spec \
                     build, which only the Spec and Chain prompts point at, \
                     and {SUSPENSE_FLUENCY_FILE} for the file how Suspense \
                     works is written to, which every prompt but Freeform's \
                     points at."
                ),
                "Open a project to edit its system prompts.",
            ),
            Section::InjectedPrompts => (
                "Injected prompts",
                format!(
                    "These are injected into the system prompts wherever their \
                     placeholder is written: the spec reading at {SPEC_READING}. \
                     They are saved with the project as they are edited. \
                     {HARNESS_DIRECTORY} stands for the harness's directory, {}.",
                    crate::harness::directory(ProjectDirectory::get(cx).as_deref())
                ),
                "Open a project to edit its injected prompts.",
            ),
            Section::Sandbox => (
                "Sandbox",
                "Every run of a harness runs natively under a sandbox the \
                 operating system enforces, so a Code run can't reach the \
                 spec's source and a Spec run can't reach the code."
                    .to_string(),
                "",
            ),
            Section::Updates => (
                "Updates",
                "Suspense updates itself from its edge releases: a newer one \
                 is downloaded in the background and put in place when \
                 Suspense restarts."
                    .to_string(),
                "",
            ),
            Section::Agent => (
                "Agent",
                "Every run of the open project goes to the harness picked \
                 here: its tasks, its questions, and the application's own \
                 for it. It is the project's own, saved with it. A \
                 conversation is only carried on by the harness it began with."
                    .to_string(),
                "",
            ),
        };
        let body: AnyElement = if section == Section::Agent {
            self.render_agents(cx).into_any_element()
        } else if section == Section::Sandbox {
            self.render_sandbox(cx).into_any_element()
        } else if section == Section::Updates {
            render_updates(cx).into_any_element()
        } else if ProjectDirectory::get(cx).is_some() {
            let prompts: Vec<AnyElement> = self
                .prompts
                .iter()
                .enumerate()
                .filter(|(_, prompt)| section.holds(prompt.prompt))
                .map(|(ix, prompt)| {
                    self.render_prompt(ix, prompt, window, cx)
                        .into_any_element()
                })
                .collect();
            v_flex().gap_5().children(prompts).into_any_element()
        } else {
            div()
                .text_color(muted)
                .child(without_project)
                .into_any_element()
        };

        let heading = h_flex()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().font_semibold().child("Settings"))
            .child(
                Button::new("settings-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close the settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSettings))),
            );
        let page = div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(background)
            .text_color(foreground)
            .child(
                v_flex()
                    .gap_4()
                    .p_6()
                    .child(div().text_lg().font_semibold().child(title))
                    .child(div().text_sm().text_color(muted).child(blurb))
                    .child(body),
            );
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(background)
            .text_color(foreground)
            .child(heading)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().min_w_0().h_full().child(
                        crate::scrollbar::with_scrollbar(
                            "settings",
                            &self.scroll,
                            page,
                            true,
                            None,
                            cx,
                        ),
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::fs;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext};

    use super::{Section, SettingsWindow};
    use crate::chat_input::SendMode;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::system_prompts;
    use crate::system_prompts::Prompt;

    /// The sidebar ends with Updates, after Sandbox.
    #[test]
    fn updates_come_last() {
        let labels: Vec<&str> = Section::ALL.iter().map(|section| section.label()).collect();
        assert_eq!(
            labels,
            ["System prompts", "Injected prompts", "Agent", "Sandbox", "Updates"]
        );
    }

    fn text(
        settings: &Entity<SettingsWindow>,
        which: impl Into<Prompt>,
        cx: &mut VisualTestContext,
    ) -> String {
        let which = which.into();
        settings.read_with(cx, |this, cx| {
            let prompt = this
                .prompts
                .iter()
                .find(|prompt| prompt.prompt == which)
                .unwrap();
            prompt.editor.read(cx).value().to_string()
        })
    }

    /// The window shows each mode's prompt for the open project, saving the
    /// defaults so they can be edited by hand. An edit is saved as it is made,
    /// an edit made by hand shows once the prompts are read again, and a
    /// prompt can be put back to its default.
    #[gpui_kit::test]
    async fn edits_the_projects_system_prompts(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-settings-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let mut settings = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();

        for prompt in Prompt::ALL {
            assert_eq!(
                text(&settings, prompt, cx),
                system_prompts::default_prompt(prompt)
            );
            assert!(
                system_prompts::file(prompt, &dir).exists(),
                "{prompt:?} was not saved"
            );
        }
        // Every tab's system prompt is listed, in order, but Freeform's,
        // which sends none; nor is a file saved for it.
        let listed: Vec<Prompt> = settings.read_with(cx, |this, _| {
            this.prompts.iter().map(|p| p.prompt).collect()
        });
        assert_eq!(
            listed,
            [
                Prompt::System,
                Prompt::Mode(SendMode::Code),
                Prompt::Mode(SendMode::Both),
                Prompt::Mode(SendMode::Spec),
                Prompt::Mode(SendMode::Ask),
                Prompt::SpecReading,
                Prompt::CodeToSpec,
                Prompt::SpecToCode,
                Prompt::Resume,
            ]
        );
        assert!(!system_prompts::file(SendMode::Freeform, &dir).exists());

        settings.update_in(cx, |this, window, cx| {
            // Code's, after the system prompt's.
            this.prompts
                .iter()
                .find(|p| p.prompt == Prompt::Mode(SendMode::Code))
                .unwrap()
                .editor
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        });
        for key in "Hi.".chars() {
            cx.update(|window, cx| window.input(&key.to_string(), cx));
            cx.run_until_parked();
        }
        let typed = text(&settings, SendMode::Code, cx);
        assert!(typed.contains("Hi."), "{typed}");
        assert_eq!(system_prompts::load(SendMode::Code, &dir).unwrap(), typed);

        system_prompts::save(SendMode::Spec, "Edited by hand.", &dir).unwrap();
        settings.update_in(cx, |this, window, cx| this.load(window, cx));
        cx.run_until_parked();
        assert_eq!(text(&settings, SendMode::Spec, cx), "Edited by hand.");
        // Reading them again saves nothing over them.
        assert_eq!(system_prompts::load(SendMode::Code, &dir).unwrap(), typed);

        settings.update_in(cx, |this, window, cx| {
            this.reset(SendMode::Spec.into(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            system_prompts::load(SendMode::Spec, &dir).unwrap(),
            system_prompts::default_prompt(SendMode::Spec)
        );

        // The spec reading is edited in its own section, picked from the
        // sidebar, which stays picked for the next settings opened.
        assert_eq!(
            settings.read_with(cx, |_, cx| SettingsWindow::section(cx)),
            Section::SystemPrompts
        );
        settings.update(cx, |this, cx| this.pick(Section::InjectedPrompts, cx));
        system_prompts::save(Prompt::SpecReading, "Read it all.", &dir).unwrap();
        settings.update_in(cx, |this, window, cx| this.load(window, cx));
        cx.run_until_parked();
        assert_eq!(text(&settings, Prompt::SpecReading, cx), "Read it all.");
        settings.update_in(cx, |this, window, cx| {
            this.reset(Prompt::SpecReading, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            system_prompts::load(Prompt::SpecReading, &dir).unwrap(),
            system_prompts::default_prompt(Prompt::SpecReading)
        );
        // Beneath it, the code-to-spec prompt, saved beside the templates,
        // with a line saying what it is for, and reset in the same way.
        assert_eq!(
            system_prompts::file(Prompt::CodeToSpec, &dir),
            dir.join(".suspense/system-prompts/code-to-spec.md")
        );
        let shown = |id: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
        };
        assert!(shown("reset-spec-reading-system-prompt", cx));
        assert!(shown("reset-code-to-spec-system-prompt", cx));
        assert!(shown("code-to-spec-note", cx));
        let order: Vec<Prompt> = settings.read_with(cx, |this, _| {
            this.prompts
                .iter()
                .map(|p| p.prompt)
                .filter(|p| Section::InjectedPrompts.holds(*p))
                .collect()
        });
        assert_eq!(
            order,
            [Prompt::SpecReading, Prompt::CodeToSpec, Prompt::SpecToCode, Prompt::Resume]
        );
        system_prompts::save(Prompt::CodeToSpec, "Say ${CODE_RESULT}.", &dir).unwrap();
        settings.update_in(cx, |this, window, cx| this.load(window, cx));
        cx.run_until_parked();
        assert_eq!(
            text(&settings, Prompt::CodeToSpec, cx),
            "Say ${CODE_RESULT}."
        );
        settings.update_in(cx, |this, window, cx| {
            this.reset(Prompt::CodeToSpec, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            system_prompts::load(Prompt::CodeToSpec, &dir).unwrap(),
            system_prompts::default_prompt(Prompt::CodeToSpec)
        );
        assert_eq!(
            text(&settings, Prompt::CodeToSpec, cx),
            system_prompts::default_prompt(Prompt::CodeToSpec)
        );
        // Not among the system prompts.
        settings.update(cx, |this, cx| this.pick(Section::SystemPrompts, cx));
        assert!(!shown("reset-code-to-spec-system-prompt", cx));
        assert!(!shown("code-to-spec-note", cx));
        settings.update(cx, |this, cx| this.pick(Section::InjectedPrompts, cx));

        // Kept by the application rather than the window, so it outlives it.
        assert_eq!(
            cx.update(|_, cx| SettingsWindow::section(cx)),
            Section::InjectedPrompts
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// Letting Spec tasks read the code is the open project's, saved with it
    /// as soon as it is set, and off to start with.
    #[gpui_kit::test]
    async fn lets_spec_tasks_read_the_code(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-settings-reads-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let mut settings = None;
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        assert!(!crate::project_settings::spec_reads_code(&dir));
        settings.update(cx, |this, cx| this.set_spec_reads_code(true, cx));
        assert!(crate::project_settings::spec_reads_code(&dir));
        settings.update(cx, |this, cx| this.set_spec_reads_project(true, cx));
        assert!(crate::project_settings::spec_reads_project(&dir));
        // Unchecking the first unchecks the second.
        settings.update(cx, |this, cx| this.set_spec_reads_code(false, cx));
        assert!(!crate::project_settings::spec_reads_code(&dir));
        assert!(!crate::project_settings::ProjectSettings::load(&dir).spec_reads_project);
        // Nor can it be checked alone.
        settings.update(cx, |this, cx| this.set_spec_reads_project(true, cx));
        assert!(!crate::project_settings::ProjectSettings::load(&dir).spec_reads_project);
        fs::remove_dir_all(&dir).ok();
    }

    /// The Agent section offers each harness, the open project's picked;
    /// picking another sends the project's runs from then on to it, saved
    /// with the project, its directory standing in for HARNESS_DIRECTORY.
    /// With no project open, none is offered.
    /// The Sandbox section is a column of cards: Status and Outside the
    /// sandbox always, the project's own between them once one is open,
    /// its grid saying each mode's reach; a classification change shows in
    /// the grid at once, and is saved.
    #[gpui_kit::test]
    async fn the_sandbox_section_is_a_column_of_cards(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut settings = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        settings.update(cx, |this, cx| this.pick(Section::Sandbox, cx));
        cx.run_until_parked();
        let shown = |id: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
        };
        // No project: Status and Outside the sandbox alone.
        assert!(shown("settings-sandbox-status", cx));
        assert!(shown("settings-sandbox-outside", cx));
        assert!(!shown("settings-sandbox-reach", cx));

        let dir = std::env::temp_dir().join(format!("suspense-settings-sandbox-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        for folder in ["src", "spec", "docs", ".claude/reference"] {
            fs::create_dir_all(dir.join(folder)).unwrap();
        }
        crate::sandbox::Classification {
            code_roots: vec!["src".into()],
            spec_roots: vec!["spec".into()],
            ..crate::sandbox::Classification::default()
        }
        .save(&dir)
        .unwrap();
        cx.update(|_, cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        for card in [
            "settings-sandbox-status",
            "settings-sandbox-reach",
            "settings-sandbox-classification",
            "settings-sandbox-spec-tasks",
            "settings-sandbox-outside",
        ] {
            assert!(shown(card, cx), "no {card} card");
        }
        // The cards go top to bottom in order.
        let top = |id: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, _| window.find(id).bounds().top())
        };
        assert!(top("settings-sandbox-status", cx) < top("settings-sandbox-reach", cx));
        assert!(top("settings-sandbox-reach", cx) < top("settings-sandbox-classification", cx));
        assert!(top("settings-sandbox-spec-tasks", cx) < top("settings-sandbox-outside", cx));
        // Removing the code's folder: the Code row reads No access for Code
        // at once, and the change is saved.
        let reach = |cx: &mut VisualTestContext| {
            settings.read_with(cx, |_, cx| {
                let project = ProjectDirectory::get(cx).unwrap();
                crate::sandbox::reach::reach(
                    &[crate::sandbox::ProfileKind::Code],
                    crate::agent::Agent::Claude,
                    &project,
                )[0]
                .1[0]
                    .clone()
                    .unwrap()
                    .access
            })
        };
        assert_eq!(reach(cx), Some(crate::sandbox::Access::ReadWrite));
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("settings-code-remove-0", cx);
        });
        cx.run_until_parked();
        assert_eq!(reach(cx), None);
        assert!(crate::sandbox::Classification::load(&dir).code_roots.is_empty());
        // A cell set is marked, saved, and Reset grid puts it back; the
        // Spec column's Code row is the Read the code switch.
        use crate::sandbox::reach::{Area, Level};
        settings.update(cx, |this, cx| {
            this.set_level(crate::sandbox::ProfileKind::Code, Area::Reference, Some(Level::NoAccess), cx)
        });
        assert!(shown("settings-reach-set-2-0", cx), "the set cell isn't marked");
        assert_eq!(
            crate::sandbox::reach::level(
                &crate::sandbox::Classification::load(&dir),
                crate::sandbox::ProfileKind::Code,
                Area::Reference
            ),
            Some(Level::NoAccess)
        );
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("settings-reset-grid", cx);
        });
        cx.run_until_parked();
        assert!(crate::sandbox::Classification::load(&dir).levels.is_empty());
        assert!(!shown("settings-reach-set-2-0", cx));
        settings.update(cx, |this, cx| {
            this.set_level(crate::sandbox::ProfileKind::Spec, Area::Code, Some(Level::Read), cx)
        });
        assert!(crate::project_settings::spec_reads_code(&dir), "the grid and the switch disagree");
        assert!(crate::sandbox::Classification::load(&dir).levels.is_empty());
        // An added area is a row of its own, before Other files, saying what
        // it covers; removed, it goes.
        settings.update(cx, |this, cx| {
            this.classify(
                |classification| {
                    classification.areas.push(crate::sandbox::AddedArea {
                        name: "Docs".into(),
                        roots: vec!["docs".into()],
                        ..crate::sandbox::AddedArea::default()
                    })
                },
                cx,
            )
        });
        assert!(shown("settings-area-covered-4", cx), "no row for the added area");
        settings.update(cx, |this, cx| this.set_level(crate::sandbox::ProfileKind::Code, Area::Added(0), Some(Level::Read), cx));
        assert_eq!(
            crate::sandbox::Classification::load(&dir).areas[0].levels.get("code"),
            Some(&Level::Read)
        );
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("settings-area-remove-4", cx);
        });
        cx.run_until_parked();
        assert!(crate::sandbox::Classification::load(&dir).areas.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[gpui_kit::test]
    async fn picks_the_projects_harness(cx: &mut TestAppContext) {
        use crate::agent::{self, Agent};

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut settings = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        settings.update(cx, |this, cx| this.pick(Section::Agent, cx));
        cx.run_until_parked();
        // Runs outside any project go to Claude Code.
        assert_eq!(cx.update(|_, cx| agent::current(cx)), Agent::Claude);

        let dir = std::env::temp_dir().join(format!("suspense-settings-harness-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        cx.update(|_, cx| ProjectDirectory::set(dir.clone(), cx));
        let project = cx.update(|_, cx| ProjectDirectory::get(cx).unwrap());
        let click = |id: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.render_frame(cx);
                window.click(id, cx);
            });
            cx.run_until_parked();
        };
        click("settings-agent-codex", cx);
        assert_eq!(cx.update(|_, cx| agent::current(cx)), Agent::Codex);
        assert_eq!(crate::harness::directory(Some(&project)), ".codex");
        let saved = crate::project_settings::ProjectSettings::load(&project);
        assert_eq!(saved.harness.as_deref(), Some("codex"));
        click("settings-agent-opencode", cx);
        assert_eq!(agent::of_project(Some(&project)), Agent::OpenCode);
        click("settings-agent-claude", cx);
        assert_eq!(agent::of_project(Some(&project)), Agent::Claude);
        fs::remove_dir_all(&dir).ok();
    }

    /// Each prompt's editor is as tall as its prompt, growing as lines are
    /// added, so the whole prompt shows and only the page scrolls.
    #[gpui_kit::test]
    async fn prompt_editors_grow_to_fit(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-settings-grow-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let mut settings = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        let settings = settings.unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        let height = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                let _ = cx;
            });
            cx.run_until_parked();
            settings.update_in(cx, |this, window, cx| {
                let prompt = &this.prompts[0];
                prompt.fit.heights(&prompt.editor, window, cx).0
            })
        };
        let short = {
            settings.update_in(cx, |this, window, cx| {
                this.prompts[0]
                    .editor
                    .update(cx, |editor, cx| editor.set_value("one", window, cx))
            });
            height(cx)
        };
        settings.update_in(cx, |this, window, cx| {
            let long = (0..40)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            this.prompts[0]
                .editor
                .update(cx, |editor, cx| editor.set_value(long, window, cx))
        });
        let tall = height(cx);
        assert!(
            tall > short * 20.,
            "{tall:?} isn't forty lines tall, next to {short:?}"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
