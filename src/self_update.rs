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

actions!(suspense, [ShowUpdatesLog]);

/// The repository the running build was published from, `owner/name`, baked
/// in by the edge releases workflow; none for a build made anywhere else.
pub const REPOSITORY: Option<&str> = option_env!("SUSPENSE_REPOSITORY");

/// How long after starting the first check waits.
const FIRST_CHECK: Duration = Duration::from_secs(5);

/// How often it checks while it runs.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a request to GitHub may take.
const TIMEOUT_SECS: &str = "30";

/// The User-Agent every request to GitHub sends, naming Suspense and its
/// version, as GitHub refuses requests without one.
fn user_agent() -> String {
    format!("User-Agent: Suspense/{}", crate::version::VERSION)
}

/// The N of a release's tag, which must be `v0.1.N`; none for any other.
pub fn tag_sequence(tag: &str) -> Option<u64> {
    tag.strip_prefix('v').filter(|rest| rest.starts_with("0.1.")).and_then(sequence)
}

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
        return Some(
            "This build wasn't published as an edge release, so it doesn't update itself.",
        );
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
pub fn newest(
    releases: &Value,
    running: u64,
    asset: impl Fn(u64) -> Option<String>,
) -> Option<Release> {
    let mut best: Option<Release> = None;
    for release in releases.as_array()? {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(n) = release
            .get("tag_name")
            .and_then(Value::as_str)
            .and_then(tag_sequence)
        else {
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
                .and_then(|digest| {
                    let (kind, hex) = digest.split_once(':')?;
                    kind.eq_ignore_ascii_case("sha256").then_some(hex)
                })
                .map(str::to_lowercase),
        });
    }
    best
}

/// A check GitHub refused for its limit on requests made without signing
/// in, and when it resets, in seconds since the Unix epoch, where it said.
#[derive(Debug)]
pub struct RateLimited {
    pub reset: Option<u64>,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reset {
            Some(reset) => write!(
                f,
                "GitHub's rate limit was reached; try again after {}",
                utc_time(reset)
            ),
            None => write!(f, "GitHub's rate limit was reached; try again later"),
        }
    }
}

impl std::error::Error for RateLimited {}

/// Asks GitHub for the repository's releases, 100 to a page. Blocking.
fn fetch_releases(repository: &str) -> Result<Value> {
    let url = format!("https://api.github.com/repos/{repository}/releases?per_page=100");
    let headers_file = std::env::temp_dir().join(format!(
        "suspense-github-headers-{}",
        std::process::id()
    ));
    // Not `-f`, so a refusal's status and message can be told.
    let output = crate::process::command("curl")
        .args(["-sSL", "--max-time", TIMEOUT_SECS])
        .args(["-H", "Accept: application/vnd.github+json"])
        .arg("-H")
        .arg(user_agent())
        .arg("-D")
        .arg(&headers_file)
        .args(["-w", "\n%{http_code}"])
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .context("couldn't run curl to ask GitHub for releases")?;
    let headers = std::fs::read_to_string(&headers_file).unwrap_or_default();
    std::fs::remove_file(&headers_file).ok();
    if !output.status.success() {
        bail!(
            "couldn't reach GitHub: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = output.stdout;
    let split = stdout.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let (body, code) = stdout.split_at(split);
    let code: u16 = String::from_utf8_lossy(code).trim().parse().unwrap_or(0);
    read_answer(code, &headers, body, now())
}

/// GitHub's answer, `body` with the status `code` and `headers`, at `now`:
/// the releases, or why it refused, telling its rate limit from any other
/// refusal.
pub fn read_answer(code: u16, headers: &str, body: &[u8], now: u64) -> Result<Value> {
    if code == 200 {
        return serde_json::from_slice(body).context("GitHub's answer couldn't be read");
    }
    // The last of each header, as redirects each give their own.
    let header = |name: &str| {
        headers
            .lines()
            .rev()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
    };
    let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok());
    let retry_after = header("retry-after").and_then(|v| v.parse::<u64>().ok());
    if matches!(code, 403 | 429) && (remaining == Some(0) || retry_after.is_some()) {
        let reset = header("x-ratelimit-reset")
            .and_then(|v| v.parse::<u64>().ok())
            .or(retry_after.map(|after| now + after));
        return Err(RateLimited { reset }.into());
    }
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|answer| answer.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| {
            String::from_utf8_lossy(body)
                .trim()
                .chars()
                .take(200)
                .collect()
        });
    if code == 0 {
        bail!("GitHub gave no answer: {message}");
    }
    bail!("GitHub refused the check ({code}): {message}")
}

/// `seconds` since the Unix epoch as a UTC time, "2026-10-05 14:02 UTC".
pub fn utc_time(seconds: u64) -> String {
    let (date, time) = utc_parts(seconds);
    format!("{date} {} UTC", &time[..5])
}

/// `seconds` since the Unix epoch as a UTC date and time, "2026-10-05" and
/// "14:02:31".
fn utc_parts(seconds: u64) -> (String, String) {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let secs = seconds % 86_400;
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
    )
}

/// The binary running, as it will be relaunched.
pub fn running_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("couldn't find the running binary")?;
    Ok(dunce::canonicalize(&exe).unwrap_or(exe))
}

/// Where an update is staged beside `exe`, and its version kept.
pub fn staged(exe: &Path) -> PathBuf {
    sibling(exe, "update")
}

fn staged_version(exe: &Path) -> PathBuf {
    sibling(exe, "update-version")
}

/// The version an update put in place last, to be confirmed at the next
/// launch.
fn updated_marker(exe: &Path) -> PathBuf {
    sibling(exe, "updated")
}

/// A release whose update didn't take effect, never put in place again by
/// itself.
fn skipped(exe: &Path) -> PathBuf {
    sibling(exe, "skip")
}

/// The release put in place that didn't take effect, if one didn't.
pub fn skipped_version() -> Option<String> {
    let exe = running_binary().ok()?;
    let version = std::fs::read_to_string(skipped(&exe)).ok()?;
    let version = version.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// What is staged beside the running binary, by its version, if anything.
pub fn staged_now() -> Option<String> {
    let exe = running_binary().ok()?;
    staged(&exe).is_file().then(|| {
        std::fs::read_to_string(staged_version(&exe))
            .map(|version| version.trim().to_string())
            .unwrap_or_else(|_| "a version not recorded".into())
    })
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
            .args(["-fsSL", "-H"])
            .arg(user_agent())
            .arg("-o")
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
        match &release.sha256 {
            Some(expected) => {
                let actual: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                if !actual.eq_ignore_ascii_case(expected) {
                    bail!(
                        "the download's SHA-256 doesn't match GitHub's: {actual}, where GitHub gives {expected}"
                    );
                }
                log::write(format!("download {}: {} bytes, SHA-256 matches", release.version(), bytes.len()));
            }
            None => log::write(format!(
                "download {}: {} bytes; GitHub gives no digest to check",
                release.version(),
                bytes.len()
            )),
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
        let name = if cfg!(windows) {
            "suspense.exe"
        } else {
            "suspense"
        };
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
        log::write(format!(
            "staged {} at {}",
            release.version(),
            staged(exe).display()
        ));
        Ok(())
    })();
    std::fs::remove_file(&archive).ok();
    std::fs::remove_dir_all(&unpacked).ok();
    result
}

/// Puts an update staged beside `exe` in its place, setting the running
/// binary aside first where the platform won't replace it; a step that fails
/// puts back the binary that was there. One no newer than `running`, the N
/// of the version running, is deleted instead. The version put in place, if
/// one was, recorded to be confirmed at the next launch.
pub fn apply(exe: &Path, running: Option<u64>) -> Result<Option<String>> {
    let staged = staged(exe);
    if !staged.is_file() {
        return Ok(None);
    }
    let version = std::fs::read_to_string(staged_version(exe))
        .map(|version| version.trim().to_string())
        .unwrap_or_default();
    if let (Some(n), Some(running)) = (sequence(&version), running)
        && n <= running
    {
        std::fs::remove_file(&staged).ok();
        std::fs::remove_file(staged_version(exe)).ok();
        log::write(format!(
            "deleted the staged {version}: it is no newer than the {} running",
            crate::version::VERSION
        ));
        return Ok(None);
    }
    let result = (|| {
        if cfg!(windows) {
            let aside = aside(exe);
            std::fs::remove_file(&aside).ok();
            std::fs::rename(exe, &aside).with_context(|| {
                format!(
                    "couldn't set the running binary {} aside as {}",
                    exe.display(),
                    aside.display()
                )
            })?;
            if let Err(err) = std::fs::rename(&staged, exe) {
                // Never left without a binary.
                std::fs::rename(&aside, exe).ok();
                return Err(err).with_context(|| {
                    format!(
                        "couldn't put the update {} in place at {}",
                        staged.display(),
                        exe.display()
                    )
                });
            }
        } else {
            // A rename over the running binary replaces it in one step, or
            // not at all.
            std::fs::rename(&staged, exe).with_context(|| {
                format!(
                    "couldn't put the update {} in place at {}",
                    staged.display(),
                    exe.display()
                )
            })?;
        }
        anyhow::Ok(())
    })();
    match &result {
        Ok(()) => log::write(format!("put {version} in place at {}", exe.display())),
        Err(err) => log::write(format!("couldn't put {version} in place: {err:#}")),
    }
    result?;
    std::fs::remove_file(staged_version(exe)).ok();
    std::fs::write(updated_marker(exe), &version).ok();
    Ok(Some(version))
}

/// What happened at launch, before anything else.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AtLaunch {
    /// An update a quit that never finished left staged, put in place now:
    /// it takes effect at the launch after.
    pub finished: Option<String>,
    /// The update put in place last, now confirmed running.
    pub confirmed: Option<String>,
    /// Why updating failed, or the update that didn't take effect.
    pub failed: Option<String>,
}

/// At launch, before anything else: deletes a binary set aside by an
/// update, tried again at each launch while it can't be; confirms the
/// update put in place last is the version now running, never putting in
/// place by itself again one that isn't; and puts in place an update a quit
/// that never finished left staged.
pub fn at_launch() -> AtLaunch {
    let mut launch = AtLaunch::default();
    if cant_update().is_some() {
        return launch;
    }
    let exe = match running_binary() {
        Ok(exe) => exe,
        Err(err) => {
            launch.failed = Some(format!("{err:#}"));
            return launch;
        }
    };
    let set_aside = aside(&exe);
    if set_aside.exists()
        && let Err(err) = std::fs::remove_file(&set_aside)
    {
        log::write(format!(
            "couldn't delete {} yet, trying again next launch: {err}",
            set_aside.display()
        ));
    }
    let running = crate::version::VERSION;
    if let Ok(updated) = std::fs::read_to_string(updated_marker(&exe)) {
        let updated = updated.trim().to_string();
        std::fs::remove_file(updated_marker(&exe)).ok();
        if updated.is_empty() || updated == running {
            log::write(format!("launch: confirmed {running} is running"));
            std::fs::remove_file(skipped(&exe)).ok();
            launch.confirmed = Some(running.to_string());
        } else {
            log::write(format!(
                "launch: the update to {updated} didn't take effect; {running} is running"
            ));
            std::fs::write(skipped(&exe), &updated).ok();
            launch.failed = Some(format!(
                "The update to Suspense {updated} didn't take effect: Suspense {running} is still running. It won't be put in place again by itself; Check for updates tries it again."
            ));
        }
    }
    match apply(&exe, sequence(running)) {
        Ok(finished) => launch.finished = finished,
        Err(err) => launch.failed = Some(format!("Suspense couldn't update: {err:#}")),
    }
    launch
}

/// Where the updates stand, as the Updates section shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: String,
        got: u64,
        total: u64,
    },
    Ready {
        version: String,
    },
    /// The update put in place last is running, until the next check.
    Updated {
        version: String,
    },
    /// The running binary's directory can't be written: the release's page,
    /// to update by hand.
    NotWritable {
        dir: PathBuf,
        page: String,
    },
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
    /// Until when GitHub's rate limit holds, in seconds since the Unix epoch:
    /// no automatic check is made before.
    pub rate_limited_until: Option<u64>,
    /// The newest release the last check found, whether or not it was newer.
    pub newest_found: Option<String>,
    /// The Updates section's Details are shown.
    pub details_open: bool,
    /// The check under way was asked for from the ribbon's Check for
    /// Updates, which says how it went in a notification.
    notify_outcome: bool,
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
    pub fn init(at_launch: AtLaunch, cx: &mut App) {
        let checks = if cant_update().is_none() {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_CHECK).await;
                loop {
                    cx.update(|cx| {
                        // Not before GitHub's rate limit resets.
                        if Self::get(cx).is_some_and(|updates| {
                            updates.automatic
                                && updates.rate_limited_until.is_none_or(|until| now() >= until)
                        }) {
                            Self::check(false, cx);
                        }
                    });
                    cx.background_executor().timer(CHECK_EVERY).await;
                }
            })
        } else {
            Task::ready(())
        };
        let status = match (&at_launch.failed, &at_launch.confirmed) {
            (Some(why), _) => Status::Failed(why.clone()),
            (None, Some(version)) => {
                notify_updated(version, cx);
                Status::Updated {
                    version: version.clone(),
                }
            }
            (None, None) => Status::Idle,
        };
        let finished_at_launch = at_launch.finished;
        cx.set_global(Self {
            status,
            last_checked: None,
            automatic: preference::load(),
            finished_at_launch,
            rate_limited_until: None,
            newest_found: None,
            details_open: false,
            notify_outcome: false,
            got: Arc::default(),
            _checks: checks,
        });
        cx.on_app_quit(|_| {
            // However Suspense quits, a staged update is put in place.
            if cant_update().is_none()
                && let Ok(exe) = running_binary()
            {
                match apply(&exe, sequence(crate::version::VERSION)) {
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
        let Some(updates) = cx.try_global::<Self>() else {
            return;
        };
        if updates.status == status {
            return;
        }
        if updates.notify_outcome {
            let before = updates.status.clone();
            Self::notify_outcome(&before, &status, cx);
        }
        cx.update_global::<Self, _>(|updates, _| updates.status = status);
        cx.refresh_windows();
    }

    /// Says how a check asked for from the ribbon went, as `status` follows
    /// `before`; once it is over, nothing more.
    fn notify_outcome(before: &Status, status: &Status, cx: &mut App) {
        use gpui_kit::component::notification::Notification;
        let (note, over) = outcome(before, status);
        if over {
            cx.update_global::<Self, _>(|updates, _| updates.notify_outcome = false);
        }
        if let Some(note) = note {
            push_notification(
                match note {
                    Outcome::UpToDate(message) => {
                        Notification::success(message).title("Suspense is up to date")
                    }
                    Outcome::Downloading(message) => Notification::info(message),
                    Outcome::Failed(why) => {
                        Notification::error(why).title("Could not check for updates")
                    }
                },
                cx,
            );
        }
    }

    /// The ribbon's Check for Updates: checks straight away, saying how it
    /// went in a notification; with an update already staged, says it is
    /// ready again instead.
    pub fn check_from_ribbon(cx: &mut App) {
        let Some(updates) = Self::get(cx) else {
            return;
        };
        if cant_update().is_some() || Self::busy(cx) {
            return;
        }
        if let Status::Ready { version } = &updates.status {
            let version = version.clone();
            notify_ready(&version, cx);
            return;
        }
        cx.update_global::<Self, _>(|updates, _| updates.notify_outcome = true);
        Self::check(true, cx);
    }

    /// What the ribbon's Check for Updates says it would do, or is doing:
    /// whether it can be clicked, and its tooltip.
    pub fn ribbon_state(cx: &App) -> (bool, String) {
        if let Some(why) = cant_update() {
            return (false, why.to_string());
        }
        match Self::get(cx).map(|updates| &updates.status) {
            Some(Status::Checking) => (false, "Checking for updates…".into()),
            Some(Status::Downloading { version, .. }) => {
                (false, format!("Downloading Suspense {version}…"))
            }
            _ => (true, "Check for a newer version of Suspense".into()),
        }
    }

    /// Turns automatic checks on or off, remembering the choice.
    /// Shows or hides the Updates section's Details.
    pub fn toggle_details(cx: &mut App) {
        cx.update_global::<Self, _>(|updates, _| updates.details_open = !updates.details_open);
        cx.refresh_windows();
    }

    pub fn set_automatic(on: bool, cx: &mut App) {
        preference::save(on);
        cx.update_global::<Self, _>(|updates, _| updates.automatic = on);
        cx.refresh_windows();
    }

    /// Whether a check or download is under way.
    pub fn busy(cx: &App) -> bool {
        Self::get(cx).is_some_and(|updates| {
            matches!(
                updates.status,
                Status::Checking | Status::Downloading { .. }
            )
        })
    }

    /// Checks for an update now, and downloads one found; `asked` when the
    /// user asked, so a failure says why, and quiet otherwise.
    pub fn check(asked: bool, cx: &mut App) {
        let (Some(repository), Some(running)) = (REPOSITORY, sequence(crate::version::VERSION))
        else {
            return;
        };
        if Self::busy(cx) || cx.try_global::<Self>().is_none() {
            return;
        }
        let before = Self::get(cx)
            .map(|updates| updates.status.clone())
            .unwrap_or(Status::Idle);
        Self::set(Status::Checking, cx);
        log::write(format!(
            "check{}: {repository}, running {}",
            if asked { " (asked)" } else { "" },
            crate::version::VERSION
        ));
        let found = cx.background_spawn(async move {
            let releases = fetch_releases(repository)?;
            // The newest whatever the version running, to show in Details.
            let newest_any = newest(&releases, 0, asset_name).map(|release| release.version());
            let count = releases.as_array().map_or(0, Vec::len);
            anyhow::Ok((newest(&releases, running, asset_name), newest_any, count))
        });
        cx.spawn(async move |cx| {
            let found = found.await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|updates, _| updates.last_checked = Some(now()));
                let found = match found {
                    Ok((release, newest_any, count)) => {
                        log::write(match &release {
                            Some(release) => format!(
                                "check: {count} releases; taking {}",
                                release.version()
                            ),
                            None => format!(
                                "check: {count} releases; none newer than {} with this platform's asset (newest {})",
                                crate::version::VERSION,
                                newest_any.as_deref().unwrap_or("none")
                            ),
                        });
                        cx.update_global::<Self, _>(|updates, _| {
                            updates.newest_found = newest_any;
                            updates.rate_limited_until = None;
                        });
                        // One whose update didn't take effect is never put
                        // in place again by itself.
                        let skip = skipped_version();
                        match release {
                            Some(release)
                                if !asked && skip.as_deref() == Some(&release.version()) =>
                            {
                                log::write(format!(
                                    "check: not taking {} again by itself; its update didn't take effect",
                                    release.version()
                                ));
                                Self::set(
                                    match before.clone() {
                                        Status::Checking => Status::Idle,
                                        before => before,
                                    },
                                    cx,
                                );
                                return;
                            }
                            release => Ok(release),
                        }
                    }
                    Err(err) => {
                        log::write(format!("check failed: {err:#}"));
                        if let Some(limit) = err.downcast_ref::<RateLimited>() {
                            let until = limit.reset;
                            cx.update_global::<Self, _>(|updates, _| {
                                updates.rate_limited_until = until;
                            });
                        }
                        Err(err)
                    }
                };
                match found {
                    Ok(None) => {
                        // A staged update stays ready.
                        let status = match before {
                            Status::Ready { .. } => before,
                            _ => Status::UpToDate,
                        };
                        Self::set(status, cx)
                    }
                    Ok(Some(release)) => {
                        // Asked for by hand, a release that didn't take
                        // effect is tried again.
                        if let Ok(exe) = running_binary() {
                            std::fs::remove_file(skipped(&exe)).ok();
                        }
                        Self::download(release, cx)
                    }
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
        let got = Self::get(cx)
            .map(|updates| updates.got.clone())
            .unwrap_or_default();
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
                            Self::set(
                                Status::Downloading {
                                    version,
                                    got: now,
                                    total,
                                },
                                cx,
                            )
                        });
                    }
                }
            };
            cx.update(|cx| match result {
                Ok(()) => {
                    Self::set(
                        Status::Ready {
                            version: version.clone(),
                        },
                        cx,
                    );
                    notify_ready(&version, cx);
                }
                Err(err) => {
                    log::write(format!("download {version} failed: {err:#}"));
                    Self::set(Status::Failed(format!("{err:#}")), cx)
                }
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

/// What a check asked for from the ribbon says, as it goes.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    UpToDate(String),
    Downloading(String),
    Failed(String),
}

/// What a check asked for from the ribbon says as `status` follows
/// `before`, if anything, and whether the check is then over: up to date,
/// with the version running; downloading, once, as a download starts; or why
/// it couldn't. A download staged says so in its own notification.
fn outcome(before: &Status, status: &Status) -> (Option<Outcome>, bool) {
    match status {
        Status::Checking => (None, false),
        Status::Downloading { version, .. } => match before {
            Status::Downloading { .. } => (None, false),
            _ => (
                Some(Outcome::Downloading(format!(
                    "Downloading Suspense {version}…"
                ))),
                false,
            ),
        },
        Status::UpToDate => (
            Some(Outcome::UpToDate(format!(
                "Suspense {} is the newest version.",
                crate::version::VERSION
            ))),
            true,
        ),
        Status::Failed(why) => (Some(Outcome::Failed(why.clone())), true),
        Status::NotWritable { dir, .. } => (
            Some(Outcome::Failed(format!(
                "Suspense can't update itself in {}, as it can't write there. The release's page is linked in Settings, under Updates.",
                dir.display()
            ))),
            true,
        ),
        Status::Ready { .. } | Status::Updated { .. } | Status::Idle => (None, true),
    }
}

/// Shows `note` in the active window.
fn push_notification(note: gpui_kit::component::notification::Notification, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    cx.defer(move |cx| {
        let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
            return;
        };
        handle
            .update(cx, |_, window, cx| window.push_notification(note, cx))
            .ok();
    });
}

/// Says, in the window, that Suspense was updated to `version`.
fn notify_updated(version: &str, cx: &mut App) {
    use gpui_kit::component::notification::Notification;
    push_notification(
        Notification::success(format!("Updated to Suspense {version}")),
        cx,
    );
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

/// What updating rests on, as the Updates section's Details show it: each
/// as a label and its value.
pub fn details(cx: &App) -> Vec<(&'static str, String)> {
    let exe = running_binary().ok();
    let dir = exe.as_ref().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let updates = Updates::get(cx);
    vec![
        ("Version", crate::version::VERSION.to_string()),
        (
            "Repository",
            REPOSITORY
                .map(str::to_string)
                .unwrap_or_else(|| "none, so this isn't an edge build".into()),
        ),
        (
            "Binary",
            match (&exe, &dir) {
                (Some(exe), Some(dir)) => format!(
                    "{} ({})",
                    exe.display(),
                    if writable(dir) {
                        "its folder can be written"
                    } else {
                        "its folder can't be written"
                    }
                ),
                _ => "not found".into(),
            },
        ),
        (
            "Last check",
            match updates.and_then(|updates| updates.last_checked) {
                Some(then) => format!(
                    "{}, newest release {}",
                    utc_time(then),
                    updates
                        .and_then(|updates| updates.newest_found.clone())
                        .unwrap_or_else(|| "not found".into())
                ),
                None => "none yet".into(),
            },
        ),
        ("Staged", staged_now().unwrap_or_else(|| "nothing".into())),
    ]
}

/// The details as plain text, with the log's last 50 lines, for a bug report.
pub fn details_text(cx: &App) -> String {
    let mut text = String::from("Suspense updates\n");
    for (label, value) in details(cx) {
        text.push_str(&format!("{label}: {value}\n"));
    }
    text.push_str("\nupdates.log, last 50 lines:\n");
    for line in log::tail(50) {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// What updating does, one line per step with its time, in `updates.log` in
/// Suspense's own data directory, beside its preferences, keeping the last
/// 1,000 lines, as the SelfUpdateScope's diagnostics say. Tests write none.
pub mod log {
    use std::path::PathBuf;

    /// How many lines it keeps.
    const KEEP: usize = 1000;

    /// The log's file.
    pub fn file() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(dirs::config_dir()?.join("suspense").join("updates.log"))
    }

    /// Adds `line`, with the time, keeping the last lines.
    pub fn write(line: impl AsRef<str>) {
        let Some(file) = file() else {
            return;
        };
        let (date, time) = super::utc_parts(super::now());
        let mut lines: Vec<String> = std::fs::read_to_string(&file)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default();
        lines.push(format!("{date} {time} {}", line.as_ref()));
        let from = lines.len().saturating_sub(KEEP);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&file, lines[from..].join("\n") + "\n").ok();
    }

    /// Its last `n` lines.
    pub fn tail(n: usize) -> Vec<String> {
        let Some(file) = file() else {
            return Vec::new();
        };
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(n)..]
            .iter()
            .map(|line| line.to_string())
            .collect()
    }
}

/// Whether to check for updates automatically, remembered for the user, as
/// the UserPreferencesScope says; on until turned off. Tests neither read
/// nor write it.
pub mod preference {
    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(
            dirs::config_dir()?
                .join("suspense")
                .join("check-for-updates"),
        )
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

    use super::{
        Outcome, Status, apply, newest, outcome, sequence, staged, staged_version, writable,
    };
    #[cfg(unix)]
    use super::{Release, download};

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
            assert_eq!(super::at_launch(), super::AtLaunch::default());
        }
    }

    /// A check asked for from the ribbon says it is up to date, that it is
    /// downloading, once, or why it couldn't, and is over once it has said
    /// so or staged the update.
    #[test]
    fn the_ribbons_check_says_how_it_went() {
        let downloading = |got| Status::Downloading {
            version: "0.1.9".into(),
            got,
            total: 10,
        };
        assert_eq!(outcome(&Status::Idle, &Status::Checking), (None, false));
        let (note, over) = outcome(&Status::Checking, &Status::UpToDate);
        assert!(
            matches!(note, Some(Outcome::UpToDate(message)) if message.contains(crate::version::VERSION))
        );
        assert!(over);
        assert_eq!(
            outcome(&Status::Checking, &downloading(0)),
            (
                Some(Outcome::Downloading("Downloading Suspense 0.1.9…".into())),
                false
            )
        );
        // Once, not as each piece comes.
        assert_eq!(outcome(&downloading(0), &downloading(5)), (None, false));
        assert_eq!(
            outcome(
                &downloading(5),
                &Status::Ready {
                    version: "0.1.9".into()
                }
            ),
            (None, true)
        );
        assert_eq!(
            outcome(&Status::Checking, &Status::Failed("offline".into())),
            (Some(Outcome::Failed("offline".into())), true)
        );
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
        assert_eq!(apply(&exe, Some(5)).unwrap(), None, "nothing was staged");
        std::fs::write(staged(&exe), "new").unwrap();
        std::fs::write(staged_version(&exe), "0.1.7").unwrap();
        assert_eq!(apply(&exe, Some(5)).unwrap().as_deref(), Some("0.1.7"));
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!staged(&exe).exists());
        // Recorded, to be confirmed at the next launch.
        assert_eq!(
            std::fs::read_to_string(super::updated_marker(&exe)).unwrap(),
            "0.1.7"
        );
        assert!(writable(&dir));

        // One no newer than the version running is deleted, never put in
        // place.
        std::fs::write(staged(&exe), "older").unwrap();
        std::fs::write(staged_version(&exe), "0.1.7").unwrap();
        assert_eq!(apply(&exe, Some(7)).unwrap(), None);
        assert!(!staged(&exe).exists() && !staged_version(&exe).exists());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only tags `v0.1.N` count as releases.
    #[test]
    fn only_v_0_1_n_tags_count() {
        use super::tag_sequence;
        assert_eq!(tag_sequence("v0.1.42"), Some(42));
        assert_eq!(tag_sequence("0.1.42"), None);
        assert_eq!(tag_sequence("v0.2.1"), None);
        assert_eq!(tag_sequence("nightly"), None);
    }

    /// GitHub's rate limit is told from any other refusal, and says when it
    /// resets; any other refusal says GitHub's status and message.
    #[test]
    fn refusals_say_why() {
        use super::{RateLimited, read_answer};
        let ok = read_answer(200, "", b"[]", 0).unwrap();
        assert_eq!(ok, json!([]));
        let limited = read_answer(
            403,
            "HTTP/2 403\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1791200000\r\n",
            br#"{"message":"API rate limit exceeded"}"#,
            0,
        )
        .unwrap_err();
        let limit = limited.downcast_ref::<RateLimited>().expect("not a rate limit");
        assert_eq!(limit.reset, Some(1_791_200_000));
        assert!(limited.to_string().starts_with("GitHub's rate limit was reached; try again after 2026-"));
        let retry = read_answer(429, "retry-after: 60\n", b"", 100).unwrap_err();
        assert_eq!(retry.downcast_ref::<RateLimited>().unwrap().reset, Some(160));
        let missing = read_answer(404, "", br#"{"message":"Not Found"}"#, 0).unwrap_err();
        assert!(missing.downcast_ref::<RateLimited>().is_none());
        assert_eq!(missing.to_string(), "GitHub refused the check (404): Not Found");
    }

    #[test]
    fn times_read_as_utc() {
        assert_eq!(super::utc_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::utc_time(1_791_200_000), "2026-10-05 11:33 UTC");
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
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
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
        assert_eq!(
            std::fs::read_to_string(staged(&exe)).unwrap(),
            "the new binary"
        );
        assert_eq!(got.load(Ordering::Relaxed), bytes.len() as u64);
        std::fs::remove_dir_all(&dir).ok();
    }
}
