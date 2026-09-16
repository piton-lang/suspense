//! A folder picker drawn by the application rather than the platform: a bar
//! with up, home, the current folder's path as a breadcrumb, and a "Show
//! hidden" checkbox; a "New Folder" button that opens a row for naming one;
//! the current folder's folders, which a click selects and a second click goes
//! into; and "Choose" and "Cancel" along the bottom. It can all be driven from
//! the keyboard, as a platform's own file dialog can: the arrows, Home, End,
//! and the page keys move the selection, typing selects by name, Enter goes in,
//! Backspace goes up, and so on.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::checkbox::checkbox;
use crate::main_window::FocusChat;
use crate::scroll_column;

actions!(
    folder_browser,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectPageDown,
        SelectPageUp,
        OpenSelected,
        GoUp,
        GoHome,
        ChooseFolderAction,
        ToggleHidden,
        NewFolderAction,
    ]
);

const CONTEXT: &str = "FolderBrowser";

/// How many folders the page keys move.
const PAGE: usize = 10;

/// How long a pause starts typing to select afresh.
const TYPE_AHEAD_PAUSE: Duration = Duration::from_secs(1);

pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("home", SelectFirst, context),
        KeyBinding::new("end", SelectLast, context),
        KeyBinding::new("pagedown", SelectPageDown, context),
        KeyBinding::new("pageup", SelectPageUp, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new("right", OpenSelected, context),
        KeyBinding::new("backspace", GoUp, context),
        KeyBinding::new("left", GoUp, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-up", GoUp, context),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-up", GoUp, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-h", GoHome, context),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-home", GoHome, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-enter", ChooseFolderAction, context),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-enter", ChooseFolderAction, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-.", ToggleHidden, context),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-h", ToggleHidden, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-n", NewFolderAction, context),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-n", NewFolderAction, context),
    ]);
}

/// The index `step` folders on from `selected` among `count`, stopping at the
/// ends; with nothing selected, stepping forward starts at the first and
/// back at the last.
pub fn step_selection(selected: Option<usize>, count: usize, step: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let last = count as isize - 1;
    let next = match selected {
        None if step < 0 => last,
        None => 0,
        Some(ix) => (ix as isize + step).clamp(0, last),
    };
    Some(next as usize)
}

/// The first of `folders` whose name starts with `typed`, ignoring case.
pub fn type_ahead_match(folders: &[PathBuf], typed: &str) -> Option<usize> {
    let typed = typed.to_lowercase();
    folders.iter().position(|folder| {
        folder
            .file_name()
            .is_some_and(|name| name.to_string_lossy().to_lowercase().starts_with(&typed))
    })
}

/// Why `name` can't be a new folder in `dir`, if it can't.
pub fn new_folder_problem(dir: &Path, name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        Some("Give the folder a name".into())
    } else if name.contains(['/', '\\']) {
        Some("A name can't contain a slash".into())
    } else if name == "." || name == ".." {
        Some("A name can't be . or ..".into())
    } else if dir.join(name).symlink_metadata().is_ok() {
        Some(format!("{name} is already here"))
    } else {
        None
    }
}

/// Creates the folder `name` in `dir`, returning it.
pub fn create_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if let Some(problem) = new_folder_problem(dir, name) {
        return Err(problem);
    }
    let folder = dir.join(name.trim());
    std::fs::create_dir(&folder)
        .map_err(|err| format!("Couldn't create {}: {err}", folder.display()))?;
    Ok(folder)
}

/// The row for naming a new folder, while it is open.
struct NewFolder {
    input: Entity<InputState>,
    /// Why the folder couldn't be created, when it couldn't.
    error: Option<SharedString>,
    _subscription: Subscription,
}

/// Emitted with the folder chosen.
pub struct ChooseFolder(pub PathBuf);

/// Emitted when the browser is left without choosing.
pub struct CancelFolder;

/// The folders in `dir`, sorted by name ignoring case, leaving out those whose
/// names start with a dot unless `show_hidden`.
pub fn list_folders(dir: &Path, show_hidden: bool) -> std::io::Result<Vec<PathBuf>> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            // Following links, so a link to a folder counts as one.
            std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
        })
        .map(|entry| entry.path())
        .filter(|path| {
            show_hidden
                || !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    folders.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    Ok(folders)
}

/// Each folder along `dir`'s path, from the top of the file system down to
/// `dir`, with the name it shows as in the breadcrumb.
pub fn crumbs(dir: &Path) -> Vec<(String, PathBuf)> {
    dir.ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            (name, path.to_path_buf())
        })
        .collect()
}

pub struct FolderBrowser {
    title: SharedString,
    dir: PathBuf,
    show_hidden: bool,
    /// The current folder's folders, or why they couldn't be read.
    folders: Result<Vec<PathBuf>, SharedString>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    new_folder: Option<NewFolder>,
    /// The list of folders, which has focus when the browser opens.
    focus_handle: FocusHandle,
    /// What has been typed to select a folder by name, and when last.
    typed: String,
    typed_at: Option<Instant>,
}

impl Focusable for FolderBrowser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ChooseFolder> for FolderBrowser {}
impl EventEmitter<CancelFolder> for FolderBrowser {}

impl FolderBrowser {
    /// A browser titled `title`, starting in `dir`, or the home folder when
    /// `dir` isn't a folder.
    pub fn new(title: impl Into<SharedString>, dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let dir = if dir.is_dir() { dir } else { home() };
        let mut this = Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            typed: String::new(),
            typed_at: None,
            title: title.into(),
            dir: PathBuf::new(),
            show_hidden: false,
            folders: Ok(Vec::new()),
            selected: None,
            scroll: ScrollHandle::new(),
            new_folder: None,
        };
        this.load(dir);
        this
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[cfg(test)]
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What "Choose" chooses: the selected folder, or the current one.
    pub fn choice(&self) -> PathBuf {
        self.selected
            .and_then(|ix| self.folders.as_ref().ok()?.get(ix).cloned())
            .unwrap_or_else(|| self.dir.clone())
    }

    fn load(&mut self, dir: PathBuf) {
        self.folders = list_folders(&dir, self.show_hidden)
            .map_err(|err| format!("Couldn't read {}: {err}", dir.display()).into());
        self.dir = dir;
        self.selected = None;
        self.new_folder = None;
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    /// Opens the row for naming a new folder in the current one, its name
    /// field focused.
    pub fn open_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.create_new_folder(window, cx),
                InputEvent::Change => {
                    if let Some(row) = &mut this.new_folder {
                        row.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.new_folder = Some(NewFolder {
            input,
            error: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_new_folder_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = &self.new_folder {
            row.input.update(cx, |input, cx| {
                input.set_value(name.to_string(), window, cx)
            });
        }
    }

    #[cfg(test)]
    pub fn new_folder_open(&self) -> bool {
        self.new_folder.is_some()
    }

    /// <Escape>: closes the new folder row, or, without it open, leaves the
    /// browser without choosing.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_folder.is_some() {
            self.close_new_folder(window, cx);
        } else {
            cx.emit(CancelFolder);
        }
    }

    /// Closes the new folder row, handing the keyboard back to the list.
    fn close_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_folder = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Creates the folder named in the row, then selects it; or says why it
    /// couldn't be, keeping the row open.
    pub fn create_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = &mut self.new_folder else {
            return;
        };
        let name = row.input.read(cx).value().to_string();
        match create_folder(&self.dir, &name) {
            Err(err) => {
                row.error = Some(err.into());
                cx.notify();
            }
            Ok(folder) => {
                self.load(self.dir.clone());
                self.focus_handle.focus(window, cx);
                self.selected = self
                    .folders
                    .as_ref()
                    .ok()
                    .and_then(|folders| folders.iter().position(|path| *path == folder));
                if let Some(ix) = self.selected {
                    self.scroll.scroll_to_item(ix);
                }
                cx.notify();
            }
        }
    }

    /// Goes to `dir`.
    pub fn go(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.load(dir);
        cx.notify();
    }

    /// Goes up to the folder holding this one, selecting the folder left.
    fn up(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let left = self.dir.clone();
        self.go(parent, cx);
        self.select_path(&left);
    }

    fn set_show_hidden(&mut self, show: bool, cx: &mut Context<Self>) {
        let selected = self
            .selected
            .and_then(|ix| self.folders.as_ref().ok()?.get(ix).cloned());
        self.show_hidden = show;
        self.load(self.dir.clone());
        if let Some(selected) = selected {
            self.select_path(&selected);
        }
        cx.notify();
    }

    /// Selects `path` if it is listed, scrolling it into view.
    fn select_path(&mut self, path: &Path) {
        let ix = self
            .folders
            .as_ref()
            .ok()
            .and_then(|folders| folders.iter().position(|folder| folder == path));
        if ix.is_some() {
            self.select(ix);
        }
    }

    /// Selects the folder at `ix`, scrolling it into view.
    fn select(&mut self, ix: Option<usize>) {
        self.selected = ix;
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix);
        }
    }

    fn folder_count(&self) -> usize {
        self.folders.as_ref().map_or(0, Vec::len)
    }

    /// Selects the first folder, or with `last` the last one.
    fn select_end(&mut self, last: bool, cx: &mut Context<Self>) {
        let count = self.folder_count();
        if count > 0 {
            self.select(Some(if last { count - 1 } else { 0 }));
            cx.notify();
        }
    }

    /// Moves the selection `step` folders, stopping at the ends.
    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        self.select(step_selection(self.selected, self.folder_count(), step));
        cx.notify();
    }

    /// Goes into the selected folder, or, with none selected, chooses the
    /// current one.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected {
            Some(ix) => self.click_folder(ix, 2, cx),
            None => cx.emit(ChooseFolder(self.dir.clone())),
        }
    }

    /// Typing selects the first folder whose name starts with what has been
    /// typed, building up while the keys come quickly.
    fn type_ahead(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if self.new_folder.is_some()
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.function
        {
            return;
        }
        let Some(typed) = keystroke
            .key_char
            .as_deref()
            .filter(|typed| !typed.is_empty() && typed.chars().all(|c| !c.is_control()))
        else {
            return;
        };
        let fresh = self
            .typed_at
            .is_none_or(|at| at.elapsed() >= TYPE_AHEAD_PAUSE);
        if fresh {
            self.typed.clear();
        }
        // A space only continues a name, rather than starting one.
        if self.typed.is_empty() && typed.trim().is_empty() {
            return;
        }
        self.typed.push_str(typed);
        self.typed_at = Some(Instant::now());
        if let Some(ix) = self
            .folders
            .as_ref()
            .ok()
            .and_then(|folders| type_ahead_match(folders, &self.typed))
        {
            self.select(Some(ix));
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A click on the folder at `ix`: the first selects it, the second goes
    /// into it.
    pub fn click_folder(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
        let Some(folder) = self.folders.as_ref().ok().and_then(|f| f.get(ix)).cloned() else {
            return;
        };
        if clicks >= 2 {
            self.go(folder, cx);
        } else {
            self.selected = Some(ix);
            cx.notify();
        }
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let crumbs = crumbs(&self.dir);
        let last = crumbs.len().saturating_sub(1);
        h_flex()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("folder-up")
                    .ghost()
                    .small()
                    .icon(IconName::CornerLeftUp)
                    .tooltip_with_action(
                        "Go up to the folder holding this one",
                        &GoUp,
                        Some(CONTEXT),
                    )
                    .disabled(self.dir.parent().is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.up(cx))),
            )
            .child(
                Button::new("folder-home")
                    .ghost()
                    .small()
                    .icon(IconName::House)
                    .tooltip_with_action("Go to your home folder", &GoHome, Some(CONTEXT))
                    .on_click(cx.listener(|this, _, _, cx| this.go(home(), cx))),
            )
            .child(
                Button::new("folder-new")
                    .ghost()
                    .small()
                    .icon(IconName::FolderPlus)
                    .label("New Folder")
                    .tooltip_with_action(
                        "Create a folder in this one",
                        &NewFolderAction,
                        Some(CONTEXT),
                    )
                    .disabled(self.folders.is_err())
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_folder(window, cx))),
            )
            .child(
                h_flex()
                    .id("folder-crumbs")
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .gap_0p5()
                    .children(crumbs.into_iter().enumerate().map(|(ix, (name, path))| {
                        // The top of the file system is already a slash.
                        let separator =
                            (ix >= 2).then(|| div().text_color(theme.muted_foreground).child("/"));
                        let crumb = Button::new(("folder-crumb", ix))
                            .ghost()
                            .xsmall()
                            .label(name)
                            .selected(ix == last)
                            .on_click(cx.listener(move |this, _, _, cx| this.go(path.clone(), cx)));
                        h_flex().flex_none().children(separator).child(crumb)
                    })),
            )
            .child(
                checkbox("folder-show-hidden", "Show hidden")
                    .checked(self.show_hidden)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_show_hidden(*checked, cx)
                    })),
            )
    }

    fn render_folders(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let folders = match &self.folders {
            Err(err) => {
                return div()
                    .flex_1()
                    .p_4()
                    .text_color(theme.danger)
                    .child(err.clone())
                    .into_any_element();
            }
            Ok(folders) if folders.is_empty() && self.new_folder.is_none() => {
                return div()
                    .flex_1()
                    .p_4()
                    .text_color(theme.muted_foreground)
                    .child("No folders here")
                    .into_any_element();
            }
            Ok(folders) => folders,
        };
        let new_folder = self.new_folder.as_ref().map(|row| {
            let name = row.input.read(cx).value();
            let problem = row.error.clone().map(|err| err.to_string()).or_else(|| {
                (!name.is_empty())
                    .then(|| new_folder_problem(&self.dir, &name))
                    .flatten()
            });
            let can_create = new_folder_problem(&self.dir, &name).is_none();
            let row_element = v_flex()
                .id("folder-new-row")
                .gap_1()
                .px_3()
                .py_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::FolderClosed).text_color(theme.muted_foreground))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&row.input).small()),
                        )
                        .child(
                            Button::new("folder-new-create")
                                .primary()
                                .small()
                                .label("Create")
                                .disabled(!can_create)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_new_folder(window, cx)
                                })),
                        )
                        .child(
                            Button::new("folder-new-cancel")
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .tooltip("Don't create a folder")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_new_folder(window, cx)
                                })),
                        ),
                )
                .when_some(problem, |row, problem| {
                    row.child(
                        div()
                            .pl_6()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(problem),
                    )
                });
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row_element)
        });
        let rows = folders.iter().enumerate().map(|(ix, folder)| {
            let name = folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let selected = self.selected == Some(ix);
            let row = h_flex()
                .id(("folder-row", ix))
                .gap_2()
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(selected, |row| {
                    row.bg(theme.list_active)
                        .text_color(theme.accent_foreground)
                })
                .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.click_folder(ix, event.click_count(), cx)
                }))
                .child(Icon::new(IconName::FolderClosed).text_color(theme.muted_foreground))
                .child(div().min_w_0().truncate().child(name));
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row)
        });
        // The rows are the list's own children, so a folder's index is the
        // item to scroll to.
        let list = v_flex()
            .id("folder-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_2()
            .children(new_folder)
            .children(rows);
        div()
            .flex_1()
            .min_h_0()
            .child(scroll_column::with_scroll_column(
                "folder-list",
                &self.scroll,
                list,
                true,
                None,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for FolderBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted) = (cx.theme().border, cx.theme().muted_foreground);
        let choice = self.choice();
        let browser = v_flex()
            .id("folder-browser")
            .key_context(CONTEXT)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.type_ahead(event, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_end(false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select_end(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| this.step(PAGE as isize, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| this.step(-(PAGE as isize), cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.up(cx)))
            .on_action(cx.listener(|this, _: &GoHome, _, cx| this.go(home(), cx)))
            .on_action(cx.listener(|this, _: &ChooseFolderAction, _, cx| {
                cx.emit(ChooseFolder(this.choice()))
            }))
            .on_action(cx.listener(|this, _: &ToggleHidden, _, cx| {
                this.set_show_hidden(!this.show_hidden, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFolderAction, window, cx| {
                if this.folders.is_ok() {
                    this.open_new_folder(window, cx)
                }
            }))
            // The window's Esc, which outranks any other, closes the new
            // folder row first.
            .on_action(cx.listener(|this, _: &FocusChat, window, cx| this.escape(window, cx)))
            // The name field's own Escape, passed up once it has nothing to
            // cancel itself.
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
            .size_full()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .font_semibold()
                    .child(self.title.clone()),
            )
            .child(self.render_bar(cx))
            .child(
                v_flex()
                    .id("folder-list-area")
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.focus_handle)
                    .child(self.render_folders(cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(muted)
                            .child(choice.display().to_string()),
                    )
                    .child(
                        Button::new("folder-cancel")
                            .label("Cancel")
                            .tooltip_with_action("Leave without choosing", &Escape, None)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CancelFolder))),
                    )
                    .child(
                        Button::new("folder-choose")
                            .primary()
                            .label("Choose")
                            .tooltip_with_action(
                                "Choose this folder",
                                &ChooseFolderAction,
                                Some(CONTEXT),
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| cx.emit(ChooseFolder(this.choice()))),
                            ),
                    ),
            );
        // Lets UI tests find the browser; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(browser)
    }
}

/// The user's home folder, or the top of the file system without one.
pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Focusable as _, TestAppContext};

    use super::{
        ChooseFolder, FolderBrowser, create_folder, crumbs, list_folders, new_folder_problem,
        step_selection, type_ahead_match,
    };

    #[test]
    fn selection_steps_stop_at_the_ends() {
        assert_eq!(step_selection(None, 0, 1), None);
        assert_eq!(step_selection(None, 5, 1), Some(0));
        assert_eq!(step_selection(None, 5, -1), Some(4));
        assert_eq!(step_selection(Some(3), 5, 10), Some(4));
        assert_eq!(step_selection(Some(3), 5, -10), Some(0));
        let folders = ["Alpha", "beta", "Bravo"].map(PathBuf::from);
        assert_eq!(type_ahead_match(&folders, "b"), Some(1));
        assert_eq!(type_ahead_match(&folders, "BR"), Some(2));
        assert_eq!(type_ahead_match(&folders, "z"), None);
    }

    /// The browser is driven from the keyboard: arrows, Home, End, and the page
    /// keys move the selection; typing selects by name; Enter and Right go
    /// in; Backspace goes up with the folder left selected; Ctrl+H shows
    /// hidden folders; Ctrl+Shift+N opens the new folder row; Ctrl+Enter
    /// chooses; and Escape cancels.
    #[gpui_kit::test]
    async fn browses_from_the_keyboard(cx: &mut TestAppContext) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/folder-browser-keys-test");
        std::fs::remove_dir_all(&dir).ok();
        for folder in ["alpha", "beta/inner", "bravo", "gamma", ".hidden"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for n in 0..20 {
            std::fs::create_dir_all(dir.join(format!("z{n:02}"))).unwrap();
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::main_window::bind_keys(cx);
        });
        let mut browser = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| FolderBrowser::new("Pick", dir.clone(), cx));
            view.read(cx).focus_handle(cx).focus(window, cx);
            browser = Some(view.clone());
            Root::new(view, window, cx)
        });
        let browser = browser.unwrap();
        let handle = window.into();
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        let _subscriptions = cx.update(|cx| {
            let (chosen, cancelled) = (chosen.clone(), cancelled.clone());
            (
                cx.subscribe(&browser, move |_, ChooseFolder(folder), _| {
                    chosen.borrow_mut().push(folder.clone())
                }),
                cx.subscribe(&browser, move |_, _: &super::CancelFolder, _| {
                    cancelled.set(true)
                }),
            )
        });
        let press = |keys: &[&str], cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                for key in keys {
                    window.press(key, cx);
                }
            })
            .unwrap();
            cx.run_until_parked();
        };
        let selected = |cx: &mut TestAppContext| browser.read_with(cx, |b, _| b.selected());
        let name = |cx: &mut TestAppContext| {
            browser.read_with(cx, |b, _| {
                b.choice()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        };

        // alpha, beta, bravo, gamma, z00..z19
        press(&["down"], cx);
        assert_eq!(selected(cx), Some(0));
        press(&["down", "down", "up"], cx);
        assert_eq!(selected(cx), Some(1));
        press(&["end"], cx);
        assert_eq!(selected(cx), Some(23));
        press(&["home"], cx);
        assert_eq!(selected(cx), Some(0));
        press(&["pagedown"], cx);
        assert_eq!(selected(cx), Some(10));
        press(&["pageup", "pageup"], cx);
        assert_eq!(selected(cx), Some(0));

        press(&["g"], cx);
        assert_eq!(name(cx), "gamma");
        std::thread::sleep(Duration::from_millis(1100));
        press(&["b", "r"], cx);
        assert_eq!(name(cx), "bravo");
        std::thread::sleep(Duration::from_millis(1100));
        press(&["b"], cx);
        assert_eq!(name(cx), "beta");

        press(&["enter"], cx);
        assert_eq!(
            browser.read_with(cx, |b, _| b.dir().to_path_buf()),
            dir.join("beta")
        );
        press(&["backspace"], cx);
        assert_eq!(browser.read_with(cx, |b, _| b.dir().to_path_buf()), dir);
        assert_eq!(name(cx), "beta", "going up didn't select the folder left");
        press(&["right"], cx);
        assert_eq!(
            browser.read_with(cx, |b, _| b.dir().to_path_buf()),
            dir.join("beta")
        );
        press(&["left"], cx);
        assert_eq!(name(cx), "beta");

        press(&["ctrl-h"], cx);
        assert!(browser.read_with(cx, |b, _| b.show_hidden()));
        assert_eq!(
            name(cx),
            "beta",
            "showing hidden folders lost the selection"
        );
        press(&["ctrl-h"], cx);
        assert!(!browser.read_with(cx, |b, _| b.show_hidden()));

        press(&["ctrl-shift-n"], cx);
        assert!(browser.read_with(cx, |b, _| b.new_folder_open()));
        press(&["escape"], cx);
        assert!(!browser.read_with(cx, |b, _| b.new_folder_open()));
        assert!(
            !cancelled.get(),
            "Escape in the row also cancelled the browser"
        );

        press(&["ctrl-enter"], cx);
        assert_eq!(*chosen.borrow(), [dir.join("beta")]);
        press(&["escape"], cx);
        assert!(cancelled.get(), "Escape didn't cancel");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A new folder needs a name that isn't taken and doesn't leave the
    /// folder; one that is fine is created.
    #[test]
    fn new_folders_are_checked_then_created() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/folder-browser-new-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("taken")).unwrap();
        for bad in ["", " ", "a/b", "a\\b", ".", "..", "taken"] {
            assert!(
                new_folder_problem(&dir, bad).is_some(),
                "{bad:?} was allowed"
            );
        }
        assert_eq!(create_folder(&dir, " fresh "), Ok(dir.join("fresh")));
        assert!(dir.join("fresh").is_dir());
        assert!(create_folder(&dir, "fresh").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only folders are listed, sorted ignoring case, with hidden ones left
    /// out unless asked for.
    #[test]
    fn lists_folders_sorted_leaving_out_hidden_ones() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/folder-browser-test");
        std::fs::remove_dir_all(&dir).ok();
        for folder in ["beta", "Alpha", ".hidden", "gamma"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        std::fs::write(dir.join("file.txt"), "").unwrap();
        let names = |show_hidden| {
            list_folders(&dir, show_hidden)
                .unwrap()
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(false), ["Alpha", "beta", "gamma"]);
        assert_eq!(names(true), [".hidden", "Alpha", "beta", "gamma"]);
        assert!(list_folders(&dir.join("missing"), false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn crumbs_run_from_the_top_down() {
        assert_eq!(
            crumbs(Path::new("/home/me/code")),
            [
                ("/".to_string(), PathBuf::from("/")),
                ("home".to_string(), PathBuf::from("/home")),
                ("me".to_string(), PathBuf::from("/home/me")),
                ("code".to_string(), PathBuf::from("/home/me/code")),
            ]
        );
    }
}
