//! The macOS backend, as the SandboxScope says: Seatbelt. A sandbox profile
//! is generated from the policy, in the sandbox profile language, and the
//! run's program is started through `/usr/bin/sandbox-exec` with it, so it
//! is in force before the program runs and inherited by every process it
//! starts, with no administrator rights.
//!
//! `sandbox-exec` and its profile language are deprecated by Apple and
//! undocumented in parts; this backend is untested on a Mac at the time of
//! writing, as SANDBOX.md says.

use std::process::Command;

use super::{Access, Compiled, Refused};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub fn status() -> Result<String, Refused> {
    if std::path::Path::new(SANDBOX_EXEC).is_file() {
        Ok("Seatbelt: files only, not local sockets".into())
    } else {
        Err(Refused {
            message: format!("{SANDBOX_EXEC}, which the sandbox needs, isn't on this Mac"),
            details: String::new(),
        })
    }
}

/// A path as the profile language quotes it.
fn quoted(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Seatbelt profile `compiled` gives: deny by default, allow reading,
/// or reading and writing, only the policy's paths and what any process
/// needs to run, and deny the denied paths outright.
pub fn profile(compiled: &Compiled) -> String {
    let mut out = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec process-fork process-info* signal (target same-sandbox))\n\
         (allow sysctl-read mach-lookup ipc-posix-shm* ipc-posix-sem iokit-open user-preference-read)\n\
         (allow network*)\n\
         (allow file-read-metadata)\n\
         (allow file-read* file-write* (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (literal \"/dev/dtracehelper\"))\n\
         (allow file-read* (literal \"/dev/random\") (literal \"/dev/urandom\") (literal \"/\"))\n",
    );
    for rule in &compiled.rules {
        let what = match rule.access {
            Access::ReadOnly => "file-read*",
            Access::ReadWrite => "file-read* file-write*",
        };
        out.push_str(&format!(
            "(allow {what} (subpath {}))\n",
            quoted(&rule.path)
        ));
    }
    // Last, so they win over any grant.
    for denied in &compiled.denied {
        out.push_str(&format!(
            "(deny file-read* file-write* (subpath {}))\n",
            quoted(denied)
        ));
    }
    out
}

pub fn enforce(command: &mut Command, compiled: &Compiled) -> Result<(), Refused> {
    status()?;
    let program = command.get_program().to_owned();
    let args: Vec<std::ffi::OsString> = command.get_args().map(ToOwned::to_owned).collect();
    let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    let dir = command.get_current_dir().map(ToOwned::to_owned);
    let mut wrapped = crate::process::command(SANDBOX_EXEC);
    wrapped
        .arg("-p")
        .arg(profile(compiled))
        .arg(program)
        .args(args);
    for (key, value) in envs {
        match value {
            Some(value) => wrapped.env(key, value),
            None => wrapped.env_remove(key),
        };
    }
    if let Some(dir) = dir {
        wrapped.current_dir(dir);
    }
    *command = wrapped;
    Ok(())
}
