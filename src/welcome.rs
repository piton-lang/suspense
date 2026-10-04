//! The welcome page, as the WelcomeScope says: a checklist of what Suspense
//! needs, each check run in the background, saying what isn't in place, why
//! it is needed, and how to fix it, with the action that fixes it where
//! Suspense can take one. It opens at launch while something needs
//! attention, and from the Application tab's Welcome command at any time.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::{self, Agent};
use crate::container::{self, MachineAction, Platform, PodmanState};
use crate::project_directory::ProjectDirectory;

actions!(suspense, [OpenWelcome]);

/// Emitted when the welcome page is closed.
pub struct CloseWelcome;

/// Emitted by Get started, which closes the page, the walkthrough
/// following while it has never been taken.
pub struct GetStarted;

/// Emitted to choose another harness, in the settings' Agent section.
pub struct ChooseHarness;

/// Emitted once every check has run: whether Suspense is ready.
pub struct Checked {
    pub ready: bool,
}

/// How long a check's command may take before the check fails.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// A check, in the order they are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    Git,
    Piton,
    Harness,
    Podman,
    PodmanMachine,
    Containers,
    HarnessLogin,
}

impl Check {
    /// The checks on `platform`, in order: the Podman machine only where
    /// Podman runs one.
    pub fn all(platform: Platform) -> Vec<Check> {
        [
            Check::Git,
            Check::Piton,
            Check::Harness,
            Check::Podman,
            Check::PodmanMachine,
            Check::Containers,
            Check::HarnessLogin,
        ]
        .into_iter()
        .filter(|check| *check != Check::PodmanMachine || platform.has_machine())
        .collect()
    }

    pub fn name(self) -> &'static str {
        match self {
            Check::Git => "Git",
            Check::Piton => "Piton",
            Check::Harness => "Harness",
            Check::Podman => "Podman",
            Check::PodmanMachine => "Podman machine",
            Check::Containers => "Containers",
            Check::HarnessLogin => "Harness login",
        }
    }

    /// The checks it waits for, among `checks`: the machine for Podman,
    /// Containers for every check above it, the login for Containers.
    pub fn waits_for(self, checks: &[Check]) -> Vec<Check> {
        match self {
            Check::PodmanMachine => vec![Check::Podman],
            Check::Containers => checks
                .iter()
                .copied()
                .take_while(|check| *check != Check::Containers)
                .collect(),
            Check::HarnessLogin => vec![Check::Containers],
            _ => Vec::new(),
        }
    }

    /// What it says, muted, while it waits for the checks before it.
    fn waiting(self) -> &'static str {
        match self {
            Check::PodmanMachine => "Needs Podman",
            Check::HarnessLogin => "Needs Containers",
            _ => "Needs the checks above",
        }
    }
}

/// What fixes a failing check, where Suspense can take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// A page opened in the browser, with the button's label.
    Link(&'static str, String),
    /// Setting up or starting Podman's machine.
    Machine(MachineAction),
    /// Logging the harness in, in its container.
    LogIn,
}

/// How a check stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for the checks it needs.
    Waiting,
    Checking,
    /// Passed, with what was found, as a version.
    Passed(Option<String>),
    /// Failed: what was found, if anything, why, and what fixes it.
    Failed {
        found: Option<String>,
        why: String,
        fixes: Vec<Fix>,
    },
}

/// Runs a check. Blocking: called off the UI thread.
pub type Probe = Arc<dyn Fn(Check, Agent, Platform, Option<PathBuf>) -> Status + Send + Sync>;

/// Runs `check` for real, with `agent` the harness in use, on `platform`,
/// in the project `project_dir`, if one is open. Blocking.
pub fn probe(check: Check, agent: Agent, platform: Platform, project_dir: Option<PathBuf>) -> Status {
    match check {
        Check::Git => found_or(
            version_of("git"),
            "Git isn't installed, or isn't on the PATH. Suspense needs it, since projects are git repositories. Install it, then check again.",
            vec![Fix::Link("Download Git", "https://git-scm.com/downloads".into())],
        ),
        Check::Piton => found_or(
            version_of("piton"),
            "Piton isn't installed, or isn't on the PATH. Suspense needs it to build, slice, and check the spec. Install it, then check again.",
            vec![Fix::Link(
                "Install Piton",
                "https://github.com/piton-lang/piton-rs".into(),
            )],
        ),
        Check::Harness => found_or(
            version_of(agent.command()),
            &format!(
                "{} isn't installed: `{}` couldn't be run. Suspense needs it, since Code tasks run it on the host. Install it, or choose another harness.",
                agent.label(),
                agent.command()
            ),
            vec![Fix::Link("Install", agent::install_url(agent).into())],
        ),
        Check::Podman => found_or(
            version_of(container::podman()),
            "Podman isn't installed, or isn't on the PATH. Suspense needs it, since Spec tasks, a Chain prompt's spec steps, and questions run in a container. Install it, then check again.",
            vec![Fix::Link("Install Podman", platform.install_url().into())],
        ),
        Check::PodmanMachine => match within(move || container::podman_state(platform)) {
            Err(why) => failed(why, Vec::new()),
            Ok(PodmanState::Ready(_)) => Status::Passed(Some("Running".into())),
            Ok(PodmanState::NoMachine) => failed(
                "Podman has no machine to run containers in. Suspense needs one running for its containers.".into(),
                vec![Fix::Machine(MachineAction::SetUp)],
            ),
            Ok(PodmanState::MachineStopped) => failed(
                "Podman's machine isn't running. Suspense needs it running for its containers.".into(),
                vec![Fix::Machine(MachineAction::Start)],
            ),
            Ok(PodmanState::Missing) => failed("Podman isn't installed.".into(), Vec::new()),
        },
        Check::Containers => match within(container::check_access) {
            Err(why) => failed(why, Vec::new()),
            Ok(Ok(())) => Status::Passed(None),
            Ok(Err(said)) => failed(
                format!("Podman couldn't be accessed, so Suspense can't run its containers. Podman said: {said}"),
                Vec::new(),
            ),
        },
        Check::HarnessLogin => {
            let dir = login_dir(project_dir);
            let read = within(move || {
                let Some(image) = container::built_image(&dir) else {
                    return Ok(None);
                };
                container::logged_in(agent, &image, platform).map(Some)
            });
            let log_in = vec![Fix::LogIn];
            match read {
                Err(why) => failed(why, Vec::new()),
                Ok(Ok(Some(true))) => Status::Passed(Some("Logged in".into())),
                Ok(Ok(Some(false))) => failed(
                    format!(
                        "{} isn't logged in in its container, so it can't run Spec tasks or questions. Log in once; it stays logged in for every project.",
                        agent.label()
                    ),
                    log_in,
                ),
                Ok(Ok(None)) => failed(
                    format!(
                        "The container {} runs in hasn't been prepared yet, so its login can't be read. Log in to prepare it and log in.",
                        agent.label()
                    ),
                    log_in,
                ),
                Ok(Err(err)) => failed(format!("{err:#}"), log_in),
            }
        }
    }
}

/// Where a login reads its image from: the project open, else any
/// directory, for the default image.
pub fn login_dir(project_dir: Option<PathBuf>) -> PathBuf {
    project_dir.unwrap_or_else(std::env::temp_dir)
}

fn failed(why: String, fixes: Vec<Fix>) -> Status {
    Status::Failed {
        found: None,
        why,
        fixes,
    }
}

/// Passed with the version found, or failed as `why` says.
fn found_or(version: Result<String, String>, why: &str, fixes: Vec<Fix>) -> Status {
    match version {
        Ok(version) => Status::Passed(Some(version)),
        Err(err) if err.contains("didn't answer") => failed(err, fixes),
        Err(_) => failed(why.to_string(), fixes),
    }
}

/// Runs `f` on a thread of its own, giving up once it has taken longer than
/// [`TIMEOUT`].
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(f()).ok();
    });
    rx.recv_timeout(TIMEOUT).map_err(|_| didnt_answer())
}

fn didnt_answer() -> String {
    format!("It didn't answer within {} seconds.", TIMEOUT.as_secs())
}

/// The version `program --version` reports, without a console window, and
/// stopped once it has taken longer than [`TIMEOUT`].
fn version_of(program: impl AsRef<std::ffi::OsStr>) -> Result<String, String> {
    let mut child = crate::process::command(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            child.kill().ok();
            child.wait().ok();
            return Err(didnt_answer());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        std::io::Read::read_to_string(&mut stdout, &mut out).ok();
    }
    if !status.success() {
        return Err(format!("it ended with {status}"));
    }
    Ok(version_in(&out))
}

/// The version in what `--version` printed: its first word that reads as
/// one, else its first line.
pub fn version_in(printed: &str) -> String {
    let first = printed.lines().next().unwrap_or_default().trim();
    first
        .split_whitespace()
        .map(|word| word.trim_start_matches('v'))
        .find(|word| word.contains('.') && word.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(first)
        .to_string()
}

/// How many checks need attention, of those listed: every one not passed.
pub fn needing_attention(statuses: &[(Check, Status)]) -> usize {
    statuses
        .iter()
        .filter(|(_, status)| !matches!(status, Status::Passed(_)))
        .count()
}

/// Whether to show the welcome page at launch while something needs
/// attention, remembered for the user, as the UserPreferencesScope says; on
/// until turned off. Tests neither read nor write it.
pub mod preference {
    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(dirs::config_dir()?.join("suspense").join("show-welcome"))
    }

    pub fn load() -> bool {
        #[cfg(not(test))]
        if let Some(file) = file()
            && std::fs::read_to_string(file).is_ok_and(|text| text.trim() == "off")
        {
            return false;
        }
        true
    }

    /// Saves the choice; it is only a convenience, so failing to is ignored.
    pub fn save(on: bool) {
        #[cfg(not(test))]
        if let Some(file) = file() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(file, if on { "on" } else { "off" }).ok();
        }
        #[cfg(test)]
        let _ = on;
    }
}

/// The welcome page.
pub struct WelcomeView {
    platform: Platform,
    probe: Probe,
    statuses: Vec<(Check, Status)>,
    /// Counts the runs, so a check run before Check again is forgotten.
    generation: usize,
    /// What a Podman machine action prints, as it runs.
    machine_output: Vec<String>,
    machine_running: bool,
    show_at_launch: bool,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<CloseWelcome> for WelcomeView {}
impl EventEmitter<GetStarted> for WelcomeView {}
impl EventEmitter<ChooseHarness> for WelcomeView {}
impl EventEmitter<Checked> for WelcomeView {}

impl Focusable for WelcomeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WelcomeView {
    /// The page, its checks run for real, started straight away.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_probe(Platform::current(), Arc::new(probe), cx)
    }

    /// The page, its checks run by `probe`, started straight away.
    pub fn with_probe(platform: Platform, probe: Probe, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            platform,
            probe,
            statuses: Vec::new(),
            generation: 0,
            machine_output: Vec::new(),
            machine_running: false,
            show_at_launch: preference::load(),
            focus_handle: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        this.check_again(cx);
        this
    }

    /// How each check stands, in order.
    #[cfg(test)]
    pub fn statuses(&self) -> &[(Check, Status)] {
        &self.statuses
    }

    /// Whether every check passes.
    pub fn is_ready(&self) -> bool {
        !self.statuses.is_empty() && needing_attention(&self.statuses) == 0
    }

    fn is_checking(&self) -> bool {
        self.statuses
            .iter()
            .any(|(_, status)| matches!(status, Status::Waiting | Status::Checking))
            && self
                .statuses
                .iter()
                .any(|(_, status)| *status == Status::Checking)
    }

    /// Runs every check anew.
    pub fn check_again(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self._tasks.clear();
        self.statuses = Check::all(self.platform)
            .into_iter()
            .map(|check| (check, Status::Waiting))
            .collect();
        self.advance(cx);
    }

    /// Runs `check` again, and the checks waiting on it.
    pub fn check_from(&mut self, check: Check, cx: &mut Context<Self>) {
        let checks: Vec<Check> = self.statuses.iter().map(|(check, _)| *check).collect();
        let mut again = vec![check];
        // Everything waiting on what runs again, however far down.
        loop {
            let more: Vec<Check> = checks
                .iter()
                .copied()
                .filter(|later| {
                    !again.contains(later)
                        && later.waits_for(&checks).iter().any(|dep| again.contains(dep))
                })
                .collect();
            if more.is_empty() {
                break;
            }
            again.extend(more);
        }
        for (check, status) in &mut self.statuses {
            if again.contains(check) {
                *status = Status::Waiting;
            }
        }
        self.advance(cx);
    }

    /// Starts each check waiting whose checks before it have all passed,
    /// all at once; once nothing more runs, says whether Suspense is ready.
    fn advance(&mut self, cx: &mut Context<Self>) {
        let checks: Vec<Check> = self.statuses.iter().map(|(check, _)| *check).collect();
        let passed = |check: Check, statuses: &[(Check, Status)]| {
            statuses
                .iter()
                .any(|(c, status)| *c == check && matches!(status, Status::Passed(_)))
        };
        let ready: Vec<Check> = self
            .statuses
            .iter()
            .filter(|(check, status)| {
                *status == Status::Waiting
                    && check
                        .waits_for(&checks)
                        .iter()
                        .all(|dep| passed(*dep, &self.statuses))
            })
            .map(|(check, _)| *check)
            .collect();
        let agent = agent::current();
        let project_dir = ProjectDirectory::get(cx);
        for check in ready {
            self.set(check, Status::Checking);
            let probe = self.probe.clone();
            let (platform, project_dir) = (self.platform, project_dir.clone());
            let run = cx.background_spawn(async move { probe(check, agent, platform, project_dir) });
            let generation = self.generation;
            self._tasks.push(cx.spawn(async move |this, cx| {
                let status = run.await;
                this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.set(check, status);
                        this.advance(cx);
                    }
                })
                .ok();
            }));
        }
        if !self
            .statuses
            .iter()
            .any(|(_, status)| *status == Status::Checking)
        {
            cx.emit(Checked {
                ready: self.is_ready(),
            });
        }
        cx.notify();
    }

    fn set(&mut self, check: Check, status: Status) {
        if let Some((_, slot)) = self.statuses.iter_mut().find(|(c, _)| *c == check) {
            *slot = status;
        }
    }

    /// Sets up or starts Podman's machine, showing what it prints, then
    /// checks the machine again, and what waits on it.
    fn run_machine(&mut self, action: MachineAction, cx: &mut Context<Self>) {
        if self.machine_running {
            return;
        }
        self.machine_running = true;
        self.machine_output.clear();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let run = cx.background_spawn(async move {
            container::run_machine_action(action, &mut |line| {
                tx.send(line).ok();
            })
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let mut run = std::pin::pin!(run);
            let result = loop {
                let tick = cx.background_executor().timer(Duration::from_millis(100));
                match futures::future::select(run.as_mut(), std::pin::pin!(tick)).await {
                    futures::future::Either::Left((result, _)) => break result,
                    futures::future::Either::Right(_) => {
                        let lines: Vec<String> = rx.try_iter().collect();
                        if !lines.is_empty() {
                            this.update(cx, |this, cx| {
                                this.machine_output.extend(lines);
                                cx.notify();
                            })
                            .ok();
                        }
                    }
                }
            };
            this.update(cx, |this, cx| {
                this.machine_output.extend(rx.try_iter());
                if let Err(err) = result {
                    this.machine_output.push(format!("{err:#}"));
                }
                this.machine_running = false;
                this.check_from(Check::PodmanMachine, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Logs the harness in, in the login view, checking its login again,
    /// and what waits on it, once it is.
    fn log_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = agent::current();
        crate::harness::forget_login(agent);
        let this = cx.entity().downgrade();
        crate::login_view::LoginView::open(
            agent,
            login_dir(ProjectDirectory::get(cx)),
            window,
            cx,
            move |window, cx| {
                use gpui_kit::component::WindowExt as _;
                window.close_dialog(cx);
                this.update(cx, |this, cx| this.check_from(Check::HarnessLogin, cx))
                    .ok();
            },
        );
    }

    fn render_fix(&self, check: Check, fix: &Fix, cx: &mut Context<Self>) -> AnyElement {
        let id = |what: &str| SharedString::from(format!("welcome-{}-{what}", check.name()));
        match fix {
            Fix::Link(label, url) => {
                let url = url.clone();
                Button::new(id("link"))
                    .small()
                    .label(*label)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            Fix::Machine(action) => {
                let action = *action;
                Button::new(id("machine"))
                    .small()
                    .primary()
                    .label(action.label())
                    .loading(self.machine_running)
                    .disabled(self.machine_running)
                    .on_click(cx.listener(move |this, _, _, cx| this.run_machine(action, cx)))
                    .into_any_element()
            }
            Fix::LogIn => Button::new(id("log-in"))
                .small()
                .primary()
                .label("Log in")
                .on_click(cx.listener(|this, _, window, cx| this.log_in(window, cx)))
                .into_any_element(),
        }
    }

    fn render_check(&self, ix: usize, check: Check, status: &Status, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, success, danger, warning) = (
            theme.muted_foreground,
            theme.success,
            theme.danger,
            theme.warning,
        );
        let _ = warning;
        let icon: AnyElement = match status {
            Status::Checking => Spinner::new().small().into_any_element(),
            Status::Waiting => Icon::new(IconName::Circle)
                .small()
                .text_color(muted)
                .into_any_element(),
            Status::Passed(_) => Icon::new(IconName::CircleCheck)
                .small()
                .text_color(success)
                .into_any_element(),
            // Every check is required, so a failing one is a red cross.
            Status::Failed { .. } => Icon::new(IconName::CircleX)
                .small()
                .text_color(danger)
                .into_any_element(),
        };
        let found = match status {
            Status::Waiting => Some(check.waiting().to_string()),
            Status::Checking => Some("Checking…".to_string()),
            Status::Passed(found) => found.clone(),
            Status::Failed { found, .. } => found.clone(),
        };
        let guidance = match status {
            Status::Failed { why, fixes, .. } => {
                let mut fixes: Vec<AnyElement> =
                    fixes.iter().map(|fix| self.render_fix(check, fix, cx)).collect();
                if check == Check::Harness {
                    fixes.push(
                        Button::new("welcome-choose-harness")
                            .small()
                            .label("Choose another harness")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChooseHarness)))
                            .into_any_element(),
                    );
                }
                Some(
                    v_flex()
                        .gap_2()
                        .pl_6()
                        .child(div().text_sm().child(why.clone()))
                        .when(!fixes.is_empty(), |guidance| {
                            guidance.child(h_flex().gap_2().children(fixes))
                        }),
                )
            }
            _ => None,
        };
        let output = (check == Check::PodmanMachine && !self.machine_output.is_empty()).then(|| {
            let theme = cx.theme();
            v_flex()
                .ml_6()
                .p_2()
                .rounded(theme.radius)
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .children(self.machine_output.iter().map(|line| div().child(line.clone())))
        });
        v_flex()
            .id(("welcome-check", ix))
            .gap_2()
            .py_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_none().size_4().child(icon))
                    .child(div().font_medium().child(check.name()))
                    .children(found.map(|found| div().text_sm().text_color(muted).child(found))),
            )
            .children(guidance)
            .children(output)
            .into_any_element()
    }
}

impl Render for WelcomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, muted, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
        );
        let heading = h_flex()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().font_semibold().child("Welcome to Suspense"))
            .child(
                Button::new("welcome-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close the welcome page")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseWelcome))),
            );
        let statuses = self.statuses.clone();
        let rows: Vec<AnyElement> = statuses
            .iter()
            .enumerate()
            .map(|(ix, (check, status))| self.render_check(ix, *check, status, cx))
            .collect();
        let checking = self.is_checking();
        let ready_line: AnyElement = if checking {
            div().text_color(muted).child("Checking…").into_any_element()
        } else if self.is_ready() {
            h_flex()
                .gap_3()
                .child(div().font_medium().child("Suspense is ready"))
                .child(
                    Button::new("welcome-get-started")
                        .primary()
                        .small()
                        .label("Get started")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(GetStarted))),
                )
                .into_any_element()
        } else {
            let count = needing_attention(&self.statuses);
            div()
                .font_medium()
                .child(if count == 1 {
                    "1 thing needs attention".to_string()
                } else {
                    format!("{count} things need attention")
                })
                .into_any_element()
        };
        let foot = v_flex()
            .gap_3()
            .pt_3()
            .border_t_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("welcome-check-again")
                            .small()
                            .label("Check again")
                            .on_click(cx.listener(|this, _, _, cx| this.check_again(cx))),
                    )
                    .child(ready_line),
            )
            .child(
                crate::checkbox::checkbox(
                    "welcome-show-at-launch",
                    "Show at launch when something needs attention",
                )
                .checked(self.show_at_launch)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.show_at_launch = *checked;
                    preference::save(*checked);
                    cx.notify();
                })),
            );
        let page = v_flex()
            .id("welcome")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(div().text_sm().text_color(muted).child(
                "Suspense runs coding harnesses on a project's code and spec. \
                 Here it checks that what it needs is in place.",
            ))
            .child(v_flex().children(rows))
            .child(foot);
        let view = v_flex()
            .id("welcome-view")
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(background)
            .text_color(foreground)
            .child(heading)
            .child(page);
        gpui_kit::TestSupportExt::test_support(view)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{Check, Fix, Status, WelcomeView, needing_attention, version_in};
    use crate::container::Platform;

    #[test]
    fn versions_are_read_from_what_was_printed() {
        assert_eq!(version_in("git version 2.45.1\n"), "2.45.1");
        assert_eq!(version_in("2.0.14 (Claude Code)"), "2.0.14");
        assert_eq!(version_in("podman version 5.2.0"), "5.2.0");
        assert_eq!(version_in("piton v0.4.0"), "0.4.0");
        assert_eq!(version_in("something"), "something");
    }

    #[test]
    fn checks_go_in_order_and_wait_for_those_before_them() {
        let linux = Check::all(Platform::Linux);
        assert!(!linux.contains(&Check::PodmanMachine));
        let mac = Check::all(Platform::MacOs);
        assert_eq!(
            mac,
            [
                Check::Git,
                Check::Piton,
                Check::Harness,
                Check::Podman,
                Check::PodmanMachine,
                Check::Containers,
                Check::HarnessLogin
            ]
        );
        assert_eq!(Check::PodmanMachine.waits_for(&mac), [Check::Podman]);
        assert_eq!(Check::Containers.waits_for(&mac).len(), 5);
        assert_eq!(Check::HarnessLogin.waits_for(&mac), [Check::Containers]);
    }

    /// Checks run, those waiting on a failed one never do, and the page says
    /// how many need attention; once fixed, a check runs again with those
    /// waiting on it, and Suspense is ready.
    #[gpui_kit::test]
    async fn checks_run_in_order_and_again_once_fixed(cx: &mut TestAppContext) {
        let machine_up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: super::Probe = {
            let machine_up = machine_up.clone();
            Arc::new(move |check, _, _, _| match check {
                Check::PodmanMachine if !machine_up.load(std::sync::atomic::Ordering::SeqCst) => {
                    Status::Failed {
                        found: None,
                        why: "stopped".into(),
                        fixes: vec![Fix::Machine(crate::container::MachineAction::Start)],
                    }
                }
                _ => Status::Passed(Some("1.0".into())),
            })
        };
        cx.update(crate::project_directory::ProjectDirectory::init);
        let view = cx.new(|cx| WelcomeView::with_probe(Platform::MacOs, probe, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let statuses = view.statuses();
            assert!(matches!(statuses[0].1, Status::Passed(_)));
            assert!(matches!(statuses[4].1, Status::Failed { .. }));
            // Containers and the login wait for the machine.
            assert_eq!(statuses[5].1, Status::Waiting);
            assert_eq!(statuses[6].1, Status::Waiting);
            assert_eq!(needing_attention(statuses), 3);
            assert!(!view.is_ready());
        });
        machine_up.store(true, std::sync::atomic::Ordering::SeqCst);
        view.update(cx, |view, cx| view.check_from(Check::PodmanMachine, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.is_ready()));
    }
}
