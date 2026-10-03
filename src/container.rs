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

    /// Whether a host path can be mounted at the same path in a container:
    /// on Linux, and on macOS, whose Podman machine shares the user's files
    /// at their own paths; not on Windows, whose paths a Linux container
    /// can't hold.
    pub fn same_paths(self) -> bool {
        self != Platform::Windows
    }
}

/// The kinds of run that go in a container, each seeing what its mode may
/// use, as the mounts say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    /// A Spec task, or a Chain prompt's spec step or spec follow-up.
    Spec,
    /// A question from the Ask tab.
    Question,
    /// A Code task, or a Chain prompt's code step, with the project's option
    /// on.
    Code,
}

impl RunKind {
    /// Where a prompt sent in `mode` runs: in a container of this kind, or
    /// on the host for none. `code_in_container` is the project's "Run code
    /// tasks in a container" option. A Chain prompt's own task is its spec
    /// step; its code step is sent in Code. Freeform always runs on the host.
    pub fn of(mode: Option<SendMode>, code_in_container: bool) -> Option<Self> {
        match mode? {
            SendMode::Spec | SendMode::Both => Some(RunKind::Spec),
            SendMode::Ask => Some(RunKind::Question),
            SendMode::Code => code_in_container.then_some(RunKind::Code),
            SendMode::Freeform => None,
        }
    }

    /// Which conversation its sessions are kept in.
    fn conversation(self) -> &'static str {
        match self {
            RunKind::Spec => "spec",
            RunKind::Code => "code",
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

/// Where the user's files live in a container on Windows, where they can't
/// be mounted at their own paths.
const WORKSPACE: &str = "/workspace";

/// The home directory of the harness in a container.
pub const HOME: &str = "/home/suspense";

/// Where a Code run's caches live in its container.
const CACHE: &str = "/cache";

/// How one run is put in a container: what it mounts, what it hides, and
/// the volumes it keeps its login, sessions, and caches in.
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
                mounts.push(ro(fluency));
                mounts.push(ro(config));
                mounts.extend(understanding.map(|file| rw(file.to_path_buf())));
            }
            RunKind::Question => {
                mounts.push(ro(code.clone()));
                mounts.push(ro(reference));
                mounts.push(ro(config));
            }
            RunKind::Code => {
                mounts.push(rw(code.clone()));
                mounts.push(ro(reference));
                mounts.push(ro(config));
                mounts.extend(understanding.map(|file| rw(file.to_path_buf())));
            }
        }
        // What a mount holds that the run must not see: the other location,
        // the project's data, and its git directory.
        let mut unseen = vec![data, project_dir.join(".git")];
        match kind {
            RunKind::Spec => unseen.push(code),
            RunKind::Question | RunKind::Code => unseen.extend(spec),
        }
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
        }
    }

    /// This plan as the host stands: a folder it writes to that isn't there
    /// yet made, as the compiled reference before the first build, and
    /// anything else that isn't there left out, as the fluency file before
    /// it is written, since only what is there can be mounted.
    pub fn as_on_host(&self) -> Self {
        let mut plan = self.clone();
        plan.mounts.retain(|mount| {
            if !mount.host.exists() && mount.writable && mount.host.extension().is_none() {
                std::fs::create_dir_all(&mount.host).ok();
            }
            mount.host.exists()
        });
        plan.hidden.retain(|hidden| hidden.exists());
        plan
    }

    /// Where the host path `host` is in the container: the same path where
    /// the platform allows, and otherwise under the workspace, from the
    /// project directory.
    pub fn container_path(&self, host: &Path) -> PathBuf {
        if self.platform.same_paths() {
            return host.to_path_buf();
        }
        let within = host.strip_prefix(&self.project_dir).unwrap_or(host);
        let mut path = PathBuf::from(WORKSPACE);
        for part in within.components() {
            path.push(part.as_os_str());
        }
        path
    }

    /// The host path of `path`, a path the run reported from its container;
    /// none when it is nowhere on the host.
    #[cfg(test)]
    pub fn to_host(&self, path: &Path) -> Option<PathBuf> {
        if self.platform.same_paths() {
            return Some(path.to_path_buf());
        }
        let within = path.strip_prefix(WORKSPACE).ok()?;
        Some(self.project_dir.join(within))
    }

    /// A line the run printed, every path in it from its container mapped
    /// back to the host, so whatever reads it sees host paths.
    pub fn map_line(&self, line: &str) -> String {
        if self.platform.same_paths() {
            return line.to_string();
        }
        // JSON escapes a Windows path's separators.
        let host = self.project_dir.to_string_lossy().replace('\\', "\\\\");
        line.replace(WORKSPACE, &host)
    }

    /// The volume holding the harness's sessions for this run's
    /// conversation in its project.
    pub fn session_volume(&self) -> String {
        session_volume(&self.project_dir, self.kind)
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
        // The harness's login and settings, shared by every project, and the
        // sessions of this conversation in this project over them.
        out.extend([
            "-v".into(),
            format!("{}:{HOME}:U", home_volume(self.agent)).into(),
        ]);
        out.extend([
            "-v".into(),
            format!(
                "{}:{HOME}/{}:U",
                self.session_volume(),
                session_dir(self.agent)
            )
            .into(),
        ]);
        if self.kind == RunKind::Code {
            out.extend([
                "-v".into(),
                format!("{}:{CACHE}:U", cache_volume(&self.project_dir)).into(),
            ]);
            out.extend(["-e".into(), format!("CARGO_HOME={CACHE}/cargo").into()]);
            out.extend([
                "-e".into(),
                format!("CARGO_TARGET_DIR={CACHE}/target").into(),
            ]);
        }
        for mount in &self.mounts {
            let mut spec: OsString = "type=bind,src=".into();
            spec.push(mount.host.as_os_str());
            spec.push(",dst=");
            spec.push(self.container_path(&mount.host).as_os_str());
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
        out.push("-w".into());
        out.push(self.container_path(&self.project_dir).into_os_string());
        out.push(image.into());
        out.push(command.into());
        out.extend(args.iter().cloned());
        out
    }
}

/// The volume a harness keeps its login and settings in, shared by every
/// project.
pub fn home_volume(agent: Agent) -> String {
    format!("suspense-home-{}", agent.command())
}

/// The volume a harness keeps the sessions of a project's conversation in.
pub fn session_volume(project_dir: &Path, kind: RunKind) -> String {
    format!(
        "suspense-sessions-{}-{}",
        project_key(project_dir),
        kind.conversation()
    )
}

/// The volume a project's Code runs keep their caches in.
pub fn cache_volume(project_dir: &Path) -> String {
    format!("suspense-cache-{}", project_key(project_dir))
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
        "FROM docker.io/library/node:22-bookworm-slim\n\
         RUN apt-get update \\\n \
         && apt-get install -y --no-install-recommends git ca-certificates ripgrep \\\n \
         && rm -rf /var/lib/apt/lists/*\n",
    );
    file.push_str(&format!("RUN npm install -g {packages}\n"));
    if with_piton {
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
    }
    Ok(images.used().to_string())
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

/// The project's "Run code tasks in a container" option, from its
/// settings, as the ProjectDataScope says; off when they don't say.
pub fn code_in_container(project_dir: &Path) -> bool {
    crate::project_settings::read(project_dir).run_code_tasks_in_container
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
    /// reference, and the config, all read only, and never the spec; a Code
    /// run the code, read and write, and never the spec.
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
        assert_eq!(mounted(&spec, &project.join("src")), None);
        assert!(
            !spec
                .mounts
                .iter()
                .any(|mount| mount.host.starts_with(project.join("src")))
        );
        let spec_args = args(&spec).join(" ");
        assert!(!spec_args.contains("/home/me/proj/src"), "{spec_args}");
        assert!(spec_args.contains("--userns=keep-id"));

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
        assert!(
            !question
                .mounts
                .iter()
                .any(|mount| mount.host.starts_with(project.join("spec")))
        );

        let code = Plan::new(
            RunKind::Code,
            Agent::Claude,
            project,
            &locations(project),
            Some(&understanding),
            Platform::Linux,
        );
        assert_eq!(mounted(&code, &project.join("src")), Some(true));
        assert_eq!(
            mounted(&code, &project.join(".claude/reference")),
            Some(false)
        );
        assert!(
            !code
                .mounts
                .iter()
                .any(|mount| mount.host.starts_with(project.join("spec")))
        );

        // Nothing else from the host: no home, no git directory.
        for plan in [&spec, &question, &code] {
            for mount in &plan.mounts {
                assert!(mount.host.starts_with(project), "{mount:?}");
                assert!(!mount.host.starts_with(project.join(".git")));
            }
        }
    }

    /// Code runs go in a container only with the project's option on; Spec,
    /// Chain, and questions always; Freeform never.
    #[test]
    fn which_runs_go_in_a_container() {
        assert_eq!(
            RunKind::of(Some(SendMode::Spec), false),
            Some(RunKind::Spec)
        );
        assert_eq!(
            RunKind::of(Some(SendMode::Both), false),
            Some(RunKind::Spec)
        );
        assert_eq!(
            RunKind::of(Some(SendMode::Ask), false),
            Some(RunKind::Question)
        );
        assert_eq!(RunKind::of(Some(SendMode::Code), false), None);
        assert_eq!(RunKind::of(Some(SendMode::Code), true), Some(RunKind::Code));
        assert_eq!(RunKind::of(Some(SendMode::Freeform), true), None);
    }

    /// A code location holding the spec and the project's data hides them
    /// from a question run over it.
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
        for hidden in [
            project.join("spec"),
            project.join(".suspense"),
            project.join(".git"),
        ] {
            assert!(question.hidden.contains(&hidden), "{hidden:?} isn't hidden");
        }
        let joined = args(&question).join(" ");
        assert!(
            joined.contains("type=tmpfs,dst=/p/spec,ro=true"),
            "{joined}"
        );
    }

    /// Each harness keeps its login in a volume of its own, shared by every
    /// project, and each conversation of a project its sessions.
    #[test]
    fn volumes_are_named_for_what_they_keep() {
        let a = Path::new("/home/me/alpha");
        assert_eq!(home_volume(Agent::Claude), "suspense-home-claude");
        assert_ne!(
            session_volume(a, RunKind::Spec),
            session_volume(a, RunKind::Code)
        );
        assert_ne!(
            session_volume(a, RunKind::Spec),
            session_volume(Path::new("/home/me/beta"), RunKind::Spec)
        );
        assert!(session_volume(a, RunKind::Question).starts_with("suspense-sessions-alpha-"));
        let plan = Plan::new(
            RunKind::Code,
            Agent::Claude,
            a,
            &locations(a),
            None,
            Platform::Linux,
        );
        let joined = args(&plan).join(" ");
        assert!(joined.contains(&format!("{}:/home/suspense:U", home_volume(Agent::Claude))));
        assert!(joined.contains(&format!("{}:/cache:U", cache_volume(a))));
    }

    /// Where mounts can't be at their host paths, as on Windows, what a run
    /// reports from its container is mapped back to the host.
    #[test]
    fn container_paths_map_back_to_the_host() {
        let project = Path::new("/home/me/proj");
        let linux = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            project,
            &locations(project),
            None,
            Platform::Linux,
        );
        assert_eq!(
            linux.container_path(&project.join("spec")),
            project.join("spec")
        );
        assert_eq!(
            linux.map_line("/home/me/proj/spec/a.pi"),
            "/home/me/proj/spec/a.pi"
        );

        let windows = Plan::new(
            RunKind::Spec,
            Agent::Claude,
            project,
            &locations(project),
            None,
            Platform::Windows,
        );
        assert_eq!(
            windows.container_path(&project.join("spec/a.pi")),
            PathBuf::from("/workspace/spec/a.pi")
        );
        assert_eq!(
            windows.to_host(Path::new("/workspace/spec/a.pi")),
            Some(project.join("spec/a.pi"))
        );
        assert_eq!(windows.to_host(Path::new("/etc/passwd")), None);
        assert_eq!(
            windows.map_line(r#"{"file_path":"/workspace/spec/a.pi"}"#),
            r#"{"file_path":"/home/me/proj/spec/a.pi"}"#
        );
        assert!(args(&windows).join(" ").contains("-w /workspace"));
        assert!(!args(&windows).join(" ").contains("keep-id"));
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
}
