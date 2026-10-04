//! Self-update, as the SelfUpdateScope says: a build published by the edge
//! releases workflow, which bakes in the repository it came from, looks for
//! a newer `0.1.N` among that repository's releases, downloads its binary for
//! this platform in the background, stages it beside the running binary, and
//! puts it in place whenever Suspense quits; Restart quits, as quitting does,
//! and starts it again. A build made anywhere else never updates itself.
//!
//! GitHub is asked with `curl`, and archives unpacked with `tar`, as every
//! platform Suspense is published for has them, each started without a
//! window (see [`crate::process`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui_kit::*;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The repository the running build was published from, `owner/name`, baked
/// in by the edge releases workflow; none for a build made anywhere else.
pub const REPOSITORY: Option<&str> = option_env!("SUSPENSE_REPOSITORY");

/// How long after starting the first check waits.
const FIRST_CHECK: Duration = Duration::from_secs(5);

/// How often it checks while it runs.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a request to GitHub may take.
const TIMEOUT_SECS: &str = "30";

/// The N of a version or tag `0.1.N`, as `v0.1.42` or `0.1.42`.
pub fn sequence(version: &str) -> Option<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .strip_prefix("0.1.")?
        .parse()
        .ok()
}

/// The asset an edge release `0.1.n` holds for the platform running; none
/// where it publishes none.
pub fn asset_name(n: u64) -> Option<String> {
    let platform = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64.tar.gz"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64.zip"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-aarch64.tar.gz"
    } else {
        return None;
    };
    Some(format!("suspense-0.1.{n}-{platform}"))
}

/// A release to update to: its version, its page, and its asset for this
/// platform, with the size and digest GitHub gives for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub n: u64,
    pub page: String,
    pub asset_url: String,
    pub size: u64,
    /// The asset's SHA-256, in hex, where GitHub gives one.
    pub sha256: Option<String>,
}

impl Release {
    pub fn version(&self) -> String {
        format!("0.1.{}", self.n)
    }
}

/// Why a build can't update itself, if it can't.
pub fn cant_update() -> Option<&'static str> {
    if REPOSITORY.is_none() {
        return Some("This build wasn't published as an edge release, so it doesn't update itself.");
    }
    if sequence(crate::version::VERSION).is_none() {
        return Some("This build's version isn't an edge release's, so it doesn't update itself.");
    }
    if asset_name(0).is_none() {
        return Some("No edge release is published for this platform.");
    }
    None
}

/// The newest release among GitHub's `releases`, newer than `running`, with
/// an asset `asset(n)` names; versions compared by their number.
pub fn newest(releases: &Value, running: u64, asset: impl Fn(u64) -> Option<String>) -> Option<Release> {
    let mut best: Option<Release> = None;
    for release in releases.as_array()? {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(n) = release.get("tag_name").and_then(Value::as_str).and_then(sequence) else {
            continue;
        };
        if n <= running || best.as_ref().is_some_and(|best| best.n >= n) {
            continue;
        }
        let Some(name) = asset(n) else { continue };
        let Some(found) = release
            .get("assets")
            .and_then(Value::as_array)
            .and_then(|assets| {
                assets
                    .iter()
                    .find(|a| a.get("name").and_then(Value::as_str) == Some(&name))
            })
        else {
            continue;
        };
        let (Some(url), Some(size)) = (
            found.get("browser_download_url").and_then(Value::as_str),
            found.get("size").and_then(Value::as_u64),
        ) else {
            continue;
        };
        best = Some(Release {
            n,
            page: release
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            asset_url: url.to_string(),
            size,
            sha256: found
                .get("digest")
                .and_then(Value::as_str)
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .map(str::to_lowercase),
        });
    }
    best
}

/// Asks GitHub for the repository's releases. Blocking.
fn fetch_releases(repository: &str) -> Result<Value> {
    let url = format!("https://api.github.com/repos/{repository}/releases?per_page=100");
    let output = crate::process::command("curl")
        .args(["-fsSL", "--max-time", TIMEOUT_SECS])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", "User-Agent: suspense"])
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .context("couldn't run curl to ask GitHub for releases")?;
    if !output.status.success() {
        bail!(
            "couldn't reach GitHub: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("GitHub's answer couldn't be read")
}

/// The binary running, as it will be relaunched.
pub fn running_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("couldn't find the running binary")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Where an update is staged beside `exe`, and its version kept.
pub fn staged(exe: &Path) -> PathBuf {
    sibling(exe, "update")
}

fn staged_version(exe: &Path) -> PathBuf {
    sibling(exe, "update-version")
}

/// Where the running binary is set aside while an update takes its place,
/// where the platform won't replace a running binary.
pub fn aside(exe: &Path) -> PathBuf {
    sibling(exe, "old")
}

fn sibling(exe: &Path, what: &str) -> PathBuf {
    let name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "suspense".into());
    exe.with_file_name(format!(".{name}.{what}"))
}

/// Whether a file can be written in `dir`.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".suspense-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    std::fs::remove_file(&probe).ok();
    ok
}

/// Downloads `release`'s asset, checks it, and stages its binary beside
/// `exe`, telling `got` how much has come. Blocking.
pub fn download(release: &Release, exe: &Path, got: &AtomicU64) -> Result<()> {
    let archive = sibling(exe, "download");
    let unpacked = sibling(exe, "unpack");
    std::fs::remove_file(&archive).ok();
    std::fs::remove_dir_all(&unpacked).ok();
    let result = (|| {
        let mut curl = crate::process::command("curl")
            .args(["-fsSL", "-H", "User-Agent: suspense", "-o"])
            .arg(&archive)
            .arg(&release.asset_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("couldn't run curl to download the update")?;
        // How much has come, as the file grows.
        let status = loop {
            if let Some(status) = curl.try_wait()? {
                break status;
            }
            if let Ok(meta) = std::fs::metadata(&archive) {
                got.store(meta.len(), Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        if !status.success() {
            let mut why = String::new();
            if let Some(mut stderr) = curl.stderr.take() {
                std::io::Read::read_to_string(&mut stderr, &mut why).ok();
            }
            bail!("the download failed: {}", why.trim());
        }
        let bytes = std::fs::read(&archive)?;
        got.store(bytes.len() as u64, Ordering::Relaxed);
        if bytes.len() as u64 != release.size {
            bail!(
                "the download was {} bytes, where GitHub gives {}",
                bytes.len(),
                release.size
            );
        }
        if let Some(expected) = &release.sha256 {
            let actual: String = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if &actual != expected {
                bail!("the download's SHA-256 doesn't match GitHub's");
            }
        }
        std::fs::create_dir_all(&unpacked)?;
        let untar = crate::process::command("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .stdin(Stdio::null())
            .output()
            .context("couldn't run tar to unpack the update")?;
        if !untar.status.success() {
            bail!(
                "the update couldn't be unpacked: {}",
                String::from_utf8_lossy(&untar.stderr).trim()
            );
        }
        let name = if cfg!(windows) { "suspense.exe" } else { "suspense" };
        let binary = unpacked.join(name);
        if !binary.is_file() {
            bail!("the update holds no {name}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
        }
        // A newer one replaces any staged before it.
        std::fs::rename(&binary, staged(exe)).context("couldn't stage the update")?;
        std::fs::write(staged_version(exe), release.version()).ok();
        Ok(())
    })();
    std::fs::remove_file(&archive).ok();
    std::fs::remove_dir_all(&unpacked).ok();
    result
}

/// Puts an update staged beside `exe` in its place, setting the running
/// binary aside first where the platform won't replace it; a step that fails
/// puts back the binary that was there. The version put in place, if one was.
pub fn apply(exe: &Path) -> Result<Option<String>> {
    let staged = staged(exe);
    if !staged.is_file() {
        return Ok(None);
    }
    let version = std::fs::read_to_string(staged_version(exe)).ok();
    if cfg!(windows) {
        let aside = aside(exe);
        std::fs::remove_file(&aside).ok();
        std::fs::rename(exe, &aside).context("couldn't set the running binary aside")?;
        if let Err(err) = std::fs::rename(&staged, exe) {
            // Never left without a binary.
            std::fs::rename(&aside, exe).ok();
            return Err(err).context("couldn't put the update in place");
        }
    } else {
        // A rename over the running binary replaces it in one step, or not
        // at all.
        std::fs::rename(&staged, exe).context("couldn't put the update in place")?;
    }
    std::fs::remove_file(staged_version(exe)).ok();
    Ok(Some(version.unwrap_or_default().trim().to_string()))
}

/// At launch, before anything else: deletes a binary set aside by an update,
/// and puts in place an update a quit that never finished left staged, which
/// takes effect at the launch after. The version so put in place, if any, or
/// why it couldn't be.
pub fn at_launch() -> Result<Option<String>, String> {
    if cant_update().is_some() {
        return Ok(None);
    }
    let exe = running_binary().map_err(|err| format!("{err:#}"))?;
    std::fs::remove_file(aside(&exe)).ok();
    apply(&exe).map_err(|err| format!("Suspense couldn't update: {err:#}"))
}

/// Where the updates stand, as the Updates section shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String, got: u64, total: u64 },
    Ready { version: String },
    /// The running binary's directory can't be written: the release's page,
    /// to update by hand.
    NotWritable { dir: PathBuf, page: String },
    Failed(String),
}

/// The application's updates.
pub struct Updates {
    pub status: Status,
    /// When the last check was made, in seconds since the Unix epoch.
    pub last_checked: Option<u64>,
    /// Whether to check for updates automatically.
    pub automatic: bool,
    /// An update a quit that never finished left staged, put in place at
    /// this launch: it takes effect once Suspense restarts.
    pub finished_at_launch: Option<String>,
    got: Arc<AtomicU64>,
    _checks: Task<()>,
}

impl Global for Updates {}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Set when Restart quits, so Suspense starts again once it has.
static RESTART: AtomicBool = AtomicBool::new(false);

impl Updates {
    /// Starts looking for updates, if this build updates itself, and puts a
    /// staged update in place whenever Suspense quits.
    pub fn init(at_launch: Result<Option<String>, String>, cx: &mut App) {
        let checks = if cant_update().is_none() {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_CHECK).await;
                loop {
                    cx.update(|cx| {
                        if Self::get(cx).is_some_and(|updates| updates.automatic) {
                            Self::check(false, cx);
                        }
                    });
                    cx.background_executor().timer(CHECK_EVERY).await;
                }
            })
        } else {
            Task::ready(())
        };
        let (status, finished_at_launch) = match at_launch {
            Ok(finished) => (Status::Idle, finished),
            Err(why) => (Status::Failed(why), None),
        };
        cx.set_global(Self {
            status,
            last_checked: None,
            automatic: preference::load(),
            finished_at_launch,
            got: Arc::default(),
            _checks: checks,
        });
        cx.on_app_quit(|_| {
            // However Suspense quits, a staged update is put in place.
            if cant_update().is_none()
                && let Ok(exe) = running_binary()
            {
                match apply(&exe) {
                    Ok(_) => {
                        if RESTART.load(Ordering::SeqCst) {
                            crate::process::command(&exe).spawn().ok();
                        }
                    }
                    Err(err) => eprintln!("couldn't put the update in place: {err:#}"),
                }
            }
            async {}
        })
        .detach();
    }

    pub fn get(cx: &App) -> Option<&Self> {
        cx.try_global::<Self>()
    }

    fn set(status: Status, cx: &mut App) {
        if let Some(updates) = cx.try_global::<Self>() {
            if updates.status == status {
                return;
            }
        }
        cx.update_global::<Self, _>(|updates, _| updates.status = status);
        cx.refresh_windows();
    }

    /// Turns automatic checks on or off, remembering the choice.
    pub fn set_automatic(on: bool, cx: &mut App) {
        preference::save(on);
        cx.update_global::<Self, _>(|updates, _| updates.automatic = on);
        cx.refresh_windows();
    }

    /// Whether a check or download is under way.
    pub fn busy(cx: &App) -> bool {
        Self::get(cx).is_some_and(|updates| {
            matches!(updates.status, Status::Checking | Status::Downloading { .. })
        })
    }

    /// Checks for an update now, and downloads one found; `asked` when the
    /// user asked, so a failure says why, and quiet otherwise.
    pub fn check(asked: bool, cx: &mut App) {
        let (Some(repository), Some(running)) =
            (REPOSITORY, sequence(crate::version::VERSION))
        else {
            return;
        };
        if Self::busy(cx) || cx.try_global::<Self>().is_none() {
            return;
        }
        let before = Self::get(cx).map(|updates| updates.status.clone()).unwrap_or(Status::Idle);
        Self::set(Status::Checking, cx);
        let found = cx.background_spawn(async move {
            let releases = fetch_releases(repository)?;
            anyhow::Ok(newest(&releases, running, asset_name))
        });
        cx.spawn(async move |cx| {
            let found = found.await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|updates, _| updates.last_checked = Some(now()));
                match found {
                    Ok(None) => {
                        // A staged update stays ready.
                        let status = match before {
                            Status::Ready { .. } => before,
                            _ => Status::UpToDate,
                        };
                        Self::set(status, cx)
                    }
                    Ok(Some(release)) => Self::download(release, cx),
                    Err(err) if asked => Self::set(Status::Failed(format!("{err:#}")), cx),
                    // Quiet, trying again at the next.
                    Err(_) => Self::set(
                        match before {
                            Status::Checking => Status::Idle,
                            before => before,
                        },
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// Downloads and stages `release`, in the background.
    fn download(release: Release, cx: &mut App) {
        // Already staged: nothing more to fetch.
        if let Some(Status::Ready { version }) = Self::get(cx).map(|updates| &updates.status)
            && *version == release.version()
        {
            return;
        }
        let exe = match running_binary() {
            Ok(exe) => exe,
            Err(err) => return Self::set(Status::Failed(format!("{err:#}")), cx),
        };
        let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        if !writable(&dir) {
            return Self::set(
                Status::NotWritable {
                    dir,
                    page: release.page,
                },
                cx,
            );
        }
        let got = Self::get(cx).map(|updates| updates.got.clone()).unwrap_or_default();
        got.store(0, Ordering::Relaxed);
        let version = release.version();
        Self::set(
            Status::Downloading {
                version: version.clone(),
                got: 0,
                total: release.size,
            },
            cx,
        );
        let total = release.size;
        let downloaded = cx.background_spawn({
            let got = got.clone();
            async move { download(&release, &exe, &got) }
        });
        cx.spawn(async move |cx| {
            let mut downloaded = std::pin::pin!(downloaded);
            // How much has come, shown as it comes.
            let result = loop {
                let tick = cx.background_executor().timer(Duration::from_millis(300));
                match futures::future::select(downloaded.as_mut(), std::pin::pin!(tick)).await {
                    futures::future::Either::Left((result, _)) => break result,
                    futures::future::Either::Right(_) => {
                        let now = got.load(Ordering::Relaxed);
                        let version = version.clone();
                        cx.update(|cx| {
                            Self::set(Status::Downloading { version, got: now, total }, cx)
                        });
                    }
                }
            };
            cx.update(|cx| match result {
                Ok(()) => {
                    Self::set(Status::Ready { version: version.clone() }, cx);
                    notify_ready(&version, cx);
                }
                Err(err) => Self::set(Status::Failed(format!("{err:#}")), cx),
            });
        })
        .detach();
    }

    /// Quits, as quitting does, asking first while work runs, and starts
    /// Suspense again once the update is in place.
    pub fn restart(cx: &mut App) {
        crate::main_window::MainWindow::quit_from_anywhere(true, cx);
    }
}

/// Suspense is quitting to start again.
pub fn restart_on_quit() {
    RESTART.store(true, Ordering::SeqCst);
}

/// Says, in the window, that `version` is ready, with Restart and Later.
fn notify_ready(version: &str, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    use gpui_kit::component::notification::Notification;
    use gpui_kit::component::{Sizable as _, h_flex};
    let title = format!("Suspense {version} is ready");
    cx.defer(move |cx| {
        let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
            return;
        };
        handle
            .update(cx, |_, window, cx| {
                let note = Notification::info("Restart to start using it.")
                    .title(title.clone())
                    .autohide(false)
                    .content(|_, _, cx| {
                        let note = cx.entity().downgrade();
                        h_flex()
                            .gap_2()
                            .pt_2()
                            .child(
                                Button::new("update-restart")
                                    .small()
                                    .primary()
                                    .label("Restart")
                                    .on_click(|_, _, cx| Updates::restart(cx)),
                            )
                            .child(Button::new("update-later").small().label("Later").on_click(
                                move |_, window, cx| {
                                    note.update(cx, |note, cx| note.dismiss(window, cx)).ok();
                                },
                            ))
                            .into_any_element()
                    });
                window.push_notification(note, cx);
            })
            .ok();
    });
}

/// Whether to check for updates automatically, remembered for the user, as
/// the UserPreferencesScope says; on until turned off. Tests neither read
/// nor write it.
pub mod preference {
    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(dirs::config_dir()?.join("suspense").join("check-for-updates"))
    }

    pub fn load() -> bool {
        #[cfg(not(test))]
        if let Some(file) = file()
            && std::fs::read_to_string(file).is_ok_and(|text| text.trim() == "off")
        {
            return false;
        }
        true
    }

    /// Saves the choice; it is only a convenience, so failing to is ignored.
    pub fn save(on: bool) {
        #[cfg(not(test))]
        if let Some(file) = file() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(file, if on { "on" } else { "off" }).ok();
        }
        #[cfg(test)]
        let _ = on;
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    #[cfg(unix)]
    use sha2::{Digest as _, Sha256};

    #[cfg(unix)]
    use super::{Release, download};
    use super::{apply, newest, sequence, staged, staged_version, writable};

    #[test]
    fn versions_are_compared_by_number() {
        assert_eq!(sequence("v0.1.42"), Some(42));
        assert_eq!(sequence("0.1.9"), Some(9));
        assert_eq!(sequence("v1.0.0"), None);
        let asset = |n: u64| Some(format!("suspense-0.1.{n}-test.tar.gz"));
        let release = |n: u64, with_asset: bool| {
            json!({
                "tag_name": format!("v0.1.{n}"),
                "html_url": format!("https://example/releases/v0.1.{n}"),
                "prerelease": true,
                "assets": if with_asset { json!([{
                    "name": format!("suspense-0.1.{n}-test.tar.gz"),
                    "browser_download_url": format!("https://example/{n}"),
                    "size": 10,
                    "digest": "sha256:ABC"
                }]) } else { json!([]) }
            })
        };
        // 0.1.10 is newer than 0.1.9, though it sorts before it as text; the
        // newest lacks this platform's asset, so the next is taken.
        let releases = json!([release(9, true), release(10, true), release(11, false)]);
        let found = newest(&releases, 8, asset).unwrap();
        assert_eq!(found.n, 10);
        assert_eq!(found.sha256.as_deref(), Some("abc"));
        assert_eq!(found.page, "https://example/releases/v0.1.10");
        // Never back, nor to the same.
        assert_eq!(newest(&releases, 10, asset), None);
    }

    /// A build that doesn't know the repository it was published from, as
    /// one made by a developer, never updates itself, and says why.
    #[test]
    fn a_build_made_elsewhere_never_updates() {
        if super::REPOSITORY.is_none() {
            assert!(super::cant_update().is_some());
            assert_eq!(super::at_launch(), Ok(None));
        }
    }

    /// An update staged is put in place over the binary, and a step that
    /// fails leaves the binary that was there.
    #[test]
    fn a_staged_update_takes_the_binarys_place() {
        let dir = std::env::temp_dir().join(format!("suspense-self-update-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("suspense");
        std::fs::write(&exe, "old").unwrap();
        assert_eq!(apply(&exe).unwrap(), None, "nothing was staged");
        std::fs::write(staged(&exe), "new").unwrap();
        std::fs::write(staged_version(&exe), "0.1.7").unwrap();
        assert_eq!(apply(&exe).unwrap().as_deref(), Some("0.1.7"));
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!staged(&exe).exists());
        assert!(writable(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A download is checked against the size and digest GitHub gives, and
    /// its binary staged beside the running one.
    #[cfg(unix)]
    #[test]
    fn a_download_is_checked_and_staged() {
        let dir = std::env::temp_dir().join(format!("suspense-update-dl-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("pack")).unwrap();
        std::fs::write(dir.join("pack/suspense"), "the new binary").unwrap();
        let archive = dir.join("suspense-0.1.5-test.tar.gz");
        assert!(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(dir.join("pack"))
                .arg("suspense")
                .status()
                .unwrap()
                .success()
        );
        let bytes = std::fs::read(&archive).unwrap();
        let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        let exe = dir.join("bin/suspense");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(&exe, "old").unwrap();
        let mut release = Release {
            n: 5,
            page: String::new(),
            asset_url: format!("file://{}", archive.display()),
            size: bytes.len() as u64,
            sha256: Some("0000".into()),
        };
        let got = AtomicU64::new(0);
        let err = download(&release, &exe, &got).unwrap_err();
        assert!(format!("{err:#}").contains("SHA-256"), "{err:#}");
        assert!(!staged(&exe).exists());
        release.sha256 = Some(digest);
        download(&release, &exe, &got).unwrap();
        assert_eq!(std::fs::read_to_string(staged(&exe)).unwrap(), "the new binary");
        assert_eq!(got.load(Ordering::Relaxed), bytes.len() as u64);
        std::fs::remove_dir_all(&dir).ok();
    }
}
