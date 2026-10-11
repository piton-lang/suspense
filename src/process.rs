//! Starts the programs the application runs, as the ApplicationScope says:
//! on Windows, without a console window, since a release build there is a GUI
//! application with no console, and Windows gives every console program it
//! starts a window of its own unless told not to. Elsewhere, as ever. A
//! program named, rather than given as a path, is found where the user's
//! own terminal would find it, as [`crate::programs`] says.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// Windows' `CREATE_NO_WINDOW` process creation flag.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command running `program`, which opens no window of its own: every
/// child process the application starts is made here. A bare name is run
/// from where it is found, and the program is given the PATH searched.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let program = program.as_ref();
    let found = program
        .to_str()
        .filter(|name| crate::programs::is_bare(name))
        .and_then(crate::programs::find);
    #[allow(unused_mut)]
    let mut command = match found {
        Some(path) => crate::programs::command_for(&path),
        None => Command::new(program),
    };
    command.env("PATH", crate::programs::path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Marks `command`'s process, and all it starts, as the one the system ends
/// first when it runs out of memory, as the ApplicationScope's memory
/// pressure says: on Linux, its `oom_score_adj` set to 1000 in the child
/// just before it runs its program, which everything it starts inherits.
/// Suspense's own is left as it was given.
pub fn mark_first_to_end(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: only async-signal-safe calls between fork and exec, on a
        // path and contents made before the fork.
        unsafe {
            command.pre_exec(|| {
                const PATH: &[u8] = b"/proc/self/oom_score_adj\0";
                const SCORE: &[u8] = b"1000";
                let fd = libc::open(PATH.as_ptr().cast(), libc::O_WRONLY);
                if fd >= 0 {
                    libc::write(fd, SCORE.as_ptr().cast(), SCORE.len());
                    libc::close(fd);
                }
                // Never a reason not to run it.
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Marks the running child `pid` as the one the system ends first when it
/// runs out of memory, from Suspense, just after it starts, as a child
/// started without [`mark_first_to_end`] is, so starting it stays cheap.
pub fn mark_started(pid: u32) {
    #[cfg(target_os = "linux")]
    std::fs::write(format!("/proc/{pid}/oom_score_adj"), "1000").ok();
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// How many processes the system has ended for running out of memory, as
/// Linux counts them; none counted elsewhere.
pub fn oom_kills() -> u64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/vmstat")
            .ok()
            .and_then(|stat| {
                stat.lines()
                    .find_map(|line| line.strip_prefix("oom_kill ")?.trim().parse().ok())
            })
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    0
}

/// Whether a process that ended with `status` was ended for the machine
/// running out of memory: killed by SIGKILL with the system recording an
/// out-of-memory kill since `kills_before`, counted as it started.
pub fn out_of_memory(status: std::process::ExitStatus, kills_before: u64) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal() == Some(libc::SIGKILL) && oom_kills() > kills_before
    }
    #[cfg(not(unix))]
    {
        let _ = (status, kills_before);
        false
    }
}

/// What a run says of `program` ended for the machine running out of
/// memory.
pub fn out_of_memory_message(program: &str) -> String {
    format!("`{program}` was ended because the machine ran out of memory")
}

/// Whether systemd's user manager can give a run a transient scope of its
/// own, asked once.
fn user_scopes() -> bool {
    static ABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ABLE.get_or_init(|| {
        if cfg!(test) || !cfg!(target_os = "linux") || !Path::new("/run/systemd/system").exists() {
            return false;
        }
        let Some(systemd_run) = crate::programs::find("systemd-run") else {
            return false;
        };
        Command::new(systemd_run)
            .args(["--user", "--scope", "--quiet", "--collect", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// `program` with `args`, run in a transient user scope of its own where
/// the system runs systemd, so its out-of-memory handling ends that scope
/// and not Suspense's; elsewhere run as it is. Made as [`command`] makes a
/// command, its working directory and environment still to be given.
pub fn command_in_own_scope(program: impl AsRef<OsStr>, args: &[&OsStr]) -> Command {
    if user_scopes() {
        let mut command = self::command("systemd-run");
        command.args(["--user", "--scope", "--quiet", "--collect", "--"]);
        let found = program
            .as_ref()
            .to_str()
            .filter(|name| crate::programs::is_bare(name))
            .and_then(crate::programs::find);
        match found {
            Some(path) => command.arg(path),
            None => command.arg(program.as_ref()),
        };
        command.args(args);
        command
    } else {
        let mut command = self::command(program);
        command.args(args);
        command
    }
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
    kills_before: u64,
) {
    let how = match &status {
        Ok(status) if out_of_memory(*status, kills_before) => {
            "ended because the machine ran out of memory".to_string()
        }
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

/// Why `command` couldn't be run, saying, where its program couldn't be
/// found, every folder searched, and where it was found but won't run, so.
fn why_not_run(command: &Command, err: std::io::Error) -> std::io::Error {
    if err.kind() != std::io::ErrorKind::NotFound {
        return err;
    }
    let program = command.get_program().to_string_lossy();
    let why = match crate::programs::find(&program) {
        Some(path) => format!("`{}` was found, but it won't run: {err}", path.display()),
        None => crate::programs::not_found(&program),
    };
    std::io::Error::new(err.kind(), why)
}

impl Logged for Command {
    fn output_logged(&mut self) -> std::io::Result<std::process::Output> {
        let started = std::time::Instant::now();
        let kills_before = oom_kills();
        let output = self.output().map_err(|err| why_not_run(self, err));
        let (status, stderr) = match &output {
            Ok(output) => (Ok(output.status), Some(output.stderr.as_slice())),
            Err(err) => (Err(std::io::Error::new(err.kind(), err.to_string())), None),
        };
        log_ended(&describe(self), &directory(self), started.elapsed(), status, stderr, kills_before);
        output
    }

    fn status_logged(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        let kills_before = oom_kills();
        let status = self.status().map_err(|err| why_not_run(self, err));
        let logged = match &status {
            Ok(status) => Ok(*status),
            Err(err) => Err(std::io::Error::new(err.kind(), err.to_string())),
        };
        log_ended(&describe(self), &directory(self), started.elapsed(), logged, None, kills_before);
        status
    }

    fn spawn_logged(&mut self) -> std::io::Result<std::process::Child> {
        let (described, dir) = (describe(self), directory(self));
        let child = self.spawn().map_err(|err| why_not_run(self, err));
        match &child {
            Ok(child) => {
                mark_started(child.id());
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
                    kills_before: oom_kills(),
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
    /// The system's out-of-memory kills as it started.
    kills_before: u64,
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
        spawned.kills_before,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every child it starts is marked as the one the system ends first
    /// when it runs out of memory, and Suspense itself is not.
    #[cfg(target_os = "linux")]
    #[test]
    fn children_are_ended_first_when_memory_runs_out() {
        let mut marked = command("sh");
        marked.args(["-c", "cat /proc/self/oom_score_adj"]);
        mark_first_to_end(&mut marked);
        let output = marked.output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1000");
        let own = std::fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        assert_ne!(own.trim(), "1000", "Suspense marked itself");
        assert!(out_of_memory_message("cargo").contains("ran out of memory"));
    }

    /// A command is logged with its arguments, but never an environment
    /// variable's value or a prompt.
    #[test]
    fn commands_are_described_without_secrets_or_prompts() {
        let mut podman = Command::new("podman");
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
