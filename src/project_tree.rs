//! Sidebar along the left of the main window: a tree view of the project
//! directory. Folders come before files, both by name, and a folder's contents
//! are read when it is first expanded. Clicking a file asks for it to be
//! opened. Every folder read is watched, and re-read when its entries change
//! on disk. Right-clicking offers to create, rename, or delete files and
//! folders, named in the tree itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeItem, TreeState};
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::chat_input::SendMode;
use crate::git_status::{GitStatus, Status};
use crate::project_directory::ProjectDirectory;
use crate::theme::Hue;

/// Names never listed.
const HIDDEN_NAMES: &[&str] = &[".git"];

/// Indent added per level of depth: an icon and the gap after it, so a
/// folder's contents' icons sit beneath its name.
pub const INDENT: Pixels = px(24.);

/// How tall each row is.
pub const ROW_HEIGHT: Pixels = px(24.);

/// How far a row's background, when selected or hovered, is inset from either
/// side of the tree.
pub const ROW_INSET: Pixels = px(8.);

/// Where a top-level row's icon starts, from the tree's left edge.
pub const ICON_LEFT: Pixels = px(16.);

/// Between an icon and its name.
const NAME_GAP: Pixels = px(8.);

/// The softly rounded corners of a row's background.
const ROW_RADIUS: Pixels = px(4.);

/// How much of the selected row's background a hovered row has.
const HOVER_OPACITY: f32 = 0.5;

/// The project's configuration file, shown with a settings icon.
const CONFIG_NAME: &str = "piton.config.pi";

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

/// Emitted when a changed file's diff button is clicked.
pub struct OpenDiff(pub PathBuf);

/// Emitted when a file or folder is renamed, to its new path, or deleted.
pub struct EntryMoved {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
}

/// What is being named in the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Naming {
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
}

impl Naming {
    /// The folder the name goes in.
    fn folder(&self) -> Option<&Path> {
        match self {
            Self::NewFile(dir) | Self::NewFolder(dir) => Some(dir),
            Self::Rename(path) => path.parent(),
        }
    }

    /// The tree item the input takes the place of.
    fn item_id(&self) -> String {
        match self {
            Self::NewFile(dir) | Self::NewFolder(dir) => format!("{}\0new", dir.display()),
            Self::Rename(path) => path.to_string_lossy().into_owned(),
        }
    }
}

/// A name being given in the tree.
struct Editing {
    naming: Naming,
    input: Entity<InputState>,
    /// Why the name can't be used, or why the file system refused it.
    problem: Option<SharedString>,
    _subscription: Subscription,
}

/// The tree's menu, open where it was right-clicked.
struct OpenMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    /// What had focus before, given it back if the menu closes without
    /// anything else taking it.
    previous_focus: Option<FocusHandle>,
    _dismissed: Subscription,
}

/// Why `name` can't be given to something in `dir`, other than `renaming`.
pub fn name_problem(name: &str, dir: &Path, renaming: Option<&Path>) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Give it a name");
    }
    if name.contains(['/', '\\']) {
        return Some("A name can't contain a slash");
    }
    if name == "." || name == ".." {
        return Some("That name isn't allowed");
    }
    let path = dir.join(name);
    if renaming != Some(path.as_path()) && path.symlink_metadata().is_ok() {
        return Some("Something with that name is already here");
    }
    None
}

/// Where the project's spec and code are, as the ProjectLocationsScope says:
/// each missing from the config is left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Locations {
    /// The spec location, `root` of its piton-config.
    pub spec: Option<PathBuf>,
    /// The code location, `codeRoot` of its belay-config.
    pub code: Option<PathBuf>,
}

impl Locations {
    pub fn read(project_dir: &Path) -> Self {
        let location = |key: &str| {
            let value = crate::hidden_anchor::config_value(project_dir, key).ok()?;
            let value = value.trim().trim_start_matches("./").trim_end_matches('/');
            (!value.is_empty() && value != ".").then(|| project_dir.join(value))
        };
        Self {
            spec: location("root"),
            code: location("codeRoot"),
        }
    }
}

/// Which colour a row's icon takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconTint {
    /// Muted text, as most are.
    Muted,
    /// Spec's colour: the spec location's folder and its .pi files.
    Spec,
    /// Code's colour: the code location's folder.
    Code,
    /// The theme's orange: the project's piton.config.pi.
    Config,
}

/// How a file or folder's row looks, apart from its git status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub icon: IconName,
    pub tint: IconTint,
    /// Dimmed, name and icon both, as tertiary text is.
    pub dimmed: bool,
}

/// A dot file that is part of how the project is kept, such as .gitignore,
/// shown like any other file rather than dimmed.
fn kept_dot_file(name: &str) -> bool {
    name.starts_with(".git") || name.ends_with("ignore")
}

/// How the row for `path`, named `name`, looks in a project at `root` with
/// `locations`, given whether git ignores it.
pub fn look(
    path: &Path,
    name: &str,
    is_dir: bool,
    ignored: bool,
    root: &Path,
    locations: &Locations,
) -> Look {
    let is_pi = !is_dir && name.ends_with(".pi");
    let config = !is_dir && path.parent() == Some(root) && name == CONFIG_NAME;
    let icon = if is_dir {
        IconName::Folder
    } else if config {
        IconName::FileCog
    } else if is_pi {
        IconName::FileCode
    } else {
        IconName::FileText
    };
    let at = |location: &Option<PathBuf>| location.as_deref() == Some(path);
    let in_spec = locations
        .spec
        .as_deref()
        .is_some_and(|spec| path.starts_with(spec));
    let tint = if config {
        IconTint::Config
    } else if is_dir && at(&locations.spec) || is_pi && in_spec {
        IconTint::Spec
    } else if is_dir && at(&locations.code) {
        IconTint::Code
    } else {
        IconTint::Muted
    };
    let dotted = name.starts_with('.') && (is_dir || !kept_dot_file(name));
    Look {
        icon,
        tint,
        dimmed: ignored || dotted,
    }
}

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
    /// Where the project's spec and code are, as its piton.config.pi says.
    locations: Locations,
    /// A name being given in the tree, while one is.
    editing: Option<Editing>,
    /// Selected once the tree lists it, as a folder just made is.
    select_when_listed: Option<PathBuf>,
    /// The one menu right-clicking opens, while it is open.
    menu: Option<OpenMenu>,
    /// Reads the git status now and then; replaced with the project.
    _git_refresh: Task<()>,
    /// How each project switched away from was left: its expanded folders
    /// and the path selected, for when it's back.
    left: HashMap<PathBuf, (HashSet<PathBuf>, Option<PathBuf>)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenFile> for ProjectTree {}
impl EventEmitter<OpenDiff> for ProjectTree {}
impl EventEmitter<EntryMoved> for ProjectTree {}

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
            locations: Locations::default(),
            editing: None,
            select_when_listed: None,
            menu: None,
            _git_refresh: Task::ready(()),
            left: HashMap::new(),
            _subscriptions: subscriptions,
        };
        this.open(ProjectDirectory::get(cx), cx);
        this
    }

    fn open(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if root == self.root {
            return;
        }
        // The project left keeps how it was, for when it's back.
        if let Some(left) = self.root.take() {
            let selected = self
                .tree
                .read(cx)
                .selected_item()
                .map(|item| PathBuf::from(item.id.as_ref()));
            self.left
                .insert(left, (std::mem::take(&mut self.expanded), selected));
        }
        let (expanded, selected) = root
            .as_ref()
            .and_then(|root| self.left.remove(root))
            .unwrap_or_default();
        self.root = root.clone();
        self.locations = root.as_deref().map(Locations::read).unwrap_or_default();
        self.editing = None;
        self.listings.clear();
        self.expanded = expanded;
        self.select_when_listed = selected;
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
            // Its expanded folders are read again, shown open once listed.
            for folder in self.expanded.clone() {
                self.load(folder, cx);
            }
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
                if this.root.as_ref() == Some(&dir) {
                    this.locations = Locations::read(&dir);
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
        let select = self
            .select_when_listed
            .as_ref()
            .map(|path| SharedString::from(path.to_string_lossy().into_owned()));
        let selected_now = self.tree.update(cx, |tree, cx| {
            // Replacing the items clears the selection; keep it on the same path.
            let selected = tree.selected_item().map(|item| item.id.clone());
            tree.set_items(items, cx);
            match select.and_then(|id| tree.index_of(&id)) {
                Some(ix) => {
                    tree.set_selected_index(Some(ix), cx);
                    true
                }
                None => {
                    if let Some(ix) = selected.and_then(|id| tree.index_of(&id)) {
                        tree.set_selected_index(Some(ix), cx);
                    }
                    false
                }
            }
        });
        if selected_now {
            self.select_when_listed = None;
        }
    }

    /// Opens the tree's menu at `position`, in place of any open: making a
    /// file or folder in `folder`, and renaming or deleting `path`, if any.
    fn open_menu(
        &mut self,
        folder: PathBuf,
        path: Option<PathBuf>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = cx.entity().downgrade();
        let view = PopupMenu::build(window, cx, move |menu, _, _| {
            entry_menu(menu, &tree, folder, path)
        });
        let previous_focus = window
            .focused(cx)
            .filter(|focused| {
                self.menu
                    .as_ref()
                    .is_none_or(|open| *focused != open.view.focus_handle(cx))
            })
            .or_else(|| {
                self.menu
                    .as_ref()
                    .and_then(|open| open.previous_focus.clone())
            });
        let dismissed =
            cx.subscribe_in(&view, window, |this, view, _: &DismissEvent, window, cx| {
                let Some(open) = this.menu.take_if(|open| open.view == *view) else {
                    return;
                };
                // Given back only if nothing else, like a name, took the focus.
                let menu_focused = view.focus_handle(cx).contains_focused(window, cx);
                if (window.focused(cx).is_none() || menu_focused)
                    && let Some(previous) = &open.previous_focus
                {
                    window.focus(previous, cx);
                }
                cx.notify();
            });
        view.focus_handle(cx).focus(window, cx);
        self.menu = Some(OpenMenu {
            view,
            position,
            previous_focus,
            _dismissed: dismissed,
        });
        cx.notify();
    }

    /// Starts naming what `naming` makes or renames, in the tree itself.
    pub fn start_naming(&mut self, naming: Naming, window: &mut Window, cx: &mut Context<Self>) {
        let (value, placeholder) = match &naming {
            Naming::Rename(path) => (
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                "",
            ),
            Naming::NewFile(_) => (String::new(), "File name"),
            Naming::NewFolder(_) => (String::new(), "Folder name"),
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(value)
                .placeholder(placeholder)
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, input, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.commit_naming(window, cx),
                    // Focus moved elsewhere ends the naming; the name's row only
                    // going undrawn for a frame, as the tree lays its rows out
                    // afresh, leaves the focus where it was.
                    InputEvent::Blur => {
                        let focused = gpui_kit::Focusable::focus_handle(input.read(cx), cx)
                            .is_focused(window);
                        if !focused {
                            this.cancel_naming(cx);
                        }
                    }
                    InputEvent::Change => {
                        this.check_name(cx);
                        cx.notify();
                    }
                    InputEvent::Focus => {}
                },
            );
        // A new entry's folder opens to show its row.
        if let Naming::NewFile(dir) | Naming::NewFolder(dir) = &naming
            && Some(dir) != self.root.as_ref()
        {
            self.expanded.insert(dir.clone());
            if !self.listings.contains_key(dir) {
                self.load(dir.clone(), cx);
            }
        }
        // Focused straight away, before its row is even drawn, so a menu
        // closing as the naming starts leaves the focus here rather than
        // handing it back to whatever had it before.
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        // Renaming changes no rows: the name takes the label's place.
        let adds_a_row = !matches!(naming, Naming::Rename(_));
        self.editing = Some(Editing {
            naming,
            input,
            problem: None,
            _subscription: subscription,
        });
        if adds_a_row {
            self.rebuild(cx);
        }
        cx.notify();
    }

    /// What is being named, while something is.
    #[cfg(test)]
    pub fn naming(&self) -> Option<&Naming> {
        self.editing.as_ref().map(|editing| &editing.naming)
    }

    /// Notes why the name typed can't be used, if it can't.
    fn check_name(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let name = editing.input.read(cx).value().trim().to_string();
        let renaming = match &editing.naming {
            Naming::Rename(path) => Some(path.as_path()),
            _ => None,
        };
        editing.problem = editing
            .naming
            .folder()
            .and_then(|dir| name_problem(&name, dir, renaming))
            .map(SharedString::from);
    }

    /// Leaves everything as it was.
    pub fn cancel_naming(&mut self, cx: &mut Context<Self>) {
        if let Some(editing) = self.editing.take() {
            if !matches!(editing.naming, Naming::Rename(_)) {
                self.rebuild(cx);
            }
            cx.notify();
        }
    }

    /// Makes or renames it, if the name can be used.
    pub fn commit_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.check_name(cx);
        let Some(editing) = &self.editing else {
            return;
        };
        if editing.problem.is_some() {
            cx.notify();
            return;
        }
        let name = editing.input.read(cx).value().trim().to_string();
        let naming = editing.naming.clone();
        let Some(dir) = naming.folder().map(Path::to_path_buf) else {
            return;
        };
        let path = dir.join(&name);
        let done = match &naming {
            Naming::NewFile(_) => std::fs::File::create_new(&path).map(|_| ()),
            Naming::NewFolder(_) => std::fs::create_dir(&path),
            Naming::Rename(from) if *from == path => Ok(()),
            Naming::Rename(from) => std::fs::rename(from, &path),
        };
        if let Err(err) = done {
            if let Some(editing) = &mut self.editing {
                editing.problem = Some(format!("Couldn't: {err}").into());
            }
            cx.notify();
            return;
        }
        self.editing = None;
        match naming {
            Naming::NewFile(_) => {
                self.select_when_listed = Some(path.clone());
                cx.emit(OpenFile(path));
            }
            Naming::NewFolder(_) => self.select_when_listed = Some(path),
            Naming::Rename(from) => {
                // A folder keeps what was open inside it open.
                let moved: Vec<PathBuf> = self
                    .expanded
                    .iter()
                    .filter(|open| open.starts_with(&from))
                    .cloned()
                    .collect();
                for open in moved {
                    self.expanded.remove(&open);
                    if let Ok(within) = open.strip_prefix(&from) {
                        self.expanded.insert(path.join(within));
                    }
                }
                self.listings.retain(|listed, _| !listed.starts_with(&from));
                self.select_when_listed = Some(path.clone());
                if from != path {
                    cx.emit(EntryMoved {
                        from,
                        to: Some(path),
                    });
                }
            }
        }
        // Shown straight away rather than when the watcher next reports.
        self.load(dir, cx);
        self.rebuild(cx);
        window.refresh();
        cx.notify();
    }

    /// Asks first, then deletes `path` from disk for good.
    pub fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let description = if path.is_dir() {
            "This can't be undone. Everything in the folder is deleted too."
        } else {
            "This can't be undone."
        };
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, path) = (this.clone(), path.clone());
            alert
                .title(SharedString::from(format!("Delete {name}?")))
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.delete(path.clone(), window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Deletes `path` from disk for good, or says why it couldn't.
    pub fn delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let deleted = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match deleted {
            Ok(()) => {
                cx.emit(EntryMoved {
                    from: path.clone(),
                    to: None,
                });
                if let Some(dir) = path.parent() {
                    self.load(dir.to_path_buf(), cx);
                }
            }
            Err(err) => window.push_notification(
                Notification::error(format!("{err}")).title("Could not delete"),
                cx,
            ),
        }
    }

    fn children_of(&self, dir: &Path) -> Vec<TreeItem> {
        let placeholder = |label: SharedString| {
            // Keyed under the folder so it never collides with a real path.
            TreeItem::new(format!("{}\0", dir.display()), label).disabled(true)
        };
        // A new file or folder being named has a row of its own at the top.
        let new_row = self
            .editing
            .as_ref()
            .filter(|editing| {
                matches!(&editing.naming, Naming::NewFile(new) | Naming::NewFolder(new) if new == dir)
            })
            .map(|editing| TreeItem::new(editing.naming.item_id(), ""));
        let children = match self.listings.get(dir) {
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
        };
        match new_row {
            Some(row) => std::iter::once(row)
                .chain(children.into_iter().filter(|item| !item.is_disabled()))
                .collect(),
            None => children,
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

/// A row of the tree: 24 pixels tall, its background, when selected or
/// hovered, inset from either side, with the icon and name inside it.
fn render_entry(
    tree: &WeakEntity<ProjectTree>,
    ix: usize,
    entry: &TreeEntry,
    selected: bool,
    cx: &mut App,
) -> Div {
    let palette = *crate::theme::palette(cx);
    let muted = cx.theme().muted_foreground;
    let dim = crate::theme::color(palette.text_tertiary);
    // The name being given here, in place of the label, while it is.
    let editing = tree.upgrade().and_then(|tree| {
        let tree = tree.read(cx);
        let editing = tree.editing.as_ref()?;
        (editing.naming.item_id() == entry.item().id.as_ref()).then(|| {
            (
                editing.input.clone(),
                editing.problem.clone(),
                matches!(editing.naming, Naming::NewFolder(_)),
            )
        })
    });
    // Its git status, when the project is in a repository.
    let status = (!entry.is_disabled())
        .then(|| {
            let tree = tree.upgrade()?;
            let path = Path::new(entry.item().id.as_ref());
            tree.read(cx).git.as_ref()?.of(path, entry.is_folder())
        })
        .flatten();
    // Its icon, the icon's colour, and whether it is dimmed; a folder's icon
    // is the same whether it is expanded or not, and there are no chevrons.
    let look = if let Some((_, _, new_folder)) = &editing
        && entry.item().id.ends_with("\0new")
    {
        Some(Look {
            icon: if *new_folder {
                IconName::Folder
            } else {
                IconName::FileText
            },
            tint: IconTint::Muted,
            dimmed: false,
        })
    } else if entry.is_disabled() {
        None
    } else {
        tree.upgrade().and_then(|tree| {
            let tree = tree.read(cx);
            let root = tree.root.as_deref()?;
            Some(look(
                Path::new(entry.item().id.as_ref()),
                &entry.item().label,
                entry.is_folder(),
                status == Some(Status::Ignored),
                root,
                &tree.locations,
            ))
        })
    };
    let dimmed = look.is_some_and(|look| look.dimmed);
    let icon = look.map(|look| {
        let color = if look.dimmed {
            dim
        } else {
            match look.tint {
                IconTint::Muted => muted,
                IconTint::Spec => crate::chat_input::mode_color(SendMode::Spec, cx),
                IconTint::Code => crate::chat_input::mode_color(SendMode::Code, cx),
                IconTint::Config => Hue::Orange.of(&palette),
            }
        };
        Icon::new(look.icon).small().text_color(color)
    });

    // A placeholder ("Loading…", "Empty", or an error) is not an entry.
    let id = if entry.is_disabled() {
        "project-placeholder"
    } else {
        "project-entry"
    };
    let row =
        h_flex()
            .id((id, ix))
            .size_full()
            .min_w_0()
            .gap(NAME_GAP)
            .pl(ICON_LEFT - ROW_INSET + INDENT * entry.depth() as f32)
            .pr_1()
            .rounded(ROW_RADIUS)
            .map(|row| {
                let background = crate::theme::color(palette.ribbon);
                if selected && !entry.is_disabled() {
                    row.bg(background)
                } else if entry.is_disabled() {
                    row
                } else {
                    row.hover(|row| row.bg(background.opacity(HOVER_OPACITY)))
                }
            })
            // Lets UI tests find the icon; inert in normal builds.
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id(("project-icon", ix))
                    .flex_none()
                    .size_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(icon),
            ))
            .map(|row| match editing.clone() {
                // The name is typed where the label is, over it: the label stays
                // laid out, unseen, so the row is exactly as it was, and why a name
                // can't be used shows beneath the row, over the rows below.
                Some((input, problem, _)) => row.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        // Its keys are its own, not the tree's or the row's.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_key_down({
                            let tree = tree.clone();
                            move |event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape"
                                    && !event.keystroke.modifiers.modified()
                                {
                                    cx.stop_propagation();
                                    tree.update(cx, |tree, cx| tree.cancel_naming(cx)).ok();
                                }
                            }
                        })
                        .child(div().invisible().truncate().child(
                            if entry.item().label.is_empty() {
                                SharedString::from("\u{a0}")
                            } else {
                                entry.item().label.clone()
                            },
                        ))
                        // An outline around the name, outside the label's box.
                        .child(
                            div()
                                .absolute()
                                .top(px(-1.))
                                .bottom(px(-1.))
                                .left(px(-3.))
                                .right(px(-1.))
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(if problem.is_some() {
                                    cx.theme().danger
                                } else {
                                    cx.theme().ring
                                }),
                        )
                        .child(gpui_kit::TestSupportExt::test_support(
                            div()
                                .id("project-naming")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .child(
                                    Input::new(&input)
                                        .appearance(false)
                                        .size_full()
                                        .p_0()
                                        .text_sm(),
                                ),
                        ))
                        .children(problem.map(|problem| {
                            div().absolute().top_full().left(px(-3.)).child(
                                deferred(
                                    anchored().snap_to_window().child(
                                        gpui_kit::TestSupportExt::test_support(
                                            div()
                                                .id("project-naming-problem")
                                                .mt_1()
                                                .px_1p5()
                                                .py_0p5()
                                                .rounded(cx.theme().radius)
                                                .border_1()
                                                .border_color(cx.theme().danger)
                                                .bg(cx.theme().popover)
                                                .text_xs()
                                                .text_color(cx.theme().danger)
                                                .child(problem),
                                        ),
                                    ),
                                )
                                .with_priority(1),
                            )
                        })),
                ),
                // The name, in its git status's colour, unless it is dimmed;
                // the selected row's in semibold.
                None => row.child(gpui_kit::TestSupportExt::test_support(
                    div()
                        .id(("project-name", ix))
                        .truncate()
                        .when(selected && !entry.is_disabled(), |label| {
                            label.font_semibold()
                        })
                        .when(entry.is_disabled(), |label| {
                            label.italic().text_color(muted)
                        })
                        .when_some(status.map(|status| status.color(cx)), |label, color| {
                            label.text_color(color)
                        })
                        .when(dimmed, |label| label.text_color(dim))
                        .child(entry.item().label.clone()),
                )),
            })
            // A new or changed file has a button beside it that opens its diff.
            .when(
                !entry.is_folder()
                    && matches!(
                        status,
                        Some(Status::Added | Status::Modified | Status::Untracked)
                    ),
                |row| {
                    let tree = tree.clone();
                    let path = PathBuf::from(entry.item().id.as_ref());
                    row.child(div().flex_1()).child(
                        div()
                            .flex_none()
                            // Not also a click on the row, which opens the file.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                Button::new(("project-diff", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::FileDiff)
                                    .tooltip("Show the changes")
                                    .on_click(move |_, _, cx| {
                                        tree.update(cx, |_, cx| cx.emit(OpenDiff(path.clone())))
                                            .ok();
                                    }),
                            ),
                    )
                },
            )
            // Folders expand in the tree itself; a file is opened elsewhere.
            .when(
                !entry.is_folder() && !entry.is_disabled() && editing.is_none(),
                |row| {
                    let tree = tree.clone();
                    let path = PathBuf::from(entry.item().id.as_ref());
                    row.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        tree.update(cx, |_, cx| cx.emit(OpenFile(path.clone())))
                            .ok();
                    })
                },
            );

    // Right-clicked, the tree's menu opens for the row: making things beside
    // a file or in a folder, and renaming or deleting it; for a placeholder,
    // making things in its folder. The name being given has none.
    let target = if editing.is_some() {
        None
    } else if entry.is_disabled() {
        let id = entry.item().id.as_ref();
        Some((
            PathBuf::from(id.split('\0').next().unwrap_or_default()),
            None,
        ))
    } else {
        let path = PathBuf::from(entry.item().id.as_ref());
        let folder = if entry.is_folder() {
            path.clone()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        Some((folder, Some(path)))
    };
    let row = row.on_mouse_down(MouseButton::Right, {
        let tree = tree.clone();
        move |event, window, cx| {
            // Not also the menu for the space beneath the rows.
            cx.stop_propagation();
            let Some((folder, path)) = target.clone() else {
                return;
            };
            tree.update(cx, |tree, cx| {
                tree.open_menu(folder, path, event.position, window, cx)
            })
            .ok();
        }
    });
    // Lets UI tests find the row; inert in normal builds.
    let row = gpui_kit::TestSupportExt::test_support(row);

    div()
        .h(ROW_HEIGHT)
        .w_full()
        .px(ROW_INSET)
        .when(entry.is_disabled(), |row| row.text_color(muted))
        .child(row)
}

/// The menu a right-click opens: making a file or folder in `folder`, and for
/// an entry at `path`, renaming or deleting it.
fn entry_menu(
    menu: PopupMenu,
    tree: &WeakEntity<ProjectTree>,
    folder: PathBuf,
    path: Option<PathBuf>,
) -> PopupMenu {
    let item = |label: &'static str, icon: IconName, naming: Naming| {
        let tree = tree.clone();
        PopupMenuItem::new(label)
            .icon(icon)
            .on_click(move |_, window, cx| {
                // Straight away, so the name has focus before the menu closes
                // and the menu leaves it there.
                tree.update(cx, |tree, cx| tree.start_naming(naming.clone(), window, cx))
                    .ok();
            })
    };
    let menu = menu
        .item(item(
            "New File…",
            IconName::FilePlus,
            Naming::NewFile(folder.clone()),
        ))
        .item(item(
            "New Folder…",
            IconName::FolderPlus,
            Naming::NewFolder(folder),
        ));
    let Some(path) = path else {
        return menu;
    };
    let delete = {
        let (tree, path) = (tree.clone(), path.clone());
        PopupMenuItem::new("Delete")
            .icon(IconName::Trash)
            .on_click(move |_, window, cx| {
                tree.update(cx, |tree, cx| tree.confirm_delete(path.clone(), window, cx))
                    .ok();
            })
    };
    menu.separator()
        .item(item("Rename…", IconName::Pencil, Naming::Rename(path)))
        .item(delete)
}

impl Render for ProjectTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // On the darkest surface, a step past the ribbon's command area.
        let sidebar = v_flex()
            .id("project-tree")
            .size_full()
            .bg(crate::theme::color(crate::theme::palette(cx).darkest))
            .text_sm();
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
                // The empty space beneath the rows offers to make a file or
                // folder at the top of the project.
                let root = self.root.clone().unwrap_or_default();
                let tree = div()
                    .id("project-tree-space")
                    .size_full()
                    .py_1()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_menu(root.clone(), None, event.position, window, cx)
                        }),
                    )
                    // The tree's one menu, where it was right-clicked.
                    .children(self.menu.as_ref().map(|open| {
                        deferred(
                            anchored()
                                .position(open.position)
                                .snap_to_window_with_margin(px(8.))
                                .child(open.view.clone()),
                        )
                        .with_priority(gpui_kit::base::POPUP_PRIORITY)
                    }))
                    // The base tree rather than gpui-kit's, which lays its own
                    // scrollbar over the rows: the scroll column is the only
                    // scrollbar.
                    .child(
                        gpui_kit::base::Tree::new(&self.tree)
                            .item(move |ix, entry, entry_state, _, cx| {
                                render_entry(&this, ix, entry, entry_state.is_selected(), cx)
                                    .into_any_element()
                            })
                            .list_style(StyleRefinement::default().flex_grow_1().size_full())
                            .relative()
                            .size_full(),
                    );
                // Lets UI tests find the space the rows are in; inert in
                // normal builds.
                let tree = gpui_kit::TestSupportExt::test_support(tree);
                sidebar.child(crate::scrollbar::with_scrollbar(
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

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn labels(tree: &Entity<ProjectTree>, cx: &App) -> Vec<String> {
        let state = tree.read(cx).tree.read(cx);
        (0..)
            .map_while(|ix| state.entry(ix))
            .map(|entry| entry.item().label.to_string())
            .collect()
    }

    /// A project with a spec and a code location, for the tree's look: once
    /// open, with the spec folder expanded and selected, the tree lists
    /// spec, lib, index.pi, src, piton.config.pi.
    async fn open_located_project(
        name: &str,
        cx: &mut TestAppContext,
    ) -> (
        std::path::PathBuf,
        Entity<ProjectTree>,
        gpui_kit::AnyWindowHandle,
    ) {
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec/lib")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("spec/index.pi"), "").unwrap();
        fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Config:\n    root: ./spec\n\nbelay-config Belay:\n    codeRoot: ./src\n",
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle: gpui_kit::AnyWindowHandle = window.into();
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(260.), gpui_kit::px(400.)));
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            labels(&tree, cx) == ["spec", "src", "piton.config.pi"]
                && window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            labels(&tree, cx) == ["spec", "lib", "index.pi", "src", "piton.config.pi"]
                && window.try_find(("project-entry", 4usize)).is_some()
        })
        .await;
        (dir, tree, handle)
    }

    /// Each row is 24 pixels tall, one straight after another; a top-level
    /// row's icon starts 16 pixels from the tree's left edge, its name 8
    /// pixels after the icon, and each level is indented 24 pixels further,
    /// so a folder's contents' icons sit beneath its name.
    #[gpui_kit::test]
    async fn rows_are_24px_tall_and_indented_24px_a_level(cx: &mut TestAppContext) {
        use gpui_kit::px;
        let (dir, _, handle) = open_located_project("tree-rows", cx).await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let space = window.find("project-tree-space").bounds();
            let row = |ix: usize| window.find(("project-entry", ix)).bounds();
            let icon = |ix: usize| window.find(("project-icon", ix)).bounds();
            let name = |ix: usize| window.find(("project-name", ix)).bounds();
            for ix in 0..5 {
                assert_eq!(row(ix).size.height, super::ROW_HEIGHT, "row {ix}'s height");
                assert_eq!(
                    (
                        row(ix).left() - space.left(),
                        space.right() - row(ix).right()
                    ),
                    (super::ROW_INSET, super::ROW_INSET),
                    "row {ix} isn't inset 8px from either side"
                );
                if ix > 0 {
                    assert_eq!(row(ix).top() - row(ix - 1).top(), px(24.), "row {ix}");
                }
                assert_eq!(
                    name(ix).left() - icon(ix).right(),
                    px(8.),
                    "row {ix}'s name isn't 8px after its icon"
                );
            }
            // spec, lib, index.pi, src, piton.config.pi
            for (ix, depth) in [(0, 0.), (1, 1.), (2, 1.), (3, 0.), (4, 0.)] {
                assert_eq!(
                    icon(ix).left() - space.left(),
                    super::ICON_LEFT + super::INDENT * depth,
                    "row {ix}'s icon at depth {depth}"
                );
            }
            assert_eq!(
                icon(1).left(),
                name(0).left(),
                "lib's icon isn't beneath spec's name"
            );
        })
        .unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    /// The tree is on the darkest surface; the selected row's background is
    /// the ribbon's command area colour, inset 8 pixels from either side with
    /// rounded corners, and no other row has one, in dark and light mode.
    #[gpui_kit::test]
    async fn the_selected_row_is_on_the_ribbons_colour(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::{point, px};
        let (dir, tree, handle) = open_located_project("tree-selected", cx).await;
        assert_eq!(
            cx.update(|cx| tree.read(cx).tree.read(cx).selected_index()),
            Some(0),
            "the spec folder clicked isn't selected"
        );
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            for _ in 0..2 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let palette = crate::theme::palette(cx);
                let frame = crate::frame_image::Frame::of(window);
                let space = window.find("project-tree-space").bounds();
                let selected = window.find(("project-entry", 0usize)).bounds();
                let other = window.find(("project-entry", 3usize)).bounds();
                let middle = |b: gpui_kit::Bounds<gpui_kit::Pixels>| b.top() + b.size.height / 2.;
                let y = middle(selected);
                assert_eq!(
                    frame.at(point(selected.left() + px(40.), y)),
                    palette.ribbon,
                    "{mode:?}: the selected row"
                );
                assert_eq!(
                    frame.at(point(space.left() + px(4.), y)),
                    palette.darkest,
                    "{mode:?}: the selected row isn't inset at its left"
                );
                assert_eq!(
                    frame.at(point(space.right() - px(4.), y)),
                    palette.darkest,
                    "{mode:?}: the selected row isn't inset at its right"
                );
                assert_eq!(
                    frame.at(point(other.left() + px(40.), middle(other))),
                    palette.darkest,
                    "{mode:?}: a row not selected has a background"
                );
                // Its background drawn with rounded corners.
                let scale = window.scale_factor();
                assert!(
                    window.painted_quads().iter().any(|quad| {
                        (quad.bounds.origin.x.0 / scale - selected.left().as_f32()).abs() < 0.5
                            && (quad.bounds.size.height.0 / scale - 24.).abs() < 0.5
                            && quad.corner_radii.top_left.0 > 0.
                    }),
                    "{mode:?}: no rounded background drawn for the selected row"
                );
            })
            .unwrap();
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// The tree's scrollbar is laid over the tree's darkest surface, as the
    /// mockup is: #222222 down both of the column's sides and around its
    /// buttons' arrows, and, with nothing to scroll, the thumb filling the
    /// track in #222222 too, a row of #191919 at either end.
    #[gpui_kit::test]
    async fn the_scrollbar_is_laid_over_the_trees_surface(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::{point, px};
        let (dir, _, handle) = open_located_project("tree-scrollbar", cx).await;
        cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let darkest = crate::theme::palette(cx).darkest;
            assert_eq!(darkest, 0x111111);
            let frame = crate::frame_image::Frame::of(window);
            let column = window.find("project-tree-scroll-column").bounds();
            let track = window.find("project-tree-scroll-track").bounds();
            let up = window.find("project-tree-scroll-up").bounds();
            let at = |x: f32, y: f32, from: gpui_kit::Bounds<gpui_kit::Pixels>| {
                frame.at(point(from.left() + px(x + 0.5), from.top() + px(y + 0.5)))
            };
            let raised = 0x222222;
            let near = |a: u32, b: u32| (a as i32 - b as i32).abs() <= 0x010101;
            for y in [
                0.,
                8.,
                20.,
                column.size.height.as_f32() / 2.,
                column.size.height.as_f32() - 1.,
            ] {
                assert_eq!(at(-1., y, column), darkest, "beside the column at {y}");
                for x in [0., 17.] {
                    let c = at(x, y, column);
                    assert!(
                        near(c, raised),
                        "the column's side is {c:06x} at ({x}, {y})"
                    );
                }
            }
            // The up button: its arrow, and raised around it.
            let c = at(2., 8., up);
            assert!(near(c, raised), "the up button is {c:06x}");
            assert!(!near(at(8., 10., up), raised), "the up button has no arrow");
            // The thumb fills the track: its ends half way between the
            // surface and the thumb, and raised between.
            let end = 0x191919;
            let last = track.size.height.as_f32() - 1.;
            for x in [1., 8., 16.] {
                for y in [0., last] {
                    let c = at(x, y, track);
                    assert!(
                        (c as i32 - end as i32).abs() <= 0x010101,
                        "the thumb's end is {c:06x} at ({x}, {y})"
                    );
                }
                let c = at(x, last / 2., track);
                assert!(near(c, raised), "the thumb is {c:06x} at {x}");
            }
        })
        .unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    /// The spec location's folder and its .pi files take Spec's colour, the
    /// code location's folder Code's, and the project's piton.config.pi the
    /// theme's orange with a settings icon; everything else is muted. Dot
    /// files and folders, but not .gitignore and the like, and whatever git
    /// ignores, are dimmed. A folder's icon is the same expanded or not.
    #[test]
    fn locations_colour_their_icons_and_dot_files_are_dimmed() {
        use gpui_kit::assets::IconName;

        use super::{IconTint, Locations, look};

        let root = std::env::temp_dir().join(format!("suspense-tree-look-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("piton.config.pi"),
            "export piton-config Config:\n    root: ./spec/\n\nbelay-config Belay:\n    codeRoot: ./src\n",
        )
        .unwrap();
        let locations = Locations::read(&root);
        assert_eq!(
            locations,
            Locations {
                spec: Some(root.join("spec")),
                code: Some(root.join("src")),
            }
        );
        assert_eq!(
            Locations::read(&root.join("nowhere")),
            Locations::default(),
            "a project without a config has no locations"
        );

        let of = |path: &str, is_dir: bool| {
            let path = root.join(path);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            look(&path, &name, is_dir, false, &root, &locations)
        };
        let tint = |path: &str, is_dir: bool| of(path, is_dir).tint;
        assert_eq!(tint("spec", true), IconTint::Spec);
        assert_eq!(tint("spec/index.pi", false), IconTint::Spec);
        assert_eq!(tint("spec/lib/index.pi", false), IconTint::Spec);
        assert_eq!(
            tint("spec/lib", true),
            IconTint::Muted,
            "only the location's own folder"
        );
        assert_eq!(tint("spec/notes.md", false), IconTint::Muted);
        assert_eq!(tint("src", true), IconTint::Code);
        assert_eq!(tint("src/main.rs", false), IconTint::Muted);
        assert_eq!(
            tint("examples/a.pi", false),
            IconTint::Muted,
            "a .pi file outside the spec"
        );
        assert_eq!(tint("assets", true), IconTint::Muted);
        assert_eq!(tint("piton.config.pi", false), IconTint::Config);
        assert_eq!(
            tint("spec/piton.config.pi", false),
            IconTint::Spec,
            "not the project's config"
        );

        assert_eq!(of("spec", true).icon, IconName::Folder);
        assert_eq!(of("spec/index.pi", false).icon, IconName::FileCode);
        assert_eq!(of("piton.config.pi", false).icon, IconName::FileCog);
        assert_eq!(of("Cargo.toml", false).icon, IconName::FileText);

        let dimmed = |path: &str, is_dir: bool| of(path, is_dir).dimmed;
        for (path, is_dir) in [
            (".claude", true),
            (".piton", true),
            (".suspense", true),
            (".github", true),
            (".env", false),
        ] {
            assert!(dimmed(path, is_dir), "{path} isn't dimmed");
        }
        for (path, is_dir) in [
            (".gitignore", false),
            (".gitattributes", false),
            (".dockerignore", false),
            ("assets", true),
            ("Cargo.lock", false),
            ("spec", true),
        ] {
            assert!(!dimmed(path, is_dir), "{path} is dimmed");
        }
        let target = root.join("targets");
        assert!(
            look(&target, "targets", true, true, &root, &locations).dimmed,
            "a folder git ignores isn't dimmed"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// Spec's and Code's colours, as the tree draws their locations' icons,
    /// are the modes' own: blue and red.
    #[gpui_kit::test]
    fn location_colours_are_the_modes(cx: &mut TestAppContext) {
        use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};

        use crate::chat_input::{SendMode, mode_color};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            for mode in [ThemeMode::Dark, ThemeMode::Light] {
                Theme::change(mode, None, cx);
                let same = |a: gpui_kit::Hsla, b: gpui_kit::Hsla| {
                    (a.h - b.h).abs() < 1e-4
                        && (a.s - b.s).abs() < 1e-4
                        && (a.l - b.l).abs() < 1e-4
                        && a.a == 1.
                };
                assert!(
                    same(mode_color(SendMode::Spec, cx), cx.theme().blue),
                    "{mode:?}: Spec"
                );
                assert!(
                    same(mode_color(SendMode::Code, cx), cx.theme().red),
                    "{mode:?}: Code"
                );
            }
        });
    }

    /// A project switched away from comes back with its folders expanded as
    /// they were left.
    #[gpui_kit::test]
    async fn projects_keep_their_expanded_folders(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!("suspense-tree-kept-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(a.join("spec")).unwrap();
        fs::write(a.join("spec/index.pi"), "").unwrap();
        fs::create_dir_all(b.join("src")).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, cx| {
            labels(&tree, cx) == ["spec"] && window.try_find(("project-entry", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-entry", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            labels(&tree, cx) == ["spec", "index.pi"]
        })
        .await;

        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |_, cx| labels(&tree, cx) == ["src"])
            .await;
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            labels(&tree, cx) == ["spec", "index.pi"]
        })
        .await;
        fs::remove_dir_all(&root).ok();
    }

    /// Picking a project lists its top level, folders first; clicking a
    /// folder shows its contents beneath it, and clicking a file opens it.
    #[gpui_kit::test]
    async fn lists_the_project_and_expands_folders(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec")).unwrap();
        // As the project's root is named, as /private/var for /var on macOS.
        let dir = dunce::canonicalize(&dir).unwrap();
        fs::write(dir.join("spec").join("index.pi"), "").unwrap();
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
        let index = dir.join("spec").join("index.pi");
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

    /// What right-clicking offers, to make, rename, and delete: a new file is
    /// named in its own row and opens, a name already used or with a slash can't
    /// be given, Esc leaves things as they were, a renamed folder stays open
    /// and says where it went, and a deleted file goes, once confirmed.
    /// Right-clicking an entry opens one menu, and choosing from it names the
    /// thing in place: the name's input takes focus and keeps it, and the rows
    /// hold still. Renaming lays nothing out anew; a new file or folder adds
    /// only its own row, as tall as any other, pushing the rows beneath it down
    /// by that much and no more.
    #[gpui_kit::test]
    async fn the_menu_names_things_in_place(cx: &mut TestAppContext) {
        use gpui_kit::{Bounds, MouseButton, Pixels, VisualTestContext};

        use super::Naming;
        let dir = std::env::temp_dir().join(format!("suspense-tree-menu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec")).unwrap();
        for file in ["a.txt", "b.txt", "c.txt"] {
            fs::write(dir.join(file), "").unwrap();
        }
        let dir = dunce::canonicalize(&dir).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::main_window::bind_keys(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle = window.into();
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["spec", "a.txt", "b.txt", "c.txt"]
        })
        .await;

        let rows = |cx: &mut TestAppContext| -> Vec<Bounds<Pixels>> {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                (0..6usize)
                    .filter_map(|ix| window.try_find(("project-entry", ix)))
                    .map(|row| row.bounds())
                    .collect()
            })
            .unwrap()
        };
        let frames = |cx: &mut TestAppContext| {
            for _ in 0..6 {
                cx.run_until_parked();
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
        };
        // Right-clicks row `ix`, and chooses item `item` from the menu that
        // opens, checking only one did.
        let choose = |ix: usize, item: usize, cx: &mut TestAppContext| {
            let at = rows(cx)[ix].center();
            let mut visual = VisualTestContext::from_window(handle, cx);
            visual.simulate_mouse_move(at, None, Default::default());
            visual.simulate_mouse_down(at, MouseButton::Right, Default::default());
            visual.simulate_mouse_up(at, MouseButton::Right, Default::default());
            frames(cx);
            let item_at = cx
                .update_window(handle, |_, window, _| {
                    let menus = window
                        .within("popup-menu")
                        .try_find(item)
                        .map(|i| i.bounds());
                    menus.expect("no menu opened").center()
                })
                .unwrap();
            let mut visual = VisualTestContext::from_window(handle, cx);
            visual.simulate_mouse_move(item_at, None, Default::default());
            visual.simulate_mouse_down(item_at, MouseButton::Left, Default::default());
            visual.simulate_mouse_up(item_at, MouseButton::Left, Default::default());
            frames(cx);
            cx.update_window(handle, |_, window, _| {
                assert!(
                    window.try_find("popup-menu").is_none(),
                    "a menu is still open after choosing from one"
                );
            })
            .unwrap();
        };
        let naming_focused = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                let tree = tree.read(cx);
                tree.editing.as_ref().is_some_and(|editing| {
                    gpui_kit::Focusable::focus_handle(editing.input.read(cx), cx).is_focused(window)
                })
            })
            .unwrap()
        };

        // Rename a.txt: nothing moves, and the input keeps focus.
        let before = rows(cx);
        choose(1, 3, cx);
        assert_eq!(
            tree.read_with(cx, |tree, _| tree.naming().cloned()),
            Some(Naming::Rename(dir.join("a.txt")))
        );
        for _ in 0..5 {
            frames(cx);
            assert!(naming_focused(cx), "the name lost focus");
            assert_eq!(rows(cx), before, "the rows moved while renaming");
        }
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        frames(cx);
        assert_eq!(tree.read_with(cx, |tree, _| tree.naming().cloned()), None);
        assert_eq!(rows(cx), before, "the rows moved once renaming ended");

        // A new file beside b.txt: a row of its own, as tall as the others,
        // and the rest pushed down by just that.
        choose(2, 0, cx);
        assert_eq!(
            tree.read_with(cx, |tree, _| tree.naming().cloned()),
            Some(Naming::NewFile(dir.clone()))
        );
        for _ in 0..5 {
            frames(cx);
            assert!(naming_focused(cx), "the new file's name lost focus");
        }
        let after = rows(cx);
        // A row's height, and how far one row is from the next.
        let (height, pitch) = (before[0].size.height, before[1].top() - before[0].top());
        let naming = cx
            .update_window(handle, |_, window, _| {
                window.find("project-naming").bounds()
            })
            .unwrap();
        assert!(
            naming.size.height <= height,
            "the name {naming:?} is taller than a row {height:?}"
        );
        // The new row, at the top, is as tall as any other, and every entry
        // sits one row lower beneath it.
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            after[0], before[0],
            "the new row isn't where the first row was"
        );
        for (was, now) in before.iter().zip(&after[1..]) {
            assert_eq!(now.size, was.size, "a row changed size");
            assert_eq!(
                now.top() - was.top(),
                pitch,
                "a row moved by other than a row"
            );
        }

        // A name that can't be used says so without moving anything.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                let input = tree.editing.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("b.txt", window, cx));
                // Set here rather than typed, so checked as typing would.
                tree.check_name(cx);
                cx.notify();
            })
        })
        .unwrap();
        frames(cx);
        assert!(tree.read_with(cx, |tree, _| {
            tree.editing.as_ref().unwrap().problem.is_some()
        }));
        assert_eq!(
            rows(cx),
            after,
            "the rows moved to show why the name can't be used"
        );
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        frames(cx);
        assert_eq!(rows(cx), before);
        cx.update_window(handle, |_, window, _| window.remove_window())
            .unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui_kit::test]
    async fn files_are_made_renamed_and_deleted_in_the_tree(cx: &mut TestAppContext) {
        use super::{EntryMoved, Naming};
        let dir = std::env::temp_dir().join(format!("suspense-tree-edit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("spec")).unwrap();
        fs::write(dir.join("spec/index.pi"), "").unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();
        let dir = dunce::canonicalize(&dir).unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::main_window::bind_keys(cx);
            ProjectDirectory::init(cx);
        });
        // The tree, and the dialogs the main window draws over it.
        struct WithDialogs(Entity<ProjectTree>);
        impl gpui_kit::Render for WithDialogs {
            fn render(
                &mut self,
                window: &mut gpui_kit::Window,
                cx: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                use gpui_kit::{ParentElement as _, Styled as _};
                gpui_kit::div()
                    .size_full()
                    .child(self.0.clone())
                    .children(Root::render_dialog_layer(window, cx))
            }
        }
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| WithDialogs(tree.clone()));
            Root::new(view, window, cx)
        });
        let handle = window.into();
        let opened = Rc::new(RefCell::new(Vec::new()));
        let moved = Rc::new(RefCell::new(Vec::new()));
        let _subscriptions = cx.update(|cx| {
            let (opened, moved) = (opened.clone(), moved.clone());
            (
                cx.subscribe(&tree, move |_, OpenFile(path), _| {
                    opened.borrow_mut().push(path.clone())
                }),
                cx.subscribe(&tree, move |_, event: &EntryMoved, _| {
                    moved
                        .borrow_mut()
                        .push((event.from.clone(), event.to.clone()))
                }),
            )
        });
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        wait_for_disk(cx, handle, |cx| labels(&tree, cx) == ["spec", "a.txt"]).await;

        // A new file, in the project's top level.
        let name = |text: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                tree.update(cx, |tree, cx| {
                    let input = tree.editing.as_ref().unwrap().input.clone();
                    input.update(cx, |input, cx| {
                        input.set_value(text.to_string(), window, cx)
                    });
                    tree.commit_naming(window, cx);
                });
                window.render_frame(cx);
            })
            .unwrap();
        };
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.start_naming(Naming::NewFile(dir.clone()), window, cx)
            });
            window.render_frame(cx);
            window.find("project-naming");
        })
        .unwrap();
        assert_eq!(labels(&tree, &cx.app.borrow())[0], "", "no row of its own");

        name("a.txt", cx);
        assert!(tree.read_with(cx, |tree, _| {
            tree.editing.as_ref().unwrap().problem.is_some()
        }));
        name("x/y", cx);
        assert!(tree.read_with(cx, |tree, _| {
            tree.editing.as_ref().unwrap().problem.is_some()
        }));
        name("notes.md", cx);
        assert!(dir.join("notes.md").is_file());
        assert_eq!(opened.borrow().as_slice(), [dir.join("notes.md")]);
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["spec", "a.txt", "notes.md"]
        })
        .await;

        // Esc leaves everything as it was.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.start_naming(Naming::NewFolder(dir.join("spec")), window, cx)
            });
        })
        .unwrap();
        wait_for_disk(cx, handle, |cx| labels(&tree, cx)[1] == "").await;
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("project-naming");
            let focused = tree.read(cx).editing.as_ref().map(|editing| {
                gpui_kit::Focusable::focus_handle(editing.input.read(cx), cx).is_focused(window)
            });
            assert_eq!(focused, Some(true), "the name being given hasn't focus");
            window.press("escape", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(tree.read_with(cx, |tree, _| tree.naming().is_none()));

        // A new folder in a folder, which opens to show it.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.start_naming(Naming::NewFolder(dir.join("spec")), window, cx)
            });
        })
        .unwrap();
        name("scope", cx);
        assert!(dir.join("spec/scope").is_dir());
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["spec", "scope", "index.pi", "a.txt", "notes.md"]
        })
        .await;

        // Renamed, the folder stays open, and says where it went.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.start_naming(Naming::Rename(dir.join("spec")), window, cx)
            });
        })
        .unwrap();
        name("specs", cx);
        assert!(dir.join("specs/index.pi").is_file());
        assert_eq!(
            moved.borrow().as_slice(),
            [(dir.join("spec"), Some(dir.join("specs")))]
        );
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["specs", "scope", "index.pi", "a.txt", "notes.md"]
        })
        .await;

        // Deleted once confirmed.
        cx.update_window(handle, |_, window, cx| {
            tree.update(cx, |tree, cx| {
                tree.confirm_delete(dir.join("a.txt"), window, cx)
            });
            window.render_frame(cx);
        })
        .unwrap();
        assert!(
            dir.join("a.txt").exists(),
            "deleted before it was confirmed"
        );
        cx.wait_for(handle, TIMEOUT, |window, _| window.try_find("ok").is_some())
            .await;
        // Clicked once the dialog has risen under the pointer, however slow
        // the machine.
        let start = std::time::Instant::now();
        while dir.join("a.txt").exists() {
            assert!(start.elapsed() < TIMEOUT, "never deleted");
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                if window.try_find("ok").is_some() {
                    window.click("ok", cx);
                }
            })
            .unwrap();
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(moved.borrow().last(), Some(&(dir.join("a.txt"), None)));
        wait_for_disk(cx, handle, |cx| {
            labels(&tree, cx) == ["specs", "scope", "index.pi", "notes.md"]
        })
        .await;

        fs::remove_dir_all(&dir).ok();
    }

    /// In a git repository, a changed or new file has a diff button beside
    /// it, which asks for its diff rather than opening the file; an unchanged
    /// file has none.
    #[gpui_kit::test]
    async fn changed_files_have_a_diff_button(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-tree-diff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // As the project's root is named, as /private/var for /var on macOS.
        let dir = dunce::canonicalize(&dir).unwrap();
        fs::write(dir.join("changed.txt"), "one\n").unwrap();
        fs::write(dir.join("same.txt"), "same\n").unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first"]);
        fs::write(dir.join("changed.txt"), "two\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            ProjectDirectory::init(cx);
        });
        let tree = cx.update(|cx| cx.new(ProjectTree::new));
        let window = cx.add_window(|window, cx| Root::new(tree.clone(), window, cx));
        let handle = window.into();
        let (opened, diffs) = (
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        let _subscriptions = cx.update(|cx| {
            let (opened, diffs) = (opened.clone(), diffs.clone());
            (
                cx.subscribe(&tree, move |_, OpenFile(path), _| {
                    opened.borrow_mut().push(path.clone())
                }),
                cx.subscribe(&tree, move |_, super::OpenDiff(path), _| {
                    diffs.borrow_mut().push(path.clone())
                }),
            )
        });
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("project-diff", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, _| {
            assert!(
                window.try_find(("project-diff", 1usize)).is_none(),
                "the unchanged file has a diff button"
            );
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.click(("project-diff", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(*diffs.borrow(), [dir.join("changed.txt")]);
        assert!(
            opened.borrow().is_empty(),
            "the diff button also opened the file"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
