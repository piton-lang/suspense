//! A project's run targets: the ways it is built, run, and tested, each a
//! name and a shell command, found once by the harness and saved with the
//! project in `.suspense/run.json`, then run from the ribbon's Code tab with
//! their output streamed into the Run panel.

use std::io::{BufRead as _, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow};
use gpui_kit::assets::IconName;
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

use crate::baked_prompts::{fill, run_targets};
use crate::hidden_anchor::APP_DIR;

/// The file the targets are saved in, within the project's data.
const FILE: &str = "run.json";

/// What a target does, which picks its icon.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Run,
    Build,
    Test,
    #[default]
    #[serde(other)]
    Other,
}

impl Kind {
    pub fn icon(self) -> IconName {
        match self {
            Kind::Run => IconName::Play,
            Kind::Build => IconName::Hammer,
            Kind::Test => IconName::FlaskConical,
            Kind::Other => IconName::Terminal,
        }
    }
}

/// A way to run the project.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Target {
    pub name: String,
    pub command: String,
    pub kind: Kind,
    /// Runs the project from a release build: the one Ctrl/Cmd+Shift+F5
    /// runs.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub release: bool,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Targets {
    targets: Vec<Target>,
}

/// The targets worth keeping: each with a name and a command, and only the
/// first marked release still marked.
fn kept(targets: Vec<Target>) -> Vec<Target> {
    let mut release_seen = false;
    targets
        .into_iter()
        .filter(|target| !target.name.trim().is_empty() && !target.command.trim().is_empty())
        .map(|target| {
            let release = target.release && !release_seen;
            release_seen |= release;
            Target {
                name: target.name.trim().to_string(),
                command: target.command.trim().to_string(),
                kind: target.kind,
                release,
            }
        })
        .collect()
}

/// The target marked release, by its place among `targets`.
pub fn release(targets: &[Target]) -> Option<usize> {
    targets.iter().position(|target| target.release)
}

pub fn path(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(FILE)
}

/// The targets saved with the project; none when it has none, or its file
/// can't be read.
pub fn load(project_dir: &Path) -> Vec<Target> {
    std::fs::read_to_string(path(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Targets>(&text).ok())
        .map(|saved| kept(saved.targets))
        .unwrap_or_default()
}

pub fn save(project_dir: &Path, targets: &[Target]) -> Result<()> {
    let file = path(project_dir);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&Targets {
        targets: targets.to_vec(),
    })?;
    std::fs::write(&file, text + "\n").with_context(|| format!("could not save {}", file.display()))
}

/// The targets in what the harness replied, which may have a little around
/// its JSON; an error when it holds none.
pub fn parse_reply(text: &str) -> Result<Vec<Target>> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let reply: Targets =
        serde_json::from_str(&text[start..=end]).context("the reply isn't the JSON asked for")?;
    let targets = kept(reply.targets);
    if targets.is_empty() {
        return Err(anyhow!("the harness found no way to run the project"));
    }
    Ok(targets)
}

/// The prompt asking the harness how the project in `project_dir` is run.
pub fn find_prompt(project_dir: &Path) -> String {
    let dir = project_dir.display().to_string();
    format!(
        "{}\n\n{}\n\n{}\n",
        fill(run_targets::INTRO, &[("projectDir", &dir)]),
        run_targets::TASK,
        run_targets::REPLY_FORMAT,
    )
}

/// The open project's targets, and which is running, for the ribbon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectTargets {
    pub targets: Vec<Target>,
    pub running: Option<usize>,
}

impl Global for ProjectTargets {}

impl ProjectTargets {
    pub fn get(cx: &App) -> ProjectTargets {
        cx.try_global::<ProjectTargets>()
            .cloned()
            .unwrap_or_default()
    }

    /// Replaces them, telling those watching only when they changed.
    pub fn set(targets: ProjectTargets, cx: &mut App) {
        if cx.try_global::<ProjectTargets>() != Some(&targets) {
            cx.set_global(targets);
        }
    }
}

/// A target's command running, its whole process group with it.
pub struct Running {
    child: Arc<Mutex<Child>>,
    /// What it has printed and not yet been taken.
    printed: Arc<Mutex<Vec<String>>>,
    /// How many of its output and error are still being read.
    open_pipes: Arc<std::sync::atomic::AtomicUsize>,
    /// When it was first seen to have ended.
    ended_at: Mutex<Option<std::time::Instant>>,
}

impl Running {
    /// Starts `command` in `project_dir`, keeping each line it prints, output
    /// and errors together, for [`Self::take_printed`].
    pub fn start(project_dir: &Path, command: &str) -> Result<Self> {
        let printed = Arc::new(Mutex::new(Vec::new()));
        let on_line = {
            let printed = printed.clone();
            move |line: String| printed.lock().unwrap().push(line)
        };
        let mut shell = shell(command);
        // Asked for colour, though its output isn't a terminal, as the usual
        // variables ask it; the Run panel shows it in the theme's colours.
        shell
            .env("CLICOLOR_FORCE", "1")
            .env("FORCE_COLOR", "1")
            .env("CARGO_TERM_COLOR", "always")
            .env_remove("NO_COLOR");
        if !matches!(std::env::var("TERM").as_deref(), Ok(term) if !term.is_empty() && term != "dumb")
        {
            shell.env("TERM", "xterm-256color");
        }
        shell
            .current_dir(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so stopping it stops all it started.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut shell, 0);
        let mut child = shell
            .spawn()
            .with_context(|| format!("could not run {command}"))?;
        let on_line = Arc::new(on_line);
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn Read + Send>),
        ];
        let open_pipes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for pipe in pipes.into_iter().flatten() {
            let on_line = on_line.clone();
            let open = open_pipes.clone();
            open.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(pipe);
                let mut line = Vec::new();
                while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
                    let text = String::from_utf8_lossy(&line);
                    on_line(text.trim_end_matches(['\n', '\r']).to_string());
                    line.clear();
                }
                open.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            printed,
            open_pipes,
            ended_at: Mutex::new(None),
        })
    }

    /// The lines printed since last taken.
    pub fn take_printed(&self) -> Vec<String> {
        std::mem::take(&mut *self.printed.lock().unwrap())
    }

    /// How it ended, once it has and all it printed has been read: its exit
    /// code, or `None` when a signal ended it. Something it left running that
    /// keeps its output open is given a second before it counts as ended.
    pub fn exited(&self) -> Option<Option<i32>> {
        let status = self.child.lock().unwrap().try_wait().ok()??;
        let ended_at = *self
            .ended_at
            .lock()
            .unwrap()
            .get_or_insert_with(std::time::Instant::now);
        let read = self.open_pipes.load(std::sync::atomic::Ordering::SeqCst) == 0;
        (read || ended_at.elapsed() > std::time::Duration::from_secs(1)).then_some(status.code())
    }

    /// Waits for it to end; its exit code, or `None` when a signal ended it.
    #[cfg(test)]
    pub fn wait(&self) -> Result<Option<i32>> {
        loop {
            if let Some(status) = self.child.lock().unwrap().try_wait()? {
                return Ok(status.code());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Stops it, and everything it started.
    pub fn stop(&self) {
        let mut child = self.child.lock().unwrap();
        #[cfg(unix)]
        {
            let group = format!("-{}", child.id());
            Command::new("kill")
                .args(["-TERM", "--", &group])
                .status()
                .ok();
        }
        #[cfg(windows)]
        {
            Command::new("taskkill")
                .args(["/T", "/F", "/PID", &child.id().to_string()])
                .status()
                .ok();
        }
        child.kill().ok();
    }
}

fn shell(command: &str) -> Command {
    if cfg!(windows) {
        let mut shell = Command::new("cmd");
        shell.args(["/C", command]);
        shell
    } else {
        let mut shell = Command::new("sh");
        shell.args(["-c", command]);
        shell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_read_and_saved_targets_load() {
        let targets = parse_reply(
            r#"Here: {"targets": [
                {"name": "Run", "command": "cargo run", "kind": "run"},
                {"name": "", "command": "x"},
                {"name": "Lint", "command": "cargo clippy", "kind": "lint"},
                {"name": "Test", "command": " cargo test ", "kind": "test"},
                {"name": "Run Release", "command": "cargo run --release", "kind": "run", "release": true},
                {"name": "Also", "command": "x", "release": true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|t| (t.name.as_str(), t.command.as_str(), t.kind))
                .collect::<Vec<_>>(),
            [
                ("Run", "cargo run", Kind::Run),
                ("Lint", "cargo clippy", Kind::Other),
                ("Test", "cargo test", Kind::Test),
                ("Run Release", "cargo run --release", Kind::Run),
                ("Also", "x", Kind::Other)
            ]
        );
        assert_eq!(release(&targets), Some(3));
        assert!(!targets[4].release, "only the first is the release");
        assert!(parse_reply(r#"{"targets": []}"#).is_err());
        assert!(parse_reply("nothing").is_err());

        let dir = std::env::temp_dir().join(format!("suspense-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_empty());
        save(&dir, &targets).unwrap();
        assert_eq!(load(&dir), targets);
        std::fs::remove_dir_all(&dir).ok();

        assert!(find_prompt(Path::new("/p")).contains("the project in /p"));
    }

    #[cfg(unix)]
    #[test]
    fn commands_stream_their_output_and_can_be_stopped() {
        let run = Running::start(Path::new("."), "echo one; echo two >&2; exit 3").unwrap();
        assert_eq!(run.wait().unwrap(), Some(3));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(run.exited(), Some(Some(3)));
        let mut seen = run.take_printed();
        seen.sort();
        assert_eq!(seen, ["one", "two"]);

        let run = Running::start(Path::new("."), "sleep 30 & sleep 30").unwrap();
        let started = std::time::Instant::now();
        run.stop();
        assert_eq!(run.wait().unwrap(), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
