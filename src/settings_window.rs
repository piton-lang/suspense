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
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::{self, Agent};
use crate::growing_input::GrowToFit;
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
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
    Containers,
    Updates,
}

impl Section {
    const ALL: [Section; 5] = [
        Section::SystemPrompts,
        Section::InjectedPrompts,
        Section::Agent,
        Section::Containers,
        Section::Updates,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::SystemPrompts => "System prompts",
            Section::InjectedPrompts => "Injected prompts",
            Section::Agent => "Agent",
            Section::Containers => "Containers",
            Section::Updates => "Updates",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Section::SystemPrompts => "system-prompts",
            Section::InjectedPrompts => "injected-prompts",
            Section::Agent => "agent",
            Section::Containers => "containers",
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
                    Prompt::SpecReading | Prompt::CodeToSpec | Prompt::SpecToCode
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

/// One prompt, as edited.
struct PromptEditor {
    prompt: Prompt,
    editor: Entity<EditorState>,
    /// How the editor grows to fit the whole prompt, which it never scrolls.
    fit: GrowToFit,
    /// Why the prompt could not be read or saved, until it can be.
    error: Option<SharedString>,
}

/// Where the containers runs go in stand, as the Containers section shows
/// them.
#[derive(Clone, Debug, PartialEq)]
struct Containers {
    podman: crate::container::PodmanState,
    /// Whether each harness is logged in in its container, where that can
    /// be asked: Podman running and the image built.
    logins: Vec<(Agent, Option<bool>)>,
}

pub struct SettingsWindow {
    prompts: Vec<PromptEditor>,
    /// Where the containers stand, once read.
    containers: Option<Containers>,
    _containers_read: Task<()>,
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

        let mut this = Self {
            prompts,
            containers: None,
            project_settings_error: None,
            _containers_read: Task::ready(()),
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
        self.read_containers(project_dir, cx);
        cx.notify();
    }

    /// Reads, in the background, whether Podman can run containers and
    /// whether each harness is logged in in its container.
    fn read_containers(&mut self, project_dir: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        let read = cx.background_spawn(async move {
            let platform = crate::container::Platform::current();
            let podman = crate::container::podman_state(platform);
            let image = matches!(podman, crate::container::PodmanState::Ready(_))
                .then(|| crate::container::built_image(project_dir.as_deref()?))
                .flatten();
            let logins = Agent::RUNNABLE
                .into_iter()
                .map(|agent| {
                    let logged_in = image
                        .as_deref()
                        .and_then(|image| crate::container::logged_in(agent, image, platform).ok());
                    (agent, logged_in)
                })
                .collect();
            Containers { podman, logins }
        });
        self._containers_read = cx.spawn(async move |this, cx| {
            let containers = read.await;
            this.update(cx, |this, cx| {
                this.containers = Some(containers);
                cx.notify();
            })
            .ok();
        });
    }

    /// Logs `agent` in again in its container, in the login view.
    fn log_in_again(&mut self, agent: Agent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        crate::harness::forget_login(agent);
        let this = cx.entity().downgrade();
        crate::login_view::LoginView::open(
            agent,
            project_dir.clone(),
            window,
            cx,
            move |window, cx| {
                window.close_dialog(cx);
                this.update(cx, |this, cx| {
                    this.read_containers(Some(project_dir.clone()), cx)
                })
                .ok();
            },
        );
    }

    /// Whether Podman can run the containers, and each harness's login in
    /// its container.
    fn render_containers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::container::{MachineAction, Platform, PodmanState};
        let muted = cx.theme().muted_foreground;
        let platform = Platform::current();
        let podman = match self.containers.as_ref().map(|containers| &containers.podman) {
            None => div().text_color(muted).child("Checking Podman…").into_any_element(),
            Some(PodmanState::Ready(version)) => div()
                .child(format!("Podman {version}, ready to run containers."))
                .into_any_element(),
            Some(PodmanState::Missing) => v_flex()
                .gap_1()
                .child("Podman isn't installed, so Spec tasks, a Chain prompt's spec steps, and questions can't run.")
                .child(
                    Button::new("settings-install-podman")
                        .small()
                        .label("Install Podman")
                        .on_click(move |_, _, cx| cx.open_url(platform.install_url())),
                )
                .into_any_element(),
            Some(state) => {
                let action = if *state == PodmanState::NoMachine {
                    MachineAction::SetUp
                } else {
                    MachineAction::Start
                };
                v_flex()
                    .gap_1()
                    .child(if action == MachineAction::SetUp {
                        "Podman has no machine to run containers in."
                    } else {
                        "Podman's machine isn't running."
                    })
                    .child(
                        Button::new("settings-podman-machine")
                            .small()
                            .label(action.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let project_dir = ProjectDirectory::get(cx);
                                let run = cx.background_spawn(async move {
                                    crate::container::run_machine_action(action, &mut |_| {}).ok();
                                });
                                this._containers_read = cx.spawn(async move |this, cx| {
                                    run.await;
                                    this.update(cx, |this, cx| this.read_containers(project_dir, cx))
                                        .ok();
                                });
                            })),
                    )
                    .into_any_element()
            }
        };
        let logins = v_flex()
            .gap_2()
            .children(Agent::RUNNABLE.into_iter().map(|agent| {
                let state = self
                    .containers
                    .as_ref()
                    .and_then(|containers| {
                        containers
                            .logins
                            .iter()
                            .find(|(known, _)| *known == agent)
                            .map(|(_, logged_in)| *logged_in)
                    })
                    .flatten();
                h_flex()
                    .gap_2()
                    .child(div().w(px(120.)).child(agent.label()))
                    .child(
                        div()
                            .w(px(120.))
                            .text_sm()
                            .text_color(muted)
                            .child(match state {
                                Some(true) => "Logged in",
                                Some(false) => "Not logged in",
                                None => "Unknown",
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-log-in-{}",
                            agent.command()
                        )))
                        .small()
                        .label("Log in again")
                        .disabled(ProjectDirectory::get(cx).is_none())
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.log_in_again(agent, window, cx),
                        )),
                    )
            }));
        // For the open project alone: whether its Spec runs may read the code.
        let reads_code = ProjectDirectory::get(cx).map(|project_dir| {
            let settings = crate::project_settings::ProjectSettings::load(&project_dir);
            let reads = settings.spec_reads_code;
            // Offered only while they may read the code.
            let whole = reads.then(|| {
                v_flex()
                    .pl_6()
                    .gap_1()
                    .child(
                        crate::checkbox::checkbox(
                            "settings-spec-reads-project",
                            "Also let them read the whole project",
                        )
                        .checked(settings.spec_reads_project)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_spec_reads_project(*checked, cx)
                        })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(
                                "The whole project directory is mounted read only, but for \
                                 its .git and .suspense folders.",
                            ),
                    )
            });
            v_flex()
                .gap_1()
                .child(
                    crate::checkbox::checkbox("settings-spec-reads-code", "Let Spec tasks read the code")
                        .checked(reads)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_spec_reads_code(*checked, cx)
                        })),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(
                            "The code is mounted read only, so Spec tasks and a Chain prompt's \
                             spec steps can read it but never change it.",
                        ),
                )
                .children(whole)
                .children(self.project_settings_error.clone().map(|error| {
                    div().text_sm().text_color(cx.theme().danger).child(error)
                }))
        });
        v_flex().gap_5().children(reads_code).child(podman).child(
            v_flex()
                .gap_2()
                .child(div().font_semibold().child("Logins"))
                .child(logins),
        )
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
            Section::Containers => (
                "Containers",
                "Spec tasks, a Chain prompt's spec steps, and questions always \
                 run in a container holding only what their mode may use, so \
                 a spec run can't read or change the code. Code tasks always \
                 run on the host."
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
        } else if section == Section::Containers {
            self.render_containers(cx).into_any_element()
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

    /// The sidebar ends with Updates, after Containers.
    #[test]
    fn updates_come_last() {
        let labels: Vec<&str> = Section::ALL.iter().map(|section| section.label()).collect();
        assert_eq!(
            labels,
            ["System prompts", "Injected prompts", "Agent", "Containers", "Updates"]
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
            [Prompt::SpecReading, Prompt::CodeToSpec, Prompt::SpecToCode]
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
