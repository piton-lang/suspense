//! A run of the harness as a table of everything it did: a row for each
//! piece of reply text, tool call, and error, in order, filling in as the
//! reply streams.
//!
//! Every table is virtualized, so drawing one costs no more than drawing the
//! rows in view however long the output grows. A reply keeps its rows as its
//! events arrive, and notes which rows changed, so the table only has rows
//! measured again when they are new or changed, and drawing a row does the
//! same little work whichever row it is.

use std::cell::{OnceCell, RefCell};
use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_kit::component::input::Rope;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{TableCell, TableHead, TableHeader, TableRow};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::file_link::{self, OpenFile};
use crate::harness::HarnessEvent;
use crate::markdown::{self, MarkdownKey, MarkdownKind, MarkdownStates};
use crate::measured_list::{MeasuredList, RenderRow};
use crate::project_directory::ProjectDirectory;
use crate::scrollbar::{self, Scroll, SetLock};
use crate::shell_format;
use crate::theme::Hue;

/// The widths of the output table's badge and status columns; the detail
/// column takes the rest.
pub(crate) const KIND_WIDTH: Pixels = px(130.);
pub(crate) const STATUS_WIDTH: Pixels = px(110.);

/// How many lines of the harness's latest raw output stand in for output
/// that has yet to arrive, and how much of each line is kept.
pub(crate) const RAW_TAIL_LINES: usize = 3;
pub(crate) const RAW_LINE_CHARS: usize = 400;

/// How far beyond the rows in view rows are laid out, so scrolling doesn't
/// show them popping in.
pub(crate) const OVERDRAW: Pixels = px(600.);

/// A harness reply as it streams in.
pub(crate) struct Reply {
    pub(crate) parts: Vec<ReplyPart>,
    /// The parts shown as the table's rows, in order.
    rows: Vec<RowSource>,
    /// Parts before this one are shown as rows, or left out, for good.
    settled_parts: usize,
    /// The last row that is a tool call.
    last_tool_row: Option<usize>,
    /// The harness's last few raw output lines, newest last, each cut short.
    pub(crate) raw_tail: VecDeque<String>,
    /// Each raw line's highlighting once worked out, with the highlight theme
    /// it was worked out for, beside the line.
    raw_styles: RefCell<VecDeque<Option<(usize, Arc<JsonStyles>)>>>,
    pub(crate) done: bool,
    /// Unique among replies, so a table drawn for one knows when it is given
    /// another.
    uid: u64,
    /// Counts the changes to its rows.
    edit: u64,
    /// Each row that may have changed height, with the change it last changed
    /// at, in the order of those changes.
    changes: Vec<(u64, usize)>,
}

type JsonStyles = Vec<(Range<usize>, HighlightStyle)>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum RowSource {
    Part(usize),
    /// The harness is still at work, and nothing is known of what comes next.
    Pending,
}

pub(crate) enum ReplyPart {
    Text(TextPart),
    Tool(ToolCall),
    Error(String),
}

/// Reply text, and the markdown it is shown as once worked out.
pub(crate) struct TextPart {
    text: String,
    shown: OnceCell<SharedString>,
}

impl std::ops::Deref for TextPart {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl TextPart {
    fn new(text: String) -> Self {
        Self {
            text,
            shown: OnceCell::new(),
        }
    }

    fn push_str(&mut self, delta: &str) {
        self.text.push_str(delta);
        self.shown = OnceCell::new();
    }

    /// The text as the markdown it is shown as.
    fn shown(&self) -> SharedString {
        self.shown
            .get_or_init(|| markdown::without_inline_code(&self.text).into())
            .clone()
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct ToolCall {
    id: String,
    pub(crate) name: String,
    pub(crate) summary: Option<String>,
    pub(crate) state: ToolState,
    /// What its output cell shows of its summary, once worked out, and the
    /// project directory it was worked out for: a command laid out and
    /// fenced as markdown, or anything else with its paths made relative.
    shown: RefCell<Option<(Option<PathBuf>, SharedString)>>,
}

impl ToolCall {
    /// The tool's name, for the output column. Left out when the badge already
    /// says what the tool is and there is a summary to show instead; a tool
    /// the badge only calls "Tool" keeps its name.
    pub(crate) fn shown_name(&self) -> Option<&str> {
        let named_by_badge = ToolKind::of(&self.name) != ToolKind::Other;
        (!named_by_badge || self.summary.is_none()).then_some(self.name.as_str())
    }

    /// Whether the call reads or edits one file, which its summary names.
    fn works_on_a_file(&self) -> bool {
        matches!(
            self.name.as_str(),
            "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookRead" | "NotebookEdit"
        )
    }

    fn is_command(&self) -> bool {
        ToolKind::of(&self.name) == ToolKind::Command
    }

    /// What its output cell shows of its summary, worked out once for each
    /// project directory.
    pub(crate) fn shown_summary(&self, project_dir: Option<&Path>) -> Option<SharedString> {
        let summary = self.summary.as_deref()?;
        let mut shown = self.shown.borrow_mut();
        if let Some((dir, text)) = shown.as_ref()
            && dir.as_deref() == project_dir
        {
            return Some(text.clone());
        }
        let relative = relative_to_project(summary, project_dir);
        let text: SharedString = if self.is_command() {
            command_markdown(&relative).into()
        } else {
            relative.into()
        };
        *shown = Some((project_dir.map(Path::to_path_buf), text.clone()));
        Some(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ToolState {
    Running,
    Done,
    Failed,
}

impl ToolState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// The general type of a tool call, shown as its badge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ToolKind {
    Read,
    Edit,
    Command,
    Web,
    Agent,
    Other,
}

impl ToolKind {
    pub(crate) fn of(tool_name: &str) -> Self {
        match tool_name {
            "Read" | "Glob" | "Grep" | "LS" | "NotebookRead" => Self::Read,
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => Self::Edit,
            "Bash" | "BashOutput" | "KillShell" | "KillBash" => Self::Command,
            "WebFetch" | "WebSearch" => Self::Web,
            "Task" | "Agent" => Self::Agent,
            _ => Self::Other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Read => "Files",
            Self::Edit => "Edit",
            Self::Command => "Command",
            Self::Web => "Web",
            Self::Agent => "Agent",
            Self::Other => "Tool",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Read => IconName::BookOpen,
            Self::Edit => IconName::FilePenLine,
            Self::Command => IconName::SquareTerminal,
            Self::Web => IconName::Globe,
            Self::Agent => IconName::Bot,
            Self::Other => IconName::Wrench,
        }
    }

    /// What stands in for a call's input while it streams in, for a kind whose
    /// raw output would only be noise until the input is known. A tool of no
    /// known kind has none, and shows the raw output instead.
    pub(crate) fn pending_input(self) -> Option<&'static str> {
        match self {
            Self::Read => Some("Choosing what to read…"),
            Self::Edit => Some("Writing the edit…"),
            Self::Command => Some("Writing the command…"),
            Self::Web => Some("Preparing the request…"),
            Self::Agent => Some("Briefing the agent…"),
            Self::Other => None,
        }
    }

    fn hue(self) -> Hue {
        match self {
            Self::Read => Hue::Blue,
            Self::Edit => Hue::Amber,
            Self::Command => Hue::Purple,
            Self::Web => Hue::Cyan,
            Self::Agent => Hue::Green,
            Self::Other => Hue::Grey,
        }
    }
}

/// A row of a task's output table.
#[derive(Debug, PartialEq)]
pub(crate) enum OutputRow<'a> {
    /// Reply text; empty while its first words are on their way.
    Text(&'a str),
    Tool(&'a ToolCall),
    Error(&'a str),
    /// The harness is still at work, and nothing is known of what comes next.
    Pending,
}

impl OutputRow<'_> {
    /// The kind of the row, as a badge in its own colour, or a skeleton while
    /// the kind is not yet known.
    pub(crate) fn badge(&self, cx: &App) -> AnyElement {
        let (tag, icon, label) = match self {
            Self::Text(_) => (Tag::secondary(), IconName::MessageSquare, "Reply"),
            Self::Tool(call) => {
                let kind = ToolKind::of(&call.name);
                (crate::theme::tag(kind.hue(), cx), kind.icon(), kind.label())
            }
            Self::Error(_) => (Tag::danger(), IconName::TriangleAlert, "Error"),
            Self::Pending => {
                return Skeleton::new()
                    .w(px(84.))
                    .h_6()
                    .rounded_md()
                    .into_any_element();
            }
        };
        tag.text_sm()
            .child(
                h_flex()
                    .gap_1()
                    .child(Icon::new(icon).xsmall())
                    .child(label),
            )
            .into_any_element()
    }

    /// Whether the row is still waiting on part of what it shows: text with no
    /// words yet, or a running tool call whose input is not yet known.
    pub(crate) fn is_partial(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Tool(call) => call.summary.is_none() && call.state == ToolState::Running,
            Self::Error(_) => false,
            Self::Pending => true,
        }
    }
}

impl Default for Reply {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            parts: Vec::new(),
            rows: vec![RowSource::Pending],
            settled_parts: 0,
            last_tool_row: None,
            raw_tail: VecDeque::new(),
            raw_styles: RefCell::default(),
            done: false,
            uid: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            edit: 0,
            changes: Vec::new(),
        }
    }
}

impl Reply {
    /// How many rows its table has.
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Row `ix` of its table.
    pub(crate) fn row(&self, ix: usize) -> Option<OutputRow<'_>> {
        Some(match self.rows.get(ix)? {
            RowSource::Pending => OutputRow::Pending,
            RowSource::Part(part) => match &self.parts[*part] {
                ReplyPart::Text(text) => OutputRow::Text(text),
                ReplyPart::Tool(call) => OutputRow::Tool(call),
                ReplyPart::Error(error) => OutputRow::Error(error),
            },
        })
    }

    /// The row the harness is on now.
    pub(crate) fn last_row(&self) -> Option<OutputRow<'_>> {
        self.row(self.rows.len().checked_sub(1)?)
    }

    /// The rows of the output table, in the order they happened.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<OutputRow<'_>> {
        (0..self.rows.len()).filter_map(|ix| self.row(ix)).collect()
    }

    /// The markdown row `ix` shows, and where it's shown, for a table
    /// numbered `table`.
    fn row_markdown(
        &self,
        table: usize,
        ix: usize,
        project_dir: Option<&Path>,
    ) -> Option<(MarkdownKey, SharedString)> {
        let RowSource::Part(part) = self.rows.get(ix)? else {
            return None;
        };
        let (kind, text) = match &self.parts[*part] {
            ReplyPart::Text(text) if !text.trim().is_empty() => {
                (MarkdownKind::Output, text.shown())
            }
            ReplyPart::Tool(call) if call.is_command() => {
                (MarkdownKind::Command, call.shown_summary(project_dir)?)
            }
            _ => return None,
        };
        Some((
            MarkdownKey {
                kind,
                table,
                row: ix,
            },
            text,
        ))
    }

    /// Reply text row `ix` shows, as the markdown it is shown as.
    fn shown_text(&self, ix: usize) -> Option<SharedString> {
        let RowSource::Part(part) = self.rows.get(ix)? else {
            return None;
        };
        match &self.parts[*part] {
            ReplyPart::Text(text) => Some(text.shown()),
            _ => None,
        }
    }

    /// Raw output line `ix`, highlighted as JSON, worked out once for each
    /// highlight theme. A line cut short is still highlighted as far as it
    /// parses.
    pub(crate) fn raw_line(&self, ix: usize, cx: &App) -> Option<StyledText> {
        let line = self.raw_tail.get(ix)?;
        let theme = &cx.theme().highlight_theme;
        let key = Arc::as_ptr(theme) as usize;
        let mut cache = self.raw_styles.borrow_mut();
        let styles = match cache.get(ix) {
            Some(Some((for_theme, styles))) if *for_theme == key => styles.clone(),
            _ => {
                let styles = Arc::new(json_highlights(line, theme));
                if let Some(slot) = cache.get_mut(ix) {
                    *slot = Some((key, styles.clone()));
                }
                styles
            }
        };
        Some(
            StyledText::new(SharedString::from(line.clone()))
                .with_highlights(styles.iter().cloned()),
        )
    }

    /// The rows that may have changed height since change `edit`.
    fn changed_since(&self, edit: u64) -> impl Iterator<Item = usize> + '_ {
        let start = self.changes.partition_point(|(at, _)| *at <= edit);
        self.changes[start..].iter().map(|(_, row)| *row)
    }

    /// Notes that row `ix` may have changed height.
    fn touch(&mut self, ix: usize) {
        self.edit += 1;
        match self.changes.last_mut() {
            Some((at, row)) if *row == ix => *at = self.edit,
            _ => self.changes.push((self.edit, ix)),
        }
    }

    /// The row part `part` is shown as, if it is shown.
    fn row_of(&self, part: usize) -> Option<usize> {
        self.rows
            .binary_search_by(|row| match row {
                RowSource::Part(at) => at.cmp(&part),
                RowSource::Pending => std::cmp::Ordering::Greater,
            })
            .ok()
    }

    /// Brings the rows up to date with the parts. Text with nothing in it is
    /// left out, unless it is the last thing in a reply still streaming. Until
    /// the reply is done, it ends in a row that is still filling in, adding a
    /// pending row if the last is already complete. Rows only change or are
    /// added at the end as the reply streams, so a row keeps its index, and
    /// only the rows from the last part on are gone over again.
    fn refresh(&mut self) {
        let settled = self.settled_parts;
        let keep = self
            .rows
            .partition_point(|row| matches!(row, RowSource::Part(part) if *part < settled));
        self.rows.truncate(keep);
        self.last_tool_row = self.last_tool_row.filter(|row| *row < keep);
        let last = self.parts.len().checked_sub(1);
        for (ix, part) in self.parts.iter().enumerate().skip(settled) {
            let shown = match part {
                ReplyPart::Text(text) => {
                    !(text.trim().is_empty() && (self.done || Some(ix) != last))
                }
                ReplyPart::Tool(_) => {
                    self.last_tool_row = Some(self.rows.len());
                    true
                }
                ReplyPart::Error(_) => true,
            };
            if shown {
                self.rows.push(RowSource::Part(ix));
            }
        }
        if !self.done && !self.last_row().is_some_and(|row| row.is_partial()) {
            self.rows.push(RowSource::Pending);
        }
        self.settled_parts = self.parts.len().saturating_sub(1);
        for ix in keep..self.rows.len() {
            self.touch(ix);
        }
    }

    /// Folds a harness event into the reply. Returns an error to show when the
    /// run failed.
    pub(crate) fn apply(&mut self, event: HarnessEvent) -> Option<String> {
        match event {
            HarnessEvent::Output(mut line) => {
                if let Some((end, _)) = line.char_indices().nth(RAW_LINE_CHARS) {
                    line.truncate(end);
                }
                // Always as tall, however many lines there are, so no row
                // changes height.
                let styles = self.raw_styles.get_mut();
                if self.raw_tail.len() == RAW_TAIL_LINES {
                    self.raw_tail.pop_front();
                    styles.pop_front();
                }
                self.raw_tail.push_back(line);
                styles.push_back(None);
                return None;
            }
            HarnessEvent::TextStarted => self
                .parts
                .push(ReplyPart::Text(TextPart::new(String::new()))),
            HarnessEvent::TextDelta(delta) => match self.parts.last_mut() {
                Some(ReplyPart::Text(text)) => text.push_str(&delta),
                _ => self.parts.push(ReplyPart::Text(TextPart::new(delta))),
            },
            HarnessEvent::ToolStarted { id, name } => self.parts.push(ReplyPart::Tool(ToolCall {
                id,
                name,
                summary: None,
                state: ToolState::Running,
                shown: RefCell::default(),
            })),
            HarnessEvent::ToolInput { id, summary } => {
                if let Some(part) = self.tool_part(&id) {
                    if let ReplyPart::Tool(call) = &mut self.parts[part] {
                        call.summary = Some(summary);
                        call.shown = RefCell::default();
                    }
                    if let Some(row) = self.row_of(part) {
                        self.touch(row);
                    }
                }
            }
            HarnessEvent::ToolFinished { id, is_error } => {
                if let Some(part) = self.tool_part(&id) {
                    if let ReplyPart::Tool(call) = &mut self.parts[part] {
                        call.state = if is_error {
                            ToolState::Failed
                        } else {
                            ToolState::Done
                        };
                    }
                    if let Some(row) = self.row_of(part) {
                        self.touch(row);
                    }
                }
            }
            HarnessEvent::Finished { is_error, result } => {
                self.done = true;
                self.settle_tools(if is_error {
                    ToolState::Failed
                } else {
                    ToolState::Done
                });
                if is_error {
                    self.refresh();
                    return Some(if result.is_empty() {
                        "The harness reported an error.".into()
                    } else {
                        result
                    });
                }
                // Nothing streamed (e.g. an older harness): show the result.
                if !self.has_text() && !result.is_empty() {
                    self.parts.push(ReplyPart::Text(TextPart::new(result)));
                }
            }
            HarnessEvent::Failed(error) => {
                self.done = true;
                self.settle_tools(ToolState::Failed);
                self.refresh();
                return Some(error);
            }
            HarnessEvent::Session(_) => return None,
        }
        self.refresh();
        None
    }

    /// Ends a run that ended without a result: tool calls still running
    /// failed with it.
    pub(crate) fn stop(&mut self) {
        self.done = true;
        self.settle_tools(ToolState::Failed);
        self.refresh();
    }

    /// Gives tool calls still running when the run ended the state they ended
    /// in, so none is left reading "running".
    fn settle_tools(&mut self, state: ToolState) {
        let mut settled = Vec::new();
        for (ix, part) in self.parts.iter_mut().enumerate() {
            if let ReplyPart::Tool(call) = part
                && call.state == ToolState::Running
            {
                call.state = state;
                settled.push(ix);
            }
        }
        for part in settled {
            if let Some(row) = self.row_of(part) {
                self.touch(row);
            }
        }
    }

    fn tool_part(&self, tool_id: &str) -> Option<usize> {
        self.parts
            .iter()
            .rposition(|part| matches!(part, ReplyPart::Tool(call) if call.id == tool_id))
    }

    /// Whether the run has ended, with a result or without.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Adds an error row to the end of the output.
    pub(crate) fn push_error(&mut self, error: String) {
        self.parts.push(ReplyPart::Error(error));
        self.refresh();
    }

    fn has_text(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ReplyPart::Text(text) if !text.trim().is_empty()))
    }
}

/// Whether a table's steps up to its answer are shown, and how to show or
/// hide them.
pub(crate) struct Steps {
    pub shown: bool,
    pub toggle: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// How a table's rows sit in a list: each row in turn, or, with its steps
/// collapsed behind a row that shows or hides them, that row first, then any
/// steps shown, then the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    rows: usize,
    /// How many steps there are, and whether they are shown.
    steps: Option<(usize, bool)>,
}

/// What an item of a table's list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Steps,
    Row(usize),
}

impl Layout {
    /// With `steps_shown`, a finished reply's rows up to its last tool call
    /// collapse, unless there's nothing after it.
    pub(crate) fn of(reply: &Reply, steps_shown: Option<bool>) -> Self {
        let rows = reply.row_count();
        let steps = steps_shown.filter(|_| reply.is_done()).and_then(|shown| {
            let steps = reply.last_tool_row? + 1;
            (steps < rows).then_some((steps, shown))
        });
        Self { rows, steps }
    }

    /// How many items it takes in a list.
    pub(crate) fn items(&self) -> usize {
        match self.steps {
            None => self.rows,
            Some((_, true)) => self.rows + 1,
            Some((steps, false)) => self.rows - steps + 1,
        }
    }

    fn item(&self, item: usize) -> Item {
        match self.steps {
            None => Item::Row(item),
            Some(_) if item == 0 => Item::Steps,
            Some((_, true)) => Item::Row(item - 1),
            Some((steps, false)) => Item::Row(item - 1 + steps),
        }
    }

    /// The item row `row` is, unless it's a step collapsed away.
    fn item_of(&self, row: usize) -> Option<usize> {
        match self.steps {
            None => Some(row),
            Some((_, true)) => Some(row + 1),
            Some((steps, false)) => (row >= steps).then(|| row - steps + 1),
        }
    }
}

/// What a list was last told of a table's rows, so it can be told only what
/// changed since.
#[derive(Default)]
pub(crate) struct TableSync {
    reply: u64,
    table: usize,
    layout: Option<Layout>,
    edit: u64,
    markdown: u64,
}

impl TableSync {
    /// Tells `list`, where the table numbered `table` showing `reply` takes
    /// the items from `base` on, what changed: rows added or gone, rows that
    /// changed or whose markdown finished parsing, which are measured again,
    /// or a different reply or a different shape, which replaces its items.
    pub(crate) fn update(
        &mut self,
        list: &MeasuredList,
        base: usize,
        table: usize,
        reply: &Reply,
        layout: Layout,
        cx: &App,
    ) {
        let old = self.layout.map_or(0, |old| old.items());
        let same = self.reply == reply.uid
            && self.table == table
            && self.layout.is_some_and(|old| old.steps == layout.steps);
        if !same {
            list.splice(base..base + old, layout.items());
            *self = Self {
                reply: reply.uid,
                table,
                layout: Some(layout),
                edit: reply.edit,
                markdown: MarkdownStates::latest(cx),
            };
            return;
        }
        let new = layout.items();
        if new != old {
            let kept = old.min(new);
            list.splice(base + kept..base + old, new - kept);
        }
        let remeasure = |row: usize| {
            if let Some(item) = layout.item_of(row).filter(|item| *item < new) {
                list.remeasure(base + item..base + item + 1);
            }
        };
        for row in reply.changed_since(self.edit) {
            remeasure(row);
        }
        for key in MarkdownStates::changed_since(&mut self.markdown, cx) {
            if key.table == table && key.kind != MarkdownKind::Prompt {
                remeasure(key.row);
            }
        }
        self.edit = reply.edit;
        self.layout = Some(layout);
    }

    /// The table's layout, as the list was last told it.
    pub(crate) fn layout(&self) -> Option<Layout> {
        self.layout
    }
}

/// Finds a table's reply as its rows are drawn, from wherever it is kept.
pub(crate) type ReplyOf = Rc<dyn for<'a> Fn(&'a App) -> Option<&'a Reply>>;

pub(crate) fn reply_of(find: impl for<'a> Fn(&'a App) -> Option<&'a Reply> + 'static) -> ReplyOf {
    Rc::new(find)
}

/// How a table is shown where it's used.
pub(crate) struct TableView<'a> {
    /// The element id of its scrolling output.
    pub id: ElementId,
    /// What its scrollbar's element ids start with.
    pub scrollbar: SharedString,
    /// Numbers the table apart from any other, in its rows' element ids and
    /// its markdown's keys.
    pub table: usize,
    /// Opens the files its output links to, and those its file tools worked
    /// on, when clicked.
    pub open: Option<&'a OpenFile>,
    /// Collapses its steps up to the answer once the run is over.
    pub steps: Option<Steps>,
    /// Whether its scroll is locked to the bottom, and how to switch it.
    pub lock: Option<(bool, SetLock)>,
    /// Space around the table, inside its scrolling output.
    pub padding: Edges<Pixels>,
    /// As tall as its rows up to this height, rather than filling the space
    /// it is given.
    pub max_height: Option<Pixels>,
}

/// A task's output table, with the header kept above its rows as they scroll
/// beside a scrollbar.
pub(crate) struct TaskTable {
    list: MeasuredList,
    sync: RefCell<TableSync>,
}

impl TaskTable {
    pub(crate) fn new() -> Self {
        Self {
            list: MeasuredList::new(OVERDRAW),
            sync: RefCell::default(),
        }
    }

    /// Forgets what it was drawn for, so it's drawn afresh, scrolled to the
    /// top.
    pub(crate) fn forget(&self) {
        self.list.reset(0);
        *self.sync.borrow_mut() = TableSync::default();
    }

    #[cfg(test)]
    pub(crate) fn list(&self) -> &MeasuredList {
        &self.list
    }

    pub(crate) fn scroll(&self) -> Scroll {
        self.list.scroll()
    }

    pub(crate) fn scroll_to_end(&self) {
        self.list.scroll_to_end();
    }

    pub(crate) fn scroll_to_top(&self) {
        self.list.scroll_to_top();
    }

    /// Whether the end of the output, where the raw tail is, was in view when
    /// last laid out.
    pub(crate) fn end_in_view(&self) -> bool {
        let state = self.list.state();
        let count = state.item_count();
        if count == 0 {
            return true;
        }
        match state.item_is_below_viewport(count - 1) {
            Some(below) => !below,
            // Not measured: off screen once the list has been laid out, since
            // everything in view is measured; otherwise not yet known.
            None => state.viewport_bounds().size.height <= px(0.),
        }
    }

    /// The table for `reply`, whose rows find it again with `reply_of`.
    pub(crate) fn render(
        &self,
        reply: &Reply,
        reply_of: ReplyOf,
        view: TableView,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let padding = view.padding;
        let fill = view.max_height.is_none();
        let output = div()
            .id(view.id.clone())
            .pt(padding.top)
            .pr(padding.right)
            .pb(padding.bottom)
            .pl(padding.left)
            .map(|output| {
                if fill {
                    output.size_full()
                } else {
                    output.w_full()
                }
            });

        // An unfinished reply always has a row, so only a finished one is
        // empty.
        let content = if reply.row_count() == 0 {
            div()
                .text_color(theme.muted_foreground)
                .child("No output.")
                .into_any_element()
        } else {
            let layout = Layout::of(reply, view.steps.as_ref().map(|steps| steps.shown));
            {
                let mut sync = self.sync.borrow_mut();
                // Another task's output starts afresh, at its top.
                if sync.reply != reply.uid || sync.table != view.table {
                    self.list.reset(0);
                    *sync = TableSync::default();
                }
                sync.update(&self.list, 0, view.table, reply, layout, cx);
            }
            if view.lock.as_ref().is_some_and(|(locked, _)| *locked) {
                self.list.scroll_to_end();
            }
            let rows = self.list.element(table_rows(
                view.table,
                reply_of,
                layout,
                view.open.cloned(),
                view.steps,
                cx,
            ));
            let rows = match view.max_height {
                None => div().flex_1().min_h_0().child(rows),
                // As tall as the rows, until there's no more room.
                Some(_) => div().min_h_0().h(self.list.total_height()).child(rows),
            };
            v_flex()
                .map(|table| match view.max_height {
                    None => table.size_full(),
                    Some(max) => table.w_full().max_h(max),
                })
                .overflow_hidden()
                .text_base()
                .line_height(relative(1.5))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.tokens.table)
                .child(output_header(cx))
                .child(rows)
                .into_any_element()
        };
        // Lets UI tests find the output; inert in normal builds.
        let output = gpui_kit::TestSupportExt::test_support(output.child(content));
        scrollbar::with_scrollbar(view.scrollbar, &self.list, output, fill, view.lock, cx)
    }
}

/// Draws the items of a table laid out as `layout`, each a row of the table,
/// with a line between one and the next, for a list.
pub(crate) fn table_rows(
    table: usize,
    reply_of: ReplyOf,
    layout: Layout,
    open: Option<OpenFile>,
    steps: Option<Steps>,
    cx: &App,
) -> RenderRow {
    let border = cx.theme().border;
    let project_dir: Rc<Option<PathBuf>> = Rc::new(ProjectDirectory::get(cx));
    let steps = steps.map(Rc::new);
    Rc::new(move |item, _, cx| {
        #[cfg(test)]
        ROWS_DRAWN.with(|drawn| drawn.set(drawn.get() + 1));
        let row = match layout.item(item) {
            Item::Steps => match (&steps, layout.steps) {
                (Some(steps), Some((count, _))) => steps_row(table, count, steps, cx),
                _ => div().into_any_element(),
            },
            Item::Row(ix) => {
                // Its markdown's state is kept, so it's parsed once, and
                // measures as tall out of view as in it.
                let piece = reply_of(cx)
                    .and_then(|reply| reply.row_markdown(table, ix, (*project_dir).as_deref()));
                if let Some((key, text)) = piece {
                    MarkdownStates::prepare(key, &text, cx);
                }
                let Some(reply) = reply_of(cx) else {
                    return div().into_any_element();
                };
                let Some(row) = reply.row(ix) else {
                    return div().into_any_element();
                };
                output_row(
                    table,
                    reply,
                    ix,
                    &row,
                    open.as_ref(),
                    (*project_dir).as_deref(),
                    cx,
                )
                .into_any_element()
            }
        };
        div()
            .w_full()
            .when(item > 0, |row| row.border_t_1().border_color(border))
            .child(row)
            .into_any_element()
    })
}

#[cfg(test)]
thread_local! {
    static ROWS_DRAWN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many table rows were drawn on this thread since last asked, laid out
/// in view or measured out of it.
#[cfg(test)]
pub(crate) fn rows_drawn() -> usize {
    ROWS_DRAWN.with(|drawn| drawn.replace(0))
}

/// The row that shows or hides a table's `count` steps.
fn steps_row(table: usize, count: usize, steps: &Steps, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let label = match (steps.shown, count) {
        (false, 1) => "Show 1 step".to_string(),
        (false, count) => format!("Show {count} steps"),
        (true, 1) => "Hide 1 step".to_string(),
        (true, count) => format!("Hide {count} steps"),
    };
    let toggle = steps.toggle.clone();
    let toggle_row = h_flex()
        .id(("output-steps", table))
        .w_full()
        .gap_1p5()
        .cursor_pointer()
        .text_color(theme.muted_foreground)
        .child(Icon::new(if steps.shown {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        }))
        .child(label)
        .on_click(move |_, window, cx| toggle(window, cx));
    // Lets UI tests find the row; inert in normal builds.
    TableRow::new()
        .child(
            TableCell::new()
                .flex_1()
                .min_w_0()
                .child(gpui_kit::TestSupportExt::test_support(toggle_row)),
        )
        .into_any_element()
}

/// Markdown whose headings are sized from the window's base font size; left
/// to its default, the smaller headings come out below the body text. With
/// `open`, a link to a file opens it in the editor.
pub(crate) fn markdown_view(
    key: MarkdownKey,
    text: &str,
    open: Option<&OpenFile>,
    cx: &App,
) -> TextView {
    shown_markdown_view(key, markdown::without_inline_code(text).into(), open, cx)
}

/// [`markdown_view`] of markdown already as it is shown.
pub(crate) fn shown_markdown_view(
    key: MarkdownKey,
    shown: SharedString,
    open: Option<&OpenFile>,
    cx: &App,
) -> TextView {
    let view = cached_text_view(key, shown, cx).style(TextViewStyle {
        heading_base_font_size: cx.theme().font_size,
        ..TextViewStyle::default()
    });
    match open {
        Some(open) => {
            let open = open.clone();
            view.on_link_click(move |url, _, window, cx| {
                file_link::open_link(url, &open, window, cx)
            })
        }
        None => view,
    }
}

/// A command, laid out across lines and highlighted as a code block.
fn command_markdown(command: &str) -> String {
    code_block("bash", &shell_format::format_command(command))
}

/// `markdown` shown at `key`: with the state kept for it once one is (see
/// [`MarkdownStates`]), or one of the element's own until then.
fn cached_text_view(key: MarkdownKey, markdown: SharedString, cx: &App) -> TextView {
    match MarkdownStates::cached(key, &markdown, cx) {
        Some(state) => TextView::new(&state),
        None => TextView::markdown(
            ElementId::NamedInteger(
                format!("{:?}-{}", key.kind, key.table).into(),
                key.row as u64,
            ),
            markdown,
        ),
    }
}

/// `code` as a fenced markdown code block, its fence longer than any run of
/// backticks inside it.
fn code_block(language: &str, code: &str) -> String {
    let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{code}\n{fence}")
}

/// `text` with the project directory written relative to it: paths inside the
/// directory lose its path, and the directory itself reads `.`. A path that
/// only starts with the same characters, like a sibling directory, is kept.
pub(crate) fn relative_to_project(text: &str, project_dir: Option<&Path>) -> String {
    let Some(dir) = project_dir
        .and_then(Path::to_str)
        .map(|dir| dir.trim_end_matches('/'))
        .filter(|dir| !dir.is_empty())
    else {
        return text.to_string();
    };
    let is_path_char = |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~');
    let ends_path = |rest: &str| rest.chars().next().is_none_or(|c| !is_path_char(c));

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(dir) {
        out.push_str(&rest[..at]);
        let after = &rest[at + dir.len()..];
        let starts_path = out.chars().last().is_none_or(|c| !is_path_char(c));
        rest = if starts_path && ends_path(after) {
            out.push('.');
            after
        } else if starts_path && after.starts_with('/') {
            let inside = &after[1..];
            if ends_path(inside) {
                out.push('.');
            }
            inside
        } else {
            out.push_str(dir);
            after
        };
    }
    out.push_str(rest);
    out
}

/// The header of a task's output table, with the theme's bevel.
pub(crate) fn output_header(cx: &App) -> TableHeader {
    TableHeader::new().relative().child(
        TableRow::new()
            .child(TableHead::new().w(KIND_WIDTH).flex_none().child("Type"))
            .child(TableHead::new().flex_1().min_w_0().child("Output"))
            .child(TableHead::new().w(STATUS_WIDTH).flex_none().child("Status"))
            // Laid over the whole header, taking no room of the row's.
            .child(
                TableHead::new()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .p_0()
                    .child(crate::theme::bevel(crate::theme::Bevel::Raised, cx)),
            ),
    )
}

/// Row `row_ix` of a task's output table.
fn output_row(
    task_ix: usize,
    reply: &Reply,
    row_ix: usize,
    row: &OutputRow,
    open: Option<&OpenFile>,
    project_dir: Option<&Path>,
    cx: &App,
) -> TableRow {
    let theme = cx.theme();
    let detail = match row {
        OutputRow::Text(text) if text.trim().is_empty() => raw_tail(reply, cx),
        OutputRow::Pending => raw_tail(reply, cx),
        OutputRow::Text(_) => div()
            .w_full()
            .min_w_0()
            .children(reply.shown_text(row_ix).map(|shown| {
                shown_markdown_view(
                    MarkdownKey {
                        kind: MarkdownKind::Output,
                        table: task_ix,
                        row: row_ix,
                    },
                    shown,
                    open,
                    cx,
                )
            }))
            .into_any_element(),
        OutputRow::Tool(call) => {
            let summary = call.shown_summary(project_dir);
            match summary {
                // A command is laid out across lines and highlighted, so it
                // can be read rather than cut off.
                Some(command) if call.is_command() => div()
                    .w_full()
                    .min_w_0()
                    .child(cached_text_view(
                        MarkdownKey {
                            kind: MarkdownKind::Command,
                            table: task_ix,
                            row: row_ix,
                        },
                        command,
                        cx,
                    ))
                    .into_any_element(),
                // A known tool whose input is on its way shows only a
                // skeleton, with nothing else beside it.
                None if row.is_partial() && ToolKind::of(&call.name).pending_input().is_some() => {
                    Skeleton::new()
                        .w(relative(0.6))
                        .h_4()
                        .rounded_md()
                        .into_any_element()
                }
                summary => h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .when_some(call.shown_name(), |row, name| {
                        row.child(div().flex_none().font_medium().child(name.to_string()))
                    })
                    // Its input is on its way, and there is no telling
                    // what it will be.
                    .when(row.is_partial(), |row| {
                        row.child(div().flex_1().min_w_0().child(raw_tail(reply, cx)))
                    })
                    .when_some(summary, |row, summary| {
                        let text = div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .child(summary);
                        // A file a file tool worked on opens when clicked.
                        let file = open
                            .filter(|_| call.works_on_a_file())
                            .zip(call.summary.clone());
                        match file {
                            Some((open, target)) => {
                                let open = open.clone();
                                let link = text
                                    .id(("output-file", row_ix))
                                    .text_color(theme.link)
                                    .cursor_pointer()
                                    .hover(|style| style.underline())
                                    .on_click(move |_, window, cx| {
                                        file_link::open_link(&target, &open, window, cx)
                                    });
                                // Lets UI tests find the link; inert in normal builds.
                                row.child(gpui_kit::TestSupportExt::test_support(link))
                            }
                            None => row.child(text),
                        }
                    })
                    .into_any_element(),
            }
        }
        OutputRow::Error(error) => div()
            .min_w_0()
            .text_color(theme.foreground)
            .child(error.to_string())
            .into_any_element(),
    };
    let detail = div()
        .id(("output-row", row_ix))
        .w_full()
        .min_w_0()
        .child(detail);

    let status = row_status(row, cx);

    TableRow::new()
        .child(
            TableCell::new()
                .w(KIND_WIDTH)
                .flex_none()
                .items_start()
                .child(row.badge(cx)),
        )
        .child(
            TableCell::new()
                .flex_1()
                .min_w_0()
                .items_start()
                // Lets UI tests find the row; inert in normal builds.
                .child(gpui_kit::TestSupportExt::test_support(detail)),
        )
        .child(
            TableCell::new()
                .w(STATUS_WIDTH)
                .flex_none()
                .items_start()
                .children(status),
        )
}

/// A tool call's state in the status column, as an icon and spelled out
/// rather than only coloured. Other rows have none.
pub(crate) fn row_status(row: &OutputRow, cx: &App) -> Option<AnyElement> {
    let OutputRow::Tool(call) = row else {
        return None;
    };
    let theme = cx.theme();
    let icon = match call.state {
        ToolState::Running => Spinner::new().small().into_any_element(),
        ToolState::Done => Icon::new(IconName::Check)
            .small()
            .text_color(theme.success)
            .into_any_element(),
        ToolState::Failed => Icon::new(IconName::X)
            .small()
            .text_color(theme.danger)
            .into_any_element(),
    };
    Some(
        h_flex()
            .gap_1p5()
            .child(icon)
            .child(
                div()
                    .map(|state| match call.state {
                        // A failure reads at full strength.
                        ToolState::Failed => state.font_medium(),
                        _ => state.text_color(theme.muted_foreground),
                    })
                    .child(call.state.label()),
            )
            .into_any_element(),
    )
}

/// The harness's latest raw output, standing in for output that has yet to
/// arrive: a line for each of its last few events, newest last, highlighted as
/// JSON and cut off at the cell's edge. It is always as tall as a full tail, so
/// the row keeps its height as lines stream in.
fn raw_tail(reply: &Reply, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .min_w_0()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .children((0..RAW_TAIL_LINES).map(|ix| {
            let line: AnyElement = match reply.raw_line(ix, cx) {
                Some(line) => line.into_any_element(),
                None if ix == 0 => "Waiting for the harness…".into_any_element(),
                // A no-break space keeps a line with nothing in it a line tall.
                None => "\u{a0}".into_any_element(),
            };
            div().w_full().min_w_0().truncate().child(line)
        }))
        .into_any_element()
}

pub(crate) fn json_highlights(line: &str, theme: &HighlightTheme) -> JsonStyles {
    let mut highlighter = SyntaxHighlighter::new("json");
    highlighter.update(None, &Rope::from(line), None);
    highlighter.styles(&(0..line.len()), theme)
}
