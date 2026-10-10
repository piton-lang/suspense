//! Finds the programs the application runs where the user's own terminal
//! would, as the ApplicationScope says, however Suspense was started: an
//! application started from the desktop, the dock, Finder, or the Start menu
//! isn't given the PATH the user's shell sets up.
//!
//! On macOS and Linux, the login shell's PATH, read once at launch, then the
//! application's own, then the places programs installed for the user are
//! put. On Windows, the user's and the machine's PATH as the registry holds
//! them now, then the application's own, then npm's global folder, each name
//! tried with each extension PATHEXT lists.
//!
//! What is found is remembered until [`forget`], which the Welcome checks
//! call, so a program installed while Suspense runs is found without
//! restarting it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::{LazyLock, Mutex, MutexGuard};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// How long the login shell, and npm, are given to say what they set up.
#[cfg(unix)]
const TIMEOUT: Duration = Duration::from_secs(5);

/// The folders searched, and the programs found in them, until forgotten.
#[derive(Default)]
struct Found {
    dirs: Option<Vec<PathBuf>>,
    path: Option<OsString>,
    programs: HashMap<String, Option<PathBuf>>,
}

fn found() -> MutexGuard<'static, Found> {
    static FOUND: LazyLock<Mutex<Found>> = LazyLock::new(Mutex::default);
    // A poisoned lock still holds what was found.
    FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts reading the login shell's PATH at launch, in the background, so
/// the first program run doesn't wait on it.
pub fn read_at_launch() {
    #[cfg(unix)]
    std::thread::spawn(|| {
        login_path();
    });
}

/// Forgets what was found, so every program is looked for again, in the
/// folders as they are now.
pub fn forget() {
    *found() = Found::default();
}

/// Every folder searched for a program, in the order searched, each once.
pub fn searched() -> Vec<PathBuf> {
    if let Some(dirs) = found().dirs.clone() {
        return dirs;
    }
    let dirs = search_dirs();
    found().dirs = Some(dirs.clone());
    dirs
}

/// The PATH every program the application runs is given, the folders
/// searched, so what it runs in turn, as the `node` an npm-installed
/// program needs, is found as well.
pub fn path() -> OsString {
    if let Some(path) = found().path.clone() {
        return path;
    }
    let path = std::env::join_paths(searched()).unwrap_or_default();
    found().path = Some(path.clone());
    path
}

/// Where `program` is, when it is a bare name: the first match in the
/// folders searched. A path is taken as it is, when it is a file.
pub fn find(program: &str) -> Option<PathBuf> {
    if !is_bare(program) {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    if let Some(found) = found().programs.get(program) {
        return found.clone();
    }
    let at = searched().iter().find_map(|dir| in_dir(dir, program));
    found().programs.insert(program.to_string(), at.clone());
    at
}

/// Whether `program` is a name to search for, rather than a path.
pub fn is_bare(program: &str) -> bool {
    !program.is_empty() && !program.contains(['/', '\\'])
}

/// Why `program` couldn't be found: every folder that was searched.
pub fn not_found(program: &str) -> String {
    let dirs = searched();
    if !is_bare(program) || dirs.is_empty() {
        return format!("`{program}` couldn't be found.");
    }
    let dirs: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
    format!(
        "`{program}` couldn't be found in any of the folders searched: {}.",
        dirs.join(", ")
    )
}

/// `program` in `dir`, if it is there and can be run.
#[cfg(unix)]
fn in_dir(dir: &Path, program: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(program);
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(path)
}

/// `program` in `dir`, with each extension PATHEXT lists, or as it is when
/// it has one of them already: never an extensionless file, as npm's shell
/// script beside its `.cmd`.
#[cfg(windows)]
fn in_dir(dir: &Path, program: &str) -> Option<PathBuf> {
    let exts = extensions();
    let has_ext = Path::new(program)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .is_some_and(|ext| exts.iter().any(|known| known.eq_ignore_ascii_case(&ext)));
    if has_ext {
        let path = dir.join(program);
        return path.is_file().then_some(path);
    }
    exts.iter()
        .map(|ext| dir.join(format!("{program}{ext}")))
        .find(|path| path.is_file())
}

/// The extensions PATHEXT lists, and `.ps1`, as a package manager's shim
/// may be, when it doesn't.
#[cfg(windows)]
fn extensions() -> Vec<String> {
    let listed = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let mut exts: Vec<String> = listed
        .split(';')
        .map(str::trim)
        .filter(|ext| ext.starts_with('.'))
        .map(str::to_string)
        .collect();
    if !exts.iter().any(|ext| ext.eq_ignore_ascii_case(".ps1")) {
        exts.push(".PS1".into());
    }
    exts
}

/// The command that runs `path`, found by [`find`]: a `.ps1` through
/// PowerShell with no profile. A `.cmd` or `.bat` is run by the standard
/// library through `cmd.exe`, its arguments quoted for it.
pub fn command_for(path: &Path) -> Command {
    #[cfg(windows)]
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
    {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(path);
        return command;
    }
    Command::new(path)
}

/// Adds each of `dirs` to `into`, once.
fn add(into: &mut Vec<PathBuf>, dirs: impl IntoIterator<Item = PathBuf>) {
    for dir in dirs {
        if !dir.as_os_str().is_empty() && dir.is_dir() && !into.contains(&dir) {
            into.push(dir);
        }
    }
}

fn own_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

#[cfg(unix)]
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    add(&mut dirs, login_path().iter().cloned());
    add(&mut dirs, own_path());
    add(&mut dirs, user_dirs());
    // npm's global prefix, as the npm found so far gives it.
    let npm = dirs.iter().find_map(|dir| in_dir(dir, "npm"));
    add(&mut dirs, npm.and_then(|npm| npm_prefix(&npm)).map(|prefix| prefix.join("bin")));
    add(&mut dirs, ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs
}

#[cfg(windows)]
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for path in registry::paths() {
        add(&mut dirs, std::env::split_paths(&path).collect::<Vec<_>>());
    }
    add(&mut dirs, own_path());
    add(&mut dirs, std::env::var_os("APPDATA").map(|data| PathBuf::from(data).join("npm")));
    dirs
}

/// The places programs installed for the user are put, but npm's prefix and
/// the system's: `~/.local/bin`, `~/.cargo/bin`, `~/.npm-global/bin`,
/// `~/.bun/bin`, `~/.volta/bin`, and the active Node version's `bin` of
/// nvm, fnm, or asdf.
#[cfg(unix)]
fn user_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = [".local/bin", ".cargo/bin", ".npm-global/bin", ".bun/bin", ".volta/bin"]
        .iter()
        .map(|dir| home.join(dir))
        .collect();
    dirs.extend(nvm_bin(&home));
    let fnm = std::env::var_os("FNM_DIR").map(PathBuf::from).into_iter().chain([
        home.join(".local/share/fnm"),
        home.join(".fnm"),
        home.join("Library/Application Support/fnm"),
    ]);
    dirs.extend(fnm.map(|fnm| fnm.join("aliases/default/bin")));
    let asdf = std::env::var_os("ASDF_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".asdf"));
    dirs.push(asdf.join("shims"));
    dirs
}

/// nvm's active Node version's `bin`: its default alias's version, or the
/// newest installed when it has none.
#[cfg(unix)]
fn nvm_bin(home: &Path) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("NVM_BIN") {
        return Some(PathBuf::from(bin));
    }
    let nvm = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    let versions = nvm.join("versions/node");
    let mut installed: Vec<String> = std::fs::read_dir(&versions)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    installed.sort_by_key(|version| node_version(version));
    let alias = std::fs::read_to_string(nvm.join("alias/default")).unwrap_or_default();
    let alias = alias.trim().trim_start_matches('v');
    let chosen = installed
        .iter()
        .rev()
        .find(|version| {
            let version = version.trim_start_matches('v');
            !alias.is_empty() && (version == alias || version.starts_with(&format!("{alias}.")))
        })
        .or(installed.last())?;
    Some(versions.join(chosen).join("bin"))
}

/// A Node version's numbers, for ordering.
#[cfg(unix)]
fn node_version(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// npm's global prefix, as `npm prefix -g` gives it.
#[cfg(unix)]
fn npm_prefix(npm: &Path) -> Option<PathBuf> {
    let mut command = Command::new(npm);
    command.args(["prefix", "-g"]);
    if let Some(dir) = npm.parent() {
        // npm is a Node script: its `node` is beside it.
        let mut path = vec![dir.to_path_buf()];
        path.extend(own_path());
        command.env("PATH", std::env::join_paths(path).ok()?);
    }
    let printed = printed_within(command)?;
    let prefix = printed.trim();
    (!prefix.is_empty()).then(|| PathBuf::from(prefix))
}

/// The PATH the user's login shell sets up, read once at launch.
#[cfg(unix)]
fn login_path() -> &'static [PathBuf] {
    static LOGIN: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    LOGIN.get_or_init(read_login_path)
}

/// Runs the shell SHELL names as a login, interactive shell, printing its
/// PATH after a marker, so whatever its startup files print is passed over.
#[cfg(unix)]
fn read_login_path() -> Vec<PathBuf> {
    // A test never runs the user's own shell.
    if cfg!(test) {
        return Vec::new();
    }
    const MARKER: &str = "__SUSPENSE_PATH__";
    let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty());
    let shell = shell.unwrap_or_else(|| {
        if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" }.into()
    });
    let print = if shell.ends_with("fish") {
        format!("printf '\\n{MARKER}%s\\n' (string join : $PATH)")
    } else {
        format!("printf '\\n{MARKER}%s\\n' \"$PATH\"")
    };
    let mut command = Command::new(&shell);
    command.args(["-l", "-i", "-c", &print]);
    // Its own process group, so it never takes the terminal Suspense was
    // started from, if any.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let Some(printed) = printed_within(command) else {
        crate::debug_log::log(None, format!("couldn't read the PATH {shell} sets up"));
        return Vec::new();
    };
    let path = printed
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MARKER))
        .unwrap_or_default();
    std::env::split_paths(path).collect()
}

/// What `command` prints, once it has exited with success, given
/// [`TIMEOUT`] to.
#[cfg(unix)]
fn printed_within(mut command: Command) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut out).ok();
        out
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            child.kill().ok();
            child.wait().ok();
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // What it started may hold its output open: never wait on that.
    let out = if reader.is_finished() || {
        std::thread::sleep(Duration::from_millis(100));
        reader.is_finished()
    } {
        reader.join().ok()?
    } else {
        return None;
    };
    status.success().then_some(out)
}

/// The PATH of the user and of the machine, as the registry holds them now.
#[cfg(windows)]
mod registry {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };

    /// The machine's PATH, then the user's, as Windows joins them.
    pub fn paths() -> Vec<OsString> {
        [
            (
                HKEY_LOCAL_MACHINE,
                r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            ),
            (HKEY_CURRENT_USER, "Environment"),
        ]
        .into_iter()
        .filter_map(|(key, sub)| read(key, sub, "Path"))
        .collect()
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text).encode_wide().chain([0]).collect()
    }

    /// A string value, its `%VARIABLES%` expanded.
    fn read(key: HKEY, sub: &str, name: &str) -> Option<OsString> {
        let (sub, name) = (wide(sub), wide(name));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
        let mut size: u32 = 0;
        // SAFETY: the strings are NUL-terminated; a null buffer asks the size.
        let status = unsafe {
            RegGetValueW(key, sub.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
        };
        if status != 0 || size == 0 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: the buffer is `size` bytes long.
        let status = unsafe {
            RegGetValueW(
                key,
                sub.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let len = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
        Some(OsString::from_wide(&buffer[..len]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name is searched for; a path is taken as it is.
    #[test]
    fn names_are_searched_for_and_paths_taken_as_they_are() {
        assert!(is_bare("codex"));
        assert!(!is_bare("/usr/bin/codex"));
        assert!(!is_bare(r"C:\npm\codex.cmd"));
        assert_eq!(find("/nonexistent/codex"), None);
    }

    /// A program not found names every folder searched.
    #[test]
    fn not_found_names_the_folders_searched() {
        let said = not_found("no-such-program-anywhere");
        for dir in searched() {
            assert!(said.contains(&dir.display().to_string()), "{said}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn nvm_versions_are_ordered_by_number() {
        let mut versions = vec!["v9.0.0", "v18.2.0", "v18.10.1"];
        versions.sort_by_key(|version| node_version(version));
        assert_eq!(versions, ["v9.0.0", "v18.2.0", "v18.10.1"]);
    }
}
