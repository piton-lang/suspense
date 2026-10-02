//! The diff view: a file's uncommitted changes, the file as of the last
//! commit against the file on disk, in an inset panel. It lays them out side by side or unified, switchable and
//! remembered; marks changed lines with a tint, a +/− marker, and both line
//! numbers, and the words that changed within a line with a stronger tint;
//! collapses unchanged lines far from any change into a row that expands them;
//! and steps from change to change with F7 and Shift+F7. It follows the file as
//! it changes on disk.
//!
//! It can also merge: the file on disk against an editor's unsaved text, each
//! change taken from either side, the result handed back to the editor.

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

use crate::diff::{
    FileDiff, Kind, Line, MergeHunk, MergePart, SideRow, UnifiedRow, change_starts, merge_parts,
};
use crate::project_directory::ProjectDirectory;
use crate::task_snapshot;

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

/// Emitted when a merge is applied, with the merged text.
pub struct ApplyMerge(pub String);

/// Emitted when a merge is cancelled.
pub struct CancelMerge;

/// A merge of the file on disk, on the left, and an editor's unsaved text,
/// on the right.
struct Merge {
    mine: String,
    /// The changes taken from disk, by what each side has, so a change keeps
    /// its choice as the file on disk changes around it; every other change
    /// keeps the editor's lines.
    use_disk: HashSet<MergeHunk>,
}

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
    /// While merging, what is merged and how.
    merge: Option<Merge>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _refresh: Task<()>,
}

impl EventEmitter<CloseDiff> for DiffView {}
impl EventEmitter<OpenInEditor> for DiffView {}
impl EventEmitter<ApplyMerge> for DiffView {}
impl EventEmitter<CancelMerge> for DiffView {}

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
            merge: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _refresh: refresh,
        }
    }

    /// The changes to `path`, from the repository's top `top`, between two
    /// snapshots of the working tree, the trees `before` and `after`, read
    /// from git rather than from disk, so it doesn't follow the file. A
    /// renamed file's left side is its old path, `from`, in `before`.
    pub fn between(
        top: PathBuf,
        path: PathBuf,
        from: Option<PathBuf>,
        before: String,
        after: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new(top.join(&path), cx);
        let load = cx.spawn(async move |this, cx| {
            let content = cx
                .background_spawn(async move {
                    let old =
                        task_snapshot::contents(&top, &before, from.as_deref().unwrap_or(&path))
                            .unwrap_or_default();
                    let new = task_snapshot::contents(&top, &after, &path).unwrap_or_default();
                    if new.contains(&0) || old.contains(&0) {
                        return Content::Binary;
                    }
                    Content::Diff {
                        old: String::from_utf8_lossy(&old).into_owned(),
                        new: String::from_utf8_lossy(&new).into_owned(),
                    }
                })
                .await;
            this.update(cx, |this, cx| this.show(content, cx)).ok();
        });
        // Read once, from the snapshots, in place of following the disk.
        view._refresh = load;
        view
    }

    /// A merge of the file at `path` as it is on disk, on the left, and
    /// `mine`, an editor's unsaved text, on the right, following the disk.
    /// Every change starts on the editor's side.
    pub fn merging(path: PathBuf, mine: String, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(path.clone(), cx);
        let refresh = cx.spawn({
            let mine = mine.clone();
            async move |this, cx| {
                loop {
                    let content = cx
                        .background_spawn({
                            let path = path.clone();
                            let mine = mine.clone();
                            async move { read_merge(&path, mine) }
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
        // Reads the disk against the editor's text, in place of the commit.
        view._refresh = refresh;
        view.merge = Some(Merge {
            mine,
            use_disk: HashSet::new(),
        });
        view
    }

    /// The changes being merged, in order, each with whether it is taken
    /// from disk.
    fn merge_hunks(&self) -> Vec<(MergeHunk, bool)> {
        let (Some(merge), Content::Diff { old, new }) = (&self.merge, &self.content) else {
            return Vec::new();
        };
        merge_parts(old, new)
            .into_iter()
            .filter_map(|part| match part {
                MergePart::Change(hunk) => {
                    let disk = merge.use_disk.contains(&hunk);
                    Some((hunk, disk))
                }
                MergePart::Same(_) => None,
            })
            .collect()
    }

    /// Takes the change numbered `ix` from disk, or from the editor's text.
    pub fn choose(&mut self, ix: usize, disk: bool, cx: &mut Context<Self>) {
        let Some((hunk, _)) = self.merge_hunks().into_iter().nth(ix) else {
            return;
        };
        if let Some(merge) = &mut self.merge {
            if disk {
                merge.use_disk.insert(hunk);
            } else {
                merge.use_disk.remove(&hunk);
            }
        }
        cx.notify();
    }

    /// Takes every change from disk, or from the editor's text.
    pub fn choose_all(&mut self, disk: bool, cx: &mut Context<Self>) {
        let hunks = self.merge_hunks();
        if let Some(merge) = &mut self.merge {
            merge.use_disk.clear();
            if disk {
                merge
                    .use_disk
                    .extend(hunks.into_iter().map(|(hunk, _)| hunk));
            }
        }
        cx.notify();
    }

    /// The merged text: the editor's, with each change taken from disk put
    /// back as it is on disk.
    pub fn merged_text(&self) -> Option<String> {
        let merge = self.merge.as_ref()?;
        let Content::Diff { old, new } = &self.content else {
            return Some(merge.mine.clone());
        };
        Some(
            merge_parts(old, new)
                .into_iter()
                .map(|part| match part {
                    MergePart::Same(text) => text,
                    MergePart::Change(hunk) if merge.use_disk.contains(&hunk) => hunk.old,
                    MergePart::Change(hunk) => hunk.new,
                })
                .collect(),
        )
    }

    fn apply_merge(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.merged_text() {
            cx.emit(ApplyMerge(text));
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
            .map(|header| {
                if self.merge.is_some() {
                    return header
                        .child(
                            Button::new("merge-use-all-disk")
                                .ghost()
                                .xsmall()
                                .label("Use All Disk")
                                .disabled(!has_changes)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_all(true, cx))),
                        )
                        .child(
                            Button::new("merge-use-all-mine")
                                .ghost()
                                .xsmall()
                                .label("Use All Mine")
                                .disabled(!has_changes)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_all(false, cx))),
                        )
                        .child(
                            Button::new("merge-cancel")
                                .ghost()
                                .xsmall()
                                .label("Cancel")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(CancelMerge))),
                        )
                        .child(
                            Button::new("merge-apply")
                                .primary()
                                .xsmall()
                                .label("Apply Merge")
                                .disabled(!matches!(self.content, Content::Diff { .. }))
                                .on_click(cx.listener(|this, _, _, cx| this.apply_merge(cx))),
                        );
                }
                header
                    .child(
                        Button::new("diff-open-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FileText)
                            .tooltip("Open the file in the editor")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(OpenInEditor(this.path.clone()))
                            })),
                    )
                    .child(
                        Button::new("close-diff")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Close the diff")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseDiff))),
                    )
            })
    }

    /// While merging, which side each column is: "On disk" and "Mine".
    fn render_merge_sides(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.merge.as_ref()?;
        let theme = cx.theme();
        let label = |text: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .pl(NUMBER_WIDTH)
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let sides = h_flex()
            .id("merge-sides")
            .flex_none()
            .h(ROW_HEIGHT)
            .border_b_1()
            .border_color(theme.border);
        Some(
            match self.layout {
                Layout::SideBySide => sides
                    .child(label("On disk"))
                    .child(div().w_px().h_full().bg(theme.border))
                    .child(label("Mine")),
                Layout::Unified => sides.child(label("− On disk   + Mine")),
            }
            .into_any_element(),
        )
    }

    /// While merging, how a line of `kind` in the change numbered `change`
    /// shows: dimmed when the merge doesn't keep it, and struck through when
    /// it is the editor's and the change is taken from disk.
    fn merge_style(
        &self,
        kind: Kind,
        change: Option<usize>,
        hunks: &[(MergeHunk, bool)],
    ) -> (bool, bool) {
        let Some(disk) = change.and_then(|ix| hunks.get(ix)).map(|(_, disk)| *disk) else {
            return (false, false);
        };
        match kind {
            Kind::Removed => (!disk, false),
            Kind::Added => (disk, disk),
            Kind::Unchanged => (false, false),
        }
    }

    /// The buttons choosing a side for the change numbered `change`, at its
    /// top: "Use Disk" and "Use Mine", the chosen one selected.
    fn render_choice(&self, change: usize, disk: bool, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .absolute()
            .top_0()
            .right_2()
            .h(ROW_HEIGHT)
            .items_center()
            .gap_0p5()
            .child(
                Button::new(("merge-use-disk", change))
                    .ghost()
                    .xsmall()
                    .label("Use Disk")
                    .selected(disk)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(change, true, cx))),
            )
            .child(
                Button::new(("merge-use-mine", change))
                    .ghost()
                    .xsmall()
                    .label("Use Mine")
                    .selected(!disk)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(change, false, cx))),
            )
            .into_any_element()
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
                let marks = this.merge_marks();
                match this.layout {
                    Layout::Unified => {
                        let rows = this.diff.unified(&this.expanded);
                        range
                            .map(|ix| {
                                this.render_unified_row(
                                    ix,
                                    &rows[ix],
                                    current == Some(ix),
                                    &marks,
                                    cx,
                                )
                            })
                            .collect()
                    }
                    Layout::SideBySide => {
                        let rows = this.diff.side_by_side(&this.expanded);
                        range
                            .map(|ix| {
                                this.render_side_row(ix, &rows[ix], current == Some(ix), &marks, cx)
                            })
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

    /// While merging, what is needed to show each line's place in it.
    fn merge_marks(&self) -> Option<MergeMarks> {
        self.merge.as_ref()?;
        let changes = self.diff.change_of_lines();
        let mut starts = HashSet::new();
        let mut previous = None;
        for (ix, change) in changes.iter().enumerate() {
            if change.is_some() && *change != previous {
                starts.insert(ix);
            }
            previous = *change;
        }
        Some(MergeMarks {
            changes,
            starts,
            hunks: self.merge_hunks(),
        })
    }

    /// How the line `line_ix` shows while merging: dimmed, struck through.
    fn line_style(&self, line_ix: usize, marks: &Option<MergeMarks>) -> (bool, bool) {
        let Some(marks) = marks else {
            return (false, false);
        };
        self.merge_style(
            self.diff.lines[line_ix].kind,
            marks.changes[line_ix],
            &marks.hunks,
        )
    }

    /// The choice for the change starting at the line `line_ix`, if one does.
    fn choice_at(
        &self,
        line_ix: Option<usize>,
        marks: &Option<MergeMarks>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let marks = marks.as_ref()?;
        let line_ix = line_ix.filter(|ix| marks.starts.contains(ix))?;
        let change = marks.changes[line_ix]?;
        let disk = marks.hunks.get(change)?.1;
        Some(self.render_choice(change, disk, cx))
    }

    fn render_unified_row(
        &self,
        ix: usize,
        row: &UnifiedRow,
        current: bool,
        marks: &Option<MergeMarks>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            UnifiedRow::Collapsed { start, len } => self.render_collapsed(ix, *start, *len, cx),
            UnifiedRow::Line(line_ix) => {
                let line = &self.diff.lines[*line_ix];
                let (dim, struck) = self.line_style(*line_ix, marks);
                let choice = self.choice_at(Some(*line_ix), marks, cx);
                row_base(("diff-row", ix), line.kind, current, cx)
                    .relative()
                    .when(dim, |row| row.opacity(DIMMED))
                    .child(number(line.old, cx))
                    .child(number(line.new, cx))
                    .child(line_text(line, struck, cx))
                    .children(choice)
                    .into_any_element()
            }
        }
    }

    fn render_side_row(
        &self,
        ix: usize,
        row: &SideRow,
        current: bool,
        marks: &Option<MergeMarks>,
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
            match line.map(|line_ix| (line_ix, &self.diff.lines[line_ix])) {
                Some((line_ix, line)) => {
                    let number_on_side = if old_side { line.old } else { line.new };
                    let (dim, struck) = self.line_style(line_ix, marks);
                    row_base(
                        ("diff-half", ix * 2 + old_side as usize),
                        line.kind,
                        false,
                        cx,
                    )
                    .flex_1()
                    .min_w_0()
                    .when(dim, |half| half.opacity(DIMMED))
                    .child(number(number_on_side, cx))
                    .child(line_text(line, struck, cx))
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
        let choice = self
            .choice_at(old, marks, cx)
            .or_else(|| self.choice_at(new, marks, cx));
        let border = cx.theme().border;
        h_flex()
            .id(("diff-row", ix))
            .relative()
            .w_full()
            .h(ROW_HEIGHT)
            .when(current, |row| {
                row.border_l_2().border_color(cx.theme().ring)
            })
            .child(left)
            .child(div().w_px().h_full().bg(border))
            .child(right)
            .children(choice)
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

/// How faint a line is that the merge doesn't keep.
const DIMMED: f32 = 0.4;

/// What each line's place in a merge is.
struct MergeMarks {
    /// Which change each line is in.
    changes: Vec<Option<usize>>,
    /// The lines changes start at.
    starts: HashSet<usize>,
    /// Each change, and whether it is taken from disk.
    hunks: Vec<(MergeHunk, bool)>,
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

/// A line's marker (+, −, or nothing) and text, its changed words tinted,
/// struck through when `struck`.
fn line_text(line: &Line, struck: bool, cx: &App) -> Div {
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
        .child(
            div()
                .when(struck, |text| text.line_through())
                .child(StyledText::new(line.text.clone()).with_highlights(highlights)),
        )
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
            // <Escape> cancels a merge.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.merge.is_some() && event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(CancelMerge);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_handle.focus(window, cx)),
            )
            .child(self.render_header(cx))
            .children(self.render_merge_sides(cx))
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

/// The file as it is on disk, against `mine`, an editor's text for it. A
/// file no longer on disk is empty there.
fn read_merge(path: &Path, mine: String) -> Content {
    let disk = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Content::Failed(format!("Could not read the file: {err}").into()),
    };
    if disk.contains(&0) {
        return Content::Binary;
    }
    Content::Diff {
        old: String::from_utf8_lossy(&disk).replace("\r\n", "\n"),
        new: mine,
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
