//! Starts the programs the application runs, as the ApplicationScope says:
//! on Windows, without a console window, since a release build there is a GUI
//! application with no console, and Windows gives every console program it
//! starts a window of its own unless told not to. Elsewhere, as ever.

use std::ffi::OsStr;
use std::process::Command;

/// Windows' `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command running `program`, which opens no window of its own: every
/// child process the application starts is made here.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The arguments of `command` as the debug log gives them: never the value
/// of an environment variable passed to it, as Podman's `-e KEY=VALUE`, and
/// never a prompt, as a system prompt given as an argument, which shows only
/// as how long it is.
pub fn describe(command: &Command) -> String {
    let mut words = vec![command.get_program().to_string_lossy().into_owned()];
    let mut env_next = false;
    let mut prompt_next = false;
    for arg in command.get_args() {
        let arg = arg.to_string_lossy();
        let word = if env_next {
            arg.split('=').next().unwrap_or_default().to_string() + "=…"
        } else if prompt_next || arg.contains('\n') || arg.chars().count() > 200 {
            format!("<{}-chars>", arg.chars().count())
        } else if let Some((flag, _)) = arg.split_once('=').filter(|(flag, _)| ENV_FLAGS.contains(flag)) {
            format!("{flag}=…")
        } else {
            arg.to_string()
        };
        env_next = matches!(arg.as_ref(), "-e" | "--env");
        prompt_next = PROMPT_FLAGS.contains(&arg.as_ref());
        words.push(if word.is_empty() || word.contains(' ') {
            format!("{word:?}")
        } else {
            word
        });
    }
    words.join(" ")
}

/// Flags whose value is an environment variable.
const ENV_FLAGS: [&str; 2] = ["-e", "--env"];

/// Flags whose value is a prompt.
const PROMPT_FLAGS: [&str; 2] = ["--append-system-prompt", "--system-prompt"];

/// Where `command` runs, as the debug log says.
fn directory(command: &Command) -> String {
    match command.get_current_dir() {
        Some(dir) => dir.display().to_string(),
        None => std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    }
}

/// Logs how a child process the application ran ended, with how long it
/// took, and its error output when it failed.
fn log_ended(
    described: &str,
    dir: &str,
    took: std::time::Duration,
    status: std::io::Result<std::process::ExitStatus>,
    stderr: Option<&[u8]>,
) {
    let how = match &status {
        Ok(status) if status.success() => "exited 0".to_string(),
        Ok(status) => match status.code() {
            Some(code) => format!("exited {code}"),
            None => format!("ended by {status}"),
        },
        Err(err) => format!("could not run: {err}"),
    };
    let failed = !matches!(&status, Ok(status) if status.success());
    let stderr = stderr
        .filter(|_| failed)
        .map(|stderr| String::from_utf8_lossy(stderr).trim().to_string())
        .filter(|stderr| !stderr.is_empty())
        .map(|stderr| format!("\n{stderr}"))
        .unwrap_or_default();
    crate::debug_log::log(
        None,
        format!(
            "ran {described} in {dir}: {how} after {}{stderr}",
            crate::debug_log::took(took)
        ),
    );
}

/// Runs a [`Command`] as [`Command::output`], [`Command::status`], and
/// [`Command::spawn`] do, logging it in the debug log.
pub trait Logged {
    /// As [`Command::output`], logged.
    fn output_logged(&mut self) -> std::io::Result<std::process::Output>;
    /// As [`Command::status`], logged.
    fn status_logged(&mut self) -> std::io::Result<std::process::ExitStatus>;
    /// As [`Command::spawn`], logged as it starts; how it ends is logged by
    /// [`ended`] once it is waited on.
    fn spawn_logged(&mut self) -> std::io::Result<std::process::Child>;
}

impl Logged for Command {
    fn output_logged(&mut self) -> std::io::Result<std::process::Output> {
        let started = std::time::Instant::now();
        let output = self.output();
        let (status, stderr) = match &output {
            Ok(output) => (Ok(output.status), Some(output.stderr.as_slice())),
            Err(err) => (Err(std::io::Error::new(err.kind(), err.to_string())), None),
        };
        log_ended(&describe(self), &directory(self), started.elapsed(), status, stderr);
        output
    }

    fn status_logged(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        let status = self.status();
        let logged = match &status {
            Ok(status) => Ok(*status),
            Err(err) => Err(std::io::Error::new(err.kind(), err.to_string())),
        };
        log_ended(&describe(self), &directory(self), started.elapsed(), logged, None);
        status
    }

    fn spawn_logged(&mut self) -> std::io::Result<std::process::Child> {
        let (described, dir) = (describe(self), directory(self));
        let child = self.spawn();
        match &child {
            Ok(child) => {
                crate::debug_log::log(
                    None,
                    format!("started {described} in {dir} as {}", child.id()),
                );
                let mut spawned = spawned();
                // Those never waited on are forgotten, once there are many.
                if spawned.len() >= 256 {
                    spawned.remove(0);
                }
                spawned.push(Spawned {
                    pid: child.id(),
                    described,
                    dir,
                    started: std::time::Instant::now(),
                });
            }
            Err(err) => crate::debug_log::log(
                None,
                format!("ran {described} in {dir}: could not run: {err}"),
            ),
        }
        child
    }
}

/// A child process started with [`Logged::spawn_logged`] whose end is yet
/// to be logged.
struct Spawned {
    pid: u32,
    described: String,
    dir: String,
    started: std::time::Instant,
}

fn spawned() -> std::sync::MutexGuard<'static, Vec<Spawned>> {
    static SPAWNED: std::sync::Mutex<Vec<Spawned>> = std::sync::Mutex::new(Vec::new());
    SPAWNED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The child process `pid`, started with [`Logged::spawn_logged`], ended
/// with `status`, having printed `stderr` on its error output, when that was
/// read: logged with how long it ran. Once only.
pub fn ended(pid: u32, status: Option<std::process::ExitStatus>, stderr: Option<&str>) {
    let found = {
        let mut spawned = spawned();
        let at = spawned.iter().position(|spawned| spawned.pid == pid);
        at.map(|at| spawned.remove(at))
    };
    let Some(spawned) = found else {
        return;
    };
    let status = status.ok_or_else(|| std::io::Error::other("its status is not known"));
    log_ended(
        &spawned.described,
        &spawned.dir,
        spawned.started.elapsed(),
        status,
        stderr.map(str::as_bytes),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command is logged with its arguments, but never an environment
    /// variable's value or a prompt.
    #[test]
    fn commands_are_described_without_secrets_or_prompts() {
        let mut podman = command("podman");
        podman.args(["run", "-e", "TOKEN=secret", "--env=KEY=hidden", "image", "claude"]);
        podman.args(["--append-system-prompt", "You are", "x".repeat(300).as_str(), "a\nb"]);
        let described = describe(&podman);
        assert!(!described.contains("secret") && !described.contains("hidden"), "{described}");
        assert!(described.contains("-e TOKEN=… --env=…"), "{described}");
        assert!(!described.contains("You are") && !described.contains("xxx"), "{described}");
        assert!(described.contains("--append-system-prompt <7-chars>"), "{described}");
        assert!(described.ends_with("<3-chars>"), "{described}");
        assert!(described.starts_with("podman run"), "{described}");
    }
}
