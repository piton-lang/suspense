//! Keeps a task off the location its mode leaves as it is: a Code task off
//! the spec, and a Spec task off the code, as the PromptModesScope says,
//! whatever the harness is told or does.
//!
//! A guarded run takes a snapshot of the location it may not change before
//! it starts, and, for as long as it runs, puts back whatever changes there:
//! a file changed or removed is written back as it was, and a file added is
//! removed. What the application itself saves there meanwhile, from the
//! editor, is taken as the location's new contents rather than put back.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

use crate::chat_input::SendMode;
use crate::project_tree::Locations;

/// How often a guarded location is checked while its run goes on.
const SWEEP_INTERVAL: Duration = Duration::from_millis(500);

/// Every snapshot a run is guarding, so the application's own saves reach it.
static GUARDING: Mutex<Vec<Weak<Mutex<Snapshot>>>> = Mutex::new(Vec::new());

/// The location a task sent in `mode` may not change, and what it is called:
/// the spec for Code, the code for Spec, and none for the rest.
pub fn protected(mode: SendMode, locations: &Locations) -> Option<(PathBuf, &'static str)> {
    match mode {
        SendMode::Code => locations.spec.clone().map(|spec| (spec, "spec")),
        SendMode::Spec => locations.code.clone().map(|code| (code, "code")),
        SendMode::Both | SendMode::Ask | SendMode::Freeform => None,
    }
}

/// A file as it was when the run began.
struct Kept {
    contents: Vec<u8>,
    /// Its modification time and length as last seen, so a file unchanged
    /// since isn't read again; `None` has it read on the next sweep.
    stamp: Option<(SystemTime, u64)>,
}

struct Snapshot {
    root: PathBuf,
    /// Folders inside the root that the run may change all the same: the
    /// location it works on, where that is inside this one, and the
    /// application's own data.
    exempt: Vec<PathBuf>,
    files: HashMap<PathBuf, Kept>,
    /// Every file put back, relative to the root.
    put_back: BTreeSet<PathBuf>,
}

impl Snapshot {
    fn take(root: PathBuf, exempt: Vec<PathBuf>) -> Self {
        let mut snapshot = Self {
            root,
            exempt,
            files: HashMap::new(),
            put_back: BTreeSet::new(),
        };
        for path in snapshot.listed() {
            if let Ok(contents) = std::fs::read(&path) {
                let stamp = stamp(&path);
                snapshot.files.insert(path, Kept { contents, stamp });
            }
        }
        snapshot
    }

    /// The files under the root as the ProjectLocationsScope lists them,
    /// leaving out those exempt.
    fn listed(&self) -> Vec<PathBuf> {
        if !self.root.is_dir() {
            return Vec::new();
        }
        let exempt = self.exempt.clone();
        ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .filter_entry(move |entry| {
                entry.file_name() != ".git" && !exempt.iter().any(|dir| entry.path() == dir)
            })
            .build()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| entry.into_path())
            .collect()
    }

    /// Puts back whatever has changed under the root since the snapshot.
    fn sweep(&mut self) {
        // Added: removed again.
        let added: Vec<PathBuf> = self
            .listed()
            .into_iter()
            .filter(|path| !self.files.contains_key(path))
            .collect();
        for path in added {
            if std::fs::remove_file(&path).is_ok() {
                self.note(&path);
                remove_empty_parents(&path, &self.root);
            }
        }
        let mut put_back = Vec::new();
        for (path, kept) in &mut self.files {
            let now = stamp(path);
            if now.is_some() && now == kept.stamp {
                continue;
            }
            let unchanged = now.is_some()
                && std::fs::read(path).is_ok_and(|contents| contents == kept.contents);
            if !unchanged {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                if std::fs::write(path, &kept.contents).is_err() {
                    continue;
                }
                put_back.push(path.clone());
            }
            kept.stamp = stamp(path);
        }
        for path in put_back {
            self.note(&path);
        }
    }

    fn note(&mut self, path: &Path) {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        self.put_back.insert(relative.to_path_buf());
    }

    fn covers(&self, path: &Path) -> bool {
        path.starts_with(&self.root) && !self.exempt.iter().any(|dir| path.starts_with(dir))
    }
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Removes the folders a removed file leaves empty, up to `root`.
fn remove_empty_parents(path: &Path, root: &Path) {
    let mut dir = path.parent();
    while let Some(parent) = dir.filter(|dir| *dir != root && dir.starts_with(root)) {
        if std::fs::remove_dir(parent).is_err() {
            break;
        }
        dir = parent.parent();
    }
}

/// A run's guard over the location its mode may not change. It stops
/// guarding once it is finished or dropped.
pub struct Guard {
    snapshot: Arc<Mutex<Snapshot>>,
    /// What the location is called, "spec" or "code".
    pub what: &'static str,
    stopped: Arc<AtomicBool>,
}

impl Guard {
    /// Guards the location a task sent in `mode` in `project_dir` may not
    /// change, or `None` where there is none. Reads the location, so it is
    /// best started off the main thread.
    pub fn start(mode: SendMode, project_dir: &Path) -> Option<Self> {
        let locations = Locations::read(project_dir);
        let (root, what) = protected(mode, &locations)?;
        // The application's own data, and the location the task works on,
        // are its to change even where they sit inside the guarded one.
        let mut exempt = vec![project_dir.join(".suspense")];
        exempt.extend(
            [locations.spec, locations.code]
                .into_iter()
                .flatten()
                .filter(|dir| *dir != root && dir.starts_with(&root)),
        );
        let snapshot = Arc::new(Mutex::new(Snapshot::take(root, exempt)));
        if let Ok(mut guarding) = GUARDING.lock() {
            guarding.retain(|snapshot| snapshot.strong_count() > 0);
            guarding.push(Arc::downgrade(&snapshot));
        }
        let stopped = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let snapshot = Arc::downgrade(&snapshot);
            let stopped = stopped.clone();
            move || {
                loop {
                    std::thread::sleep(SWEEP_INTERVAL);
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let Some(snapshot) = snapshot.upgrade() else {
                        break;
                    };
                    if let Ok(mut snapshot) = snapshot.lock() {
                        snapshot.sweep();
                    }
                }
            }
        });
        Some(Self {
            snapshot,
            what,
            stopped,
        })
    }

    /// The guarded location.
    pub fn root(&self) -> PathBuf {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.root.clone())
            .unwrap_or_default()
    }

    /// Puts back anything changed since the last sweep, stops guarding, and
    /// returns every file put back during the run, relative to the location.
    pub fn finish(self) -> Vec<PathBuf> {
        self.stopped.store(true, Ordering::SeqCst);
        let Ok(mut snapshot) = self.snapshot.lock() else {
            return Vec::new();
        };
        snapshot.sweep();
        snapshot.put_back.iter().cloned().collect()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// Tells every guard that the application is about to write `contents` to
/// `path` itself, so the write is kept rather than put back.
pub fn saving(path: &Path, contents: &[u8]) {
    let Ok(guarding) = GUARDING.lock() else {
        return;
    };
    for snapshot in guarding.iter().filter_map(Weak::upgrade) {
        let Ok(mut snapshot) = snapshot.lock() else {
            continue;
        };
        if snapshot.covers(path) {
            snapshot.files.insert(
                path.to_path_buf(),
                Kept {
                    contents: contents.to_vec(),
                    stamp: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("suspense-mode-guard-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("spec/ui")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n\nbelay-config B:\n    codeRoot: ./src\n",
        )
        .unwrap();
        std::fs::write(dir.join("spec/index.pi"), "spec\n").unwrap();
        std::fs::write(dir.join("spec/ui/a.pi"), "a\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn protects_the_other_location() {
        let locations = Locations {
            spec: Some("/p/spec".into()),
            code: Some("/p/src".into()),
        };
        assert_eq!(
            protected(SendMode::Code, &locations),
            Some(("/p/spec".into(), "spec"))
        );
        assert_eq!(
            protected(SendMode::Spec, &locations),
            Some(("/p/src".into(), "code"))
        );
        for mode in [SendMode::Both, SendMode::Ask, SendMode::Freeform] {
            assert_eq!(protected(mode, &locations), None);
        }
    }

    #[test]
    fn a_code_run_puts_the_spec_back() {
        let dir = project("code");
        let guard = Guard::start(SendMode::Code, &dir).unwrap();
        std::fs::write(dir.join("spec/index.pi"), "changed\n").unwrap();
        std::fs::remove_file(dir.join("spec/ui/a.pi")).unwrap();
        std::fs::create_dir_all(dir.join("spec/new")).unwrap();
        std::fs::write(dir.join("spec/new/b.pi"), "b\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() { 1; }\n").unwrap();
        let put_back = guard.finish();
        assert_eq!(
            std::fs::read_to_string(dir.join("spec/index.pi")).unwrap(),
            "spec\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("spec/ui/a.pi")).unwrap(),
            "a\n"
        );
        assert!(!dir.join("spec/new").exists());
        // The code is Code's to change.
        assert_eq!(
            std::fs::read_to_string(dir.join("src/main.rs")).unwrap(),
            "fn main() { 1; }\n"
        );
        assert_eq!(
            put_back,
            [
                PathBuf::from("index.pi"),
                PathBuf::from("new/b.pi"),
                PathBuf::from("ui/a.pi")
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_spec_run_puts_the_code_back_while_it_runs() {
        let dir = project("spec");
        let guard = Guard::start(SendMode::Spec, &dir).unwrap();
        std::fs::write(dir.join("src/main.rs"), "changed\n").unwrap();
        std::thread::sleep(SWEEP_INTERVAL * 3);
        assert_eq!(
            std::fs::read_to_string(dir.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        std::fs::write(dir.join("spec/index.pi"), "changed\n").unwrap();
        assert_eq!(guard.finish(), [PathBuf::from("main.rs")]);
        assert_eq!(
            std::fs::read_to_string(dir.join("spec/index.pi")).unwrap(),
            "changed\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_applications_own_saves_are_kept() {
        let dir = project("saved");
        let guard = Guard::start(SendMode::Code, &dir).unwrap();
        let file = dir.join("spec/index.pi");
        saving(&file, b"saved\n");
        std::fs::write(&file, "saved\n").unwrap();
        assert!(guard.finish().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
