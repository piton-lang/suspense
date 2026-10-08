//! The debug log, as the DebugLogScope says: `suspense.log` in Suspense's own
//! data directory, beside `updates.log`, one line per event with the time to
//! the millisecond, the project it concerns, and what happened, keeping the
//! last 20,000 lines across sessions. It never holds environment variables,
//! credentials, or prompt text. Tests write none, unless one sends the log
//! somewhere of its own.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many lines it keeps.
const KEEP: usize = 20_000;

/// How many lines past [`KEEP`] it may grow to in a session before the
/// oldest are dropped again.
const SLACK: usize = 2_000;

/// How long the window may go unanswering before that is logged.
pub const STALL: Duration = Duration::from_millis(200);

/// Lines written since the log was last trimmed.
struct State {
    written: usize,
}

static STATE: Mutex<State> = Mutex::new(State { written: 0 });

#[cfg(test)]
thread_local! {
    /// Where a test on this thread sends the log, if anywhere.
    static TEST_FILE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Suspense's own data directory, where `updates.log` is kept.
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("suspense"))
}

/// The log's file; none in tests that haven't sent it anywhere.
pub fn file() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_FILE.with_borrow(Clone::clone)
    }
    #[cfg(not(test))]
    {
        Some(data_dir()?.join("suspense.log"))
    }
}

/// Sends this thread's log to `file` from now on, or nowhere.
#[cfg(test)]
pub fn use_file_for_test(file: Option<PathBuf>) {
    TEST_FILE.set(file);
}

/// Starts the session's log: drops the oldest lines past [`KEEP`], and
/// logs every panic, with its backtrace, before the application exits.
pub fn init() {
    trim();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log(None, format!("panic: {info}\n{backtrace}"));
        previous(info);
    }));
    log(
        None,
        format!(
            "started Suspense {} on {} {}",
            crate::version::describe(),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
}

/// Logs `what` happened, about `project`, or the application itself.
/// A line that runs on, as a backtrace or an error output, is set in beneath
/// the line it belongs to.
pub fn log(project: Option<&Path>, what: impl AsRef<str>) {
    let Some(file) = file() else {
        return;
    };
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let project = project.map_or_else(|| "-".to_string(), |dir| dir.display().to_string());
    let what = what.as_ref().trim_end().replace('\n', "\n    ");
    let line = format!("{time} [{project}] {what}\n");
    let mut state = state();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Ok(mut out) = OpenOptions::new().create(true).append(true).open(&file) {
        out.write_all(line.as_bytes()).ok();
    }
    state.written += line.matches('\n').count();
    if state.written > SLACK {
        state.written = 0;
        drop(state);
        trim();
    }
}

/// Keeps only the log's last [`KEEP`] lines.
fn trim() {
    let Some(file) = file() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > KEEP {
        let kept = lines[lines.len() - KEEP..].join("\n") + "\n";
        std::fs::write(&file, kept).ok();
    }
}

/// How long `took` took, as the log says it: "312ms", or "4.2s".
pub fn took(took: Duration) -> String {
    if took < Duration::from_secs(1) {
        format!("{}ms", took.as_millis())
    } else {
        format!("{:.1}s", took.as_secs_f64())
    }
}

/// Something under way, logged with how long it took once [`Timed::end`]
/// says how it ended.
pub struct Timed {
    project: Option<PathBuf>,
    what: String,
    started: Instant,
}

impl Timed {
    /// `what` has begun, for `project`.
    pub fn begin(project: Option<&Path>, what: impl Into<String>) -> Self {
        Self {
            project: project.map(Path::to_path_buf),
            what: what.into(),
            started: Instant::now(),
        }
    }

    /// It ended, `how`, logged with how long it took.
    pub fn end(self, how: impl AsRef<str>) -> Duration {
        let took = self.started.elapsed();
        let how = how.as_ref();
        let how = if how.is_empty() {
            String::new()
        } else {
            format!(", {how}")
        };
        log(
            self.project.as_deref(),
            format!("{} took {}{how}", self.what, self::took(took)),
        );
        took
    }
}

/// A task's or question's phases, as the DebugLogScope's phases say: each
/// logged as it ends, with how long it took, and kept with when it began and
/// ended for the task's history record.
pub struct Phases {
    project: PathBuf,
    /// The task or question, by its hidden anchor's name.
    task: String,
    /// Those under way, by a key of their own: a tool call's id, or else the
    /// phase's name.
    open: Vec<(String, String, std::time::SystemTime, Instant, usize)>,
    /// Those ended, with the order they began in.
    done: Vec<(usize, crate::prompt_history::Phase)>,
    /// How many have begun.
    begun: usize,
}

fn millis(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

impl Phases {
    pub fn new(project: &Path, task: impl Into<String>) -> Self {
        Self {
            project: project.to_path_buf(),
            task: task.into(),
            open: Vec::new(),
            done: Vec::new(),
            begun: 0,
        }
    }

    /// The task is known by `name` from now on.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.task = name.into();
    }

    /// `name` has begun, known by `key` until it ends.
    pub fn begin_keyed(&mut self, key: impl Into<String>, name: impl Into<String>) {
        self.open.push((
            key.into(),
            name.into(),
            std::time::SystemTime::now(),
            Instant::now(),
            self.begun,
        ));
        self.begun += 1;
    }

    /// `name` has begun.
    pub fn begin(&mut self, name: &str) {
        self.begin_keyed(name, name);
    }

    /// What is known by `key` ended, `how`; logged with how long it took.
    /// Nothing, when it never began.
    pub fn end(&mut self, key: &str, how: &str) {
        let Some(at) = self.open.iter().position(|(open, ..)| open == key) else {
            return;
        };
        let (_, name, began, started, order) = self.open.remove(at);
        self.finish(order, name, began, started.elapsed(), how);
    }

    /// `name` took from `began` until now.
    pub fn since(&mut self, name: &str, began: std::time::SystemTime, how: &str) {
        let lasted = std::time::SystemTime::now()
            .duration_since(began)
            .unwrap_or_default();
        self.begun += 1;
        self.finish(self.begun - 1, name.to_string(), began, lasted, how);
    }

    fn finish(
        &mut self,
        order: usize,
        name: String,
        began: std::time::SystemTime,
        lasted: Duration,
        how: &str,
    ) {
        let how = if how.is_empty() {
            String::new()
        } else {
            format!(", {how}")
        };
        log(
            Some(&self.project),
            format!("{}: {name} took {}{how}", self.task, took(lasted)),
        );
        self.done.push((
            order,
            crate::prompt_history::Phase {
                name,
                began_at: millis(began),
                ended_at: millis(std::time::SystemTime::now()),
            },
        ));
    }

    /// Whether `key` is under way.
    pub fn is_open(&self, key: &str) -> bool {
        self.open.iter().any(|(open, ..)| open == key)
    }

    /// Ends whatever is still under way, as `how`, and gives every phase,
    /// in the order they began.
    pub fn close(mut self, how: &str) -> Vec<crate::prompt_history::Phase> {
        while let Some((key, ..)) = self.open.last().cloned() {
            self.end(&key, how);
        }
        let mut done = self.done;
        done.sort_by_key(|(order, _)| *order);
        done.into_iter().map(|(_, phase)| phase).collect()
    }

    /// Follows a harness event: its first event, the container's
    /// preparing, and each tool call by its kind and the path or command it
    /// acted on, never what the harness replied.
    pub fn follow(&mut self, event: &crate::harness::HarnessEvent) {
        use crate::harness::HarnessEvent;
        if self.is_open("first event") {
            self.end("first event", "");
        }
        match event {
            HarnessEvent::Preparing => self.begin("preparing the container"),
            HarnessEvent::Prepared => self.end("preparing the container", ""),
            HarnessEvent::ToolCalled { id, name, input, .. } => {
                let on = ["file_path", "path", "notebook_path", "command", "pattern", "url"]
                    .iter()
                    .find_map(|key| input.get(*key)?.as_str())
                    .map(|on| {
                        let line = on.lines().next().unwrap_or_default();
                        let short: String = line.chars().take(160).collect();
                        format!(" {short}")
                    })
                    .unwrap_or_default();
                if !self.is_open(id) {
                    self.begin_keyed(id.clone(), format!("tool {name}{on}"));
                }
            }
            HarnessEvent::ToolFinished { id, is_error } => {
                self.end(id, if *is_error { "failed" } else { "done" })
            }
            _ => {}
        }
    }
}

/// How many of the open project's last tasks and questions a collected
/// folder holds the records of.
const COLLECTED_TASKS: usize = 10;

/// Writes a folder, named for `at`, in `data_dir`, holding what a problem
/// can be looked into with, as the DebugLogScope's collecting says: the
/// debug log, `updates.log`, the version and platform, and of `project`,
/// when one is open, its harness report and its last ten tasks' and
/// questions' records, each beside its source, with a README saying what
/// each is. Gives the folder.
pub fn collect(
    data_dir: &Path,
    log_file: Option<&Path>,
    updates_log: Option<&Path>,
    project: Option<&Path>,
    at: chrono::DateTime<chrono::Local>,
) -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;
    let folder = data_dir.join(at.format("debug-%Y-%m-%d-%H-%M-%S").to_string());
    std::fs::create_dir_all(&folder)
        .with_context(|| format!("could not create {}", folder.display()))?;
    let copy = |from: Option<&Path>, name: &str| {
        from.filter(|from| from.exists())
            .is_some_and(|from| std::fs::copy(from, folder.join(name)).is_ok())
    };
    let mut readme = String::from(
        "# Suspense debug info\n\nCollected to look into a problem with Suspense. \
         Nothing here holds environment variables, credentials, or the text of \
         prompts beyond the history records of the tasks below.\n\n",
    );
    if copy(log_file, "suspense.log") {
        readme.push_str(
            "- `suspense.log`: the debug log, one line per event: the time, the project \
             it concerns, and what happened.\n",
        );
    }
    if copy(updates_log, "updates.log") {
        readme.push_str("- `updates.log`: what self-updating did, one line per step.\n");
    }
    let about = format!(
        "Suspense {}\nPlatform: {} {}\nBinary: {}\n",
        crate::version::describe(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::current_exe()
            .map(|exe| exe.display().to_string())
            .unwrap_or_default(),
    );
    std::fs::write(folder.join("about.txt"), about)?;
    readme.push_str("- `about.txt`: the application's version and platform.\n");
    match project {
        Some(project) => {
            let app_dir = project.join(crate::hidden_anchor::APP_DIR);
            if copy(Some(&app_dir.join("harness.json")), "harness.json") {
                readme.push_str(
                    "- `harness.json`: the open project's harness report, what its \
                     harness last said it has.\n",
                );
            }
            let copied = copy_last_tasks(project, &folder.join("history"))?;
            readme.push_str(&format!(
                "- `history/`: the history records of the open project, {}, last {copied} \
                 tasks and questions: each `.pi` is the prompt as sent, and the `.json` \
                 beside it what came of it, the phases' times included.\n",
                project.display()
            ));
        }
        None => readme.push_str(
            "\nNo project was open, so nothing of a project is here: no harness \
             report and no task history.\n",
        ),
    }
    std::fs::write(folder.join("README.md"), readme)?;
    log(project, format!("collected debug info in {}", folder.display()));
    Ok(folder)
}

/// Copies the last [`COLLECTED_TASKS`] tasks and questions of `project`,
/// each source beside its record, into `to`. Gives how many.
fn copy_last_tasks(project: &Path, to: &Path) -> anyhow::Result<usize> {
    let sent_at = |file: &Path| -> u64 {
        file.file_name()
            .and_then(|name| name.to_str()?.split_once('-')?.0.parse().ok())
            .unwrap_or(0)
    };
    let mut sources: Vec<PathBuf> = [
        crate::hidden_anchor::history_dir(project),
        crate::hidden_anchor::asks_dir(project),
    ]
    .iter()
    .filter_map(|dir| std::fs::read_dir(dir).ok())
    .flatten()
    .filter_map(|entry| Some(entry.ok()?.path()))
    .filter(|path| path.extension().is_some_and(|ext| ext == "pi"))
    .collect();
    sources.sort_by_key(|file| (sent_at(file), file.clone()));
    let last = &sources[sources.len().saturating_sub(COLLECTED_TASKS)..];
    std::fs::create_dir_all(to)?;
    for source in last {
        for file in [source.clone(), source.with_extension("json")] {
            if let Some(name) = file.file_name().filter(|_| file.exists()) {
                std::fs::copy(&file, to.join(name))?;
            }
        }
    }
    Ok(last.len())
}

/// Watches for the window going unanswering: wakes every so often on the
/// UI thread, and logs any wake that came more than [`STALL`] late.
pub fn watch_stalls(cx: &mut gpui_kit::App) {
    const EVERY: Duration = Duration::from_millis(100);
    cx.spawn(async move |cx| {
        loop {
            let asked = Instant::now();
            cx.background_executor().timer(EVERY).await;
            // Woken here only once the UI thread is free again.
            let late = asked.elapsed().saturating_sub(EVERY);
            if late > STALL {
                log(None, format!("window unresponsive for {}", took(late)));
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line has the time to the millisecond, the project or none, and
    /// what happened; what runs on is set in beneath it.
    #[test]
    fn lines_say_when_where_and_what() {
        let dir = std::env::temp_dir().join(format!("suspense-debug-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("suspense.log");
        std::fs::remove_file(&file).ok();
        use_file_for_test(Some(file.clone()));
        log(None, "started");
        log(Some(Path::new("/p/proj")), "git status took 12ms\nmore");
        use_file_for_test(None);
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        let stamp = |line: &str| {
            let (date, rest) = line.split_once(' ').unwrap();
            let time = rest.split(' ').next().unwrap();
            assert_eq!(date.len(), 10, "{line}");
            assert_eq!(time.len(), 12, "{line}");
        };
        stamp(lines[0]);
        assert!(lines[0].ends_with(" [-] started"), "{}", lines[0]);
        assert!(lines[1].ends_with(" [/p/proj] git status took 12ms"), "{}", lines[1]);
        assert_eq!(lines[2], "    more");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A collected folder holds the logs, the version, and the open
    /// project's harness report and last ten tasks and questions, each
    /// beside its source, with a README; with no project, only what needs
    /// none, and its README says so.
    #[test]
    fn collecting_writes_a_folder_named_for_the_time() {
        use chrono::TimeZone as _;
        let root = std::env::temp_dir().join(format!("suspense-collect-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let (data, project) = (root.join("data"), root.join("project"));
        let history = crate::hidden_anchor::history_dir(&project);
        let asks = crate::hidden_anchor::asks_dir(&project);
        std::fs::create_dir_all(&history).unwrap();
        std::fs::create_dir_all(&asks).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        for n in 1..=12u64 {
            std::fs::write(history.join(format!("{n}-Task{n}.pi")), "task").unwrap();
            std::fs::write(history.join(format!("{n}-Task{n}.json")), "{}").unwrap();
        }
        std::fs::write(asks.join("100-Question.pi"), "ask").unwrap();
        std::fs::write(project.join(".suspense/harness.json"), "{}").unwrap();
        let log_file = data.join("suspense.log");
        std::fs::write(&log_file, "a line\n").unwrap();
        let at = chrono::Local.with_ymd_and_hms(2026, 10, 8, 14, 3, 22).unwrap();

        let folder = collect(&data, Some(&log_file), None, Some(&project), at).unwrap();
        assert_eq!(folder, data.join("debug-2026-10-08-14-03-22"));
        for file in ["suspense.log", "about.txt", "harness.json", "README.md"] {
            assert!(folder.join(file).exists(), "no {file}");
        }
        assert!(!folder.join("updates.log").exists());
        let mut copied: Vec<String> = std::fs::read_dir(folder.join("history"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        copied.sort();
        // The last ten: tasks 4 to 12, each with its record, and the question.
        assert_eq!(copied.len(), 19, "{copied:?}");
        assert!(copied.contains(&"100-Question.pi".to_string()));
        assert!(copied.contains(&"4-Task4.json".to_string()));
        assert!(!copied.contains(&"3-Task3.pi".to_string()));

        let at = chrono::Local.with_ymd_and_hms(2026, 10, 8, 14, 3, 23).unwrap();
        let alone = collect(&data, Some(&log_file), None, None, at).unwrap();
        assert!(!alone.join("history").exists() && !alone.join("harness.json").exists());
        let readme = std::fs::read_to_string(alone.join("README.md")).unwrap();
        assert!(readme.contains("No project was open"), "{readme}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Each phase is logged as it ends and kept with its times, a tool call
    /// by its kind and what it acted on; a wait from before this boot is
    /// told from the clock.
    #[test]
    fn phases_are_logged_and_kept() {
        use crate::harness::HarnessEvent;
        let mut phases = Phases::new(Path::new("/p"), "Task_1");
        phases.since("queued", std::time::UNIX_EPOCH + Duration::from_secs(1), "");
        phases.begin("harness");
        phases.begin("first event");
        phases.follow(&HarnessEvent::ToolCalled {
            id: "t1".into(),
            name: "Read".into(),
            input: serde_json::json!({ "file_path": "/p/src/a.rs" }),
            subagent: false,
        });
        assert!(!phases.is_open("first event"));
        phases.follow(&HarnessEvent::ToolFinished { id: "t1".into(), is_error: false });
        let kept = phases.close("ended");
        let names: Vec<&str> = kept.iter().map(|phase| phase.name.as_str()).collect();
        assert_eq!(names, ["queued", "harness", "first event", "tool Read /p/src/a.rs"]);
        assert!(kept.iter().all(|phase| phase.began_at <= phase.ended_at));
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(took(Duration::from_millis(312)), "312ms");
        assert_eq!(took(Duration::from_millis(4210)), "4.2s");
    }
}
