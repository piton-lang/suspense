//! Watches the folders open files live in, so an editor learns when its file
//! is edited or renamed by anything else. One watcher serves every open file:
//! each file's project is watched folder by folder, skipping what git
//! ignores, so a file moved to another folder of the project is seen arriving
//! there, and new folders are watched as they appear.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use notify::event::{CreateKind, ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// What the watcher thread is asked to do.
enum Command {
    /// Watch the folder, and every folder beneath it git doesn't ignore.
    Tree(PathBuf),
    /// Watch just the folder.
    Folder(PathBuf),
}

struct DiskWatch {
    commands: Sender<Command>,
    subscribers: Arc<Mutex<Vec<Sender<Event>>>>,
}

fn watch() -> &'static DiskWatch {
    static WATCH: OnceLock<DiskWatch> = OnceLock::new();
    WATCH.get_or_init(|| {
        let (commands, received) = mpsc::channel();
        let subscribers: Arc<Mutex<Vec<Sender<Event>>>> = Arc::default();
        let forward = subscribers.clone();
        let new_folders = commands.clone();
        std::thread::spawn(move || {
            // Reports come on the watcher's own thread; they are only passed
            // on from there, never waited on.
            let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                let Ok(event) = event else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                // A folder made, or moved in, is watched too.
                let arrived = match event.kind {
                    EventKind::Create(CreateKind::Folder | CreateKind::Any) => event.paths.first(),
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => event.paths.first(),
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => event.paths.get(1),
                    _ => None,
                };
                if let Some(path) = arrived.filter(|path| path.is_dir()) {
                    new_folders.send(Command::Tree(path.clone())).ok();
                }
                forward
                    .lock()
                    .unwrap()
                    .retain(|subscriber| subscriber.send(event.clone()).is_ok());
            });
            let Ok(mut watcher) = watcher else { return };
            run(&mut watcher, received);
        });
        DiskWatch {
            commands,
            subscribers,
        }
    })
}

/// Watches what it is asked to, each folder once.
fn run(watcher: &mut RecommendedWatcher, commands: Receiver<Command>) {
    let mut watched = HashSet::new();
    let mut add = |dir: PathBuf, watcher: &mut RecommendedWatcher| {
        if !watched.contains(&dir) && watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
            watched.insert(dir);
        }
    };
    for command in commands {
        match command {
            Command::Folder(dir) => add(dir, watcher),
            Command::Tree(root) => {
                let folders = ignore::WalkBuilder::new(&root)
                    .hidden(false)
                    .filter_entry(|entry| entry.file_name() != ".git")
                    .build()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_dir()));
                for folder in folders {
                    add(folder.into_path(), watcher);
                }
            }
        }
    }
}

/// What happens on disk to the folders around `path`, a file open in
/// `project`, or just its own folder outside of one, from now on.
pub fn subscribe(path: &Path, project: Option<&Path>) -> Receiver<Event> {
    let watch = watch();
    let (tx, rx) = mpsc::channel();
    watch.subscribers.lock().unwrap().push(tx);
    match project.filter(|project| path.starts_with(project)) {
        Some(project) => {
            watch
                .commands
                .send(Command::Tree(project.to_path_buf()))
                .ok();
        }
        None => {
            if let Some(dir) = path.parent() {
                watch.commands.send(Command::Folder(dir.to_path_buf())).ok();
            }
        }
    }
    rx
}

/// What the reports say happened to the file at `path`.
#[derive(Debug, Default, PartialEq)]
pub struct FileChanges {
    /// Where it, or a folder holding it, was renamed to.
    pub renamed: Option<PathBuf>,
    /// Whether it may have been written, or replaced by another file.
    pub written: bool,
    /// Whether it went from its path, without saying where.
    pub vanished: bool,
    /// Files that appeared, where a file that vanished may have gone.
    pub appeared: Vec<PathBuf>,
}

/// Sorts what `events` say happened to the file at `path`.
pub fn changes(path: &Path, events: &[Event]) -> FileChanges {
    let mut changes = FileChanges::default();
    for event in events {
        match event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
                let (from, to) = (&event.paths[0], &event.paths[1]);
                if let Ok(within) = path.strip_prefix(from) {
                    changes.renamed = Some(if within.as_os_str().is_empty() {
                        to.clone()
                    } else {
                        to.join(within)
                    });
                } else if to == path {
                    // Another file moved over it, as editors save.
                    changes.written = true;
                } else {
                    changes.appeared.push(to.clone());
                }
            }
            EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::Any)) => {
                if event.paths.iter().any(|gone| path.starts_with(gone)) {
                    changes.vanished = true;
                }
            }
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                for created in &event.paths {
                    if created == path {
                        changes.written = true;
                    } else {
                        changes.appeared.push(created.clone());
                    }
                }
            }
            EventKind::Modify(_) | EventKind::Any | EventKind::Other => {
                if event.paths.iter().any(|changed| changed == path) {
                    changes.written = true;
                }
            }
            EventKind::Access(_) => {}
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
    use notify::{Event, EventKind};

    use super::{FileChanges, changes};

    fn event(kind: EventKind, paths: &[&str]) -> Event {
        let mut event = Event::new(kind);
        event.paths = paths.iter().map(PathBuf::from).collect();
        event
    }

    #[test]
    fn sorts_what_happened_to_a_file() {
        let path = Path::new("/p/src/a.rs");
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));

        assert_eq!(
            changes(path, &[event(rename, &["/p/src/a.rs", "/p/src/b.rs"])]).renamed,
            Some(PathBuf::from("/p/src/b.rs"))
        );
        // A folder holding it renamed takes it along.
        assert_eq!(
            changes(path, &[event(rename, &["/p/src", "/p/lib"])]).renamed,
            Some(PathBuf::from("/p/lib/a.rs"))
        );
        // Saved through a file moved over it, it was written, not renamed.
        assert_eq!(
            changes(path, &[event(rename, &["/p/src/.a.rs.tmp", "/p/src/a.rs"])]),
            FileChanges {
                written: true,
                ..FileChanges::default()
            }
        );
        let data = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        assert!(changes(path, &[event(data, &["/p/src/a.rs"])]).written);
        assert!(!changes(path, &[event(data, &["/p/src/other.rs"])]).written);

        let moved = changes(
            path,
            &[
                event(EventKind::Remove(RemoveKind::File), &["/p/src/a.rs"]),
                event(EventKind::Create(CreateKind::File), &["/p/docs/a.rs"]),
            ],
        );
        assert!(moved.vanished);
        assert_eq!(moved.appeared, vec![PathBuf::from("/p/docs/a.rs")]);
    }
}
