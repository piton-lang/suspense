//! A command run with everything it starts, however deep, as one tree that
//! can be stopped whole, as the RunTargetsScope's stopping says: on macOS
//! and Linux as the head of a session and process group of its own, and on
//! Windows in a job object every process it starts belongs to. Stopping asks
//! the whole tree to end first, as Ctrl+C in a terminal would, so a server
//! can shut down cleanly, and kills whatever is still running 3 seconds
//! later.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// How long the tree is given to end once asked, before it is killed.
pub const GRACE: Duration = Duration::from_secs(3);

/// How often whether it has ended is looked at.
const POLL: Duration = Duration::from_millis(50);

/// Readies `command` to run as the head of a tree of its own.
pub fn prepare(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // A session of its own, and so a process group of its own, whose id
        // is its own; nothing else is done between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Still without a window, and a console process group of its own, so
        // Ctrl+Break reaches it and not Suspense.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(crate::process::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
}

/// The tree a child started with [`prepare`] heads.
pub struct Tree {
    pid: u32,
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl Tree {
    /// Takes `child` as the head of its tree: on Windows, puts it in a job
    /// every process it starts joins, killed whole should Suspense end.
    pub fn of(child: &Child) -> Self {
        Self {
            pid: child.id(),
            #[cfg(windows)]
            job: windows::Job::with(child),
        }
    }

    /// Stops the whole tree, `leader` its head, which is reaped: asks it to
    /// end, then kills whatever is still running after [`GRACE`]. Returns
    /// once every process of it has exited. Blocking.
    pub fn stop(&self, leader: &std::sync::Mutex<Child>) {
        #[cfg(unix)]
        unix::stop(self.pid, leader);
        #[cfg(windows)]
        windows::stop(self.pid, self.job.as_ref(), leader);
    }
}

/// Reaps `leader` if it has ended; whether it has.
fn reaped(leader: &std::sync::Mutex<Child>) -> bool {
    let mut child = leader.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    matches!(child.try_wait(), Ok(Some(_)))
}

/// Waits until `gone` says the tree has ended, reaping `leader` meanwhile,
/// up to `until`. Whether it ended.
fn wait_until(
    until: Instant,
    leader: &std::sync::Mutex<Child>,
    gone: &mut dyn FnMut() -> bool,
) -> bool {
    loop {
        reaped(leader);
        if gone() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(unix)]
mod unix {
    use std::collections::HashSet;
    use std::process::Stdio;
    use std::time::Instant;

    use super::{GRACE, wait_until};

    /// Every process descending from `root`, by process id, as `ps` lists
    /// them now: taken before anything is signalled, since a process whose
    /// parent ends is handed to init and no longer known as its descendant.
    pub(super) fn descendants(root: u32) -> Vec<u32> {
        let Ok(output) = crate::process::command("ps")
            .args(["-A", "-o", "pid=", "-o", "ppid="])
            .stdin(Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
            })
            .collect();
        let mut found: Vec<u32> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::from([root]);
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for &(pid, ppid) in &pairs {
                if ppid == parent && seen.insert(pid) {
                    found.push(pid);
                    frontier.push(pid);
                }
            }
        }
        found
    }

    /// Whether any process is in the group `pgid`.
    fn group_alive(pgid: u32) -> bool {
        signal(-(pgid as i32), 0)
    }

    fn alive(pid: u32) -> bool {
        signal(pid as i32, 0)
    }

    /// Sends `sig` to `target`, a process, or a group where negative;
    /// whether something received it.
    fn signal(target: i32, sig: i32) -> bool {
        unsafe { libc::kill(target, sig) == 0 }
    }

    /// The group of `pid`, where it is still running.
    fn group_of(pid: u32) -> Option<u32> {
        let pgid = unsafe { libc::getpgid(pid as i32) };
        (pgid > 0).then_some(pgid as u32)
    }

    pub(super) fn stop(pgid: u32, leader: &std::sync::Mutex<std::process::Child>) {
        // Those that left the group, as one that started a session of its
        // own, are stopped the same way, by their own ids.
        let strays: Vec<u32> = descendants(pgid)
            .into_iter()
            .filter(|pid| group_of(*pid) != Some(pgid))
            .collect();
        let ask = |sig: i32| {
            signal(-(pgid as i32), sig);
            for pid in &strays {
                signal(*pid as i32, sig);
            }
        };
        let mut gone = || !group_alive(pgid) && strays.iter().all(|pid| !alive(*pid));
        // As Ctrl+C in a terminal, then as a polite termination.
        ask(libc::SIGINT);
        if wait_until(Instant::now() + GRACE / 6, leader, &mut gone) {
            return;
        }
        ask(libc::SIGTERM);
        if wait_until(Instant::now() + GRACE * 5 / 6, leader, &mut gone) {
            return;
        }
        // Whatever is still running is killed outright.
        ask(libc::SIGKILL);
        wait_until(Instant::now() + GRACE, leader, &mut gone);
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
        SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };

    use super::{GRACE, wait_until};

    /// A job object every process of the tree belongs to, which ends them
    /// all should it be closed, as when Suspense ends.
    pub(super) struct Job(HANDLE);

    // The handle is only used through the job's own calls, which are safe
    // from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Job {
        /// A job holding `child`, killed whole when closed; none where one
        /// couldn't be made.
        pub(super) fn with(child: &Child) -> Option<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let job = Job(job);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) == 0 {
                    return None;
                }
                Some(job)
            }
        }

        /// How many of its processes are still running.
        fn active(&self) -> u32 {
            unsafe {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                let ok = QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                );
                if ok == 0 { 0 } else { info.ActiveProcesses }
            }
        }
    }

    /// Asks the console process group `pid` heads to end, as Ctrl+Break
    /// does, reaching it through its console without Suspense taking it.
    fn ctrl_break(pid: u32) {
        unsafe {
            FreeConsole();
            if AttachConsole(pid) != 0 {
                SetConsoleCtrlHandler(None, 1);
                GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
                FreeConsole();
                SetConsoleCtrlHandler(None, 0);
            }
        }
    }

    pub(super) fn stop(pid: u32, job: Option<&Job>, leader: &std::sync::Mutex<Child>) {
        let Some(job) = job else {
            // Without a job, the tree as Windows knows it now.
            crate::process::command("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .status()
                .ok();
            wait_until(Instant::now() + GRACE, leader, &mut || false);
            return;
        };
        let mut gone = || job.active() == 0;
        ctrl_break(pid);
        if wait_until(Instant::now() + GRACE, leader, &mut gone) {
            return;
        }
        // Whatever is still running is killed outright.
        unsafe { TerminateJobObject(job.0, 1) };
        wait_until(Instant::now() + GRACE, leader, &mut gone);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Stdio;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::{Tree, prepare, unix::descendants};

    /// Stopping a tree stops every process in it, however deep, one that
    /// ignores being asked included, and one that left the group: none is
    /// left running once it returns.
    #[test]
    fn stopping_a_tree_stops_all_of_it() {
        let mut command = crate::process::command("sh");
        // A child that ignores INT and TERM, a grandchild under it, and one
        // that starts a session of its own.
        command
            .args([
                "-c",
                "trap '' INT TERM; (trap '' INT TERM; sleep 60 & sleep 60) & setsid sleep 60 & wait",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        prepare(&mut command);
        let child = command.spawn().unwrap();
        let tree = Tree::of(&child);
        let pid = child.id();
        let leader = Mutex::new(child);
        // Let it start what it starts.
        let started = Instant::now();
        while descendants(pid).len() < 3 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let all = descendants(pid);
        assert!(all.len() >= 3, "it started {all:?}");
        let stopping = Instant::now();
        tree.stop(&leader);
        // Asked first, then killed once the grace runs out.
        assert!(stopping.elapsed() >= Duration::from_millis(400));
        for pid in all {
            let running = unsafe { libc::kill(pid as i32, 0) == 0 };
            assert!(!running, "{pid} was left running");
        }
        assert!(leader.lock().unwrap().try_wait().unwrap().is_some());
    }
}
