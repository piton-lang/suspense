//! Logs a harness in in its container, as the ContainerEnvironmentScope
//! says: its login command runs in a container, under a pseudo-terminal,
//! and this view shows what it prints. Any login URL it prints opens in the
//! host's browser; a code it asks for is pasted into the field and goes to
//! the command. The credentials stay in the harness's volume.

use std::collections::HashSet;
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
    Running,
    Succeeded,
    Failed(String),
}

/// What the login's thread reports.
enum Report {
    Output(String),
    Ended(Result<(), String>),
}

pub struct LoginView {
    agent: Agent,
    /// What the command printed, line by line, the last perhaps unfinished.
    lines: Vec<String>,
    state: State,
    code: Entity<InputState>,
    /// The command's input, while it runs.
    input: Option<Box<dyn std::io::Write + Send>>,
    opened: HashSet<String>,
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
        let (tx, rx) = mpsc::channel();
        let (input_tx, input_rx) = mpsc::channel::<Box<dyn std::io::Write + Send>>();
        std::thread::spawn(move || {
            let result =
                run_login(agent, &project_dir, &tx, &input_tx).map_err(|err| format!("{err:#}"));
            tx.send(Report::Ended(result)).ok();
        });
        // Collected on a timer rather than awaited: the login reports from
        // its own thread, which must not wake app tasks.
        let poll = cx.spawn_in(window, async move |this, cx| {
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
                            Report::Ended(result) => {
                                ended = true;
                                this.input = None;
                                this.state = match result {
                                    Ok(()) => {
                                        cx.emit(LoggedIn);
                                        State::Succeeded
                                    }
                                    Err(err) => State::Failed(err),
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
        Self {
            agent,
            lines: vec![String::new()],
            state: State::Running,
            code,
            input: None,
            opened: HashSet::new(),
            _poll: poll,
        }
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
            self.lines.drain(..self.lines.len() - MAX_LINES);
        }
        for line in &self.lines {
            let shown = ConsoleLine::parse(line).text;
            if let Some(url) = container::url_in(&shown)
                && self.opened.insert(url.clone())
            {
                cx.open_url(&url);
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
) -> anyhow::Result<()> {
    let platform = container::Platform::current();
    let state = container::podman_state(platform);
    if let Some(why) = container::unavailable(&state, platform) {
        anyhow::bail!("{}", why.message);
    }
    let image = container::ensure_image(
        project_dir,
        &mut || {
            tx.send(Report::Output("Preparing environment…\n".into()))
                .ok();
        },
        &mut |line| {
            tx.send(Report::Output(format!("{line}\n"))).ok();
        },
    )?;
    let tty = cfg!(unix);
    let mut command = container::podman_command();
    command.args(container::login_args_for(agent, &image, platform, tty));
    let (mut child, mut output, input) = spawn(command, tty)?;
    input_tx.send(input).ok();
    let mut buffer = [0u8; 4096];
    loop {
        match output.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tx.send(Report::Output(
                    String::from_utf8_lossy(&buffer[..read]).replace('\r', ""),
                ))
                .ok();
            }
        }
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("the login ended with {status}");
    }
    if !container::logged_in(agent, &image, platform)? {
        anyhow::bail!("{} still isn't logged in", agent.label());
    }
    Ok(())
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
        let child = command.spawn()?;
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
        .spawn()?;
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
            State::Running => "Logging in…".to_string(),
            State::Succeeded => format!("{} is logged in.", self.agent.label()),
            State::Failed(err) => format!("Could not log in: {err}"),
        };
        let output = v_flex()
            .id("login-output")
            .h(px(260.))
            .overflow_y_scroll()
            .p_2()
            .rounded(theme.radius)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .children(
                self.lines
                    .iter()
                    .map(|line| div().child(ConsoleLine::parse(line).text)),
            );
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
            .child(div().text_sm().font_medium().child(status));
        gpui_kit::TestSupportExt::test_support(view)
    }
}
