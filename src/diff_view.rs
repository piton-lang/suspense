//! The diff view: a file's uncommitted changes, the file as of the last
//! commit against the file on disk, beside the task view in place of the
//! editor. It lays them out side by side or unified, switchable and
//! remembered; marks changed lines with a tint, a +/− marker, and both line
//! numbers, and the words that changed within a line with a stronger tint;
//! collapses unchanged lines far from any change into a row that expands them;
//! and steps from change to change with F7 and Shift+F7. It follows the file as
//! it changes on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, ColorName, Disableable as _, Icon, Selectable as _, Sizable as _,
    StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::diff::{FileDiff, Kind, Line, SideRow, UnifiedRow, change_starts};
use crate::project_directory::ProjectDirectory;

actions!(diff_view, [NextChange, PreviousChange]);

const CONTEXT: &str = "DiffView";

/// Every row is this tall, so long files scroll quickly.
const ROW_HEIGHT: Pixels = px(20.);

/// The width of each line number column.
const NUMBER_WIDTH: Pixels = px(44.);

/// How often the file is read again, to follow edits.
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f7", NextChange, Some(CONTEXT)),
        KeyBinding::new("shift-f7", PreviousChange, Some(CONTEXT)),
    ]);
}

/// Emitted when the diff is closed.
pub struct CloseDiff;

/// Emitted to open the file in the editor instead.
pub struct OpenInEditor(pub PathBuf);

/// How the changes are laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    SideBySide,
    Unified,
}

/// What was read, or why it couldn't be shown.
#[derive(Clone, Debug, PartialEq)]
enum Content {
    Loading,
    Diff { old: String, new: String },
    Binary,
    Failed(SharedString),
}

pub struct DiffView {
    path: PathBuf,
    title: SharedString,
    layout: Layout,
    content: Content,
    diff: FileDiff,
    /// Collapsed runs of unchanged lines that were expanded, by first line.
    expanded: HashSet<usize>,
    /// The change stepped to last, as an index into the change starts.
    current_change: Option<usize>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _refresh: Task<()>,
}

impl EventEmitter<CloseDiff> for DiffView {}
impl EventEmitter<OpenInEditor> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DiffView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let title = match ProjectDirectory::get(cx) {
            Some(root) => path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            None => path.display().to_string(),
        };
        let refresh = cx.spawn({
            let path = path.clone();
            async move |this, cx| {
                loop {
                    let content = cx
                        .background_spawn({
                            let path = path.clone();
                            async move { read(&path) }
                        })
                        .await;
                    let updated = this.update(cx, |this, cx| this.show(content, cx));
                    if updated.is_err() {
                        break;
                    }
                    cx.background_executor().timer(REFRESH_INTERVAL).await;
                }
            }
        });
        Self {
            path,
            title: title.into(),
            layout: layout_preference::load(),
            content: Content::Loading,
            diff: FileDiff::default(),
            expanded: HashSet::new(),
            current_change: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _refresh: refresh,
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn diff(&self) -> &FileDiff {
        &self.diff
    }

    #[cfg(test)]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Shows what was read, unless it is what is already shown.
    fn show(&mut self, content: Content, cx: &mut Context<Self>) {
        if content == self.content {
            return;
        }
        if let Content::Diff { old, new } = &content {
            self.diff = FileDiff::compute(old, new);
        }
        self.content = content;
        cx.notify();
    }

    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        if self.layout != layout {
            self.layout = layout;
            self.current_change = None;
            layout_preference::save(layout);
            cx.notify();
        }
    }

    fn expand(&mut self, start: usize, cx: &mut Context<Self>) {
        self.expanded.insert(start);
        self.current_change = None;
        cx.notify();
    }

    /// Whether each row, as laid out now, is a change.
    fn row_changes(&self) -> Vec<bool> {
        let changed =
            |ix: Option<usize>| ix.is_some_and(|ix| self.diff.lines[ix].kind != Kind::Unchanged);
        match self.layout {
            Layout::Unified => self
                .diff
                .unified(&self.expanded)
                .iter()
                .map(|row| matches!(row, UnifiedRow::Line(ix) if changed(Some(*ix))))
                .collect(),
            Layout::SideBySide => self
                .diff
                .side_by_side(&self.expanded)
                .iter()
                .map(|row| matches!(row, SideRow::Pair { old, new } if changed(*old) || changed(*new)))
                .collect(),
        }
    }

    /// The row the change stepped to starts at.
    fn current_change_row(&self) -> Option<usize> {
        let starts = change_starts(self.row_changes().into_iter());
        self.current_change.and_then(|ix| starts.get(ix).copied())
    }

    /// Scrolls to the next change, or with `step` of -1 the previous one,
    /// stopping at the first and last.
    pub fn step_change(&mut self, step: isize, cx: &mut Context<Self>) {
        let starts = change_starts(self.row_changes().into_iter());
        if starts.is_empty() {
            return;
        }
        let next = match self.current_change {
            None if step < 0 => starts.len() - 1,
            None => 0,
            Some(ix) => (ix as isize + step).clamp(0, starts.len() as isize - 1) as usize,
        };
        self.current_change = Some(next);
        self.scroll
            .scroll_to_item(starts[next], ScrollStrategy::Center);
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (added, removed) = (tone(ColorName::Green, cx), tone(ColorName::Red, cx));
        let has_changes = self.diff.has_changes();
        let layout_button =
            |id: &'static str, icon: IconName, label: &'static str, layout: Layout| {
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .label(label)
                    .selected(self.layout == layout)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_layout(layout, cx)))
            };
        h_flex()
            .flex_none()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::FileDiff)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_medium()
                    .child(self.title.clone()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        div()
                            .text_color(added)
                            .child(format!("+{}", self.diff.insertions)),
                    )
                    .child(
                        div()
                            .text_color(removed)
                            .child(format!("−{}", self.diff.deletions)),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("diff-previous-change")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronUp)
                    .tooltip_with_action("Previous change", &PreviousChange, Some(CONTEXT))
                    .disabled(!has_changes)
                    .on_click(cx.listener(|this, _, _, cx| this.step_change(-1, cx))),
            )
            .child(
                Button::new("diff-next-change")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .tooltip_with_action("Next change", &NextChange, Some(CONTEXT))
                    .disabled(!has_changes)
                    .on_click(cx.listener(|this, _, _, cx| this.step_change(1, cx))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .p_0p5()
                    .gap_0p5()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .child(layout_button(
                        "diff-side-by-side",
                        IconName::Columns2,
                        "Side by side",
                        Layout::SideBySide,
                    ))
                    .child(layout_button(
                        "diff-unified",
                        IconName::Rows2,
                        "Unified",
                        Layout::Unified,
                    )),
            )
            .child(
                Button::new("diff-open-file")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FileText)
                    .tooltip("Open the file in the editor")
                    .on_click(
                        cx.listener(|this, _, _, cx| cx.emit(OpenInEditor(this.path.clone()))),
                    ),
            )
            .child(
                Button::new("close-diff")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Close the diff")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseDiff))),
            )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let message = |text: &str| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(text.to_string())
                .into_any_element()
        };
        match &self.content {
            Content::Loading => return message("Loading…"),
            Content::Binary => return message("Binary file; its changes can't be shown."),
            Content::Failed(error) => return message(error),
            Content::Diff { .. } if self.diff.lines.is_empty() => return message("Empty file"),
            Content::Diff { .. } => {}
        }

        let this = cx.entity().downgrade();
        let count = match self.layout {
            Layout::Unified => self.diff.unified(&self.expanded).len(),
            Layout::SideBySide => self.diff.side_by_side(&self.expanded).len(),
        };
        let list = uniform_list("diff-rows", count, move |range, _, cx| {
            let Some(this) = this.upgrade() else {
                return Vec::new();
            };
            this.update(cx, |this, cx| {
                let current = this.current_change_row();
                match this.layout {
                    Layout::Unified => {
                        let rows = this.diff.unified(&this.expanded);
                        range
                            .map(|ix| {
                                this.render_unified_row(ix, &rows[ix], current == Some(ix), cx)
                            })
                            .collect()
                    }
                    Layout::SideBySide => {
                        let rows = this.diff.side_by_side(&this.expanded);
                        range
                            .map(|ix| this.render_side_row(ix, &rows[ix], current == Some(ix), cx))
                            .collect()
                    }
                }
            })
        })
        .track_scroll(&self.scroll)
        .size_full();
        let handle = self.scroll.0.borrow().base_handle.clone();
        crate::scrollbar::with_scrollbar("diff", &handle, list, true, None, cx)
    }

    fn render_unified_row(
        &self,
        ix: usize,
        row: &UnifiedRow,
        current: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            UnifiedRow::Collapsed { start, len } => self.render_collapsed(ix, *start, *len, cx),
            UnifiedRow::Line(line_ix) => {
                let line = &self.diff.lines[*line_ix];
                row_base(("diff-row", ix), line.kind, current, cx)
                    .child(number(line.old, cx))
                    .child(number(line.new, cx))
                    .child(line_text(line, cx))
                    .into_any_element()
            }
        }
    }

    fn render_side_row(
        &self,
        ix: usize,
        row: &SideRow,
        current: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (old, new) = match row {
            SideRow::Collapsed { start, len } => {
                return self.render_collapsed(ix, *start, *len, cx);
            }
            SideRow::Pair { old, new } => (*old, *new),
        };
        let half = |line: Option<usize>, old_side: bool, cx: &mut Context<Self>| {
            let theme = cx.theme();
            match line.map(|ix| &self.diff.lines[ix]) {
                Some(line) => {
                    let number_on_side = if old_side { line.old } else { line.new };
                    row_base(
                        ("diff-half", ix * 2 + old_side as usize),
                        line.kind,
                        false,
                        cx,
                    )
                    .flex_1()
                    .min_w_0()
                    .child(number(number_on_side, cx))
                    .child(line_text(line, cx))
                    .into_any_element()
                }
                // Nothing on this side: blank filler, so the sides stay level.
                None => div()
                    .flex_1()
                    .min_w_0()
                    .h(ROW_HEIGHT)
                    .bg(theme.muted.opacity(0.5))
                    .into_any_element(),
            }
        };
        let left = half(old, true, cx);
        let right = half(new, false, cx);
        let border = cx.theme().border;
        h_flex()
            .id(("diff-row", ix))
            .w_full()
            .h(ROW_HEIGHT)
            .when(current, |row| {
                row.border_l_2().border_color(cx.theme().ring)
            })
            .child(left)
            .child(div().w_px().h_full().bg(border))
            .child(right)
            .into_any_element()
    }

    fn render_collapsed(
        &self,
        ix: usize,
        start: usize,
        len: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let label = if len == 1 {
            "Show 1 unchanged line".to_string()
        } else {
            format!("Show {len} unchanged lines")
        };
        let row = h_flex()
            .id(("diff-collapsed", ix))
            .w_full()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl(NUMBER_WIDTH)
            .text_xs()
            .text_color(theme.muted_foreground)
            .bg(theme.muted.opacity(0.6))
            .cursor_pointer()
            .hover(|row| row.text_color(theme.foreground))
            .on_click(cx.listener(move |this, _, _, cx| this.expand(start, cx)))
            .child(Icon::new(IconName::UnfoldVertical).xsmall())
            .child(label);
        // Lets UI tests find the row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }
}

/// A colour's shade, readable in the light and the dark theme.
fn tone(name: ColorName, cx: &App) -> Hsla {
    name.scale(if cx.theme().is_dark() { 400 } else { 700 })
}

/// The tint behind a line of `kind`, and the stronger one behind its changed
/// words.
fn tints(kind: Kind, cx: &App) -> Option<(Hsla, Hsla)> {
    let name = match kind {
        Kind::Unchanged => return None,
        Kind::Added => ColorName::Green,
        Kind::Removed => ColorName::Red,
    };
    let base = name.scale(500);
    let (line, word) = if cx.theme().is_dark() {
        (0.14, 0.4)
    } else {
        (0.1, 0.3)
    };
    Some((base.opacity(line), base.opacity(word)))
}

/// A row of a line: tinted by its kind, and marked when it is the change
/// stepped to.
fn row_base(id: impl Into<ElementId>, kind: Kind, current: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .w_full()
        .h(ROW_HEIGHT)
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
        .whitespace_nowrap()
        .when_some(tints(kind, cx), |row, (line, _)| row.bg(line))
        .when(current, |row| row.border_l_2().border_color(theme.ring))
}

/// A line number, or blank space where the line has none on this side.
fn number(number: Option<usize>, cx: &App) -> Div {
    div()
        .flex_none()
        .w(NUMBER_WIDTH)
        .pr_2()
        .text_right()
        .text_color(cx.theme().muted_foreground)
        .children(number.map(|number| number.to_string()))
}

/// A line's marker (+, −, or nothing) and text, its changed words tinted.
fn line_text(line: &Line, cx: &App) -> Div {
    let theme = cx.theme();
    let (marker, color) = match line.kind {
        Kind::Added => ("+", Some(tone(ColorName::Green, cx))),
        Kind::Removed => ("−", Some(tone(ColorName::Red, cx))),
        Kind::Unchanged => (" ", None),
    };
    let highlights = tints(line.kind, cx)
        .map(|(_, word)| {
            line.changed
                .iter()
                .map(|range| {
                    (
                        range.clone(),
                        HighlightStyle {
                            background_color: Some(word),
                            ..HighlightStyle::default()
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    h_flex()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .w(px(16.))
                .text_center()
                .text_color(color.unwrap_or(theme.muted_foreground))
                .child(marker),
        )
        .child(StyledText::new(line.text.clone()).with_highlights(highlights))
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = v_flex()
            .id("diff-view")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.step_change(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.step_change(-1, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_handle.focus(window, cx)),
            )
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)));
        // Lets UI tests find the view; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(view)
    }
}

/// The file as of the last commit, and as it is on disk. A file new since
/// then was empty; one no longer on disk is empty now.
fn read(path: &Path) -> Content {
    let new = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Content::Failed(format!("Could not read the file: {err}").into()),
    };
    let old = committed(path).unwrap_or_default();
    if new.contains(&0) || old.contains(&0) {
        return Content::Binary;
    }
    Content::Diff {
        old: String::from_utf8_lossy(&old).into_owned(),
        new: String::from_utf8_lossy(&new).into_owned(),
    }
}

/// The file's contents as of the last commit, or `None` if it wasn't in it.
fn committed(path: &Path) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let output = Command::new("git")
        .arg("show")
        .arg(format!("HEAD:./{name}"))
        .current_dir(dir)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// Side by side or unified, as last chosen, saved beside the other per-user
/// choices. Tests neither read nor write it.
mod layout_preference {
    use super::Layout;

    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(dirs::config_dir()?.join("suspense").join("diff-layout"))
    }

    pub fn load() -> Layout {
        #[cfg(not(test))]
        if let Some(file) = file()
            && std::fs::read_to_string(file).is_ok_and(|text| text.trim() == "unified")
        {
            return Layout::Unified;
        }
        Layout::SideBySide
    }

    /// Saves the choice; it is only a convenience, so failing to is ignored.
    pub fn save(layout: Layout) {
        #[cfg(not(test))]
        if let Some(file) = file() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            let text = match layout {
                Layout::SideBySide => "side-by-side",
                Layout::Unified => "unified",
            };
            std::fs::write(file, text).ok();
        }
        #[cfg(test)]
        let _ = layout;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{DiffView, Layout};
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// A file changed since the last commit shows its changes against the
    /// commit, side by side at first; switching to unified lays the same
    /// changes out in one column; unchanged lines far from the change
    /// collapse, and expand when clicked; F7 steps to the change.
    #[gpui_kit::test]
    async fn shows_a_files_changes_either_way(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-diff-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let file = dir.join("src/main.rs");
        let old: String = (1..=30).map(|n| format!("let line_{n} = {n};\n")).collect();
        std::fs::write(&file, &old).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(
            &file,
            old.replace("let line_15 = 15;", "let line_15 = 1500;"),
        )
        .unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
            super::bind_keys(cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let diff = cx.new(|cx| DiffView::new(file.clone(), cx));
            let focus = diff.read(cx).focus_handle.clone();
            focus.focus(window, cx);
            view = Some(diff.clone());
            Root::new(diff, window, cx)
        });
        let view = view.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |_, cx| view.read(cx).diff().has_changes())
            .await;
        view.read_with(cx, |view, _| {
            assert_eq!(view.path(), file.as_path());
            assert_eq!(view.layout(), Layout::SideBySide);
            let diff = view.diff();
            assert_eq!((diff.insertions, diff.deletions), (1, 1));
            let changed = diff
                .lines
                .iter()
                .find(|line| !line.changed.is_empty())
                .unwrap();
            assert_eq!(&changed.text[changed.changed[0].clone()], "15;");
        });
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find(("diff-collapsed", 0usize)).is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.click("diff-unified", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.layout()), Layout::Unified);

        cx.update_window(handle, |_, window, cx| window.press("f7", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.current_change), Some(0));

        cx.update_window(handle, |_, window, cx| {
            window.click(("diff-collapsed", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(
                view.expanded.contains(&0),
                "the unchanged lines did not expand"
            );
        });
        std::fs::remove_dir_all(&dir).ok();
    }
}
