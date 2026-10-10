//! Logs a harness in in its container, as the ContainerEnvironmentScope
//! says: its login command runs in a container, under a pseudo-terminal,
//! and this view shows what it prints. Any login URL it prints opens in the
//! host's browser; a code it asks for is pasted into the field and goes to
//! the command. The credentials stay in the harness's volume.

use crate::process::Logged as _;
use std::io::{Read as _, Write as _};
use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::console_text::ConsoleLine;
use crate::container;

/// How often what the login printed is collected.
const POLL: Duration = Duration::from_millis(50);

/// The most lines kept.
const MAX_LINES: usize = 400;

/// Emitted once the harness is logged in.
pub struct LoggedIn;

/// Where a login stands.
#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Its image is being built, before the login command runs.
    Preparing,
    Running,
    Succeeded,
    Failed(String),
    /// Its image couldn't be built: no login was started, and it can be
    /// tried again.
    BuildFailed(String),
}

/// What the login's thread reports.
enum Report {
    Output(String),
    /// The image is being built.
    Preparing,
    /// The image is ready, and the login command runs.
    LoggingIn,
    Ended(Result<(), Failure>),
}

/// Why a login didn't succeed: its image couldn't be built, or the login
/// itself failed.
enum Failure {
    Build(String),
    Login(String),
}

pub struct LoginView {
    agent: Agent,
    project_dir: std::path::PathBuf,
    /// What the command printed, line by line, the last perhaps unfinished.
    lines: Vec<String>,
    state: State,
    code: Entity<InputState>,
    /// The command's input, while it runs.
    input: Option<Box<dyn std::io::Write + Send>>,
    /// How many lines have been dropped from the top of `lines`, so a line
    /// is known by where it came, however many were dropped since.
    dropped: usize,
    /// The first line the login command itself printed, once it has started:
    /// only its output is read for its sign-in URL, never the image build's
    /// or Podman's.
    login_from: Option<usize>,
    /// The next line to read for it.
    scanned: usize,
    /// The sign-in URL opened, once one has: a login opens at most one.
    opened: Option<String>,
    _poll: Task<()>,
}

impl EventEmitter<LoggedIn> for LoginView {}

impl LoginView {
    /// Opens a login view for `agent` over `window`, its login already
    /// running; `on_logged_in` runs once it succeeds.
    pub fn open(
        agent: Agent,
        project_dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut App,
        on_logged_in: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Entity<Self> {
        let view = cx.new(|cx| Self::new(agent, project_dir, window, cx));
        let on_logged_in = std::rc::Rc::new(on_logged_in);
        window
            .subscribe(&view, cx, move |_, _: &LoggedIn, window, cx| {
                on_logged_in(window, cx)
            })
            .detach();
        let content = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let content = content.clone();
            dialog
                .w(px(640.))
                .title(format!("Log in to {}", agent.label()))
                .content(move |body, _, _| body.child(content.clone()))
        });
        view
    }

    fn new(
        agent: Agent,
        project_dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste the code here"));
        let mut view = Self {
            agent,
            project_dir,
            lines: vec![String::new()],
            state: State::Running,
            code,
            input: None,
            dropped: 0,
            login_from: None,
            scanned: 0,
            opened: None,
            _poll: Task::ready(()),
        };
        view.start(window, cx);
        view
    }

    /// Starts the login: its image built first where it isn't there or is
    /// out of date, then its command. Started again by Try again.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lines = vec![String::new()];
        self.state = State::Running;
        self.input = None;
        self.dropped = 0;
        self.login_from = None;
        self.scanned = 0;
        self.opened = None;
        let (agent, project_dir) = (self.agent, self.project_dir.clone());
        let (tx, rx) = mpsc::channel();
        let (input_tx, input_rx) = mpsc::channel::<Box<dyn std::io::Write + Send>>();
        std::thread::spawn(move || {
            let result = run_login(agent, &project_dir, &tx, &input_tx);
            tx.send(Report::Ended(result)).ok();
        });
        // Collected on a timer rather than awaited: the login reports from
        // its own thread, which must not wake app tasks.
        self._poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let reports: Vec<Report> = rx.try_iter().collect();
                let input = input_rx.try_iter().last();
                let mut ended = false;
                let updated = this.update(cx, |this, cx| {
                    if let Some(input) = input {
                        this.input = Some(input);
                    }
                    for report in reports {
                        match report {
                            Report::Output(chunk) => this.take_output(&chunk, cx),
                            Report::Preparing => this.state = State::Preparing,
                            Report::LoggingIn => {
                                this.state = State::Running;
                                // From here on, what is printed is the
                                // login command's own.
                                let last = this.lines.len() - 1;
                                let from = this.dropped
                                    + if this.lines[last].is_empty() { last } else { last + 1 };
                                this.login_from = Some(from);
                                this.scanned = from;
                            }
                            Report::Ended(result) => {
                                ended = true;
                                this.input = None;
                                this.state = match result {
                                    Ok(()) => {
                                        cx.emit(LoggedIn);
                                        State::Succeeded
                                    }
                                    Err(Failure::Build(why)) => State::BuildFailed(why),
                                    Err(Failure::Login(why)) => State::Failed(why),
                                };
                            }
                        }
                    }
                    cx.notify();
                });
                if updated.is_err() || ended {
                    break;
                }
            }
        });
        cx.notify();
    }

    /// Takes in what the command printed, opening any login URL in it in the
    /// host's browser, once.
    fn take_output(&mut self, chunk: &str, cx: &mut Context<Self>) {
        for (ix, part) in chunk.split('\n').enumerate() {
            if ix > 0 {
                self.lines.push(String::new());
            }
            if let Some(last) = self.lines.last_mut() {
                last.push_str(part);
            }
        }
        if self.lines.len() > MAX_LINES {
            let drop = self.lines.len() - MAX_LINES;
            self.lines.drain(..drop);
            self.dropped += drop;
        }
        self.open_sign_in(cx);
    }

    /// Opens, in the host's browser, the first URL the login command printed
    /// that leads to the harness's sign-in, and no other, ever: never one
    /// the image build or Podman printed, nor any after the first. Only whole
    /// lines are read, so a URL still coming is never opened cut short.
    fn open_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.login_from.is_none() || self.opened.is_some() {
            return;
        }
        // The last line may be unfinished.
        let complete = self.dropped + self.lines.len() - 1;
        while self.scanned < complete {
            let at = self.scanned;
            self.scanned += 1;
            let Some(line) = at.checked_sub(self.dropped).and_then(|ix| self.lines.get(ix)) else {
                continue;
            };
            let shown = ConsoleLine::parse(line).text;
            if let Some(url) = container::url_in(&shown)
                && container::is_sign_in(self.agent, &url)
            {
                cx.open_url(&url);
                self.opened = Some(url);
                return;
            }
        }
    }

    /// Sends the code pasted in the field to the command.
    fn send_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        if code.is_empty() {
            return;
        }
        if let Some(input) = self.input.as_mut() {
            input.write_all(format!("{code}\r").as_bytes()).ok();
            input.flush().ok();
        }
        self.code
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// Whether the command asked for a code to be pasted.
    fn asks_for_code(&self) -> bool {
        self.lines
            .iter()
            .any(|line| container::asks_for_code(&ConsoleLine::parse(line).text))
    }
}

/// Runs `agent`'s login in its container, its image prepared first, giving
/// what it prints to `tx`, and its input to `input_tx` once it runs.
/// Blocking.
fn run_login(
    agent: Agent,
    project_dir: &std::path::Path,
    tx: &mpsc::Sender<Report>,
    input_tx: &mpsc::Sender<Box<dyn std::io::Write + Send>>,
) -> Result<(), Failure> {
    let platform = container::Platform::current();
    let state = container::podman_state(platform);
    if let Some(why) = container::unavailable(&state, platform) {
        return Err(Failure::Build(why.message));
    }
    // The image first: a login needs it, as any run in a container does.
    let image = container::ensure_image(
        project_dir,
        &mut || {
            tx.send(Report::Preparing).ok();
            tx.send(Report::Output("Preparing environment…\n".into()))
                .ok();
        },
        &mut |line| {
            tx.send(Report::Output(format!("{line}\n"))).ok();
        },
    )
    .map_err(|err| Failure::Build(format!("{err:#}")))?;
    tx.send(Report::LoggingIn).ok();
    log_in(agent, &image, platform, tx, input_tx).map_err(|err| Failure::Login(format!("{err:#}")))
}

/// Runs `agent`'s login command in its container, using `image`, its volume
/// ready for it first. Blocking.
fn log_in(
    agent: Agent,
    image: &str,
    platform: container::Platform,
    tx: &mpsc::Sender<Report>,
    input_tx: &mpsc::Sender<Box<dyn std::io::Write + Send>>,
) -> anyhow::Result<()> {
    container::ensure_volume(agent)?;
    let tty = cfg!(unix);
    let mut command = container::podman_command();
    command.args(container::login_args_for(agent, image, platform, tty));
    let (mut child, mut output, input) = spawn(command, tty)?;
    input_tx.send(input).ok();
    let mut buffer = [0u8; 4096];
    // What it printed, its last lines kept to say why should it fail.
    let mut printed = String::new();
    loop {
        match output.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let text = String::from_utf8_lossy(&buffer[..read]).replace('\r', "");
                printed.push_str(&text);
                tx.send(Report::Output(text)).ok();
            }
        }
    }
    let status = child.wait()?;
    let command = container::login_command(agent);
    let said = last_lines(&printed);
    if !status.success() {
        anyhow::bail!(
            "Logging in to {} failed: `{command}` ended with {status}.{said}",
            agent.label()
        );
    }
    if !container::logged_in(agent, image, platform)? {
        anyhow::bail!(
            "{} still isn't logged in: `{command}` ended with {status}, but wrote no credentials to {}.{said}",
            agent.label(),
            container::credentials_file(agent)
        );
    }
    Ok(())
}

/// The last lines `printed` holds, as a failed login's own words, after a
/// line break; nothing when it printed nothing.
fn last_lines(printed: &str) -> String {
    // As the terminal shows them, without its escapes.
    let lines: Vec<String> = printed
        .lines()
        .map(|line| crate::console_text::ConsoleLine::parse(line).text.to_string())
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(12)..];
    if tail.is_empty() {
        String::new()
    } else {
        format!(" It said:\n{}", tail.join("\n"))
    }
}

type Spawned = (
    std::process::Child,
    Box<dyn std::io::Read + Send>,
    Box<dyn std::io::Write + Send>,
);

/// Starts `command` under a pseudo-terminal of its own where `tty`, and with
/// pipes otherwise, giving its output and its input.
fn spawn(mut command: std::process::Command, tty: bool) -> anyhow::Result<Spawned> {
    #[cfg(unix)]
    if tty {
        use std::os::fd::FromRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let (mut leader, mut follower) = (0, 0);
        // SAFETY: openpty fills in two descriptors it opened, or fails.
        let opened = unsafe {
            libc::openpty(
                &mut leader,
                &mut follower,
                std::ptr::null_mut(),
                // Mutable on macOS, const on Linux: a mutable pointer suits
                // both.
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if opened != 0 {
            anyhow::bail!("could not open a terminal for the login");
        }
        // SAFETY: both descriptors are open and owned by nothing else.
        let follower = unsafe { std::fs::File::from_raw_fd(follower) };
        let leader = unsafe { std::fs::File::from_raw_fd(leader) };
        command
            .stdin(follower.try_clone()?)
            .stdout(follower.try_clone()?)
            .stderr(follower);
        // SAFETY: only async-signal-safe calls, in the child before exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = command.spawn_logged()?;
        // Closed in this process, so the terminal ends with the command.
        drop(command);
        let reader = leader.try_clone()?;
        return Ok((child, Box::new(reader), Box::new(leader)));
    }
    let _ = tty;
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn_logged()?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no output"))?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("no input"))?;
    Ok((child, Box::new(output), Box::new(input)))
}

impl Render for LoginView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = match &self.state {
            State::Preparing => "Preparing environment…".to_string(),
            State::Running => "Logging in…".to_string(),
            State::Succeeded => format!("{} is logged in.", self.agent.label()),
            State::Failed(err) => format!("Could not log in: {err}"),
            State::BuildFailed(err) => err.clone(),
        };
        let build_failed = matches!(self.state, State::BuildFailed(_));
        let output = v_flex()
            .id("login-output")
            .h(px(260.))
            .overflow_y_scroll()
            .p_2()
            .rounded(theme.radius)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .children(self.lines.iter().enumerate().map(|(ix, line)| {
                let text = ConsoleLine::parse(line).text.to_string();
                // Any URL shown is a link to click; only the login's own
                // sign-in ever opens by itself.
                match container::url_in(&text) {
                    Some(url) => {
                        let at = text.find(&url).unwrap_or(0);
                        let (before, after) = (&text[..at], &text[at + url.len()..]);
                        let shown = url.clone();
                        h_flex()
                            .flex_wrap()
                            .child(before.to_string())
                            .child(
                                div()
                                    .id(("login-link", ix))
                                    .text_color(theme.link)
                                    .underline()
                                    .cursor_pointer()
                                    .child(shown)
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                            .child(after.to_string())
                            .into_any_element()
                    }
                    None => div().child(text).into_any_element(),
                }
            }));
        let running = self.state == State::Running;
        let view = v_flex()
            .id("login-view")
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} logs in in its container, once for every project. A page it opens is opened in your browser.",
                        self.agent.label()
                    )),
            )
            .child(output)
            .when(running && (self.asks_for_code() || self.input.is_some()), |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.code)))
                        .child(
                            Button::new("login-send-code")
                                .primary()
                                .small()
                                .label("Send code")
                                .on_click(cx.listener(|this, _, window, cx| this.send_code(window, cx))),
                        ),
                )
            })
            .child(
                div()
                    .id("login-status")
                    .text_sm()
                    .font_medium()
                    .whitespace_normal()
                    .when(build_failed, |this| this.text_color(theme.danger))
                    .child(status),
            )
            // A build that failed can be tried again, as no login started.
            .when(build_failed, |view| {
                view.child(
                    h_flex().child(
                        Button::new("login-try-again")
                            .small()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    ),
                )
            });
        gpui_kit::TestSupportExt::test_support(view)
    }
}
