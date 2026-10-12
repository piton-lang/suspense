//! The welcome page, as the WelcomeScope says: a checklist of what Suspense
//! needs, each check run in the background, saying what isn't in place, why
//! it is needed, and how to fix it, with the action that fixes it where
//! Suspense can take one. It opens at launch while something needs
//! attention, and from the Application tab's Welcome command at any time.

use crate::process::Logged as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::{self, Agent};
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
    Sandbox,
    HarnessLogin,
}

impl Check {
    /// The checks, in order.
    pub fn all() -> Vec<Check> {
        vec![
            Check::Git,
            Check::Piton,
            Check::Harness,
            Check::Sandbox,
            Check::HarnessLogin,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            Check::Git => "Git",
            Check::Piton => "Piton",
            Check::Harness => "Harness",
            Check::Sandbox => "Sandbox",
            Check::HarnessLogin => "Harness login",
        }
    }

    /// The checks it waits for, among `checks`: the login for the harness.
    pub fn waits_for(self, checks: &[Check]) -> Vec<Check> {
        match self {
            Check::HarnessLogin if checks.contains(&Check::Harness) => vec![Check::Harness],
            _ => Vec::new(),
        }
    }

    /// What it says, muted, while it waits for the checks before it.
    fn waiting(self) -> &'static str {
        match self {
            Check::HarnessLogin => "Needs the harness",
            _ => "Needs the checks above",
        }
    }
}

/// What fixes a failing check, where Suspense can take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// A page opened in the browser, with the button's label.
    Link(&'static str, String),
    /// A terminal opened in the project to run the harness's own login
    /// command, as named.
    OpenTerminal(&'static str),
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
pub type Probe = Arc<dyn Fn(Check, Agent, Option<PathBuf>) -> Status + Send + Sync>;

/// Runs `check` for real, with `agent` the harness in use, in the project
/// `project_dir`, if one is open. Blocking.
pub fn probe(check: Check, agent: Agent, project_dir: Option<PathBuf>) -> Status {
    let _ = &project_dir;
    match check {
        Check::Git => found_or(
            "git",
            "Git",
            "Suspense needs it, since projects are git repositories. Install it, then check again.",
            vec![Fix::Link("Download Git", "https://git-scm.com/downloads".into())],
        ),
        Check::Piton => found_or(
            "piton",
            "Piton",
            "Suspense needs it to build, slice, and check the spec. Install it, then check again.",
            vec![Fix::Link(
                "Install Piton",
                "https://github.com/piton-lang/piton-rs".into(),
            )],
        ),
        Check::Harness => found_or(
            agent.command(),
            agent.label(),
            "Suspense needs it, since every task and question runs it on the host. Install it, or choose another harness.",
            vec![Fix::Link("Install", agent::install_url(agent).into())],
        ),
        // The platform's sandbox backend can enforce the profiles, or no
        // task or question will run; never offered to run without.
        Check::Sandbox => match within(crate::sandbox::status) {
            Err(why) => failed(why, Vec::new()),
            Ok(Ok(enforces)) => Status::Passed(Some(enforces)),
            Ok(Err(refused)) => failed(
                format!(
                    "{}. No task or question will run until the sandbox can.",
                    refused.message
                ),
                vec![Fix::Link("Sandbox documentation", crate::sandbox::DOCUMENTATION.into())],
            ),
        },
        // Logged in on the host, as its own status command says.
        Check::HarnessLogin => match within(move || crate::harness_login::logged_in(agent)) {
            Err(why) => failed(why, Vec::new()),
            Ok(Ok(true)) => Status::Passed(Some("Logged in".into())),
            Ok(Ok(false)) => failed(
                format!(
                    "{} isn't logged in. Log in with `{}`; it stays logged in for every project.",
                    agent.label(),
                    crate::harness_login::login_command(agent)
                ),
                vec![Fix::OpenTerminal(crate::harness_login::login_command(agent))],
            ),
            Ok(Err(err)) => failed(format!("{err:#}"), Vec::new()),
        },
    }
}

/// Where a login's terminal opens: the project open, else the home folder.
pub fn login_dir(project_dir: Option<PathBuf>) -> PathBuf {
    project_dir
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
}

fn failed(why: String, fixes: Vec<Fix>) -> Status {
    Status::Failed {
        found: None,
        why,
        fixes,
    }
}

/// Passed with the version `program` reports and where it was found, or
/// failed saying it couldn't be found, naming every folder searched, or
/// that it was found but won't run, with what it printed, then `needs`.
/// `program` is looked for again, so one installed since is found.
fn found_or(program: &str, name: &str, needs: &str, fixes: Vec<Fix>) -> Status {
    crate::programs::forget();
    let Some(path) = crate::programs::find(program) else {
        return failed(
            format!("{} {needs}", crate::programs::not_found(program)),
            fixes,
        );
    };
    let at = path.display().to_string();
    match version_of(program) {
        Ok(version) => Status::Passed(Some(format!("{version} · {at}"))),
        Err(said) => Status::Failed {
            found: Some(at.clone()),
            why: format!("{name} was found at `{at}`, but it won't run: {said} {needs}"),
            fixes,
        },
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
/// stopped once it has taken longer than [`TIMEOUT`]; else what went wrong,
/// with what it printed.
fn version_of(program: impl AsRef<std::ffi::OsStr>) -> Result<String, String> {
    let mut child = crate::process::command(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_logged()
        .map_err(|err| err.to_string())?;
    // Read as it runs, so a full pipe never holds it up.
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = String::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_string(&mut out).ok();
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = read(child.stderr.take().map(|pipe| Box::new(pipe) as _));
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
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if !status.success() {
        let printed = [err.trim(), out.trim()]
            .into_iter()
            .filter(|printed| !printed.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(if printed.is_empty() {
            format!("`--version` ended with {status}, printing nothing.")
        } else {
            format!("`--version` ended with {status}, printing: {printed}")
        });
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
    probe: Probe,
    statuses: Vec<(Check, Status)>,
    /// Counts the runs, so a check run before Check again is forgotten.
    generation: usize,
    /// Why a terminal couldn't be opened, when it couldn't.
    terminal_error: Option<String>,
    show_at_launch: bool,
    /// The project, if any, and its harness, the harness checks last ran
    /// for: run again by themselves once either changes.
    checked_for: Option<(Option<PathBuf>, Agent)>,
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
        Self::with_probe(Arc::new(probe), cx)
    }

    /// The page, its checks run by `probe`, started straight away.
    pub fn with_probe(probe: Probe, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            probe,
            statuses: Vec::new(),
            generation: 0,
            terminal_error: None,
            show_at_launch: preference::load(),
            checked_for: None,
            focus_handle: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        // Another project opened, the harness checks follow it.
        cx.observe_global::<ProjectDirectory>(|this, cx| this.follow_harness(cx))
            .detach();
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
        self.statuses = Check::all()
            .into_iter()
            .map(|check| (check, Status::Waiting))
            .collect();
        self.advance(cx);
    }

    /// What `check`'s row is called: the harness checks named for the open
    /// project's harness, or, with none open, the one a new project starts
    /// with, saying so.
    fn label(&self, check: Check, cx: &App) -> String {
        let project_dir = ProjectDirectory::get(cx);
        let agent = agent::of_project(project_dir.as_deref());
        let harness = match project_dir {
            Some(_) => agent.label().to_string(),
            None => format!("{} (for new projects)", agent.label()),
        };
        match check {
            Check::Harness => harness,
            Check::HarnessLogin => format!("{} login", agent.label()),
            check => check.name().to_string(),
        }
    }

    /// Runs the harness checks again, and those waiting on them, once
    /// another project is open, or its harness has changed, since they last
    /// ran, so they never speak of a harness the project doesn't use.
    fn follow_harness(&mut self, cx: &mut Context<Self>) {
        let project_dir = ProjectDirectory::get(cx);
        let now = (project_dir.clone(), agent::of_project(project_dir.as_deref()));
        if self.checked_for.as_ref().is_some_and(|checked| *checked != now) {
            self.checked_for = Some(now);
            if self.statuses.iter().any(|(check, _)| *check == Check::Harness) {
                self.check_from(Check::Harness, cx);
            }
        }
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
        let project_dir = ProjectDirectory::get(cx);
        let agent = agent::of_project(project_dir.as_deref());
        self.checked_for = Some((project_dir.clone(), agent));
        for check in ready {
            self.set(check, Status::Checking);
            let probe = self.probe.clone();
            let project_dir = project_dir.clone();
            let run = cx.background_spawn(async move { probe(check, agent, project_dir) });
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

    /// Opens a terminal in the project running the harness's own login
    /// command, `command`.
    fn open_terminal(&mut self, command: &'static str, cx: &mut Context<Self>) {
        let dir = login_dir(ProjectDirectory::get(cx));
        self.terminal_error = crate::harness_login::open_terminal(&dir, command)
            .err()
            .map(|err| format!("{err:#}"));
        cx.notify();
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
            Fix::OpenTerminal(command) => {
                let command = *command;
                Button::new(id("open-terminal"))
                    .small()
                    .primary()
                    .label("Open terminal")
                    .on_click(cx.listener(move |this, _, _, cx| this.open_terminal(command, cx)))
                    .into_any_element()
            }
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
        let output = (check == Check::HarnessLogin)
            .then(|| self.terminal_error.clone())
            .flatten()
            .map(|error| div().ml_6().text_sm().text_color(cx.theme().danger).child(error));
        v_flex()
            .id(("welcome-check", ix))
            .gap_2()
            .py_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_none().size_4().child(icon))
                    .child(div().font_medium().child(self.label(check, cx)))
                    .children(found.map(|found| div().text_sm().text_color(muted).child(found))),
            )
            .children(guidance)
            .children(output)
            .into_any_element()
    }
}

impl Render for WelcomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_harness(cx);
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
        let all = Check::all();
        assert_eq!(
            all,
            [Check::Git, Check::Piton, Check::Harness, Check::Sandbox, Check::HarnessLogin]
        );
        assert_eq!(Check::HarnessLogin.waits_for(&all), [Check::Harness]);
        assert!(Check::Sandbox.waits_for(&all).is_empty());
    }

    /// Checks run, those waiting on a failed one never do, and the page says
    /// how many need attention; once fixed, a check runs again with those
    /// waiting on it, and Suspense is ready.
    #[gpui_kit::test]
    async fn checks_run_in_order_and_again_once_fixed(cx: &mut TestAppContext) {
        let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: super::Probe = {
            let installed = installed.clone();
            Arc::new(move |check, _, _| match check {
                Check::Harness if !installed.load(std::sync::atomic::Ordering::SeqCst) => {
                    Status::Failed {
                        found: None,
                        why: "missing".into(),
                        fixes: vec![Fix::OpenTerminal("claude /login")],
                    }
                }
                _ => Status::Passed(Some("1.0".into())),
            })
        };
        cx.update(crate::project_directory::ProjectDirectory::init);
        let view = cx.new(|cx| WelcomeView::with_probe(probe, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let statuses = view.statuses();
            assert!(matches!(statuses[0].1, Status::Passed(_)));
            assert!(matches!(statuses[2].1, Status::Failed { .. }));
            assert!(matches!(statuses[3].1, Status::Passed(_)));
            // The login waits for the harness.
            assert_eq!(statuses[4].1, Status::Waiting);
            assert_eq!(needing_attention(statuses), 2);
            assert!(!view.is_ready());
        });
        installed.store(true, std::sync::atomic::Ordering::SeqCst);
        view.update(cx, |view, cx| view.check_from(Check::Harness, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.is_ready()));
    }
}
