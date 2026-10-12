//! Whether a harness is logged in on the host, as its own status command
//! reports, and logging it in, in a terminal, as the WelcomeScope says. The
//! harness keeps its login where it always does on the host; every run uses
//! it, granted by its sandbox.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};

use crate::agent::Agent;
use crate::process::Logged as _;

/// The harness's own command that says whether it is logged in.
fn status_args(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "status"],
        Agent::Codex => &["login", "status"],
        Agent::OpenCode => &["auth", "list"],
    }
}

/// The harness's own login command, as the welcome page names it.
pub fn login_command(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude /login",
        Agent::Codex => "codex login",
        Agent::OpenCode => "opencode auth login",
    }
}

/// Whether `agent` is logged in on the host, as `printed` by its status
/// command, which `succeeded`, says.
fn says_logged_in(agent: Agent, succeeded: bool, printed: &str) -> bool {
    match agent {
        Agent::Claude => serde_json::from_str::<serde_json::Value>(printed)
            .ok()
            .and_then(|status| status.get("loggedIn")?.as_bool())
            .unwrap_or(false),
        Agent::Codex => succeeded && printed.to_lowercase().contains("logged in"),
        // Its stored credentials, one a line.
        Agent::OpenCode => succeeded && printed.lines().any(|line| line.contains("stored")),
    }
}

/// Whether `agent` is logged in on the host. Blocking.
pub fn logged_in(agent: Agent) -> Result<bool> {
    let output = crate::process::command(agent.command())
        .args(status_args(agent))
        .envs(agent.env().iter().copied())
        .stdin(Stdio::null())
        .output_logged()
        .with_context(|| format!("could not run {}", agent.command()))?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(says_logged_in(agent, output.status.success(), &printed))
}

/// Opens a terminal in `dir` running `command`, for the user to log in
/// with; the terminal stays open after.
pub fn open_terminal(dir: &Path, command: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\" to do script \"cd '{}' && {command}\"",
            dir.display()
        );
        crate::process::command("osascript")
            .args(["-e", &script, "-e", "tell application \"Terminal\" to activate"])
            .spawn_logged()
            .context("could not open Terminal")?;
        Ok(())
    }
    #[cfg(windows)]
    {
        crate::process::command("cmd")
            .args(["/C", "start", "cmd", "/K", command])
            .current_dir(dir)
            .spawn_logged()
            .context("could not open a terminal")?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
        let line = format!("{command}; exec {shell}");
        let mut terminals: Vec<String> = std::env::var("TERMINAL").into_iter().collect();
        terminals.extend(
            ["x-terminal-emulator", "kgx", "gnome-terminal", "konsole", "alacritty", "kitty", "wezterm", "foot", "xterm"]
                .map(String::from),
        );
        for terminal in terminals {
            if crate::programs::find(&terminal).is_none() {
                continue;
            }
            let mut open = crate::process::command(&terminal);
            match terminal.as_str() {
                "gnome-terminal" | "kgx" => open.args(["--", "sh", "-c", &line]),
                "wezterm" => open.args(["start", "--", "sh", "-c", &line]),
                _ => open.args(["-e", "sh", "-c", &line]),
            };
            if open.current_dir(dir).spawn_logged().is_ok() {
                return Ok(());
            }
        }
        bail!("no terminal was found to run `{command}` in; run it in one of your own")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_harness_says_whether_it_is_logged_in() {
        assert!(says_logged_in(Agent::Claude, true, r#"{ "loggedIn": true }"#));
        assert!(!says_logged_in(Agent::Claude, true, r#"{ "loggedIn": false }"#));
        assert!(says_logged_in(Agent::Codex, true, "Logged in using ChatGPT"));
        assert!(!says_logged_in(Agent::Codex, false, "Not logged in"));
        assert!(says_logged_in(Agent::OpenCode, true, "xAI   xAI   stored"));
        assert!(!says_logged_in(Agent::OpenCode, true, ""));
        assert_eq!(login_command(Agent::Claude), "claude /login");
    }
}
