//! Sidebar along the left of the main window: a tree view of the project
//! directory. Folders come before files, both by name, and a folder's contents
//! are read when it is first expanded. Clicking a file asks for it to be
//! opened. Every folder read is watched, and re-read when its entries change
//! on disk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::git_status::GitStatus;
use crate::project_directory::ProjectDirectory;

/// Names never listed.
const HIDDEN_NAMES: &[&str] = &[".git"];

/// Indent added per level of depth.
const INDENT: Pixels = px(12.);

/// How often the git status is read again, which catches what the watcher
/// doesn't see: edits in folders never expanded, staging, commits, and
/// switching branches.
const GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// How often the watcher's reports are collected. A burst of changes (a
/// checkout, a build) within one interval costs one read per folder.
const REFRESH_INTERVAL: Duration = Duration::from_millis(150);

#[derive(PartialEq)]
struct DirEntry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

/// A folder's entries, or why it could not be read.
type Listing = Result<Vec<DirEntry>, SharedString>;

/// Emitted when a file in the tree is clicked.
pub struct OpenFile(pub PathBuf);

pub struct ProjectTree {
    root: Option<PathBuf>,
    /// Every folder read so far, keyed by path.
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    tree: Entity<TreeState>,
    /// Watches every folder in `listings`, one level deep each. `None` without
    /// a project, or if the platform refused a watcher.
    watcher: Option<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    /// Re-reads folders the watcher reports; dropped with the watcher.
    _refresh: Option<Task<()>>,
    /// The project's git status; `None` outside a repository.
    git: Option<GitStatus>,
    /// Reads the git status now and then; replaced with the project.
    _git_refresh: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenFile> for ProjectTree {}

impl ProjectTree {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            cx.observe_global::<ProjectDirectory>(|this, cx| {
                this.open(ProjectDirectory::get(cx), cx)
            }),
            cx.subscribe(&tree, |this, _, event: &TreeEvent, cx| {
                this.on_tree_event(event, cx)
            }),
        ];

        let mut this = Self {
            root: None,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            tree,
            watcher: None,
            watched: HashSet::new(),
            _refresh: None,
            git: None,
            _git_refresh: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.open(ProjectDirectory::get(cx), cx);
        this
    }

    fn open(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if root == self.root {
            return;
        }
        self.root = root.clone();
        self.listings.clear();
        self.expanded.clear();
        self.watched.clear();
        (self.watcher, self._refresh) = match &root {
            Some(_) => self.start_watching(cx),
            None => (None, None),
        };
        self.rebuild(cx);
        self.git = None;
        self._git_refresh = match root.clone() {
            Some(root) => self.watch_git(root, cx),
            None => Task::ready(()),
        };
        if let Some(root) = root {
            self.load(root, cx);
        }
        cx.notify();
    }

    /// Reads the project's git status straight away, then again every so
    /// often, showing it whenever it changes.
    fn watch_git(&self, root: PathBuf, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let status = cx
                    .background_spawn({
                        let root = root.clone();
                        async move { GitStatus::read(&root) }
                    })
                    .await;
                let updated = this.update(cx, |this, cx| {
                    if this.root.as_ref() == Some(&root) && this.git != status {
                        this.git = status;
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
                cx.background_executor().timer(GIT_REFRESH_INTERVAL).await;
            }
        })
    }

    fn on_tree_event(&mut self, event: &TreeEvent, cx: &mut Context<Self>) {
        match event {
            TreeEvent::Expanded(id) => {
                let path = PathBuf::from(id.as_ref());
                self.expanded.insert(path.clone());
                if !self.listings.contains_key(&path) {
                    self.load(path, cx);
                }
            }
            TreeEvent::Collapsed(id) => {
                self.expanded.remove(Path::new(id.as_ref()));
            }
        }
    }

    /// Creates a watcher whose reports re-read the folders they touch.
    fn start_watching(
        &self,
        cx: &mut Context<Self>,
    ) -> (Option<RecommendedWatcher>, Option<Task<()>>) {
        // Collected on a timer rather than awaited: the watcher reports from
        // its own thread, which must not wake app tasks (tests forbid it).
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else { return };
            // Reads and content edits leave a folder's entries as they were.
            if matches!(
                event.kind,
                EventKind::Access(_) | EventKind::Modify(ModifyKind::Data(_))
            ) {
                return;
            }
            for path in event.paths {
                // An entry changed within its parent; a watched folder itself
                // may also have gone.
                if let Some(parent) = path.parent() {
                    tx.send(parent.to_path_buf()).ok();
                }
                tx.send(path).ok();
            }
        });
        let Ok(watcher) = watcher else {
            return (None, None);
        };

        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;
                let dirs = rx.try_iter().collect::<HashSet<_>>();
                if dirs.is_empty() {
                    continue;
                }
                let updated = this.update(cx, |this, cx| {
                    for dir in dirs {
                        if this.listings.contains_key(&dir) {
                            this.load(dir, cx);
                        }
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        (Some(watcher), Some(refresh))
    }

    /// Reads a folder in the background, then shows its entries.
    fn load(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        // Watched before it is read, so no change slips in between.
        if let Some(watcher) = &mut self.watcher
            && !self.watched.contains(&dir)
            && watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok()
        {
            self.watched.insert(dir.clone());
        }

        let root = self.root.clone();
        let read = cx.background_spawn({
            let dir = dir.clone();
            async move { read_dir(&dir) }
        });
        cx.spawn(async move |this, cx| {
            let listing = read.await;
            this.update(cx, |this, cx| {
                // Another project may have been opened while the folder was read.
                if this.root != root {
                    return;
                }
                if this.listings.get(&dir) == Some(&listing) {
                    return;
                }
                this.forget_vanished_folders(&dir, &listing);
                // A folder left expanded that has come back is read again.
                let reappeared = listing.iter().flatten().filter(|entry| {
                    entry.is_dir
                        && this.expanded.contains(&entry.path)
                        && !this.listings.contains_key(&entry.path)
                });
                let reappeared = reappeared
                    .map(|entry| entry.path.clone())
                    .collect::<Vec<_>>();
                this.listings.insert(dir, listing);
                this.rebuild(cx);
                for path in reappeared {
                    this.load(path, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Drops what was read and watched beneath the folders `dir` no longer
    /// lists, so they are read afresh if they come back.
    fn forget_vanished_folders(&mut self, dir: &Path, listing: &Listing) {
        let Some(Ok(previous)) = self.listings.get(dir) else {
            return;
        };
        let current = listing.as_ref().map(Vec::as_slice).unwrap_or_default();
        let vanished = previous
            .iter()
            .filter(|old| {
                old.is_dir && !current.iter().any(|new| new.is_dir && new.path == old.path)
            })
            .map(|old| old.path.clone())
            .collect::<Vec<_>>();
        for gone in vanished {
            self.listings.retain(|path, _| !path.starts_with(&gone));
            self.watched.retain(|path| {
                let keep = !path.starts_with(&gone);
                if !keep && let Some(watcher) = &mut self.watcher {
                    watcher.unwatch(path).ok();
                }
                keep
            });
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let items = match &self.root {
            Some(root) => self.children_of(root),
            None => Vec::new(),
        };
        self.tree.update(cx, |tree, cx| {
            // Replacing the items clears the selection; keep it on the same path.
            let selected = tree.selected_item().map(|item| item.id.clone());
            tree.set_items(items, cx);
            if let Some(ix) = selected.and_then(|id| tree.index_of(&id)) {
                tree.set_selected_index(Some(ix), cx);
            }
        });
    }

    fn children_of(&self, dir: &Path) -> Vec<TreeItem> {
        let placeholder = |label: SharedString| {
            // Keyed under the folder so it never collides with a real path.
            TreeItem::new(format!("{}\0", dir.display()), label).disabled(true)
        };
        match self.listings.get(dir) {
            None => vec![placeholder("Loading…".into())],
            Some(Err(err)) => vec![placeholder(err.clone())],
            Some(Ok(entries)) if entries.is_empty() => vec![placeholder("Empty".into())],
            Some(Ok(entries)) => entries
                .iter()
                .map(|entry| {
                    let item = TreeItem::new(
                        entry.path.to_string_lossy().into_owned(),
                        entry.name.clone(),
                    );
                    if entry.is_dir {
                        item.expanded(self.expanded.contains(&entry.path))
                            .children(self.children_of(&entry.path))
                    } else {
                        item
                    }
                })
                .collect(),
        }
    }
}

fn read_dir(dir: &Path) -> Listing {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|err| SharedString::from(format!("Unreadable: {err}")))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| !HIDDEN_NAMES.iter().any(|name| entry.file_name() == *name))
        .map(|entry| {
            let path = entry.path();
            DirEntry {
                name: entry.file_name().to_string_lossy().into_owned().into(),
                // Follows symlinks, so a link to a folder is listed as a folder.
                is_dir: path.is_dir(),
                path,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    Ok(entries)
}

fn render_entry(
    tree: &WeakEntity<ProjectTree>,
    ix: usize,
    entry: &TreeEntry,
    cx: &mut App,
) -> ListItem {
    let muted = cx.theme().muted_foreground;
    let icon = |name: IconName| Icon::new(name).small().text_color(muted);
    // Its git status, when the project is in a repository.
    let status = (!entry.is_disabled())
        .then(|| {
            let tree = tree.upgrade()?;
            let path = Path::new(entry.item().id.as_ref());
            tree.read(cx).git.as_ref()?.of(path, entry.is_folder())
        })
        .flatten();
    let (chevron, kind) = if entry.is_disabled() {
        (None, None)
    } else if entry.is_folder() && entry.is_expanded() {
        (Some(IconName::ChevronDown), Some(IconName::FolderOpen))
    } else if entry.is_folder() {
        (Some(IconName::ChevronRight), Some(IconName::FolderClosed))
    } else {
        (None, Some(IconName::File))
    };

    // A placeholder ("Loading…", "Empty", or an error) is not an entry.
    let id = if entry.is_disabled() {
        "project-placeholder"
    } else {
        "project-entry"
    };
    let row = h_flex()
        .id((id, ix))
        .gap_1()
        .min_w_0()
        .child(div().flex_none().size_4().children(chevron.map(icon)))
        .child(div().flex_none().size_4().children(kind.map(icon)))
        .child(
            div()
                .truncate()
                .when(entry.is_disabled(), |label| {
                    label.italic().text_color(muted)
                })
                .when_some(status.map(|status| status.color(cx)), |label, color| {
                    label.text_color(color)
                })
                .child(entry.item().label.clone()),
        )
        // Folders expand in the tree itself; a file is opened elsewhere.
        .when(!entry.is_folder() && !entry.is_disabled(), |row| {
            let tree = tree.clone();
            let path = PathBuf::from(entry.item().id.as_ref());
            row.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                tree.update(cx, |_, cx| cx.emit(OpenFile(path.clone())))
                    .ok();
            })
        });

    ListItem::new(ix)
        .pl(INDENT * entry.depth() as f32 + px(8.))
        // Lets UI tests find the row; inert in normal builds.
        .child(gpui_kit::TestSupportExt::test_support(row))
}

impl Render for ProjectTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = v_flex().id("project-tree").size_full().text_sm();
        let sidebar = match self.root {
            Some(_) => {
                let this = cx.entity().downgrade();
                // The tree's own scroll position, shown in a scroll column.
                let scroll = self
                    .tree
                    .read(cx)
                    .scroll_handle()
                    .0
                    .borrow()
                    .base_handle
                    .clone();
                let tree = div()
                    .size_full()
                    .py_1()
                    .child(tree(&self.tree, move |ix, entry, _, _, cx| {
                        render_entry(&this, ix, entry, cx)
                    }));
                sidebar.child(crate::scroll_column::with_scroll_column(
                    "project-tree",
                    &scroll,
                    tree,
                    true,
                    None,
                    cx,
                ))
            }
            None => sidebar
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("No project open"),
        };
        // Lets UI tests find the sidebar; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(sidebar)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{App, AppContext as _, Entity, TestAppContext};

    use super::{OpenFile, ProjectTree};
    use crate::project_directory::ProjectDirectory;

    const TIMEOUT: Duration = Duration::from_secs(2);

    fn labels(tree: &Entity<ProjectTree>, cx: &App) -> Vec<String> {
        let state = tree.read(cx).tree.read(cx);
        (0..)
            .map_while(|ix| state.entry(ix))
            .map(|entry| entry.item().label.to_string())
            .collect()
    }

    /// Picking a project lists its top level, folders first; clicking a
    /// folder shows its contents beneath it, and clicking a file opens it.
    #[gpui_kit::test]
    async fn lists_the_project_and_expands_folders(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec")).unwrap();
        fs::write(dir.join("spec/index.pi"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle = window.into();
        let opened = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let opened = opened.clone();
            cx.subscribe(&tree, move |_, OpenFile(path), _| {
                opened.borrow_mut().push(path.clone())
            })
        });

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("project-tree").is_some()
        })
        .await;
        assert!(labels(&tree, &cx.app.borrow()).is_empty());

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            labels(&tree, cx) == ["spec", "a.txt", "Cargo.toml"]
        })
        .await;

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            labels(&tree, cx) == ["spec", "index.pi", "a.txt", "Cargo.toml"]
        })
        .await;
        assert!(
            opened.borrow().is_empty(),
            "a folder was opened: {:?}",
            opened.borrow()
        );

        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-entry", 1usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 1usize), cx)
        })
        .unwrap();
        let index = dir.join("spec/index.pi");
        cx.wait_for(handle, TIMEOUT, |_, _| *opened.borrow() == [index.clone()])
            .await;

        fs::remove_dir_all(&dir).ok();
    }

    /// Files added or removed on disk show up in the tree by themselves, in
    /// the top level and in an expanded folder alike.
    #[gpui_kit::test]
    async fn refreshes_when_the_disk_changes(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-refresh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec")).unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle = window.into();

        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            labels(&tree, cx) == ["spec", "a.txt"]
                && window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            labels(&tree, cx) == ["spec", "Empty", "a.txt"]
        })
        .await;

        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(dir.join("spec/index.pi"), "").unwrap();
        fs::remove_file(dir.join("a.txt")).unwrap();
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["spec", "index.pi", "b.txt"]
        })
        .await;

        fs::remove_dir_all(&dir).ok();
    }

    /// Like `wait_for`, but lets real time pass: the watcher reports from its
    /// own thread, which the test clock does not drive.
    async fn wait_for_disk(
        cx: &mut TestAppContext,
        handle: gpui_kit::AnyWindowHandle,
        mut predicate: impl FnMut(&App) -> bool,
    ) {
        for _ in 0..200 {
            cx.run_until_parked();
            if cx.update(|cx| predicate(cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
            cx.executor().advance_clock(Duration::from_millis(50));
        }
        cx.wait_for(handle, Duration::ZERO, |_, cx| predicate(cx))
            .await;
    }
}
