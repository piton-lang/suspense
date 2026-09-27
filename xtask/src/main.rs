//! The project's own tasks, run with `cargo xtask <task>`.
//!
//! - `install` builds Suspense in release and installs it for the current
//!   user, with an entry in the platform's app menu: a desktop entry and
//!   icons on Linux, an app in ~/Applications on macOS, and a Start menu
//!   shortcut on Windows. Running it again updates the installed copy.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// Also the Wayland app id and X11 class the window is given, so the
/// desktop entry and the running window are recognized as one app.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const APP_ID: &str = "com.piton-lang.suspense";

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => install(),
        Some("help" | "-h" | "--help") | None => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown task `{other}`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!(
        "Usage: cargo xtask <task>\n\n\
         Tasks:\n  \
         install  Build Suspense in release and install it, with an app menu entry"
    );
}

/// The repository, which holds xtask.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is in the repository")
        .to_path_buf()
}

/// Where the app's icons are, rendered from the Piton favicon.
fn icons() -> PathBuf {
    root().join("packaging").join("icons")
}

fn install() -> Result {
    let root = root();
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    println!("Building Suspense in release…");
    run(Command::new(cargo).current_dir(&root).args([
        "build",
        "--release",
        "--locked",
        "--package",
        "suspense",
    ]))?;
    let binary = root
        .join("target/release")
        .join(format!("suspense{}", env::consts::EXE_SUFFIX));

    install_for_platform(&binary)
}

/// Into ~/.local: the binary in bin, the icons in the hicolor theme, and a
/// desktop entry in applications.
#[cfg(target_os = "linux")]
fn install_for_platform(binary: &Path) -> Result {
    let home = home()?;
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));

    let installed = home.join(".local/bin/suspense");
    copy(binary, &installed)?;

    let hicolor = data.join("icons/hicolor");
    copy(
        &icons().join("suspense.svg"),
        &hicolor.join(format!("scalable/apps/{APP_ID}.svg")),
    )?;
    for size in [16, 24, 32, 48, 64, 128, 256, 512] {
        copy(
            &icons().join(format!("suspense-{size}.png")),
            &hicolor.join(format!("{size}x{size}/apps/{APP_ID}.png")),
        )?;
    }

    let applications = data.join("applications");
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Suspense\n\
         Comment=A REPL and notebook for Piton\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Development;IDE;\n\
         StartupWMClass={APP_ID}\n",
        exec = desktop_exec(&installed),
    );
    write(&applications.join(format!("{APP_ID}.desktop")), entry)?;

    // Refreshing the menu's caches is a nicety; most desktops watch these
    // folders anyway, so it's fine if the tools aren't installed.
    let _ = Command::new("update-desktop-database")
        .arg(&applications)
        .status();
    // An icon cache only needs refreshing if there is one; GTK passes over
    // one older than its folder anyway.
    if hicolor.join("icon-theme.cache").exists() {
        let _ = Command::new("gtk-update-icon-cache")
            .args(["--quiet", "--ignore-theme-index"])
            .arg(&hicolor)
            .stderr(Stdio::null())
            .status();
    }

    println!("Installed Suspense to {}", installed.display());
    if !env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|dir| dir == installed.parent().unwrap()))
    {
        println!(
            "Note: {} isn't on your PATH; the app menu entry works regardless.",
            installed.parent().unwrap().display()
        );
    }
    Ok(())
}

/// `path` quoted and escaped for a desktop entry's Exec key.
#[cfg(target_os = "linux")]
fn desktop_exec(path: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in path.display().to_string().chars() {
        match c {
            '"' | '`' | '$' | '\\' => quoted.extend(['\\', c]),
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    // The Exec key is itself an escaped string, so backslashes double again.
    quoted.replace('\\', "\\\\")
}

/// As Suspense.app in ~/Applications, where Launchpad and Spotlight find it.
#[cfg(target_os = "macos")]
fn install_for_platform(binary: &Path) -> Result {
    let app = home()?.join("Applications/Suspense.app");
    if app.exists() {
        fs::remove_dir_all(&app)
            .map_err(|err| format!("could not replace {}: {err}", app.display()))?;
    }
    let contents = app.join("Contents");
    copy(binary, &contents.join("MacOS/suspense"))?;
    copy(
        &icons().join("suspense.icns"),
        &contents.join("Resources/Suspense.icns"),
    )?;

    let version = package_version()?;
    let plist = read(&root().join("packaging/macos/Info.plist"))?
        .replace("@VERSION@", &version)
        .replace("@BUNDLE_VERSION@", &version);
    write(&contents.join("Info.plist"), plist)?;

    // Sign it ad hoc, as Apple silicon won't run an unsigned app, and let
    // Launch Services know it's there, so Finder shows its icon straight away.
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app))?;
    let _ = Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .arg("-f")
    .arg(&app)
    .status();

    println!("Installed Suspense to {}", app.display());
    Ok(())
}

/// Into %LOCALAPPDATA%\Programs\Suspense, with a shortcut in the Start menu.
#[cfg(windows)]
fn install_for_platform(binary: &Path) -> Result {
    let dir = env_path("LOCALAPPDATA")?.join("Programs").join("Suspense");
    let installed = dir.join("suspense.exe");
    copy(binary, &installed)?;
    let icon = dir.join("suspense.ico");
    copy(&icons().join("suspense.ico"), &icon)?;

    let shortcut = [
        "Microsoft",
        "Windows",
        "Start Menu",
        "Programs",
        "Suspense.lnk",
    ]
    .iter()
    .fold(env_path("APPDATA")?, |path, part| path.join(part));
    // The paths go by environment, to spare quoting them for PowerShell.
    run(Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:SUSPENSE_SHORTCUT); \
             $s.TargetPath = $env:SUSPENSE_TARGET; \
             $s.WorkingDirectory = $env:SUSPENSE_DIR; \
             $s.IconLocation = \"$env:SUSPENSE_ICON,0\"; \
             $s.Description = 'A REPL and notebook for Piton'; \
             $s.Save()",
        ])
        .env("SUSPENSE_SHORTCUT", &shortcut)
        .env("SUSPENSE_TARGET", &installed)
        .env("SUSPENSE_DIR", &dir)
        .env("SUSPENSE_ICON", &icon))?;

    println!("Installed Suspense to {}", installed.display());
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn install_for_platform(_binary: &Path) -> Result {
    Err(format!("installing isn't supported on {}", env::consts::OS))
}

#[cfg(unix)]
fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME isn't set".into())
}

#[cfg(windows)]
fn env_path(name: &str) -> Result<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} isn't set"))
}

/// The version in Suspense's Cargo.toml.
#[cfg(target_os = "macos")]
fn package_version() -> Result<String> {
    read(&root().join("Cargo.toml"))?
        .lines()
        .find_map(|line| {
            let value = line
                .strip_prefix("version")?
                .trim_start()
                .strip_prefix('=')?;
            Some(value.trim().trim_matches('"').to_string())
        })
        .ok_or_else(|| "Cargo.toml has no version".into())
}

fn run(command: &mut Command) -> Result {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|err| format!("could not run {program}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed ({status})"))
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|err| format!("could not read {}: {err}", path.display()))
}

#[cfg_attr(windows, allow(dead_code))]
fn write(path: &Path, contents: String) -> Result {
    create_parent(path)?;
    fs::write(path, contents).map_err(|err| format!("could not write {}: {err}", path.display()))
}

/// Copies `from` to `to`, making its folder. The old file is removed first,
/// so replacing a binary that's running doesn't fail on Linux ("text file
/// busy") and the running copy carries on undisturbed.
fn copy(from: &Path, to: &Path) -> Result {
    create_parent(to)?;
    #[cfg(unix)]
    let _ = fs::remove_file(to);
    fs::copy(from, to).map(drop).map_err(|err| {
        format!(
            "could not copy {} to {}: {err}",
            from.display(),
            to.display()
        )
    })
}

fn create_parent(path: &Path) -> Result {
    let parent = path.parent().expect("an install path has a folder");
    fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))
}
