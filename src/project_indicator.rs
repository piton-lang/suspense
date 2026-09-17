//! The project indicator: which project is loaded, shown as its folder's name
//! after a folder icon, or "No project" while none is, with the project's path
//! as its tooltip. Clicking it, or Ctrl+` (Cmd+` on macOS), opens a list of the
//! projects opened most recently, flush beneath it over the dimmed window,
//! with a filter to fuzzy search them and keys to move through them, ending in
//! "Open Project…", which opens a project browser.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fuzzy;
use crate::project::open_project::OpenProject;
use crate::project_directory::ProjectDirectory;
use crate::recent_projects;

actions!(project_indicator, [ToggleProjectList]);

/// Ctrl+` (Cmd+` on macOS) opens the list, or closes it.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-`", ToggleProjectList, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-`", ToggleProjectList, None),
    ]);
}

/// The widest the project's name gets before it is cut off.
const MAX_NAME_WIDTH: Pixels = px(320.);

/// The list is at least this wide, and as wide as the indicator when wider.
const LIST_WIDTH: Pixels = px(360.);

/// A folder's name, or its whole path when it has none.
fn folder_name(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// Emitted, as [`OpenProject`], when "Open Project…" is picked from the list.
impl EventEmitter<OpenProject> for ProjectIndicator {}

/// A row of the list.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Recent(PathBuf),
    OpenProject,
}

/// The list while it is open.
struct List {
    filter: Entity<InputState>,
    /// The highlighted row, among those the filter leaves.
    highlight: usize,
    _subscription: Subscription,
}

pub struct ProjectIndicator {
    list: Option<List>,
    /// Where the indicator was last laid out, for the list to sit flush
    /// beneath it.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subscription: Subscription,
}

impl ProjectIndicator {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            list: None,
            bounds: Rc::default(),
            // Always the project open now.
            _subscription: cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify()),
        }
    }

    /// Whether the list of recent projects is open.
    pub fn is_open(&self) -> bool {
        self.list.is_some()
    }

    /// Opens the list, with an empty filter taking the keyboard.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.list.is_some() {
            return;
        }
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter projects"));
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(list) = &mut this.list
            {
                list.highlight = 0;
                cx.notify();
            }
        });
        filter.read(cx).focus_handle(cx).focus(window, cx);
        self.list = Some(List {
            filter,
            highlight: 0,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Opens the list, or closes it when it is open.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.list.is_some() {
            self.close(cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Closes the list, if it is open.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.list.take().is_some() {
            cx.notify();
        }
    }

    /// The rows the filter leaves: the recent projects matching it, best
    /// match first, or all of them, newest first, with no filter; then
    /// "Open Project…", always.
    fn items(&self, cx: &App) -> Vec<Item> {
        let query = self
            .list
            .as_ref()
            .map(|list| list.filter.read(cx).value().to_string())
            .unwrap_or_default();
        let recent = recent_projects::get(cx).openable();
        let mut items: Vec<Item> = if query.trim().is_empty() {
            recent.into_iter().map(Item::Recent).collect()
        } else {
            let mut scored: Vec<(i32, usize, PathBuf)> = recent
                .into_iter()
                .enumerate()
                .filter_map(|(ix, dir)| {
                    let name = folder_name(&dir);
                    let path = dir.display().to_string();
                    let score = [
                        fuzzy::fuzzy_match(&query, &name, 0).map(|found| found.score),
                        fuzzy::fuzzy_match(&query, &path, 0).map(|found| found.score / 2),
                    ]
                    .into_iter()
                    .flatten()
                    .max()?;
                    Some((score, ix, dir))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            scored
                .into_iter()
                .map(|(_, _, dir)| Item::Recent(dir))
                .collect()
        };
        items.push(Item::OpenProject);
        items
    }

    /// Moves the highlight `step` rows, wrapping around.
    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items(cx).len() as isize;
        if let Some(list) = &mut self.list {
            list.highlight = (list.highlight as isize + step).rem_euclid(count.max(1)) as usize;
            cx.notify();
        }
    }

    /// Picks the row at `ix`: opens that project, or the project browser.
    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items(cx).into_iter().nth(ix) else {
            return;
        };
        self.close(cx);
        match item {
            Item::Recent(dir) => {
                if ProjectDirectory::get(cx).as_deref() != Some(dir.as_path()) {
                    ProjectDirectory::set(dir, cx);
                }
            }
            Item::OpenProject => cx.emit(OpenProject),
        }
    }

    /// The indicator's contents: the folder icon and the project's name.
    fn label_contents(label: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let project = ProjectDirectory::get(cx);
        let label = label.child(
            Icon::new(IconName::FolderClosed)
                .small()
                .text_color(theme.muted_foreground),
        );
        match project.as_deref().map(folder_name) {
            Some(name) => label.child(div().min_w_0().truncate().font_medium().child(name)),
            None => label.child(div().text_color(theme.muted_foreground).child("No project")),
        }
    }

    /// The list, over the dimmed window, flush beneath the indicator, which is
    /// drawn again above the dimming so it stays as it was.
    fn render_list(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let list_state = self.list.as_ref()?;
        let bounds = self.bounds.get()?;
        let theme = cx.theme();
        let palette = crate::theme::palette(cx);
        let block = crate::theme::color(palette.well);
        let line = crate::theme::color(palette.line);
        let edge = theme.border;
        let current = ProjectDirectory::get(cx);
        let items = self.items(cx);
        let highlight = list_state.highlight.min(items.len().saturating_sub(1));

        let rows = items.iter().enumerate().map(|(ix, item)| {
            let id: ElementId = match item {
                Item::Recent(_) => ("recent-project", ix).into(),
                Item::OpenProject => "open-project-from-recent".into(),
            };
            let row = h_flex()
                .id(id)
                .gap_2()
                .px_3()
                .py_1p5()
                .w_full()
                .cursor_pointer()
                .when(ix == highlight, |row| row.bg(theme.list_active))
                .hover(|row| row.bg(theme.list_hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)));
            match item {
                Item::Recent(dir) => {
                    let is_current = current.as_deref() == Some(dir.as_path());
                    let row = row
                        .child(
                            Icon::new(if is_current {
                                IconName::Check
                            } else {
                                IconName::FolderClosed
                            })
                            .small()
                            .text_color(theme.muted_foreground),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().truncate().font_medium().child(folder_name(dir)))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(dir.display().to_string()),
                                ),
                        );
                    // Lets UI tests find the row; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(row).into_any_element()
                }
                Item::OpenProject => {
                    let row = row
                        .border_t_1()
                        .border_color(line)
                        .child(
                            Icon::new(IconName::FolderOpen)
                                .small()
                                .text_color(theme.muted_foreground),
                        )
                        .child("Open Project…");
                    gpui_kit::TestSupportExt::test_support(row).into_any_element()
                }
            }
        });

        let filter = h_flex()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(line)
            .child(
                Icon::new(IconName::Search)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .child(Input::new(&list_state.filter).appearance(false).small()),
            );

        // Up and Down move the highlight, Enter picks it, and Escape closes the
        // list, before the filter takes them for itself.
        let list = v_flex()
            .id("recent-projects")
            .absolute()
            .left(bounds.left())
            .top(bounds.bottom())
            .w(bounds.size.width.max(LIST_WIDTH))
            .bg(block)
            .border_l_1()
            .border_r_1()
            .border_b_1()
            .border_color(edge)
            .shadow_md()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                match keystroke.key.as_str() {
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    "enter" => {
                        let highlight = this.list.as_ref().map_or(0, |list| list.highlight);
                        this.pick(highlight, cx);
                    }
                    "escape" => this.close(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(filter)
            .children(rows);

        // The indicator again, above the dimming, where it is.
        let cap = h_flex()
            .id("project-indicator-open")
            .absolute()
            .left(bounds.left())
            .top(bounds.top())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .items_center()
            .gap_1p5()
            .px_3()
            .bg(block)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.close(cx)));
        let cap = Self::label_contents(cap, cx);

        let viewport = window.viewport_size();
        let overlay = div()
            .id("project-indicator-dim")
            .w(viewport.width)
            .h(viewport.height)
            .bg(crate::theme::dimming(cx))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close(cx)),
            )
            .child(cap)
            // Lets UI tests find the list; inert in normal builds.
            .child(gpui_kit::TestSupportExt::test_support(list));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(overlay)).with_priority(2))
    }
}

impl Render for ProjectIndicator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let project = ProjectDirectory::get(cx);
        // The whole block is what is clicked: it fills the indicator, square,
        // with nothing around it.
        let label = h_flex()
            .id("ribbon-project-name")
            .size_full()
            .min_w_0()
            .max_w(MAX_NAME_WIDTH)
            .items_center()
            .gap_1p5()
            .px_3()
            .cursor_pointer()
            .hover(|label| label.bg(theme.list_hover))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
            .when_some(project, |label, dir| {
                let path = SharedString::from(dir.display().to_string());
                label.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
            });
        let label = Self::label_contents(label, cx);
        // The same background as a text input's.
        let block = crate::theme::color(crate::theme::palette(cx).well);
        let list = self.render_list(window, cx);
        let bounds = self.bounds.clone();
        // A solid block, the full height of wherever it sits, on a text
        // input's background. Lets UI tests find the name; inert in normal builds.
        div()
            .flex_none()
            .h_full()
            .bg(block)
            .on_prepaint(move |laid_out, _, _| bounds.set(Some(laid_out)))
            .child(gpui_kit::TestSupportExt::test_support(label))
            .children(list)
    }
}
