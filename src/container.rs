//! The container environment, as the ContainerEnvironmentScope says: runs of
//! the harness that must see only what their mode may use run in a Podman
//! container the application owns. What a run can read or change comes from
//! what is mounted into it, never from its prompt or a guard.
//!
//! This module knows whether Podman can run a container here (installed,
//! and on macOS and Windows its machine running), where to install it from,
//! the images runs use and how they are built, what each kind of run mounts,
//! the volumes a harness keeps its login, sessions, and caches in, how a
//! container's paths map back to the host's, and how a harness logs in.

use std::collections::hash_map::DefaultHasher;
use std::ffi::OsString;
use std::hash::{Hash as _, Hasher as _};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::agent::Agent;
use crate::chat_input::SendMode;
use crate::hidden_anchor::APP_DIR;
use crate::project_tree::Locations;

/// The platform the application runs on, which decides how Podman runs and
/// where to install it from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    /// Podman's installation instructions for this platform.
    pub fn install_url(self) -> &'static str {
        match self {
            Platform::MacOs => "https://podman.io/docs/installation#macos",
            Platform::Windows => "https://podman.io/docs/installation#windows",
            Platform::Linux => "https://podman.io/docs/installation#installing-on-linux",
        }
    }

    /// Whether Podman runs its containers in a machine of its own here.
    pub fn has_machine(self) -> bool {
        self != Platform::Linux
    }
}

/// The kinds of run that go in a container, each seeing what its mode may
/// use, as the mounts say. Code tasks, and a Chain prompt's code step, always
/// run on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    /// A Spec task, or a Chain prompt's spec step or spec follow-up.
    Spec,
    /// A question from the Ask tab.
    Question,
}

impl RunKind {
    /// Where a prompt sent in `mode` runs: in a container of this kind, or
    /// on the host for none. A Chain prompt's own task is its spec step; its
    /// code step is sent in Code, which, like Freeform, runs on the host.
    pub fn of(mode: Option<SendMode>) -> Option<Self> {
        match mode? {
            SendMode::Spec | SendMode::Both => Some(RunKind::Spec),
            SendMode::Ask => Some(RunKind::Question),
            SendMode::Code | SendMode::Freeform => None,
        }
    }

    /// Which conversation its sessions are kept for, by its folder's name.
    fn conversation(self) -> &'static str {
        match self {
            RunKind::Spec => "spec",
            RunKind::Question => "questions",
        }
    }
}

/// A host file or folder mounted into a run's container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub host: PathBuf,
    pub writable: bool,
}

/// Where the project is in a container, on every platform, so a harness that
/// keys its sessions on where it works finds them again after the project
/// moves.
pub const WORKSPACE: &str = "/workspace";

/// The home directory of the harness in a container.
pub const HOME: &str = "/home/suspense";

/// The folder of the project's data the harness's sessions are kept in, one
/// folder per conversation.
const SESSIONS_DIR: &str = "sessions";

/// The folder the harness's sessions for `kind`'s conversation in the project
/// at `project_dir` are kept in.
pub fn sessions_folder(project_dir: &Path, kind: RunKind) -> PathBuf {
    project_dir
        .join(APP_DIR)
        .join(SESSIONS_DIR)
        .join(kind.conversation())
}

/// How one run is put in a container: what it mounts, what it hides, and
/// where it keeps its login and sessions.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub kind: RunKind,
    pub agent: Agent,
    pub project_dir: PathBuf,
    pub platform: Platform,
    /// What is mounted from the host, parents before what they hold.
    pub mounts: Vec<Mount>,
    /// Folders inside a mount that the run must not see, hidden behind an
    /// empty, read-only folder.
    pub hidden: Vec<PathBuf>,
    /// Empty, writable folders that go with the container, standing in for
    /// a location the run mustn't see but whose place a build writes to, as
    /// the code location a Spec run's `piton build` places guidance in.
    pub scratch: Vec<PathBuf>,
    /// The folder its conversation's sessions are kept in, mounted where the
    /// harness keeps them under its home; none for a run of no conversation.
    pub sessions: Option<PathBuf>,
}

impl Plan {
    /// How a run of `kind` with `agent` in the project at `project_dir`,
    /// whose spec and code are at `locations`, is put in a container, with
    /// the task's understanding file, if it has one.
    pub fn new(
        kind: RunKind,
        agent: Agent,
        project_dir: &Path,
        locations: &Locations,
        understanding: Option<&Path>,
        platform: Platform,
    ) -> Self {
        let data = project_dir.join(APP_DIR);
        let reference = project_dir.join(agent.directory()).join("reference");
        let config = project_dir.join(crate::project_directory::CONFIG_FILE_NAME);
        let fluency = crate::piton_fluency::file(project_dir);
        let spec = locations.spec.clone();
        // A code location the config leaves out, or gives as the project
        // itself, is the whole project.
        let code = locations
            .code
            .clone()
            .unwrap_or_else(|| project_dir.to_path_buf());
        let rw = |host: PathBuf| Mount {
            host,
            writable: true,
        };
        let ro = |host: PathBuf| Mount {
            host,
            writable: false,
        };
        let mut mounts = Vec::new();
        match kind {
            RunKind::Spec => {
                mounts.extend(spec.clone().map(rw));
                mounts.push(rw(reference));
                // The build's manifest of the files it owns, so a build in
                // the container can write the reference: a copy, made fresh
                // for each run, since what a build there places, with no code
                // to place guidance in, isn't what the host's build owns.
                mounts.push(rw(piton_copy(project_dir)));
                mounts.push(ro(fluency));
                mounts.push(ro(config));
                mounts.extend(understanding.map(|file| rw(file.to_path_buf())));
            }
            // Both locations, read only: the spec's source so `piton slice`
            // can read it, nothing changed.
            RunKind::Question => {
                mounts.push(ro(code.clone()));
                mounts.extend(spec.clone().map(ro));
                mounts.push(ro(reference));
                mounts.push(ro(config));
            }
        }
        // What a mount holds that the run must not see: the project's data
        // and its git directory. A question sees both locations, whichever
        // holds the other.
        let unseen = vec![data, project_dir.join(".git")];
        // A Spec run never sees the code: an empty scratch folder stands in
        // its place, for the build to place the shape's guidance in.
        // The harness directory is scratch too, the reference mounted in it,
        // so the build can write what else it writes there, as skills.
        let scratch = match kind {
            RunKind::Spec => vec![code, project_dir.join(agent.directory())],
            RunKind::Question => Vec::new(),
        };
        let hidden = unseen
            .into_iter()
            .filter(|unseen| {
                mounts
                    .iter()
                    .any(|mount| unseen.starts_with(&mount.host) && *unseen != mount.host)
            })
            .collect();
        // Parents first, so what they hold is mounted over them.
        mounts.sort_by_key(|mount| mount.host.components().count());
        Self {
            kind,
            agent,
            project_dir: project_dir.to_path_buf(),
            platform,
            mounts,
            hidden,
            scratch,
            sessions: Some(sessions_folder(project_dir, kind)),
        }
    }

    /// This plan as the host stands: a folder it writes to that isn't there
    /// yet made, as the compiled reference before the first build or its
    /// conversation's sessions folder, and anything else that isn't there
    /// left out, as the fluency file before it is written, since only what
    /// is there can be mounted.
    pub fn as_on_host(&self) -> Self {
        let mut plan = self.clone();
        plan.mounts.retain(|mount| {
            if !mount.host.exists() && mount.writable && mount.host.extension().is_none() {
                std::fs::create_dir_all(&mount.host).ok();
            }
            mount.host.exists()
        });
        plan.hidden.retain(|hidden| hidden.exists());
        // The copy of `.piton` a Spec run is given, fresh from the project's.
        let copy = piton_copy(&plan.project_dir);
        if plan.mounts.iter().any(|mount| mount.host == copy) {
            std::fs::remove_dir_all(&copy).ok();
            std::fs::create_dir_all(&copy).ok();
            if let Ok(entries) = std::fs::read_dir(plan.project_dir.join(".piton")) {
                for entry in entries.flatten() {
                    if entry.path().is_file() {
                        std::fs::copy(entry.path(), copy.join(entry.file_name())).ok();
                    }
                }
            }
        }
        if let Some(sessions) = &plan.sessions {
            if !sessions.exists() {
                std::fs::create_dir_all(sessions).ok();
            }
            // The harness's transcripts belong to this machine: git never
            // sees them, whatever the project's own ignores say.
            if let Some(all) = sessions.parent() {
                let ignore = all.join(".gitignore");
                if !ignore.exists() {
                    std::fs::write(ignore, "*\n").ok();
                }
            }
        }
        plan
    }

    /// Where the host path `host` is in the container: under the workspace,
    /// at its place from the project directory, on every platform.
    pub fn container_path(&self, host: &Path) -> PathBuf {
        let within = host.strip_prefix(&self.project_dir).unwrap_or(host);
        let mut path = PathBuf::from(WORKSPACE);
        for part in within.components() {
            if let std::path::Component::Normal(part) = part {
                path.push(part);
            }
        }
        path
    }

    /// The host path of `path`, a path the run reported from its container;
    /// none when it is nowhere on the host, as one outside the workspace.
    #[cfg(test)]
    pub fn to_host(&self, path: &Path) -> Option<PathBuf> {
        let within = path.strip_prefix(WORKSPACE).ok()?;
        Some(self.project_dir.join(within))
    }

    /// A line the run printed, every path in it from its container mapped
    /// back to the host, so whatever reads it sees host paths: only a path
    /// that is the workspace, or is in it, never text that merely holds its
    /// name, as `/workspace-old`.
    pub fn map_line(&self, line: &str) -> String {
        // JSON escapes a Windows path's separators.
        let host = self.project_dir.to_string_lossy().replace('\\', "\\\\");
        let path_char = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+');
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(at) = rest.find(WORKSPACE) {
            let before = rest[..at].chars().next_back();
            let after = rest[at + WORKSPACE.len()..].chars().next();
            // A path of its own: nothing of another path before it, and
            // nothing after it but its end or a separator.
            let starts = before.is_none_or(|c| !path_char(c) && c != '/');
            let ends = after.is_none_or(|c| !path_char(c));
            out.push_str(&rest[..at]);
            out.push_str(if starts && ends { &host } else { WORKSPACE });
            rest = &rest[at + WORKSPACE.len()..];
        }
        out.push_str(rest);
        out
    }

    /// The arguments to `podman` that run `command` with `args` in a
    /// container of `image` as this plan says, its input open; with `tty`,
    /// under a terminal of its own.
    pub fn run_args(
        &self,
        image: &str,
        command: &str,
        args: &[OsString],
        tty: bool,
    ) -> Vec<OsString> {
        let mut out: Vec<OsString> = vec!["run".into(), "--rm".into(), "-i".into()];
        if tty {
            out.push("-t".into());
        }
        // Files it writes belong to the user, as on the host.
        if self.platform == Platform::Linux {
            out.push("--userns=keep-id".into());
        }
        out.extend(["--label".into(), "suspense=run".into()]);
        out.extend(["-e".into(), format!("HOME={HOME}").into()]);
        // The harness's login and settings, shared by every project.
        out.extend([
            "-v".into(),
            format!("{}:{HOME}:U", home_volume(self.agent)).into(),
        ]);
        // Its conversation's sessions, from the project, where the harness
        // keeps them; no other conversation's.
        if let Some(sessions) = &self.sessions {
            let mut spec: OsString = "type=bind,src=".into();
            spec.push(sessions.as_os_str());
            spec.push(format!(",dst={HOME}/{}", session_dir(self.agent)));
            out.extend(["--mount".into(), spec]);
        }
        for mount in &self.mounts {
            let mut spec: OsString = "type=bind,src=".into();
            spec.push(mount.host.as_os_str());
            spec.push(",dst=");
            // A copy of the project's `.piton` is mounted where it would be.
            let at = if mount.host == piton_copy(&self.project_dir) {
                self.container_path(&self.project_dir.join(".piton"))
            } else {
                self.container_path(&mount.host)
            };
            spec.push(at.as_os_str());
            if !mount.writable {
                spec.push(",ro=true");
            }
            out.extend(["--mount".into(), spec]);
        }
        for hidden in &self.hidden {
            let mut spec: OsString = "type=tmpfs,dst=".into();
            spec.push(self.container_path(hidden).as_os_str());
            spec.push(",ro=true");
            out.extend(["--mount".into(), spec]);
        }
        // Writable, and gone with the container.
        for scratch in &self.scratch {
            let mut spec: OsString = "type=tmpfs,dst=".into();
            spec.push(self.container_path(scratch).as_os_str());
            out.extend(["--mount".into(), spec]);
        }
        out.push("-w".into());
        out.push(WORKSPACE.into());
        out.push(image.into());
        out.push(command.into());
        out.extend(args.iter().cloned());
        out
    }
}

/// Where the copy of the project's `.piton` a Spec run's build writes to is
/// kept on the host, outside the project, made afresh for each run.
fn piton_copy(project_dir: &Path) -> PathBuf {
    std::env::temp_dir()
        .join("suspense-piton")
        .join(project_key(project_dir))
}

/// The volume a harness keeps its login and settings in, shared by every
/// project: the only volume the application keeps.
pub fn home_volume(agent: Agent) -> String {
    format!("suspense-home-{}", agent.command())
}

/// Where, under its home, a harness keeps its sessions.
fn session_dir(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => ".claude/projects",
        Agent::Codex => ".codex/sessions",
        Agent::OpenCode => ".local/share/opencode/storage",
    }
}

/// A short, stable name for the project at `project_dir`.
fn project_key(project_dir: &Path) -> String {
    let name = project_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect::<String>();
    format!("{name}-{}", short_hash(&project_dir.to_string_lossy()))
}

fn short_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

#[cfg(test)]
thread_local! {
    /// The `podman` each test runs in place of the real one.
    static TEST_PODMAN: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Has this test run `program` in place of `podman`.
#[cfg(test)]
pub fn use_podman_for_test(program: Option<PathBuf>) {
    TEST_PODMAN.set(program);
}

/// Whether runs go in containers: always, but in a test only once it has
/// stood in a `podman` of its own, so tests of everything else run their
/// fake harness on the host.
pub fn active() -> bool {
    #[cfg(test)]
    return TEST_PODMAN.with_borrow(Option::is_some);
    #[cfg(not(test))]
    true
}

/// The `podman` a test stood in, to carry to another thread.
#[cfg(test)]
pub fn podman_for_test() -> Option<PathBuf> {
    TEST_PODMAN.with_borrow(Clone::clone)
}

/// The `podman` program.
pub fn podman() -> PathBuf {
    // A test never runs the user's own Podman: without one of its own, it
    // finds none.
    #[cfg(test)]
    return TEST_PODMAN
        .with_borrow(Clone::clone)
        .unwrap_or_else(|| PathBuf::from("/nonexistent/podman"));
    #[cfg(not(test))]
    PathBuf::from("podman")
}

/// A `podman` command.
pub fn podman_command() -> Command {
    Command::new(podman())
}

/// Whether Podman can run a container here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PodmanState {
    /// `podman` isn't installed.
    Missing,
    /// On macOS or Windows, there is no Podman machine.
    NoMachine,
    /// On macOS or Windows, its machine isn't running.
    MachineStopped,
    /// It can run containers, at this version.
    Ready(String),
}

/// Whether Podman can run a container on `platform`. Blocking.
pub fn podman_state(platform: Platform) -> PodmanState {
    let Ok(output) = podman_command()
        .arg("--version")
        .stdin(Stdio::null())
        .output()
    else {
        return PodmanState::Missing;
    };
    if !output.status.success() {
        return PodmanState::Missing;
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_string();
    if !platform.has_machine() {
        return PodmanState::Ready(version);
    }
    let machines = podman_command()
        .args(["machine", "list", "--format", "json"])
        .stdin(Stdio::null())
        .output();
    match machines {
        Ok(output) if output.status.success() => {
            machine_state(&String::from_utf8_lossy(&output.stdout), version)
        }
        _ => PodmanState::NoMachine,
    }
}

/// The state `podman machine list --format json` gives.
fn machine_state(json: &str, version: String) -> PodmanState {
    let machines: Vec<Value> = serde_json::from_str(json).unwrap_or_default();
    if machines.is_empty() {
        return PodmanState::NoMachine;
    }
    let running = machines
        .iter()
        .any(|machine| machine.get("Running").and_then(Value::as_bool) == Some(true));
    if running {
        PodmanState::Ready(version)
    } else {
        PodmanState::MachineStopped
    }
}

/// What a run that needs Podman says when it can't have it, with what can be
/// done about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub message: String,
    pub action: Option<MachineAction>,
}

/// What can be done about a Podman machine that isn't there or running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineAction {
    /// `podman machine init`, then `podman machine start`.
    SetUp,
    /// `podman machine start`.
    Start,
}

impl MachineAction {
    pub fn label(self) -> &'static str {
        match self {
            MachineAction::SetUp => "Set up Podman",
            MachineAction::Start => "Start Podman",
        }
    }

    /// The `podman` commands it runs, in order.
    pub fn commands(self) -> Vec<Vec<&'static str>> {
        match self {
            MachineAction::SetUp => vec![vec!["machine", "init"], vec!["machine", "start"]],
            MachineAction::Start => vec![vec!["machine", "start"]],
        }
    }
}

/// Why a run can't have Podman as `state` stands on `platform`; none when
/// it can.
pub fn unavailable(state: &PodmanState, platform: Platform) -> Option<Unavailable> {
    match state {
        PodmanState::Ready(_) => None,
        PodmanState::Missing => Some(Unavailable {
            message: format!(
                "Podman is needed to run this in its container, and `podman` isn't installed. \
                 Install it from {}",
                platform.install_url()
            ),
            action: None,
        }),
        PodmanState::NoMachine => Some(Unavailable {
            message: "Podman is installed, but has no machine to run containers in.".into(),
            action: Some(MachineAction::SetUp),
        }),
        PodmanState::MachineStopped => Some(Unavailable {
            message: "Podman's machine isn't running.".into(),
            action: Some(MachineAction::Start),
        }),
    }
}

/// The exit status `podman run` gives when Podman itself failed, before the
/// command in the container could start.
const PODMAN_FAILED: i32 = 125;

/// The exit statuses `podman run` gives when the command couldn't be started
/// in the container: found but not runnable, or not found.
const NOT_STARTED: [i32; 2] = [126, 127];

/// What Podman printed, as the run says it: its message, or with none, its
/// exit status.
fn podman_said(code: Option<i32>, printed: &str) -> String {
    let printed = printed.trim();
    if printed.is_empty() {
        match code {
            Some(code) => format!("podman exited with status {code}"),
            None => "podman was ended before it said why".into(),
        }
    } else {
        printed.to_string()
    }
}

/// What a run says when Podman couldn't be accessed, as the
/// ContainerEnvironmentScope says: that the harness couldn't be run because
/// of it, then what Podman printed, as written.
pub fn access_message(printed: &str) -> String {
    format!(
        "The harness couldn't be run because Podman couldn't be accessed. Podman said:\n{}",
        printed.trim()
    )
}

/// Why a `podman run` that exited with `code`, having printed `printed` to
/// its error output, failed before the harness started in its container,
/// if it did; none when the harness ran, and its own failure is its to tell.
pub fn run_failure(code: Option<i32>, printed: &str) -> Option<String> {
    match code {
        Some(PODMAN_FAILED) => Some(access_message(&podman_said(code, printed))),
        Some(code) if NOT_STARTED.contains(&code) => Some(format!(
            "The harness couldn't be started in its container. Podman said:\n{}",
            podman_said(Some(code), printed)
        )),
        _ => None,
    }
}

/// Whether what Podman printed says it couldn't be used at all: the user may
/// not use it, its rootless setup is missing, or it can't reach its service
/// or machine.
pub fn says_no_access(printed: &str) -> bool {
    let printed = printed.to_lowercase();
    [
        "permission denied",
        "operation not permitted",
        "cannot connect",
        "unable to connect",
        "connection refused",
        "no such file or directory: \"/run/user",
        "subuid",
        "subgid",
        "newuidmap",
        "newgidmap",
        "user namespace",
        "rootless",
        "cannot re-exec",
        "cannot set up namespace",
    ]
    .iter()
    .any(|sign| printed.contains(sign))
}

/// Whether Podman can be used here to run containers, as `podman info`
/// tells; with why not, as it printed it. Blocking.
pub fn check_access() -> std::result::Result<(), String> {
    let output = podman_command()
        .args(["info", "--format", "{{.Host.Security.Rootless}}"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(podman_said(
        output.status.code(),
        &String::from_utf8_lossy(&output.stderr),
    ))
}

/// Runs `action`, giving each line its commands print to `on_line`. Blocking.
pub fn run_machine_action(action: MachineAction, on_line: &mut dyn FnMut(String)) -> Result<()> {
    for args in action.commands() {
        on_line(format!("$ podman {}", args.join(" ")));
        stream(podman_command().args(&args), on_line)?;
    }
    Ok(())
}

/// Runs `command`, giving each line it prints, out and error alike, to
/// `on_line`, and failing with what it last said if it fails. Blocking.
fn stream(command: &mut Command, on_line: &mut dyn FnMut(String)) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run podman")?;
    let stderr = child.stderr.take().context("no stderr")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let errors = std::thread::spawn({
        let tx = tx.clone();
        move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tx.send(line).ok();
            }
        }
    });
    let stdout = child.stdout.take().context("no stdout")?;
    let out = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            tx.send(line).ok();
        }
    });
    let mut last = String::new();
    for line in rx {
        last.clone_from(&line);
        on_line(line);
    }
    errors.join().ok();
    out.join().ok();
    let status = child.wait()?;
    if !status.success() {
        // One that couldn't use Podman says so, not only that it failed.
        if says_no_access(&last) {
            bail!("{}", access_message(&last));
        }
        bail!("podman failed: {}", last.trim());
    }
    Ok(())
}

/// The npm packages of the harnesses, as the default image installs them.
fn package(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "@anthropic-ai/claude-code",
        Agent::Codex => "@openai/codex",
        Agent::OpenCode => "opencode-ai",
    }
}

/// The version of `command` installed on the host, if it can be found.
fn host_version(command: &str) -> Option<String> {
    let output = Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    version_in(&String::from_utf8_lossy(&output.stdout))
}

/// The first version number in `text`, as `2.1.281` in
/// `2.1.281 (Claude Code)`.
fn version_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == 'v')
        .find(|word| {
            word.contains('.')
                && word
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic())
                && word.starts_with(|c: char| c.is_ascii_digit())
        })
        .map(str::to_string)
}

/// The default image's Containerfile: the harnesses at `versions`, each
/// `None` taking its latest, and piton copied in when `with_piton`.
pub fn default_containerfile(versions: &[(Agent, Option<String>)], with_piton: bool) -> String {
    let packages = versions
        .iter()
        .map(|(agent, version)| match version {
            Some(version) => format!("{}@{version}", package(*agent)),
            None => package(*agent).to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut file = String::from(
        // A recent Debian, whose C library is new enough for the host's own
        // piton, copied in, to run.
        "FROM docker.io/library/node:22-trixie-slim\n\
         RUN apt-get update \\\n \
         && apt-get install -y --no-install-recommends git ca-certificates ripgrep \\\n \
         && rm -rf /var/lib/apt/lists/*\n\
         RUN mkdir -p /workspace\n",
    );
    file.push_str(&format!("RUN npm install -g {packages}\n"));
    if with_piton {
        // The host's piton; once built, the image is checked that it runs
        // (see `check_commands`).
        file.push_str("COPY piton /usr/local/bin/piton\n");
    }
    file
}

/// The name the default image is known by, whatever version it is, for a
/// project's Containerfile to build FROM.
pub const DEFAULT_IMAGE: &str = "localhost/suspense-default";

/// Whether `image` has been built.
fn image_exists(image: &str) -> bool {
    podman_command()
        .args(["image", "exists", image])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The images a project's runs use: the default, as the host's harnesses
/// and piton make it, and the project's own over it, if it has a
/// Containerfile.
struct Images {
    containerfile: String,
    piton: Option<PathBuf>,
    default: String,
    /// The project's image, and its Containerfile.
    own: Option<(String, PathBuf)>,
}

impl Images {
    /// The commands the image holds, each of which has to run in it.
    fn commands(&self) -> Vec<&'static str> {
        let mut commands: Vec<&'static str> = Agent::RUNNABLE
            .iter()
            .map(|agent| agent.command())
            .collect();
        if self.piton.is_some() {
            commands.push("piton");
        }
        commands
    }

    fn of(project_dir: &Path) -> Self {
        // The harnesses and piton as the host has them, where it can say.
        let versions: Vec<(Agent, Option<String>)> = Agent::RUNNABLE
            .iter()
            .map(|agent| (*agent, host_version(agent.command())))
            .collect();
        let piton = which("piton").filter(|_| Platform::current() == Platform::Linux);
        let containerfile = default_containerfile(&versions, piton.is_some());
        let piton_version = piton.as_ref().and_then(|_| host_version("piton"));
        let default = format!(
            "{DEFAULT_IMAGE}:{}",
            short_hash(&format!("{containerfile}{piton_version:?}"))
        );
        let own_file = project_dir.join(APP_DIR).join("Containerfile");
        let own = std::fs::read_to_string(&own_file).ok().map(|text| {
            (
                format!(
                    "localhost/suspense-project-{}:{}",
                    project_key(project_dir),
                    short_hash(&format!("{default}{text}"))
                ),
                own_file,
            )
        });
        Self {
            containerfile,
            piton,
            default,
            own,
        }
    }

    /// The image runs use.
    fn used(&self) -> &str {
        self.own.as_ref().map_or(&self.default, |(image, _)| image)
    }
}

/// The image a run in the project at `project_dir` uses, if it has been
/// built, without building it. Blocking.
pub fn built_image(project_dir: &Path) -> Option<String> {
    let images = Images::of(project_dir);
    image_exists(images.used()).then(|| images.used().to_string())
}

/// The image a run in the project at `project_dir` uses, built first, or
/// built again, if it is out of date, each line the build prints given to
/// `on_line`, `on_build` called once before a build starts. Blocking.
pub fn ensure_image(
    project_dir: &Path,
    on_build: &mut dyn FnMut(),
    on_line: &mut dyn FnMut(String),
) -> Result<String> {
    let images = Images::of(project_dir);
    if !image_exists(&images.default) {
        on_build();
        let context = std::env::temp_dir().join(format!("suspense-image-{}", std::process::id()));
        std::fs::create_dir_all(&context)?;
        std::fs::write(context.join("Containerfile"), &images.containerfile)?;
        if let Some(piton) = &images.piton {
            std::fs::copy(piton, context.join("piton"))?;
        }
        let built = stream(
            podman_command()
                .args(["build", "-t", &images.default, "-t", DEFAULT_IMAGE, "-f"])
                .arg(context.join("Containerfile"))
                .arg(&context),
            on_line,
        );
        std::fs::remove_dir_all(&context).ok();
        built?;
        // Each command it holds has to run in it, or it is never used.
        if let Err(err) = check_commands(&images.default, &images.commands(), on_line) {
            podman_command()
                .args(["rmi", "-f", &images.default, DEFAULT_IMAGE])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
            return Err(err);
        }
    }
    // A project's own Containerfile, built FROM the default.
    if let Some((image, file)) = &images.own
        && !image_exists(image)
    {
        on_build();
        stream(
            podman_command()
                .args(["build", "-t", image, "--build-arg"])
                .arg(format!("SUSPENSE_IMAGE={}", images.default))
                .arg("-f")
                .arg(file)
                .arg(project_dir.join(APP_DIR)),
            on_line,
        )?;
        if let Err(err) = check_commands(image, &images.commands(), on_line) {
            podman_command()
                .args(["rmi", "-f", image])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok();
            return Err(err);
        }
    }
    Ok(images.used().to_string())
}

/// Runs each of `commands`' version command in `image`, as `piton --version`,
/// each line printed given to `on_line`, failing on the first that can't run,
/// naming it and saying what it printed. Blocking.
fn check_commands(image: &str, commands: &[&str], on_line: &mut dyn FnMut(String)) -> Result<()> {
    for command in commands {
        on_line(format!("$ {command} --version"));
        let output = podman_command()
            .args(["run", "--rm", image, command, "--version"])
            .stdin(Stdio::null())
            .output()
            .context("could not run podman")?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for line in printed.lines() {
            on_line(line.to_string());
        }
        if !output.status.success() {
            bail!(
                "`{command}` can't run in the container's image, so the image isn't used. It printed:\n{}",
                podman_said(output.status.code(), &printed)
            );
        }
    }
    Ok(())
}

/// Where `command` is on the `PATH`.
fn which(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|file| file.is_file())
}

/// How a harness says whether it is logged in, and logs in, in a container.
fn status_args(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "status", "--json"],
        Agent::Codex => &["login", "status"],
        Agent::OpenCode => &["auth", "list"],
    }
}

fn login_args(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "login"],
        // Device-code login needs nothing published.
        Agent::Codex => &["login", "--device-auth"],
        Agent::OpenCode => &["auth", "login"],
    }
}

/// The localhost port a harness waits on for its login's callback, which
/// the login run publishes.
fn callback_port(agent: Agent) -> Option<u16> {
    match agent {
        Agent::Codex => Some(1455),
        _ => None,
    }
}

/// Whether `agent`'s status, as `output` printed with `success`, says it is
/// logged in.
fn read_status(agent: Agent, success: bool, output: &str) -> bool {
    match agent {
        Agent::Claude => serde_json::from_str::<Value>(output.trim())
            .ok()
            .and_then(|status| status.get("loggedIn").and_then(Value::as_bool))
            .unwrap_or(false),
        Agent::Codex => success && !output.to_lowercase().contains("not logged in"),
        Agent::OpenCode => success && !output.contains("0 credentials"),
    }
}

/// A plan with nothing mounted, for a harness's own work in its volume, as
/// logging in.
fn bare(agent: Agent, kind: RunKind, platform: Platform) -> Plan {
    Plan {
        kind,
        agent,
        project_dir: PathBuf::from(HOME),
        platform,
        mounts: Vec::new(),
        hidden: Vec::new(),
        scratch: Vec::new(),
        sessions: None,
    }
}

/// Whether `agent` is logged in, in its volume, using `image`. Blocking.
pub fn logged_in(agent: Agent, image: &str, platform: Platform) -> Result<bool> {
    let args: Vec<OsString> = status_args(agent).iter().map(Into::into).collect();
    let plan = bare(agent, RunKind::Question, platform);
    let output = podman_command()
        .args(plan.run_args(image, agent.command(), &args, false))
        .stdin(Stdio::null())
        .output()
        .context("could not run podman")?;
    // Podman failing to run the status check isn't the harness logged out.
    if output.stdout.is_empty()
        && let Some(why) = run_failure(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        )
    {
        bail!("{why}");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(read_status(agent, output.status.success(), &text))
}

/// The `podman` arguments that log `agent` in, in its volume, using
/// `image`: under a terminal where `tty`, with the port its login waits on
/// published.
pub fn login_args_for(agent: Agent, image: &str, platform: Platform, tty: bool) -> Vec<OsString> {
    let args: Vec<OsString> = login_args(agent).iter().map(Into::into).collect();
    let mut run =
        bare(agent, RunKind::Question, platform).run_args(image, agent.command(), &args, tty);
    if let Some(port) = callback_port(agent) {
        // Published just before the image, after `run`'s own options.
        let at = run.len() - args.len() - 2;
        run.insert(at, format!("127.0.0.1:{port}:{port}").into());
        run.insert(at, "-p".into());
    }
    run
}

/// The first web address in `line`, as a login prints one to open.
pub fn url_in(line: &str) -> Option<String> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')'))
        .collect();
    (url.len() > "https://".len()).then_some(url)
}

/// Whether a line a login printed asks for a code to be pasted.
pub fn asks_for_code(line: &str) -> bool {
    let line = line.to_lowercase();
    line.contains("paste") && line.contains("code")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;

    fn locations(project: &Path) -> Locations {
        Locations {
            spec: Some(project.join("spec")),
            code: Some(project.join("src")),
        }
    }

    fn args(plan: &Plan) -> Vec<String> {
        plan.run_args("img", "claude", &[OsString::from("-p")], false)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn mounted(plan: &Plan, path: &Path) -> Option<bool> {
        plan.mounts
            .iter()
            .find(|mount| mount.host == path)
            .map(|mount| mount.writable)
    }

    /// A Spec run sees the spec, the reference, the fluency, the config, and
    /// its understanding file, and never the code; a question the code, the
    /// spec, the reference, and the config, all read only. Each
    /// is at its place under /workspace, never at its host path.
    #[test]
    fn each_kind_of_run_mounts_only_what_it_may_use() {
        let project = Path::new("/home/me/proj");
        let understanding = project.join(".suspense/history/1-Task.understanding.md");
        let spec = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            project,
            &locations(project),
            Some(&understanding),
            Platform::Linux,
        );
        assert_eq!(mounted(&spec, &project.join("spec")), Some(true));
        assert_eq!(
            mounted(&spec, &project.join(".claude/reference")),
            Some(true)
        );
        assert_eq!(
            mounted(&spec, &project.join(".suspense/fluency.md")),
            Some(false)
        );
        assert_eq!(
            mounted(&spec, &project.join("piton.config.pi")),
            Some(false)
        );
        assert_eq!(mounted(&spec, &understanding), Some(true));
        assert!(
            !spec
                .mounts
                .iter()
                .any(|mount| mount.host.starts_with(project.join("src")))
        );
        let spec_args = args(&spec).join(" ");
        assert!(!spec_args.contains("/home/me/proj/src"), "{spec_args}");
        assert!(
            spec_args.contains("src=/home/me/proj/spec,dst=/workspace/spec"),
            "{spec_args}"
        );
        assert!(spec_args.contains("--userns=keep-id"));
        assert!(spec_args.contains("-w /workspace"));
        // The build's manifest, and an empty scratch folder in the code's
        // place, writable, never the code itself.
        // A copy of the project's `.piton`, where it would be.
        assert_eq!(mounted(&spec, &piton_copy(project)), Some(true));
        assert!(spec_args.contains(",dst=/workspace/.piton "), "{spec_args}");
        assert!(
            !spec_args.contains("src=/home/me/proj/.piton"),
            "{spec_args}"
        );
        assert!(
            spec_args.contains("--mount type=tmpfs,dst=/workspace/src "),
            "{spec_args}"
        );
        assert!(
            spec_args.contains("--mount type=tmpfs,dst=/workspace/.claude "),
            "{spec_args}"
        );
        assert!(!spec_args.contains("dst=/workspace/src,ro"), "{spec_args}");

        let question = Plan::new(
            RunKind::Question,
            Agent::Claude,
            project,
            &locations(project),
            None,
            Platform::Linux,
        );
        assert!(question.mounts.iter().all(|mount| !mount.writable));
        assert_eq!(mounted(&question, &project.join("src")), Some(false));
        // The spec's source too, read only, for `piton slice`.
        assert_eq!(mounted(&question, &project.join("spec")), Some(false));
        assert!(question.hidden.is_empty() || !question.hidden.contains(&project.join("spec")));

        // Nothing else from the host: no home, no git directory; only the
        // copy of `.piton` the application makes.
        for plan in [&spec, &question] {
            for mount in &plan.mounts {
                assert!(
                    mount.host.starts_with(project) || mount.host == piton_copy(project),
                    "{mount:?}"
                );
                assert!(!mount.host.starts_with(project.join(".git")));
            }
        }
    }

    /// Spec, Chain, and questions run in a container; Code and Freeform never.
    #[test]
    fn which_runs_go_in_a_container() {
        assert_eq!(RunKind::of(Some(SendMode::Spec)), Some(RunKind::Spec));
        assert_eq!(RunKind::of(Some(SendMode::Both)), Some(RunKind::Spec));
        assert_eq!(RunKind::of(Some(SendMode::Ask)), Some(RunKind::Question));
        assert_eq!(RunKind::of(Some(SendMode::Code)), None);
        assert_eq!(RunKind::of(Some(SendMode::Freeform)), None);
    }

    /// A code location holding the spec and the project's data hides the
    /// data from a question run over it, never the spec, which a question
    /// sees, read only, wherever it is.
    #[test]
    fn what_a_mount_holds_that_the_run_mustnt_see_is_hidden() {
        let project = Path::new("/p");
        let whole = Locations {
            spec: Some(project.join("spec")),
            code: None,
        };
        let question = Plan::new(
            RunKind::Question,
            Agent::Claude,
            project,
            &whole,
            None,
            Platform::Linux,
        );
        assert_eq!(mounted(&question, project), Some(false));
        assert_eq!(mounted(&question, &project.join("spec")), Some(false));
        assert!(
            !question.hidden.contains(&project.join("spec")),
            "the spec is hidden"
        );
        for hidden in [project.join(".suspense"), project.join(".git")] {
            assert!(question.hidden.contains(&hidden), "{hidden:?} isn't hidden");
        }
        let joined = args(&question).join(" ");
        assert!(
            !joined.contains("type=tmpfs,dst=/workspace/spec"),
            "{joined}"
        );
        assert!(
            joined.contains("src=/p/spec,dst=/workspace/spec,ro=true"),
            "{joined}"
        );
        // A Spec run over the same project still never sees the code.
        let spec_run = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            project,
            &whole,
            None,
            Platform::Linux,
        );
        assert!(!spec_run.mounts.iter().any(|mount| mount.host == project));
    }

    /// Each harness keeps its login in a volume of its own, shared by every
    /// project, the only volume; each conversation keeps its sessions in its
    /// own folder of the project, which only its runs mount.
    #[test]
    fn sessions_are_kept_in_the_project_by_conversation() {
        let a = Path::new("/home/me/alpha");
        assert_eq!(home_volume(Agent::Claude), "suspense-home-claude");
        let spec = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            a,
            &locations(a),
            None,
            Platform::Linux,
        );
        let question = Plan::new(
            RunKind::Question,
            Agent::Claude,
            a,
            &locations(a),
            None,
            Platform::Linux,
        );
        assert_eq!(spec.sessions, Some(a.join(".suspense/sessions/spec")));
        assert_eq!(
            question.sessions,
            Some(a.join(".suspense/sessions/questions"))
        );
        let joined = args(&spec).join(" ");
        assert!(
            joined.contains("suspense-home-claude:/home/suspense:U"),
            "{joined}"
        );
        assert!(
            joined.contains(
                "src=/home/me/alpha/.suspense/sessions/spec,dst=/home/suspense/.claude/projects"
            ),
            "{joined}"
        );
        assert!(!joined.contains("sessions/questions"), "{joined}");
        assert!(
            !joined.contains("src=/home/me/alpha/.suspense/sessions,"),
            "{joined}"
        );
        assert!(
            !joined.contains("suspense-sessions-") && !joined.contains("/cache"),
            "{joined}"
        );
    }

    /// A run's sessions folder is made before it runs, and kept from git.
    #[test]
    fn the_sessions_folder_is_made_and_ignored() {
        let project =
            std::env::temp_dir().join(format!("suspense-sessions-{}", std::process::id()));
        std::fs::remove_dir_all(&project).ok();
        std::fs::create_dir_all(&project).unwrap();
        Plan::new(
            RunKind::Question,
            Agent::Claude,
            &project,
            &locations(&project),
            None,
            Platform::Linux,
        )
        .as_on_host();
        assert!(project.join(".suspense/sessions/questions").is_dir());
        assert_eq!(
            std::fs::read_to_string(project.join(".suspense/sessions/.gitignore")).unwrap(),
            "*\n"
        );
        std::fs::remove_dir_all(&project).ok();
    }

    /// On every platform, what a run reports from /workspace is mapped back
    /// to the host: only a path that is /workspace or in it.
    #[test]
    fn container_paths_map_back_to_the_host() {
        let project = Path::new("/home/me/proj");
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            let plan = Plan::new(
                RunKind::Spec,
                Agent::Claude,
                project,
                &locations(project),
                None,
                platform,
            );
            assert_eq!(
                plan.container_path(&project.join("spec/a.pi")),
                PathBuf::from("/workspace/spec/a.pi")
            );
            assert_eq!(
                plan.to_host(Path::new("/workspace/spec/a.pi")),
                Some(project.join("spec/a.pi"))
            );
            assert_eq!(plan.to_host(Path::new("/etc/passwd")), None);
            assert_eq!(
                plan.map_line(r#"{"file_path":"/workspace/spec/a.pi","cwd":"/workspace"}"#),
                r#"{"file_path":"/home/me/proj/spec/a.pi","cwd":"/home/me/proj"}"#
            );
            // Text that merely holds the name stays as it was.
            for kept in [
                "/workspace-old/a",
                "/home/workspace/a",
                "my/workspace/a",
                "/workspaces",
            ] {
                assert_eq!(plan.map_line(kept), kept, "{kept} was mapped");
            }
            assert_eq!(
                plan.map_line("cd /workspace && ls"),
                "cd /home/me/proj && ls"
            );
            assert!(args(&plan).join(" ").contains("-w /workspace"));
        }
    }

    /// Without Podman, a run says it is needed, linking its installation
    /// instructions for the platform it runs on.
    #[test]
    fn missing_podman_links_the_platforms_install_page() {
        use_podman_for_test(Some(PathBuf::from("/nonexistent/podman")));
        for (platform, url) in [
            (Platform::MacOs, "https://podman.io/docs/installation#macos"),
            (
                Platform::Windows,
                "https://podman.io/docs/installation#windows",
            ),
            (
                Platform::Linux,
                "https://podman.io/docs/installation#installing-on-linux",
            ),
        ] {
            let state = podman_state(platform);
            assert_eq!(state, PodmanState::Missing);
            let why = unavailable(&state, platform).unwrap();
            assert!(why.message.contains("Podman is needed"), "{}", why.message);
            assert!(why.message.contains(url), "{}", why.message);
            assert_eq!(why.action, None);
        }
        use_podman_for_test(None);
    }

    /// A fake `podman` printing `version`, and `machines` for its machine
    /// list.
    fn fake_podman(name: &str, machines: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("suspense-podman-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("podman");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'podman version 5.0.0' ;;\n  machine) echo '{machines}' ;;\nesac\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    /// On macOS and Windows, a machine that isn't there is offered to be set
    /// up, and one that isn't running to be started; Linux has no machine.
    #[cfg(unix)]
    #[test]
    fn a_machine_not_running_is_offered_to_be_started() {
        use_podman_for_test(Some(fake_podman("none", "[]")));
        for platform in [Platform::MacOs, Platform::Windows] {
            let state = podman_state(platform);
            assert_eq!(state, PodmanState::NoMachine);
            assert_eq!(
                unavailable(&state, platform).unwrap().action,
                Some(MachineAction::SetUp)
            );
        }
        assert_eq!(
            podman_state(Platform::Linux),
            PodmanState::Ready("5.0.0".into())
        );

        use_podman_for_test(Some(fake_podman(
            "stopped",
            r#"[{"Name":"podman-machine-default","Running":false}]"#,
        )));
        for platform in [Platform::MacOs, Platform::Windows] {
            let state = podman_state(platform);
            assert_eq!(state, PodmanState::MachineStopped);
            let why = unavailable(&state, platform).unwrap();
            assert_eq!(why.action, Some(MachineAction::Start));
            assert_eq!(why.action.unwrap().label(), "Start Podman");
        }

        use_podman_for_test(Some(fake_podman(
            "running",
            r#"[{"Name":"podman-machine-default","Running":true}]"#,
        )));
        assert_eq!(
            podman_state(Platform::MacOs),
            PodmanState::Ready("5.0.0".into())
        );
        use_podman_for_test(None);
    }

    #[test]
    fn logins_are_read_and_their_urls_found() {
        assert!(read_status(Agent::Claude, true, r#"{"loggedIn": true}"#));
        assert!(!read_status(Agent::Claude, false, r#"{"loggedIn": false}"#));
        assert_eq!(
            url_in("Visit https://claude.ai/oauth/authorize?code=1 to log in."),
            Some("https://claude.ai/oauth/authorize?code=1".into())
        );
        assert!(asks_for_code("Paste code here if prompted >"));
        let args: Vec<String> = login_args_for(Agent::Codex, "img", Platform::Linux, true)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(
            joined.contains("-p 127.0.0.1:1455:1455 img codex login"),
            "{joined}"
        );
        assert_eq!(
            version_in("2.1.281 (Claude Code)").as_deref(),
            Some("2.1.281")
        );
        assert_eq!(version_in("codex-cli 0.41.0").as_deref(), Some("0.41.0"));
    }

    #[test]
    fn the_default_image_pins_the_hosts_versions() {
        let file = default_containerfile(
            &[(Agent::Claude, Some("2.1.0".into())), (Agent::Codex, None)],
            true,
        );
        assert!(file.contains("@anthropic-ai/claude-code@2.1.0"));
        assert!(file.contains("@openai/codex\n") || file.contains("@openai/codex "));
        assert!(file.contains("COPY piton"));
        assert!(file.starts_with("FROM docker.io/library/node:22-trixie-slim\n"));
        assert!(!file.contains("rm -f /usr/local/bin/piton"), "{file}");
    }

    /// Only what is there on the host is mounted: a folder the run writes to
    /// is made first, and a file not written yet is left out.
    #[test]
    fn only_what_is_there_is_mounted() {
        let project = std::env::temp_dir().join(format!("suspense-on-host-{}", std::process::id()));
        std::fs::remove_dir_all(&project).ok();
        std::fs::create_dir_all(project.join("spec")).unwrap();
        let plan = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            &project,
            &locations(&project),
            None,
            Platform::Linux,
        )
        .as_on_host();
        assert_eq!(
            mounted(&plan, &project.join(".claude/reference")),
            Some(true)
        );
        assert!(project.join(".claude/reference").is_dir());
        assert_eq!(mounted(&plan, &project.join(".suspense/fluency.md")), None);
        std::fs::remove_dir_all(&project).ok();
    }

    /// Podman failing, before the harness starts, says Podman couldn't be
    /// accessed, with what it printed; the harness's own exit is left to
    /// the harness to tell.
    #[test]
    fn podman_failing_is_told_apart_from_the_harness() {
        let why =
            run_failure(Some(125), "Error: cannot find newuidmap: exec: not found\n").unwrap();
        assert!(
            why.starts_with("The harness couldn't be run because Podman couldn't be accessed.")
        );
        assert!(
            why.ends_with("Error: cannot find newuidmap: exec: not found"),
            "{why}"
        );
        assert!(!why.contains("harness reported"));
        assert!(
            run_failure(Some(127), "Error: executable file `claude` not found")
                .unwrap()
                .contains("couldn't be started")
        );
        assert_eq!(run_failure(Some(1), "anything"), None);
        assert_eq!(run_failure(Some(0), ""), None);
        assert!(run_failure(Some(125), "").unwrap().contains("status 125"));
        assert!(says_no_access(
            "Error: cannot connect to Podman. Please verify your connection"
        ));
        assert!(says_no_access(
            "potentially insufficient UIDs or GIDs available in user namespace"
        ));
        assert!(!says_no_access(
            "Error: building at STEP \"RUN npm install\": exit status 1"
        ));
    }

    /// `podman info` failing means Podman can't be accessed, with its reason.
    #[cfg(unix)]
    #[test]
    fn podman_info_failing_is_no_access() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir =
            std::env::temp_dir().join(format!("suspense-podman-noaccess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("podman");
        std::fs::write(&script, "#!/bin/sh\necho 'Error: cannot setup namespace using newuidmap: exit status 1' >&2\nexit 125\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        use_podman_for_test(Some(script));
        assert_eq!(
            check_access(),
            Err("Error: cannot setup namespace using newuidmap: exit status 1".into())
        );
        use_podman_for_test(None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A command that can't run in the image fails the check, naming it and
    /// saying what it printed.
    #[cfg(unix)]
    #[test]
    fn a_command_that_cant_run_in_the_image_fails_its_build() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("suspense-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("podman");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n*piton*) echo \"piton: version 'GLIBC_2.39' not found\" >&2; exit 1 ;;\n*) echo 1.0 ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        use_podman_for_test(Some(script));
        let mut lines = Vec::new();
        let err = check_commands("img", &["claude", "piton"], &mut |line| lines.push(line))
            .unwrap_err()
            .to_string();
        use_podman_for_test(None);
        assert!(
            err.starts_with("`piton` can't run in the container's image"),
            "{err}"
        );
        assert!(err.contains("GLIBC_2.39"), "{err}");
        assert!(lines.contains(&"$ claude --version".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
