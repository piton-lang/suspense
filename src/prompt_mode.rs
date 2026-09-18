//! Prompt mode: prompts, compiled on demand from Piton, are sent to a local
//! coding harness as tasks, and each task's output is shown until it finishes.
//!
//! The latest task heads the view: its status as a coloured label, the hidden
//! anchor it was compiled from, and the compiled prompt the harness received.
//! Its output fills the space beneath, one table row per thing the harness
//! did (its text, each tool call, any error), with the kind of each as a
//! badge. While the task is under way, a row stands for what the harness
//! does next, showing a few lines of its latest raw output until its parts
//! are known. A small row, always shown above the header, expands into a
//! scrollable accordion of every
//! task sent, each of which opens onto its own output. A file opened
//! from the project tree splits the view, taking half of its width; the chat
//! input is never split.
//!
//! Prompts sent while the harness works wait in a queue, saved with the
//! project (see [`crate::prompt_queue`]), listed along the bottom of the view.
//!
//! Every task is saved with the project too (see [`crate::prompt_history`]),
//! and opening the project brings back the tasks sent before.
//!
//! A question asked from the Ask tab is not one of those tasks: it runs
//! straight away, beside any task the harness is working on and any other
//! question, and never queues. Each slides up out of the chat input, above its
//! tabs and over the message list, as a single row of its task table, stacked
//! with the others. Once over, it opens onto its whole table, and only then
//! does the message list dim behind it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::TextSelection;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::table::{Table, TableBody, TableCell, TableRow};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::activity::{Job, JobKind};
use crate::chat_input::{self, ChatInput, PreviewPrompt, QueuedEdit, SendMode, Submit, TabChanged};
use crate::commit_notes;
use crate::file_link::OpenFile;
use crate::file_view::{CloseFile, FileView, OpenDefinition, SendToPrompt};
use crate::harness::{self, HarnessEvent};
use crate::hidden_anchor::{self, HiddenAnchor};
use crate::markdown;
use crate::markdown::{MarkdownKey, MarkdownKind, MarkdownStates};
use crate::measured_list::{MeasuredList, RenderRow};
use crate::piton_build;
use crate::piton_lsp::PitonSession;
use crate::project_directory::ProjectDirectory;
use crate::prompt_history::{self, RunRecord, SavedPrompt};
use crate::prompt_queue::{self, QueuedPrompt};
use crate::scrollbar::{self, SetLock};
use crate::selection_popover::{SelectionAction, selection_popover};
use crate::system_prompts;
use crate::task_table::{
    self, KIND_WIDTH, Layout as TableLayout, OutputRow, Reply, STATUS_WIDTH, Steps, TableSync,
    TableView, TaskTable, ToolKind, relative_to_project, row_status, shown_markdown_view,
};
use crate::theme::Hue;

/// The share of the width an opened file takes from the task view.
const FILE_SHARE: f32 = 0.5;

/// How long the file pane takes to slide in from the sidebar, or back into it
/// when closed, and how it moves: critically damped, so it settles without
/// bouncing.
const PANE_SLIDE_TIME: Duration = Duration::from_millis(450);
const PANE_SPRING: SpringConfig = SpringConfig::new(300., 35., 1.);

/// The narrowest either side of the file split can be dragged.
const MIN_SPLIT_WIDTH: Pixels = px(160.);

/// The tallest the expanded queue gets before it scrolls.
const MAX_QUEUE_HEIGHT: Pixels = px(240.);

/// The tallest the compiled prompt in the header gets before it scrolls.
const MAX_PROMPT_HEIGHT: Pixels = px(160.);

/// How far a question slides up out of the chat input while it is a single
/// row.
const ASK_ROW_HEIGHT: Pixels = px(80.);

/// The share of the space above the chat input the answer drawer opens to,
/// and the least and most it can be dragged to.
const DRAWER_SHARE: f32 = 0.8;
const MIN_DRAWER_SHARE: f32 = 0.2;
const MAX_DRAWER_SHARE: f32 = 0.95;

/// The least an open question's table, or the previous answers, gets in the
/// drawer, however little room the drawer's other rows leave.
const MIN_DRAWER_FILL: Pixels = px(96.);

/// How tall the strip along the drawer's top edge that resizes it is.
const DRAWER_HANDLE_HEIGHT: Pixels = px(6.);

/// How a question slides up and expands: critically damped, so it settles
/// without bouncing.
const ASK_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);

/// How dark the message list gets behind an open question, and how quickly
/// it fades there and back: slower than the slide, so the dimming is seen.
const ASK_DIM_SPRING: SpringConfig = SpringConfig::new(120., 22., 1.);

/// Where a previous answer's task index starts in its element ids, apart
/// from the tasks' and the open questions'.
const ASK_HISTORY_IX: usize = usize::MAX / 2;

/// Counted down from, by question id, for a question's task index in its
/// element ids.
const ASK_IX: usize = usize::MAX;

/// A prompt in the queue. Its hidden anchor is resolved and saved in the
/// background just after it is queued; until then it cannot be sent.
struct QueueItem {
    id: usize,
    text: SharedString,
    saved: Option<QueuedPrompt>,
    /// Queued on purpose: once saved, it waits rather than being sent at
    /// once.
    wait: bool,
}

/// A prompt on its way to the harness.
enum Sending {
    /// Typed and sent straight away, in a mode, with any text attached.
    Now(SendMode, Vec<String>),
    /// Out of the queue, saved with its anchor.
    Queued(QueuedPrompt),
}

/// A prompt sent to the harness, and what the harness did with it.
struct PromptTask {
    /// The prompt as typed, shown until it compiles.
    text: SharedString,
    compiled: Option<Compiled>,
    reply: Reply,
    status: TaskStatus,
    /// The mode it was sent in, when known, which tints its header.
    mode: Option<SendMode>,
}

/// A sent prompt once compiled.
struct Compiled {
    /// The name of the hidden anchor it was compiled from.
    anchor: SharedString,
    markdown: String,
    /// The markdown as it is shown, once worked out.
    shown: std::cell::OnceCell<SharedString>,
}

impl Compiled {
    fn new(anchor: SharedString, markdown: String) -> Self {
        Self {
            anchor,
            markdown,
            shown: std::cell::OnceCell::new(),
        }
    }

    /// The markdown as it is shown.
    fn shown(&self) -> SharedString {
        self.shown
            .get_or_init(|| markdown::without_inline_code(&self.markdown).into())
            .clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TaskStatus {
    /// Running `piton build`, so the compiled spec is up to date before the
    /// prompt is sent.
    Building,
    Compiling,
    Running,
    Done,
    Failed,
    /// From the history, with no record of what came of it.
    Unrecorded,
}

impl TaskStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Building => "Building",
            Self::Compiling => "Compiling",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Unrecorded => "Not recorded",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Building | Self::Compiling | Self::Running)
    }

    /// A label in the theme's hue for the status, with the status spelled
    /// out.
    fn tag(self, cx: &App) -> Tag {
        crate::theme::tag(
            match self {
                Self::Building => Hue::Blue,
                Self::Compiling => Hue::Cyan,
                Self::Running => Hue::Amber,
                Self::Done => Hue::Green,
                Self::Failed => Hue::Red,
                Self::Unrecorded => Hue::Grey,
            },
            cx,
        )
        .text_sm()
        .child(self.label())
    }
}

impl PromptTask {
    fn new(text: SharedString) -> Self {
        Self {
            text,
            compiled: None,
            reply: Reply::default(),
            status: TaskStatus::Compiling,
            mode: None,
        }
    }

    /// A task from the history, as it ended: its run's output is replayed
    /// through the harness's parser.
    fn restore(saved: SavedPrompt) -> Self {
        let mut task = Self::new(saved.text.into());
        task.mode = anchor_mode(&saved.anchor);
        let Some(record) = saved.record else {
            task.reply.stop();
            task.status = TaskStatus::Unrecorded;
            return task;
        };
        if let Some(markdown) = record.user_prompt {
            task.set_compiled(Compiled::new(
                saved.anchor.name().to_string().into(),
                markdown,
            ));
        }
        for event in record.output.iter().flat_map(harness::parse) {
            task.apply(event);
        }
        if let Some(error) = record.error {
            task.apply(HarnessEvent::Failed(error));
        }
        task.end();
        task
    }

    fn set_compiled(&mut self, compiled: Compiled) {
        self.compiled = Some(compiled);
        self.status = TaskStatus::Running;
    }

    /// Folds a harness event into the task's output and status.
    fn apply(&mut self, event: HarnessEvent) {
        match self.reply.apply(event) {
            Some(error) => self.fail(error),
            None if self.reply.done && self.status == TaskStatus::Running => {
                self.status = TaskStatus::Done
            }
            None => {}
        }
    }

    fn fail(&mut self, error: String) {
        self.reply.push_error(error);
        self.status = TaskStatus::Failed;
    }

    /// The run is over. One that ended without a result, as when the harness
    /// was stopped, failed, and nothing still running in it finished.
    fn end(&mut self) {
        if self.reply.done {
            return;
        }
        self.reply.stop();
        if self.status != TaskStatus::Failed {
            self.fail("The harness stopped without a result.".into());
        }
    }
}

/// A row saying how many previous items there are, which expands into a
/// scrollable accordion of every one of them, oldest first, each headed by
/// its status and prompt and opening onto its prompt and output. Previous
/// tasks and previous answers are each one of these.
///
/// The accordion is one virtualized list: each item's heading is a row of it,
/// and so, while an item is open, are its prompt, its table's header, each of
/// its table's rows, and its end. However long an open item's output, only
/// the rows in view are laid out.
struct HistoryList {
    /// Element ids: the row's toggle, the accordion, and each item's heading.
    toggle: &'static str,
    list: &'static str,
    item: &'static str,
    /// The row's label, after its count.
    singular: &'static str,
    plural: &'static str,
    /// What the row reads while expanded.
    back: &'static str,
    /// Where its items' task indices start in their element ids, so two lists
    /// never share one.
    id_base: usize,
    expanded: bool,
    /// The item opened in the accordion.
    open: Option<usize>,
    rows: MeasuredList,
    /// What the list was last laid out for.
    laid_out: RefCell<HistoryLaidOut>,
    /// Counts the times it was expanded.
    opened: usize,
    /// Just expanded, so its next layout scrolls it to the latest items.
    scroll_to_latest: Cell<bool>,
    /// Its tables collapse the steps leading up to the answer.
    collapse_steps: bool,
}

/// How many items a history list was laid out with, which was open, and what
/// of the open one's prompt and table.
#[derive(Default)]
struct HistoryLaidOut {
    count: usize,
    open: Option<usize>,
    compiled: bool,
    table: TableSync,
    markdown: u64,
}

/// What a row of a history list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryRow {
    /// An item's heading.
    Heading(usize),
    /// The open item's prompt.
    Prompt(usize),
    /// The open item's table header, or "No output." for a finished item
    /// with none.
    TableHeader(usize),
    /// A row of the open item's table.
    TableRow(usize, usize),
    /// The margin ending the open item.
    End(usize),
}

impl HistoryList {
    fn new(
        ids: (&'static str, &'static str, &'static str),
        (singular, plural, back): (&'static str, &'static str, &'static str),
        id_base: usize,
        collapse_steps: bool,
    ) -> Self {
        Self {
            toggle: ids.0,
            list: ids.1,
            item: ids.2,
            singular,
            plural,
            back,
            id_base,
            expanded: false,
            open: None,
            rows: MeasuredList::new(task_table::OVERDRAW),
            laid_out: RefCell::default(),
            opened: 0,
            scroll_to_latest: Cell::new(false),
            collapse_steps,
        }
    }

    /// The previous tasks, whose tables show whole.
    fn tasks() -> Self {
        Self::new(
            ("history-toggle", "task-list", "history-task"),
            ("previous task", "previous tasks", "Back to the latest task"),
            0,
            false,
        )
    }

    /// The previous answers, whose tables collapse their steps.
    fn answers() -> Self {
        Self::new(
            ("ask-history-toggle", "ask-list", "ask-history-task"),
            (
                "previous answer",
                "previous answers",
                "Back to the questions",
            ),
            ASK_HISTORY_IX,
            true,
        )
    }

    fn toggle(&mut self) {
        self.expanded = !self.expanded;
        if self.expanded {
            self.opened += 1;
            // The latest are the likeliest to be looked for. Scrolled once the
            // list knows every item, which it may not have been told yet.
            self.scroll_to_latest.set(true);
        }
    }

    /// The row: "No …", "1 …", or "N …", which does nothing while `enabled`
    /// is false, and expands the list when clicked.
    fn render_row(
        &self,
        count: usize,
        enabled: bool,
        select: fn(&mut PromptMode) -> &mut HistoryList,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        let theme = cx.theme();
        let label = if self.expanded {
            self.back.to_string()
        } else {
            match count {
                0 => format!("No {}", self.plural),
                1 => format!("1 {}", self.singular),
                n => format!("{n} {}", self.plural),
            }
        };
        h_flex()
            .flex_none()
            .px_2()
            .py_0p5()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new(self.toggle)
                    .ghost()
                    .xsmall()
                    .icon(if self.expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(label)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        select(this).toggle();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// How many rows the open item's block takes: its prompt, table header,
    /// table rows, and end.
    fn block(table: usize) -> usize {
        table + 3
    }

    /// What row `ix` of the list shows, with `open` open and its table `table`
    /// rows long.
    fn row_at(ix: usize, open: Option<(usize, usize)>) -> HistoryRow {
        let Some((open, table)) = open else {
            return HistoryRow::Heading(ix);
        };
        if ix <= open {
            return HistoryRow::Heading(ix);
        }
        let at = ix - open - 1;
        if at >= Self::block(table) {
            return HistoryRow::Heading(ix - Self::block(table));
        }
        match at {
            0 => HistoryRow::Prompt(open),
            1 => HistoryRow::TableHeader(open),
            at if at == table + 2 => HistoryRow::End(open),
            at => HistoryRow::TableRow(open, at - 2),
        }
    }

    /// Every item, oldest first, as an accordion that scrolls: each headed by
    /// its status and prompt, and the one open showing its prompt and output.
    #[allow(clippy::too_many_arguments)]
    fn render_list(
        &self,
        tasks: &[PromptTask],
        tasks_of: fn(&PromptMode) -> &Vec<PromptTask>,
        select: fn(&mut PromptMode) -> &mut HistoryList,
        steps_shown: &HashSet<usize>,
        open_file: &OpenFile,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        let count = tasks.len();
        let (id_base, collapse_steps) = (self.id_base, self.collapse_steps);
        let open = self.open.filter(|open| *open < count);
        let table_layout = open.map(|open| {
            let task_ix = id_base + open;
            let reply = &tasks[open].reply;
            let steps = collapse_steps.then(|| steps_shown.contains(&task_ix));
            (task_ix, reply, TableLayout::of(reply, steps))
        });

        // The list is told when items come or go, when an item opens or
        // closes, and what of the open item's prompt and table changed.
        {
            let mut laid_out = self.laid_out.borrow_mut();
            if laid_out.count != count {
                self.rows.reset(count);
                *laid_out = HistoryLaidOut {
                    count,
                    markdown: MarkdownStates::latest(cx),
                    ..HistoryLaidOut::default()
                };
            }
            if laid_out.open != open {
                if let Some(was) = laid_out.open {
                    let table = laid_out.table.layout().map_or(0, |layout| layout.items());
                    self.rows.splice(was + 1..was + 1 + Self::block(table), 0);
                    self.rows.remeasure(was..was + 1);
                }
                if let Some(open) = open {
                    self.rows.splice(open + 1..open + 1, Self::block(0));
                    self.rows.remeasure(open..open + 1);
                }
                laid_out.open = open;
                laid_out.compiled = open.is_some_and(|open| tasks[open].compiled.is_some());
                laid_out.table = TableSync::default();
            }
            if let (Some(open), Some((task_ix, reply, layout))) = (open, table_layout) {
                let compiled = tasks[open].compiled.is_some();
                let prompt_changed = MarkdownStates::changed_since(&mut laid_out.markdown, cx)
                    .into_iter()
                    .any(|key| key.kind == MarkdownKind::Prompt && key.table == task_ix);
                if compiled != laid_out.compiled || prompt_changed {
                    self.rows.remeasure(open + 1..open + 2);
                    laid_out.compiled = compiled;
                }
                // "No output." stands in the header's place, for as long as
                // there is none.
                let empty = reply.row_count() == 0;
                if laid_out.table.layout().is_some_and(|was| was.items() == 0) != empty {
                    self.rows.remeasure(open + 2..open + 3);
                }
                laid_out
                    .table
                    .update(&self.rows, open + 3, task_ix, reply, layout, cx);
            }
        }
        if self.scroll_to_latest.take() {
            self.rows.scroll_to_end();
        }

        let theme = cx.theme();
        let (border, hover, muted) = (theme.border, theme.list_hover, theme.muted_foreground);
        let (table_background, radius) = (theme.tokens.table, theme.radius);
        let item_id = self.item;
        let this = cx.entity().downgrade();
        let open_file = open_file.clone();
        let table = table_layout.map(|(task_ix, _, layout)| {
            let reply_of = task_table::reply_of({
                let this = this.clone();
                move |cx| {
                    let prompt_mode = this.upgrade()?.read(cx);
                    tasks_of(prompt_mode)
                        .get(task_ix - id_base)
                        .map(|task| &task.reply)
                }
            });
            let steps = collapse_steps.then(|| steps(task_ix, steps_shown, &cx.entity()));
            let rows = task_table::table_rows(
                task_ix,
                reply_of,
                layout,
                Some(open_file.clone()),
                steps,
                cx,
            );
            (layout.items(), rows)
        });
        let open_rows = open.zip(table.as_ref().map(|(items, _)| *items));
        let render: RenderRow = Rc::new(move |ix, window, cx| {
            let Some(entity) = this.upgrade() else {
                return div().into_any_element();
            };
            let row = Self::row_at(ix, open_rows);
            // Everything an open item shows is set in from the list's edges,
            // as wide as the list, however long its lines, so they wrap and it
            // is laid out as tall as it is measured.
            let inset = || div().w_full().px_3();
            match row {
                HistoryRow::Heading(item) => {
                    let Some(task) = tasks_of(entity.read(cx)).get(item) else {
                        return div().into_any_element();
                    };
                    let is_open = open == Some(item);
                    let task_ix = id_base + item;
                    let this = this.clone();
                    let trigger = h_flex()
                        .id(("history-trigger", task_ix))
                        .justify_between()
                        .gap_3()
                        .py_2()
                        .px_3()
                        .font_medium()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(move |_, _, cx| {
                            this.update(cx, |this, cx| {
                                let history = select(this);
                                history.open = if history.open == Some(item) {
                                    None
                                } else {
                                    Some(item)
                                };
                                cx.notify();
                            })
                            .ok();
                        })
                        .child(task_summary((item_id, item), task_ix, task, cx))
                        .child(
                            Icon::new(if is_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall()
                            .flex_none()
                            .text_color(muted),
                        );
                    // An open item's line is below its end instead.
                    div()
                        .w_full()
                        .when(!is_open, |item| item.border_b_1().border_color(border))
                        .child(trigger)
                        .into_any_element()
                }
                HistoryRow::Prompt(item) => {
                    let task_ix = id_base + item;
                    let shown = tasks_of(entity.read(cx))
                        .get(item)
                        .and_then(|task| task.compiled.as_ref())
                        .map(Compiled::shown);
                    // Its markdown's state is kept, so it's parsed once.
                    if let Some(shown) = shown {
                        MarkdownStates::prepare(prompt_key(task_ix), &shown, cx);
                    }
                    let Some(task) = tasks_of(entity.read(cx)).get(item) else {
                        return div().into_any_element();
                    };
                    // Lets UI tests find the panel; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(
                        inset()
                            .id(("history-panel", task_ix))
                            .pt_1()
                            .pb_3()
                            .child(task_prompt(task_ix, task, &open_file, cx)),
                    )
                    .into_any_element()
                }
                HistoryRow::TableHeader(item) => {
                    let empty = tasks_of(entity.read(cx))
                        .get(item)
                        .is_none_or(|task| task.reply.row_count() == 0);
                    if empty {
                        return inset()
                            .text_color(muted)
                            .child("No output.")
                            .into_any_element();
                    }
                    inset()
                        .child(
                            div()
                                .overflow_hidden()
                                .text_base()
                                .line_height(relative(1.5))
                                .rounded_t(radius)
                                .border_t_1()
                                .border_x_1()
                                .border_color(border)
                                .bg(table_background)
                                .child(task_table::output_header(cx)),
                        )
                        .into_any_element()
                }
                HistoryRow::TableRow(_, row) => {
                    let Some((_, rows)) = &table else {
                        return div().into_any_element();
                    };
                    inset()
                        .child(
                            div()
                                .text_base()
                                .line_height(relative(1.5))
                                .border_x_1()
                                .border_color(border)
                                .bg(table_background)
                                .child(rows(row, window, cx)),
                        )
                        .into_any_element()
                }
                HistoryRow::End(item) => {
                    let has_table = open_rows.is_some_and(|(_, rows)| rows > 0);
                    // Lets UI tests find the end; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(
                        inset()
                            .id(("history-panel-end", id_base + item))
                            .pb_2()
                            .border_b_1()
                            .border_color(border)
                            .when(has_table, |end| {
                                end.child(
                                    div()
                                        .h(radius.max(px(1.)))
                                        .rounded_b(radius)
                                        .border_b_1()
                                        .border_x_1()
                                        .border_color(border)
                                        .bg(table_background),
                                )
                            }),
                    )
                    .into_any_element()
                }
            }
        });
        let items = self.rows.element(render);
        let list = div()
            .id(SharedString::from(format!("{}-scroll", self.list)))
            .size_full()
            .p_4()
            .child(items);
        // Lets UI tests find the list; inert in normal builds.
        let list = gpui_kit::TestSupportExt::test_support(list);
        scrollbar::with_scrollbar(self.list, &self.rows, list, true, None, cx)
    }
}

/// The task view at `width`, its right edge at the body's, with a sliding
/// `pane` laid over its left. The task view's width doesn't change as the pane
/// slides, so nothing in it is laid out anew.
fn covered(history: Div, pane: impl IntoElement, width: Pixels, cx: &App) -> Div {
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(width)
                .child(history),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .occlude()
                .bg(cx.theme().background)
                .child(pane),
        )
}

/// Where a task's compiled prompt's markdown is kept.
fn prompt_key(task_ix: usize) -> MarkdownKey {
    MarkdownKey {
        kind: MarkdownKind::Prompt,
        table: task_ix,
        row: 0,
    }
}

/// Writes a commit note for a task from its prompt and its final summary.
type Summarize = fn(&Path, &str, &str) -> Result<Option<String>>;

/// Has `summarize` write the note of a task that asked `asked` and finished
/// with `result`, in the background, and adds it to the project's commit
/// notes. A task that changed nothing gets none, and one that couldn't be
/// written is left out: the note is only a convenience.
fn add_commit_note(
    summarize: Summarize,
    project_dir: PathBuf,
    asked: String,
    result: String,
    cx: &mut App,
) {
    let note = cx.background_spawn({
        let project_dir = project_dir.clone();
        async move { summarize(&project_dir, &asked, &result) }
    });
    cx.spawn(async move |cx| {
        if let Ok(Some(note)) = note.await
            && commit_notes::add(&project_dir, &note).is_ok()
        {
            cx.update(commit_notes::changed);
        }
    })
    .detach();
}

/// A question asked from the Ask tab.
struct Ask {
    id: usize,
    task: PromptTask,
    table: TaskTable,
    /// Its output stays scrolled to the bottom.
    locked: bool,
    /// Its run, stopped when the question is closed.
    _run: Task<()>,
}

/// A question's run as far as it went, saved beside the question in
/// `.suspense/asks` when dropped: once the run is over, or when the question
/// is closed or replaced and its run stopped.
struct AskLog {
    file: Option<PathBuf>,
    record: RunRecord,
}

impl Drop for AskLog {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let record = std::mem::take(&mut self.record);
        // Off the UI thread: a long run's output can be large.
        std::thread::spawn(move || prompt_history::save_record(&file, &record).ok());
    }
}

/// A conversation with the harness, which later runs in the same project
/// resume so the harness keeps its context.
#[derive(Clone)]
struct Session {
    project_dir: PathBuf,
    id: String,
}

impl Session {
    /// The conversation to resume from `session`, if it is `project_dir`'s.
    fn resume(session: &Option<Self>, project_dir: &Path) -> Option<String> {
        session
            .as_ref()
            .filter(|session| session.project_dir == project_dir)
            .map(|session| session.id.clone())
    }

    /// Forgets `session` if it is still `id`, which the harness could not
    /// resume, so the next run starts a new conversation.
    fn forget(session: &mut Option<Self>, id: &str) {
        if session.as_ref().is_some_and(|session| session.id == id) {
            *session = None;
        }
    }

    /// The latest conversation in a project's history.
    fn latest(history: &[SavedPrompt], project_dir: &Path) -> Option<Self> {
        let id = history.iter().rev().find_map(|saved| {
            saved
                .record
                .as_ref()?
                .output
                .iter()
                .flat_map(harness::parse)
                .filter_map(|event| match event {
                    HarnessEvent::Session(id) => Some(id),
                    _ => None,
                })
                .last()
        })?;
        Some(Self {
            project_dir: project_dir.to_path_buf(),
            id,
        })
    }
}

/// A file pane sliding closed.
struct PaneClosing {
    file: Entity<FileView>,
    /// The width it slides closed from.
    width: Pixels,
    /// Which opening of the pane this closes, so each slide animates afresh.
    slide: usize,
    closed: Instant,
}

/// Dragged by the answer drawer's top edge to resize it.
struct DrawerResize;

/// What is open in the answer drawer: the question open onto its table, and
/// which expanding of the previous answers is showing.
type DrawerContents = (Option<usize>, Option<usize>);

/// The work that belongs to one open project (see the OpenProjectsScope):
/// swapped into [`PromptMode`] while the project is on screen, or while a run
/// of it writes back from the background, and kept aside otherwise.
struct ProjectSession {
    project_dir: Option<PathBuf>,
    tasks: Vec<PromptTask>,
    working: bool,
    history_stale: bool,
    _history_load: Task<()>,
    task_history: HistoryList,
    queue: Vec<QueueItem>,
    queue_expanded: bool,
    auto_send: bool,
    queue_held: bool,
    _pending: Task<()>,
    session: Option<Session>,
    asks: Vec<Ask>,
    expanded_ask: Option<usize>,
    steps_shown: HashSet<usize>,
    answers: Vec<PromptTask>,
    ask_history: HistoryList,
    _ask_history_load: Task<()>,
    ask_session: Option<Session>,
}

impl ProjectSession {
    fn new(project_dir: Option<PathBuf>) -> Self {
        Self {
            project_dir,
            tasks: Vec::new(),
            working: false,
            history_stale: false,
            _history_load: Task::ready(()),
            task_history: HistoryList::tasks(),
            queue: Vec::new(),
            queue_expanded: false,
            auto_send: true,
            queue_held: false,
            _pending: Task::ready(()),
            session: None,
            asks: Vec::new(),
            expanded_ask: None,
            steps_shown: HashSet::new(),
            answers: Vec::new(),
            ask_history: HistoryList::answers(),
            _ask_history_load: Task::ready(()),
            ask_session: None,
        }
    }
}

/// What is running in an open project, for showing it busy.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectActivity {
    pub project_dir: PathBuf,
    /// The first line of the task the harness is working on, if it is.
    pub task: Option<SharedString>,
    /// Each question still running, by its id, with its first line.
    pub questions: Vec<(usize, SharedString)>,
}

pub struct PromptMode {
    /// The open project on screen. Its work is held in the fields marked as a
    /// project's in [`ProjectSession`], swapped in here.
    project_dir: Option<PathBuf>,
    /// The work of every other open project, by its folder.
    background: HashMap<PathBuf, ProjectSession>,
    /// A background project's work is swapped in, for a run of it to write
    /// back; nothing on screen is touched meanwhile.
    in_background: bool,
    /// Every task sent, oldest first; the last heads the view.
    tasks: Vec<PromptTask>,
    /// The latest task's output, which follows new rows while scrolled to
    /// the bottom, or always while locked there.
    output_table: TaskTable,
    output_locked: bool,
    queue_scroll: ScrollHandle,
    chat_input: Entity<ChatInput>,
    /// The queued prompt being edited in the chat input, by its id.
    editing_queued: Option<usize>,
    /// The harness is working on the latest task.
    working: bool,
    /// The project changed while the harness worked; its history loads once
    /// the run is over.
    history_stale: bool,
    _history_load: Task<()>,
    /// The row of previous tasks, expanding into every task.
    task_history: HistoryList,
    /// Prompts waiting for the harness, in the order they were sent.
    queue: Vec<QueueItem>,
    next_queue_id: usize,
    queue_expanded: bool,
    /// Send the next queued prompt as soon as the harness is free.
    auto_send: bool,
    /// A restored queue waits for "Send next" (or auto send being switched
    /// on) rather than sending on its own, until it has emptied.
    queue_held: bool,
    /// The file opened from the project tree, beside the task view.
    file: Option<Entity<FileView>>,
    /// When the file pane last opened, and how many times it has, for its
    /// slide in from the sidebar.
    pane_opened: Option<(usize, Instant)>,
    /// A file just closed, sliding back into the sidebar.
    pane_closing: Option<PaneClosing>,
    /// The file pane's width, as last laid out, for it to slide closed from.
    pane_width: Rc<Cell<Pixels>>,
    file_split: Entity<ResizableState>,
    /// The width the task view and any file share, as last laid out.
    body_width: Rc<Cell<Pixels>>,
    _pending: Task<()>,
    /// The conversation the tasks share, each resuming the last.
    session: Option<Session>,
    /// The questions asked from the Ask tab, oldest first, each run at once
    /// and apart from the tasks.
    asks: Vec<Ask>,
    next_ask_id: usize,
    /// The finished question opened onto its whole task table, if any.
    expanded_ask: Option<usize>,
    /// The popover offering to copy text selected in an answer, or attach it
    /// to the prompt: where the selecting drag ended, and what it selected.
    selection_popover: Option<(Point<Pixels>, String)>,
    /// The answers' tables whose steps were expanded, by task index.
    steps_shown: HashSet<usize>,
    /// The share of the space above the chat input the answer drawer takes
    /// while open; dragging its top edge changes it for the rest of the run.
    drawer_share: f32,
    /// What was open in the drawer when it was last dragged; while that is
    /// still what's open, the drawer follows the drag rather than sliding.
    drawer_dragged: Option<DrawerContents>,
    /// The space above the chat input, the whole drawer, and what fills it,
    /// as last laid out.
    body_height: Rc<Cell<Pixels>>,
    drawer_height: Rc<Cell<Pixels>>,
    drawer_fill_height: Rc<Cell<Pixels>>,
    /// The question rows in the stack above the tabs, as last laid out, which
    /// the previous answers slide up from rather than covering.
    stack_rows_height: Rc<Cell<Pixels>>,
    /// Every question asked before, oldest first: those saved with the
    /// project, and those closed since.
    answers: Vec<PromptTask>,
    /// The row of previous answers, shown on the Ask tab.
    ask_history: HistoryList,
    _ask_history_load: Task<()>,
    /// The chat input's Ask tab is selected.
    on_ask_tab: bool,
    /// The conversation questions share, apart from the tasks'.
    ask_session: Option<Session>,
    _file_subscriptions: Vec<Subscription>,
    /// Compiling the prompt for the chat input's preview.
    _preview: Task<()>,
    /// Writes a finished task's commit note: [`commit_notes::summarize`],
    /// replaced in tests.
    summarize: Summarize,
    _subscriptions: Vec<Subscription>,
}

impl PromptMode {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat_input = cx.new(|cx| ChatInput::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &chat_input,
                window,
                |this, _, submit: &Submit, window, cx| {
                    let (text, attached_text) = (submit.text.clone(), submit.attached_text.clone());
                    // Queued on purpose, it waits in the queue even while the
                    // harness is free; a question never queues.
                    if submit.queue && submit.mode != SendMode::Ask && this.project_dir.is_some() {
                        this.enqueue(text, true, submit.mode, attached_text, window, cx);
                    } else {
                        this.send(text, submit.mode, attached_text, window, cx)
                    }
                },
            ),
            cx.subscribe_in(
                &chat_input,
                window,
                |this, _, edit: &QueuedEdit, window, cx| this.queued_edit_over(edit, window, cx),
            ),
            cx.subscribe(&chat_input, |this, input, preview: &PreviewPrompt, cx| {
                this.preview(input, preview, cx)
            }),
            cx.subscribe(&chat_input, |this, input, _: &TabChanged, cx| {
                this.on_ask_tab = input.read(cx).mode() == SendMode::Ask;
                cx.notify();
            }),
            // A row whose markdown finished parsing is measured again.
            cx.observe_global::<MarkdownStates>(|_, cx| cx.notify()),
            cx.observe_global::<ProjectDirectory>(|this, cx| this.project_changed(cx)),
        ];

        let mut this = Self {
            project_dir: None,
            background: HashMap::new(),
            in_background: false,
            tasks: Vec::new(),
            output_table: TaskTable::new(),
            output_locked: false,
            queue_scroll: ScrollHandle::new(),
            chat_input,
            editing_queued: None,
            working: false,
            history_stale: false,
            _history_load: Task::ready(()),
            task_history: HistoryList::tasks(),
            queue: Vec::new(),
            next_queue_id: 0,
            queue_expanded: false,
            auto_send: true,
            queue_held: false,
            file: None,
            pane_opened: None,
            pane_closing: None,
            pane_width: Rc::default(),
            file_split: cx.new(|_| ResizableState::default()),
            body_width: Rc::default(),
            _pending: Task::ready(()),
            session: None,
            asks: Vec::new(),
            next_ask_id: 0,
            expanded_ask: None,
            selection_popover: None,
            steps_shown: HashSet::new(),
            drawer_share: DRAWER_SHARE,
            drawer_dragged: None,
            body_height: Rc::default(),
            drawer_height: Rc::default(),
            drawer_fill_height: Rc::default(),
            stack_rows_height: Rc::default(),
            answers: Vec::new(),
            ask_history: HistoryList::answers(),
            _ask_history_load: Task::ready(()),
            on_ask_tab: false,
            ask_session: None,
            _file_subscriptions: Vec::new(),
            _preview: Task::ready(()),
            summarize: commit_notes::summarize,
            _subscriptions: subscriptions,
        };
        this.project_changed(cx);
        this
    }

    /// Swaps the work of the project on screen with `other`'s.
    fn swap_session(&mut self, other: &mut ProjectSession) {
        use std::mem::swap;
        swap(&mut self.project_dir, &mut other.project_dir);
        swap(&mut self.tasks, &mut other.tasks);
        swap(&mut self.working, &mut other.working);
        swap(&mut self.history_stale, &mut other.history_stale);
        swap(&mut self._history_load, &mut other._history_load);
        swap(&mut self.task_history, &mut other.task_history);
        swap(&mut self.queue, &mut other.queue);
        swap(&mut self.queue_expanded, &mut other.queue_expanded);
        swap(&mut self.auto_send, &mut other.auto_send);
        swap(&mut self.queue_held, &mut other.queue_held);
        swap(&mut self._pending, &mut other._pending);
        swap(&mut self.session, &mut other.session);
        swap(&mut self.asks, &mut other.asks);
        swap(&mut self.expanded_ask, &mut other.expanded_ask);
        swap(&mut self.steps_shown, &mut other.steps_shown);
        swap(&mut self.answers, &mut other.answers);
        swap(&mut self.ask_history, &mut other.ask_history);
        swap(&mut self._ask_history_load, &mut other._ask_history_load);
        swap(&mut self.ask_session, &mut other.ask_session);
    }

    /// Follows the project on screen: the work of the one left keeps running
    /// in the background, and the one switched to comes back as it was left,
    /// or, the first time it is opened, is loaded from its data.
    fn project_changed(&mut self, cx: &mut Context<Self>) {
        let dir = ProjectDirectory::get(cx);
        if dir == self.project_dir {
            return;
        }
        let mut left = ProjectSession::new(None);
        self.swap_session(&mut left);
        if let Some(left_dir) = left.project_dir.clone() {
            self.background.insert(left_dir, left);
        }
        match dir.as_ref().and_then(|dir| self.background.remove(dir)) {
            Some(mut back) => self.swap_session(&mut back),
            None => {
                self.project_dir = dir;
                self.load_queue(cx);
                self.load_history(cx);
                self.load_answers(cx);
            }
        }
        // Nothing on screen carries over from the project left.
        self.output_locked = false;
        self.selection_popover = None;
        self.output_table.forget();
        let working = self.working;
        self.chat_input
            .update(cx, |input, cx| input.set_busy(working, cx));
        cx.notify();
    }

    /// Runs `f` with the work of the project at `dir` in place: straight away
    /// while it is on screen, or with its work swapped in from the background,
    /// touching nothing on screen, and swapped back after. Nothing runs for a
    /// project that isn't open.
    fn in_project<R>(
        &mut self,
        dir: &Path,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> Option<R> {
        if self.project_dir.as_deref() == Some(dir) {
            return Some(f(self, cx));
        }
        let mut session = self.background.remove(dir)?;
        self.swap_session(&mut session);
        let was_background = std::mem::replace(&mut self.in_background, true);
        let result = f(self, cx);
        self.in_background = was_background;
        self.swap_session(&mut session);
        self.background.insert(dir.to_path_buf(), session);
        cx.notify();
        Some(result)
    }

    /// What is running in each open project, the one on screen first, then
    /// the others; a project with nothing running is left out.
    pub fn busy_projects(&self) -> Vec<ProjectActivity> {
        fn activity(
            project_dir: Option<&PathBuf>,
            tasks: &[PromptTask],
            working: bool,
            asks: &[Ask],
        ) -> Option<ProjectActivity> {
            let task = tasks
                .last()
                .filter(|task| working && task.status.is_active())
                .map(|task| first_line(&task.text));
            let questions: Vec<(usize, SharedString)> = asks
                .iter()
                .filter(|ask| ask.task.status.is_active())
                .map(|ask| (ask.id, first_line(&ask.task.text)))
                .collect();
            if task.is_none() && questions.is_empty() {
                return None;
            }
            Some(ProjectActivity {
                project_dir: project_dir?.clone(),
                task,
                questions,
            })
        }
        let mut busy: Vec<ProjectActivity> = activity(
            self.project_dir.as_ref(),
            &self.tasks,
            self.working,
            &self.asks,
        )
        .into_iter()
        .collect();
        let mut others: Vec<ProjectActivity> = self
            .background
            .values()
            .filter_map(|session| {
                activity(
                    session.project_dir.as_ref(),
                    &session.tasks,
                    session.working,
                    &session.asks,
                )
            })
            .collect();
        others.sort_by(|a, b| a.project_dir.cmp(&b.project_dir));
        busy.extend(others);
        busy
    }

    /// What is running: the task the harness is working on, then each
    /// question still running, in the project on screen, then in each other
    /// open project, headed with its name.
    pub fn running_jobs(&self) -> Vec<Job> {
        let task = self
            .tasks
            .last()
            .filter(|task| self.working && task.status.is_active())
            .map(|task| Job {
                kind: JobKind::Task,
                title: "Task".into(),
                detail: Some(first_line(&task.text)),
                project: None,
            });
        let questions = self
            .asks
            .iter()
            .filter(|ask| ask.task.status.is_active())
            .map(|ask| Job {
                kind: JobKind::Question(ask.id),
                title: "Question".into(),
                detail: Some(first_line(&ask.task.text)),
                project: None,
            });
        let mut jobs: Vec<Job> = task.into_iter().chain(questions).collect();
        for busy in self
            .busy_projects()
            .into_iter()
            .filter(|busy| Some(&busy.project_dir) != self.project_dir.as_ref())
        {
            let project = Some(busy.project_dir.clone());
            jobs.extend(busy.task.map(|task| Job {
                kind: JobKind::Task,
                title: crate::activity::title_in("Task", project.as_deref()),
                detail: Some(task),
                project: project.clone(),
            }));
            jobs.extend(busy.questions.into_iter().map(|(id, text)| Job {
                kind: JobKind::Question(id),
                title: crate::activity::title_in("Question", project.as_deref()),
                detail: Some(text),
                project: project.clone(),
            }));
        }
        jobs
    }

    /// Starts a question that stays running, for tests elsewhere.
    #[cfg(test)]
    pub fn start_test_question(&mut self, text: &str, cx: &mut Context<Self>) -> usize {
        let id = self.push_ask(text.to_string().into(), cx);
        self.update_ask(id, |ask| ask.apply(HarnessEvent::TextStarted), cx);
        id
    }

    #[cfg(test)]
    pub fn close_test_question(&mut self, id: usize, cx: &mut Context<Self>) {
        self.close_ask(id, cx);
    }

    /// Brings the latest task's output into view: out from behind the
    /// previous tasks and the answer drawer, scrolled to its end.
    pub fn reveal_task(&mut self, cx: &mut Context<Self>) {
        self.task_history.expanded = false;
        self.close_answer_drawer();
        self.output_table.scroll_to_end();
        cx.notify();
    }

    /// Brings the question `id`'s row into view, closing the answer drawer
    /// over it.
    pub fn reveal_question(&mut self, _id: usize, cx: &mut Context<Self>) {
        self.close_answer_drawer();
        cx.notify();
    }

    fn close_answer_drawer(&mut self) {
        self.expanded_ask = None;
        if self.ask_history.expanded {
            self.ask_history.toggle();
        }
    }

    /// Whether the harness is working on a task or a question in any open
    /// project.
    pub fn is_working(&self) -> bool {
        self.working
            || self.asks.iter().any(|ask| ask.task.status.is_active())
            || !self.busy_projects().is_empty()
    }

    /// The prompts queued, oldest first.
    #[cfg(test)]
    pub fn queued_texts(&self) -> Vec<String> {
        self.queue
            .iter()
            .map(|item| item.text.to_string())
            .collect()
    }

    #[cfg(test)]
    pub fn set_working(&mut self, working: bool) {
        self.working = working;
    }

    /// Moves keyboard focus into the chat input.
    pub fn focus_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_input
            .update(cx, |chat_input, cx| chat_input.focus(window, cx));
    }

    /// Inserts a harness mention into the chat input and focuses it.
    pub fn insert_mention(&mut self, mention: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_input.update(cx, |chat_input, cx| {
            chat_input.insert_mention(mention, window, cx)
        });
    }

    /// The file open beside the chat, if any.
    pub fn open_file_view(&self) -> Option<Entity<FileView>> {
        self.file.clone()
    }

    #[cfg(test)]
    pub fn chat_input_view(&self) -> Entity<ChatInput> {
        self.chat_input.clone()
    }

    /// Opens a file beside the task view, in place of any already open.
    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, None, window, cx)
    }

    /// Opens a file clicked in a prompt or the output beside the task view.
    fn file_opener(&self, cx: &Context<Self>) -> OpenFile {
        let this = cx.entity().downgrade();
        Arc::new(move |path, window, cx| {
            this.update(cx, |this, cx| this.open_file(path, window, cx))
                .ok();
        })
    }

    /// Opens a file with the cursor at `position`, first asking whether to
    /// discard any unsaved changes to the file it replaces.
    fn open_file_at(
        &mut self,
        path: PathBuf,
        position: Option<lsp_types::Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.file.as_ref().filter(|file| file.read(cx).is_dirty()) else {
            return self.show_file(path, position, window, cx);
        };
        let title = file.read(cx).title();
        let this = cx.entity().downgrade();
        FileView::confirm_discard(title, window, cx, move |window, cx| {
            this.update(cx, |this, cx| {
                this.show_file(path.clone(), position, window, cx)
            })
            .ok();
        });
    }

    /// Closes the file open beside the chat, which slides back into the
    /// sidebar from the width it had.
    fn close_file_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(file) = self.file.take() {
            self.pane_closing = Some(PaneClosing {
                file,
                width: self.pane_width.get(),
                slide: self.pane_opened.map_or(0, |(n, _)| n),
                closed: Instant::now(),
            });
        }
        cx.notify();
    }

    /// A file or folder was renamed to `to`, or deleted: the file open beside
    /// the chat, when it is that file or inside that folder, opens again at
    /// its new path unless it has unsaved changes, or closes without asking.
    pub fn file_moved(
        &mut self,
        from: &Path,
        to: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.file.as_ref().map(|file| file.read(cx)) else {
            return;
        };
        let (open, dirty) = (file.path().to_path_buf(), file.is_dirty());
        let Ok(within) = open.strip_prefix(from) else {
            return;
        };
        match to {
            Some(_) if dirty => {}
            Some(to) => {
                let moved = if within.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(within)
                };
                self.show_file(moved, None, window, cx)
            }
            None => self.close_file_pane(cx),
        }
    }

    fn show_file(
        &mut self,
        path: PathBuf,
        position: Option<lsp_types::Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file.is_none() {
            // Fresh split sizes, so the file opens at its share of the width.
            self.file_split = cx.new(|_| ResizableState::default());
            self.pane_opened = Some((self.pane_opened.map_or(0, |(n, _)| n) + 1, Instant::now()));
            // A file still sliding closed gives way to this one.
            self.pane_closing = None;
        }
        let file = cx.new(|cx| FileView::new(path, position, window, cx));
        self._file_subscriptions = vec![
            // Text selected in the file and sent to the prompt is attached to it.
            cx.subscribe(&file, |this, _, SendToPrompt(text): &SendToPrompt, cx| {
                let text = text.clone();
                this.chat_input
                    .update(cx, |input, cx| input.attach_text(text, cx));
            }),
            cx.subscribe(&file, |this, _, _: &CloseFile, cx| this.close_file_pane(cx)),
            cx.subscribe_in(
                &file,
                window,
                |this, _, definition: &OpenDefinition, window, cx| {
                    this.open_file_at(
                        definition.path.clone(),
                        Some(definition.position),
                        window,
                        cx,
                    )
                },
            ),
        ];
        self.file = Some(file);
        cx.notify();
    }

    /// Adds a task for `text`, compiling, as the latest. Returns its index.
    fn push_task(&mut self, text: SharedString, cx: &mut Context<Self>) -> usize {
        self.tasks.push(PromptTask::new(text));
        // A new task's output starts at its top.
        self.scroll_output_to_top();
        cx.notify();
        self.tasks.len() - 1
    }

    /// Applies a harness event to the task at `ix`.
    fn apply_event(&mut self, ix: usize, event: HarnessEvent, cx: &mut Context<Self>) {
        if ix >= self.tasks.len() {
            return;
        }
        if self.in_background {
            self.tasks[ix].apply(event);
            return;
        }
        let latest = ix + 1 == self.tasks.len();
        // A raw output line only changes the raw tail at the end of the
        // output, so there is nothing to redraw while that is out of sight.
        let unseen = matches!(event, HarnessEvent::Output(_))
            && (!latest || self.task_history.expanded || !self.output_table.end_in_view());
        // Follows new output only while already scrolled to the bottom.
        let scroll = self.output_table.scroll();
        let following = scroll.offset().y <= -scroll.max_offset().y + px(1.);
        self.tasks[ix].apply(event);
        if unseen {
            return;
        }
        if (following || self.output_locked) && latest {
            self.output_table.scroll_to_end();
        }
        cx.notify();
    }

    fn scroll_output_to_top(&self) {
        // The output list is the screen's, not a background project's.
        if self.in_background {
            return;
        }
        self.output_table.scroll_to_top();
    }

    /// Heads the task at `ix` with the compiled markdown it was sent as, and
    /// the name of the anchor it was compiled from.
    fn show_compiled(
        &mut self,
        ix: usize,
        anchor: String,
        markdown: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self.tasks.get_mut(ix) {
            task.set_compiled(Compiled::new(anchor.into(), markdown));
        }
        cx.notify();
    }

    /// Compiles a prompt as it would be sent, without sending it or keeping
    /// it, for the chat input to preview.
    fn preview(
        &mut self,
        input: Entity<ChatInput>,
        preview: &PreviewPrompt,
        cx: &mut Context<Self>,
    ) {
        let id = preview.id;
        let Some(project_dir) = self.project_dir.clone() else {
            input.update(cx, |input, cx| {
                input.set_preview(id, Err("Open a project to preview the prompt.".into()), cx)
            });
            return;
        };
        let lsp = input.read(cx).lsp();
        let (text, mode, attached_text) = (
            preview.text.clone(),
            preview.mode,
            preview.attached_text.clone(),
        );
        let compile = cx.background_spawn(async move {
            let anchor = resolve_anchor(&text, mode, attached_text, lsp, &project_dir)?;
            hidden_anchor::preview(&anchor, &text, &project_dir)
        });
        self._preview = cx.spawn(async move |_, cx| {
            let compiled = compile
                .await
                .map(|compiled| compiled.user_prompt)
                .map_err(|err| format!("{err:#}"));
            input.update(cx, |input, cx| input.set_preview(id, compiled, cx));
        });
    }

    /// Sends `text` in `mode` now if the harness is free, or queues it. A
    /// question is always asked now.
    pub fn send(
        &mut self,
        text: String,
        mode: SendMode,
        attached_text: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_dir.is_none() {
            window.push_notification(
                Notification::error("Open a project before sending a prompt.")
                    .title("No project open"),
                cx,
            );
            return;
        }
        if mode == SendMode::Ask {
            self.ask(text, attached_text, cx);
        } else if self.working {
            self.enqueue(text, false, mode, attached_text, window, cx);
        } else {
            self.start(text, Sending::Now(mode, attached_text), cx);
        }
    }

    /// Replaces the queue with the one saved for the current project. A
    /// restored queue waits to be sent.
    fn load_queue(&mut self, cx: &mut Context<Self>) {
        let saved = self
            .project_dir
            .as_ref()
            .map(|project_dir| prompt_queue::load(project_dir))
            .unwrap_or_default();
        self.queue = saved
            .into_iter()
            .map(|saved| {
                self.next_queue_id += 1;
                QueueItem {
                    id: self.next_queue_id,
                    text: saved.text.clone().into(),
                    wait: false,
                    saved: Some(saved),
                }
            })
            .collect();
        self.queue_held = !self.queue.is_empty();
        cx.notify();
    }

    /// Replaces the tasks with those sent before in the current project, read
    /// in the background. Tasks are only replaced while the harness is free,
    /// so a run's task keeps its index; otherwise the history loads once the
    /// run is over, the run's own task among it.
    fn load_history(&mut self, cx: &mut Context<Self>) {
        if self.working {
            self.history_stale = true;
            return;
        }
        self.history_stale = false;
        let Some(project_dir) = self.project_dir.clone() else {
            self._history_load = Task::ready(());
            return;
        };
        let load_dir = project_dir.clone();
        let load = cx.background_spawn(async move {
            let project_dir = load_dir;
            let history = prompt_history::load(&project_dir);
            // Tasks sent now carry on the conversation the history left off.
            let session = Session::latest(&history, &project_dir);
            let tasks = history
                .into_iter()
                .map(PromptTask::restore)
                .collect::<Vec<_>>();
            (tasks, session)
        });
        self._history_load = cx.spawn(async move |this, cx| {
            let (tasks, session) = load.await;
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    if this.working {
                        this.history_stale = true;
                        return;
                    }
                    this.tasks = tasks;
                    this.session = session;
                    this.task_history.open = None;
                    this.scroll_output_to_top();
                    cx.notify();
                });
            })
            .ok();
        });
    }

    /// Sends the next queued prompt if auto send is on and the queue is not
    /// being held after a restore.
    fn auto_send_next(&mut self, cx: &mut Context<Self>) {
        if self.auto_send && !self.queue_held {
            self.send_next(cx);
        }
    }

    /// Adds `text` to the end of the queue, then resolves its hidden anchor,
    /// with the system prompt of `mode`, and saves it with the project in the
    /// background.
    fn enqueue(
        &mut self,
        text: String,
        wait: bool,
        mode: SendMode,
        attached_text: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        self.next_queue_id += 1;
        let id = self.next_queue_id;
        self.queue.push(QueueItem {
            id,
            text: text.clone().into(),
            wait,
            saved: None,
        });
        cx.notify();

        let lsp = self.chat_input.read(cx).lsp();
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let anchor = resolve_anchor(&text, mode, attached_text, lsp, &project_dir)?;
                prompt_queue::add(anchor, text, &project_dir)
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = save.await;
            this.update_in(cx, |this, window, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    this.queue_saved(id, saved, window, cx)
                });
            })
            .ok();
        })
        .detach();
    }

    fn queue_saved(
        &mut self,
        id: usize,
        saved: Result<QueuedPrompt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.queue.iter().position(|item| item.id == id) else {
            // Cancelled while it was saved.
            if let Ok(saved) = saved {
                prompt_queue::remove(&saved.file).ok();
            }
            return;
        };
        match saved {
            Ok(saved) => {
                self.queue[ix].saved = Some(saved);
                if !self.queue[ix].wait {
                    self.auto_send_next(cx);
                }
            }
            Err(err) => {
                self.queue.remove(ix);
                window.push_notification(
                    Notification::error(format!("{err:#}")).title("Could not queue the prompt"),
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// Edits the queued prompt `id` in the chat input, holding it in the queue
    /// meanwhile.
    fn edit_queued(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, item)) = self
            .queue
            .iter()
            .enumerate()
            .find(|(_, item)| item.id == id)
        else {
            return;
        };
        let Some(saved) = &item.saved else {
            return;
        };
        let (text, attached_text) = (saved.text.clone(), saved.anchor.attached_text.clone());
        let mode = anchor_mode(&saved.anchor).unwrap_or(SendMode::Both);
        // Another edit in progress simply gives way: the chat input puts back
        // what it set aside before setting it aside again.
        self.editing_queued = Some(id);
        self.chat_input.update(cx, |input, cx| {
            input.begin_editing(ix + 1, text, mode, attached_text, window, cx)
        });
        cx.notify();
    }

    /// Cancels editing a queued prompt, as switching projects does.
    pub fn cancel_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_queued.take().is_some() {
            self.chat_input
                .update(cx, |input, cx| input.cancel_editing(window, cx));
            cx.notify();
        }
    }

    /// Editing a queued prompt is over: saved, its new text takes its place
    /// in the queue, saved with the project where it was; either way the
    /// queue goes on.
    fn queued_edit_over(&mut self, edit: &QueuedEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing_queued.take() else {
            return;
        };
        let QueuedEdit::Saved {
            text,
            mode,
            attached_text,
        } = edit
        else {
            self.auto_send_next(cx);
            cx.notify();
            return;
        };
        let (Some(project_dir), Some(item)) = (
            self.project_dir.clone(),
            self.queue.iter_mut().find(|item| item.id == id),
        ) else {
            return;
        };
        let Some(old) = item.saved.take() else {
            return;
        };
        let old_text = std::mem::replace(&mut item.text, text.clone().into());
        cx.notify();
        let lsp = self.chat_input.read(cx).lsp();
        let (text, mode, attached_text) = (text.clone(), *mode, attached_text.clone());
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                match resolve_anchor(&text, mode, attached_text, lsp, &project_dir) {
                    Ok(anchor) => prompt_queue::replace(old.file.clone(), anchor, text)
                        .map_err(|err| (old, err)),
                    Err(err) => Err((old, err)),
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = save.await;
            this.update_in(cx, |this, window, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    let Some(item) = this.queue.iter_mut().find(|item| item.id == id) else {
                        // Cancelled while it saved.
                        if let Ok(saved) = &saved {
                            prompt_queue::remove(&saved.file).ok();
                        }
                        return;
                    };
                    match saved {
                        Ok(saved) => item.saved = Some(saved),
                        Err((old, err)) => {
                            // It stays as it was.
                            item.text = old_text;
                            item.saved = Some(old);
                            window.push_notification(
                                Notification::error(format!("{err:#}"))
                                    .title("Could not save the queued prompt"),
                                cx,
                            );
                        }
                    }
                    this.auto_send_next(cx);
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Keeps the chat input's note of where the prompt being edited is in the
    /// queue current.
    fn sync_editing_position(&self, cx: &mut Context<Self>) {
        let Some(id) = self.editing_queued else {
            return;
        };
        if let Some(ix) = self.queue.iter().position(|item| item.id == id) {
            self.chat_input
                .update(cx, |input, cx| input.set_editing_position(ix + 1, cx));
        }
    }

    /// Sends the first queued prompt, if the harness is free and it is saved.
    fn send_next(&mut self, cx: &mut Context<Self>) {
        // Without a project it could not be sent, and must stay queued; nor
        // while it is being edited.
        if self.working
            || self.project_dir.is_none()
            || !self.queue.first().is_some_and(|item| {
                item.saved.is_some() && (self.in_background || Some(item.id) != self.editing_queued)
            })
        {
            return;
        }
        let item = self.queue.remove(0);
        if self.queue.is_empty() {
            self.queue_held = false;
        }
        let Some(saved) = item.saved else { return };
        if !self.in_background {
            self.sync_editing_position(cx);
        }
        self.start(item.text.to_string(), Sending::Queued(saved), cx);
    }

    /// "Send next" was clicked: sends the first queued prompt, and releases a
    /// restored queue to auto send.
    fn send_next_clicked(&mut self, cx: &mut Context<Self>) {
        self.queue_held = false;
        self.send_next(cx);
    }

    /// Removes a prompt that has not been sent from the queue, and its file.
    fn cancel(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.queue.iter().position(|item| item.id == id) else {
            return;
        };
        // Cancelled while it is being edited, the edit goes too.
        if self.editing_queued == Some(id) {
            self.editing_queued = None;
            self.chat_input
                .update(cx, |input, cx| input.cancel_editing(window, cx));
        }
        let item = self.queue.remove(ix);
        if let Some(saved) = &item.saved
            && let Err(err) = prompt_queue::remove(&saved.file)
        {
            // Its file is still there, so it is still queued.
            self.queue.insert(ix, item);
            window.push_notification(
                Notification::error(format!("{err:#}")).title("Could not cancel the prompt"),
                cx,
            );
        }
        if self.queue.is_empty() {
            self.queue_held = false;
        }
        self.sync_editing_position(cx);
        cx.notify();
    }

    fn set_auto_send(&mut self, auto_send: bool, cx: &mut Context<Self>) {
        self.auto_send = auto_send;
        if auto_send {
            self.queue_held = false;
            self.send_next(cx);
        }
        cx.notify();
    }

    /// Locks the latest task's output to the bottom, or unlocks it.
    fn set_output_lock(&mut self, locked: bool, cx: &mut Context<Self>) {
        if self.output_locked == locked {
            return;
        }
        self.output_locked = locked;
        if self.output_locked {
            self.output_table.scroll_to_end();
        }
        cx.notify();
    }

    /// Replaces the previous answers with the questions saved in the current
    /// project, read in the background. Questions asked now carry on the
    /// conversation the last of them left off, unless one has been asked
    /// since.
    fn load_answers(&mut self, cx: &mut Context<Self>) {
        let Some(project_dir) = self.project_dir.clone() else {
            self.answers.clear();
            self._ask_history_load = Task::ready(());
            return;
        };
        let load_dir = project_dir.clone();
        let load = cx.background_spawn(async move {
            let project_dir = load_dir;
            let saved = prompt_history::load_asks(&project_dir);
            let session = Session::latest(&saved, &project_dir);
            let answers = saved
                .into_iter()
                .map(PromptTask::restore)
                .collect::<Vec<_>>();
            (answers, session)
        });
        self._ask_history_load = cx.spawn(async move |this, cx| {
            let (answers, session) = load.await;
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    // Those still open are not previous answers yet.
                    let open: Vec<SharedString> = this
                        .asks
                        .iter()
                        .filter_map(|ask| Some(ask.task.compiled.as_ref()?.anchor.clone()))
                        .collect();
                    this.answers = answers
                        .into_iter()
                        .filter(|answer| {
                            answer
                                .compiled
                                .as_ref()
                                .is_none_or(|compiled| !open.contains(&compiled.anchor))
                        })
                        .collect();
                    if this.ask_session.is_none() {
                        this.ask_session = session;
                    }
                    this.ask_history.open = None;
                    cx.notify();
                });
            })
            .ok();
        });
    }

    /// Sends `text` to the harness as a new task: a queued prompt with the
    /// anchor it was saved with, else with a freshly resolved one.
    fn start(&mut self, text: String, sending: Sending, cx: &mut Context<Self>) {
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        let task_ix = self.push_task(text.clone().into(), cx);
        self.tasks[task_ix].mode = match &sending {
            Sending::Now(mode, _) => Some(*mode),
            Sending::Queued(queued) => anchor_mode(&queued.anchor),
        };
        self.working = true;
        if !self.in_background {
            self.chat_input
                .update(cx, |input, cx| input.set_busy(true, cx));
        }

        let resume = Session::resume(&self.session, &project_dir);
        let lsp = self.chat_input.read(cx).lsp();
        // A prompt that works on the code or the spec is sent against a
        // freshly built spec.
        let builds = self.tasks[task_ix]
            .mode
            .is_none_or(|mode| mode != SendMode::Ask);
        if builds {
            self.tasks[task_ix].status = TaskStatus::Building;
        }
        // What the task asked, for its commit note.
        let asked = text.clone();
        let summarize = self.summarize;
        let build = builds.then(|| {
            let project_dir = project_dir.clone();
            cx.background_spawn(async move { piton_build::build(&project_dir) })
        });
        let compile = {
            let project_dir = project_dir.clone();
            async move {
                let anchor = match sending {
                    // Out of the queue first, so it is never sent twice.
                    Sending::Queued(queued) => {
                        prompt_queue::remove(&queued.file)?;
                        queued.anchor
                    }
                    Sending::Now(mode, attached_text) => {
                        resolve_anchor(&text, mode, attached_text, lsp, &project_dir)?
                    }
                };
                let file = hidden_anchor::save(&anchor, &text, &project_dir)?;
                let compiled = hidden_anchor::compile(&anchor, &file, &project_dir);
                anyhow::Ok((anchor.name().to_string(), file, compiled))
            }
        };

        self._pending = cx.spawn(async move |this, cx| {
            if let Some(build) = build {
                let built = build.await;
                // A failed build doesn't stop the prompt, which may well be the
                // one to fix the spec; it says so in the task's output.
                let failure = match built {
                    Ok(outcome) if outcome.success => None,
                    Ok(outcome) => Some(outcome.report),
                    Err(err) => Some(format!("could not run piton build: {err:#}")),
                };
                let updated = this.update(cx, |this, cx| {
                    this.in_project(&project_dir, cx, |this, cx| {
                        if let Some(task) = this.tasks.get_mut(task_ix) {
                            if task.status == TaskStatus::Building {
                                task.status = TaskStatus::Compiling;
                            }
                            if let Some(report) = failure {
                                task.reply.push_error(format!(
                                    "piton build failed, so the compiled spec may be out of date:\n{report}"
                                ));
                            }
                        }
                        cx.notify();
                    });
                });
                if updated.is_err() {
                    return;
                }
            }
            let compile = cx.background_spawn(compile);
            // What came of the prompt, saved beside it in the history once it
            // is over.
            let mut record = RunRecord::default();
            let mut prompt_file = None;
            // The harness's final summary, once the run finished without error.
            let mut finished: Option<String> = None;
            let compiled = compile.await.and_then(|(anchor, file, compiled)| {
                prompt_file = Some(file);
                Ok((anchor, compiled?))
            });
            match compiled {
                Ok((anchor, compiled)) => {
                    let prompt = compiled.user_prompt;
                    record.user_prompt = Some(prompt.clone());
                    let mut events = harness::send(
                        prompt.clone(),
                        compiled.system_prompt,
                        resume.clone().map(|session| harness::Resume {
                            session,
                            fork: false,
                        }),
                        project_dir.clone(),
                    );
                    if this
                        .update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, cx| {
                                this.show_compiled(task_ix, anchor, prompt, cx)
                            });
                        })
                        .is_err()
                    {
                        return;
                    }
                    let mut started = false;
                    while let Some(event) = events.next().await {
                        record.note(&event);
                        if let HarnessEvent::Finished {
                            is_error: false,
                            result,
                        } = &event
                        {
                            finished = Some(result.clone());
                        }
                        if this
                            .update(cx, |this, cx| {
                                let dir = project_dir.clone();
                                this.in_project(&dir, cx, |this, cx| {
                                    if let HarnessEvent::Session(id) = &event {
                                        started = true;
                                        this.session = Some(Session {
                                            project_dir: project_dir.clone(),
                                            id: id.clone(),
                                        });
                                    }
                                    this.apply_event(task_ix, event, cx)
                                });
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(resume) = resume.filter(|_| !started) {
                        this.update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, _| {
                                Session::forget(&mut this.session, &resume)
                            });
                        })
                        .ok();
                    }
                }
                Err(err) => {
                    let error = format!("{err:#}");
                    record.error = Some(error.clone());
                    this.update(cx, |this, cx| {
                        this.in_project(&project_dir, cx, |this, _| {
                            if let Some(task) = this.tasks.get_mut(task_ix) {
                                task.fail(error);
                            }
                        });
                    })
                    .ok();
                }
            }

            // Unsaved (the prompt itself could not be saved) there is nothing
            // to save it beside.
            let saved = match prompt_file {
                Some(file) => {
                    cx.background_spawn(async move { prompt_history::save_record(&file, &record) })
                        .await
                }
                None => Ok(()),
            };

            this.update(cx, |this, cx| {
                let dir = project_dir.clone();
                this.in_project(&dir, cx, |this, cx| {
                if let Some(task) = this.tasks.get_mut(task_ix) {
                    task.end();
                    if let Err(err) = saved {
                        // Shown without failing the task: the run itself is
                        // unaffected.
                        task.reply.push_error(format!(
                            "Could not save this task to the history: {err:#}"
                        ));
                    }
                }
                this.working = false;
                if this.history_stale {
                    this.load_history(cx);
                }
                if !this.in_background {
                    this.chat_input
                        .update(cx, |input, cx| input.set_busy(false, cx));
                }
                // A Code, Chain, or Spec task that finished well adds a note to
                // the next commit, written in the background.
                if builds
                    && let Some(result) = finished.take()
                    && this
                        .tasks
                        .get(task_ix)
                        .is_some_and(|task| task.status == TaskStatus::Done)
                {
                    add_commit_note(summarize, project_dir.clone(), asked, result, cx);
                }
                // Deferred: starting the next run replaces this task. Only this
                // project's queue sends, whichever project is on screen.
                let prompt_mode = cx.entity();
                let dir = project_dir.clone();
                cx.defer(move |cx| {
                    prompt_mode.update(cx, |this, cx| {
                        this.in_project(&dir, cx, |this, cx| this.auto_send_next(cx));
                    })
                });
                cx.notify();
                });
            })
            .ok();
        });
    }

    /// Asks `text` straight away, beside any task the harness is working on,
    /// in place of any question still open. It is saved apart from the
    /// history, so it never becomes one of the tasks.
    fn ask(&mut self, text: String, attached_text: Vec<String>, cx: &mut Context<Self>) {
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        let run = self.push_ask(text.clone().into(), cx);
        // Questions run at once. One that starts while another carries on the
        // conversation carries it on as a copy, so they don't write over
        // each other.
        let fork = self
            .asks
            .iter()
            .any(|ask| ask.id != run && ask.task.status.is_active());
        let resume = Session::resume(&self.ask_session, &project_dir);
        let lsp = self.chat_input.read(cx).lsp();
        let compile = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                // Every question is logged, even one whose anchor could not
                // be resolved: it is saved under a fresh anchor, with the
                // error recorded beside it.
                let (anchor, resolve_error) = match resolve_anchor(
                    &text,
                    SendMode::Ask,
                    attached_text.clone(),
                    lsp,
                    &project_dir,
                ) {
                    Ok(anchor) => (anchor, None),
                    Err(err) => {
                        let mut anchor = HiddenAnchor::random();
                        anchor.mode = Some(SendMode::Ask);
                        anchor.attached_text = attached_text;
                        (anchor, Some(err))
                    }
                };
                let file = hidden_anchor::save_ask(&anchor, &text, &project_dir);
                let compiled = match (resolve_error, &file) {
                    (Some(err), _) => Err(err),
                    (None, Ok(file)) => hidden_anchor::compile(&anchor, file, &project_dir),
                    (None, Err(_)) => Err(anyhow::anyhow!("the question was not saved")),
                };
                (
                    file,
                    compiled.map(|compiled| (anchor.name().to_string(), compiled)),
                )
            }
        });

        // Closing the question drops its run, which stops it.
        let task = cx.spawn(async move |this, cx| {
            let (file, compiled) = compile.await;
            // What came of the question, saved beside it once the run is over,
            // or stopped because the question was closed or replaced.
            let mut log = AskLog {
                file: file.as_ref().ok().cloned(),
                record: RunRecord::default(),
            };
            let compiled = match file {
                Ok(_) => compiled,
                Err(err) => Err(err),
            };
            match compiled {
                Ok((anchor, compiled)) => {
                    let prompt = compiled.user_prompt;
                    log.record.user_prompt = Some(prompt.clone());
                    let mut events = harness::send(
                        prompt.clone(),
                        compiled.system_prompt,
                        resume
                            .clone()
                            .map(|session| harness::Resume { session, fork }),
                        project_dir.clone(),
                    );
                    let compiled = Compiled::new(anchor.into(), prompt);
                    if this
                        .update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, cx| {
                                this.update_ask(run, |ask| ask.set_compiled(compiled), cx)
                            });
                        })
                        .is_err()
                    {
                        return;
                    }
                    let mut started = false;
                    while let Some(event) = events.next().await {
                        log.record.note(&event);
                        if this
                            .update(cx, |this, cx| {
                                let dir = project_dir.clone();
                                this.in_project(&dir, cx, |this, cx| {
                                    if let HarnessEvent::Session(id) = &event {
                                        started = true;
                                        this.ask_session = Some(Session {
                                            project_dir: project_dir.clone(),
                                            id: id.clone(),
                                        });
                                    }
                                    this.update_ask(run, |ask| ask.apply(event), cx)
                                });
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(resume) = resume.filter(|_| !started && !fork) {
                        this.update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, _| {
                                Session::forget(&mut this.ask_session, &resume)
                            });
                        })
                        .ok();
                    }
                }
                Err(err) => {
                    let error = format!("{err:#}");
                    log.record.error = Some(error.clone());
                    this.update(cx, |this, cx| {
                        this.in_project(&project_dir, cx, |this, cx| {
                            this.update_ask(run, |ask| ask.fail(error), cx)
                        });
                    })
                    .ok();
                }
            }
            // Once over, it opens onto its whole task table, in place of any
            // other.
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    this.update_ask(run, PromptTask::end, cx);
                    if this.asks.iter().any(|ask| ask.id == run) {
                        this.expand_ask(run, cx);
                    }
                });
            })
            .ok();
        });
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == run) {
            ask._run = task;
        }
    }

    /// Adds a question for `text`, compiling, below any others. Returns its
    /// id.
    fn push_ask(&mut self, text: SharedString, cx: &mut Context<Self>) -> usize {
        self.next_ask_id += 1;
        self.asks.push(Ask {
            id: self.next_ask_id,
            task: PromptTask::new(text),
            table: TaskTable::new(),
            locked: false,
            _run: Task::ready(()),
        });
        cx.notify();
        self.next_ask_id
    }

    /// Updates the question `id`, if it is still open.
    fn update_ask(
        &mut self,
        id: usize,
        update: impl FnOnce(&mut PromptTask),
        cx: &mut Context<Self>,
    ) {
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == id) {
            update(&mut ask.task);
            if ask.locked {
                ask.table.scroll_to_end();
            }
            cx.notify();
        }
    }

    /// Opens the finished question `id` onto its whole task table, closing
    /// any other down to its row.
    fn expand_ask(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(ask) = self.asks.iter().find(|ask| ask.id == id) {
            ask.table.scroll_to_top();
            self.expanded_ask = Some(id);
            cx.notify();
        }
    }

    /// Closes the question `id` down to its row.
    fn collapse_ask(&mut self, id: usize, cx: &mut Context<Self>) {
        if self.expanded_ask == Some(id) {
            self.expanded_ask = None;
            cx.notify();
        }
    }

    /// The question opened onto its whole task table, if it is still open.
    fn expanded(&self) -> Option<&Ask> {
        let id = self.expanded_ask?;
        self.asks
            .iter()
            .find(|ask| ask.id == id && ask.task.reply.is_done())
    }

    /// Whether the steps before the answer in the table for `task_ix` are
    /// shown, and how to show or hide them.
    fn steps(&self, task_ix: usize, cx: &Context<Self>) -> Steps {
        steps(task_ix, &self.steps_shown, &cx.entity())
    }

    /// The previous answers are expanded on the Ask tab.
    fn ask_history_shown(&self) -> bool {
        self.on_ask_tab && self.ask_history.expanded
    }

    fn drawer_contents(&self) -> DrawerContents {
        (
            self.expanded().map(|ask| ask.id),
            self.ask_history_shown().then_some(self.ask_history.opened),
        )
    }

    /// Whether the answer drawer is open: a question onto its table, or the
    /// previous answers expanded.
    fn drawer_open(&self) -> bool {
        self.drawer_contents() != (None, None)
    }

    /// How tall each thing filling the open drawer is: its share of the space
    /// above the chat input, less what the drawer's other rows take, split
    /// between the question and the previous answers if both are open.
    fn drawer_fill(&self) -> Pixels {
        let (question, answers) = self.drawer_contents();
        let open = question.iter().count() + answers.iter().count();
        let body = self.body_height.get();
        if open == 0 || body <= px(0.) {
            return px(360.);
        }
        let rest = (self.drawer_height.get() - self.drawer_fill_height.get()).max(px(0.));
        ((body * self.drawer_share - rest - self.drawer_bottom()) / open as f32)
            .max(MIN_DRAWER_FILL)
    }

    /// How far above the chat input the drawer sits: on top of any question
    /// rows still in the stack while the previous answers are expanded, so they
    /// stay in view, or right on the input otherwise.
    fn drawer_bottom(&self) -> Pixels {
        if self.ask_history_shown() {
            self.stack_rows_height.get()
        } else {
            px(0.)
        }
    }

    /// Whether the drawer follows a drag rather than sliding: it was dragged
    /// while what is open now was open.
    fn drawer_follows_drag(&self) -> bool {
        self.drawer_dragged == Some(self.drawer_contents())
    }

    /// Resizes the open drawer so its top is at `y`, within `body`, the space
    /// above the chat input.
    fn drag_drawer(&mut self, y: Pixels, body: Bounds<Pixels>, cx: &mut Context<Self>) {
        if !self.drawer_open() || body.size.height <= px(0.) {
            return;
        }
        self.drawer_share =
            ((body.bottom() - y) / body.size.height).clamp(MIN_DRAWER_SHARE, MAX_DRAWER_SHARE);
        self.drawer_dragged = Some(self.drawer_contents());
        cx.notify();
    }

    /// Closes the question `id`, stopping its run if it is still under way.
    fn close_ask(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(ix) = self.asks.iter().position(|ask| ask.id == id) {
            let mut ask = self.asks.remove(ix);
            // Stopped, if it was still running, and among the previous
            // answers from now on.
            ask.task.end();
            self.answers.push(ask.task);
        }
        if self.expanded_ask == Some(id) {
            self.expanded_ask = None;
        }
        cx.notify();
    }

    /// The questions, stacked above the chat input's tabs, the newest nearest
    /// them, each sliding up out of the input as it is asked and pushing the
    /// task view up to make room, so nothing of it is hidden. A question is a
    /// single row of its task table: the latest thing the harness did while it
    /// runs, or its status and first line once it is over. On the Ask tab, the
    /// previous answers' row heads the stack. What is open in the answer
    /// drawer leaves the stack.
    fn render_ask_stack(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.asks.is_empty() && !self.on_ask_tab {
            return None;
        }
        let expanded = self.expanded().map(|ask| ask.id);
        let history_row =
            (self.on_ask_tab && !self.ask_history_shown()).then(|| self.render_ask_history_row(cx));
        let cards: Vec<AnyElement> = self
            .asks
            .iter()
            .filter(|ask| expanded != Some(ask.id))
            .enumerate()
            // The stack's own top border, or the previous answers row's
            // bottom one, is the line above the first.
            .map(|(ix, ask)| self.render_ask_card(ask, false, ix > 0, cx))
            .collect();
        if history_row.is_none() && cards.is_empty() {
            self.stack_rows_height.set(px(0.));
            return None;
        }
        let theme = cx.theme();
        // The question rows are measured, so the previous answers can slide up
        // from on top of them.
        let rows_height = self.stack_rows_height.clone();
        // Its top line shows once it holds more than nothing, so the line never
        // lies on the chat input's own as the first question starts to rise.
        let has_content = history_row.is_some() || rows_height.get() > px(0.5);
        let rows = v_flex()
            .on_prepaint(move |bounds, _, _| rows_height.set(bounds.size.height))
            .children(cards);
        let stack = v_flex()
            .id("ask")
            .flex_none()
            .bg(theme.tab_bar)
            .when(has_content, |stack| stack.border_t_1())
            .border_color(theme.border)
            .children(history_row)
            .child(rows);
        // Lets UI tests find the questions; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(stack).into_any_element())
    }

    fn render_ask_history_row(&self, cx: &mut Context<Self>) -> AnyElement {
        self.ask_history.render_row(
            self.answers.len(),
            !self.answers.is_empty(),
            |this| &mut this.ask_history,
            cx,
        )
    }

    /// The answer drawer: a finished question open onto its whole table, or
    /// the previous answers expanded, sliding up out of the chat input over
    /// the task view and the stack of questions, which dim behind it. Its top
    /// edge resizes it.
    fn render_ask_drawer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.drawer_open() {
            return None;
        }
        let mut history: Vec<AnyElement> = Vec::new();
        if self.ask_history_shown() {
            history.push(self.render_ask_history_row(cx));
            let open_file = self.file_opener(cx);
            let list = self.ask_history.render_list(
                &self.answers,
                |this| &this.answers,
                |this| &mut this.ask_history,
                &self.steps_shown,
                &open_file,
                cx,
            );
            let fill = self.drawer_fill();
            let list = v_flex()
                .id(("ask-history", self.ask_history.opened))
                .flex_none()
                .overflow_hidden()
                .on_prepaint({
                    let filled = self.drawer_fill_height.clone();
                    move |bounds, _, _| filled.set(filled.get() + bounds.size.height)
                })
                .child(list);
            // Slides up each time it is expanded, unless it is being dragged
            // to size.
            history.push(if self.drawer_follows_drag() {
                list.h(fill).into_any_element()
            } else {
                let height = SpringAnimation::new(ASK_SPRING).to(fill).from(px(0.));
                list.with_spring(
                    ("ask-history-slide", self.ask_history.opened),
                    height,
                    |this, height| this.h(height),
                )
                .into_any_element()
            });
        }
        let card = self
            .expanded()
            // The drawer's top border is the line above it.
            .map(|ask| self.render_ask_card(ask, true, false, cx));
        let theme = cx.theme();
        let ring = theme.ring;
        let handle = div()
            .id("ask-drawer-resize")
            .group("ask-drawer-resize")
            .absolute()
            .top(-DRAWER_HANDLE_HEIGHT / 2.)
            .left_0()
            .right_0()
            .h(DRAWER_HANDLE_HEIGHT)
            .flex()
            .items_center()
            .cursor_row_resize()
            .child(
                div()
                    .w_full()
                    .h(px(2.))
                    .group_hover("ask-drawer-resize", |line| line.bg(ring)),
            )
            .on_drag(DrawerResize, |_, _, _, cx| cx.new(|_| EmptyView));
        // Lets UI tests find the edge; inert in normal builds.
        let handle = gpui_kit::TestSupportExt::test_support(handle);
        let drawer_height = self.drawer_height.clone();
        self.drawer_fill_height.set(px(0.));
        let drawer = v_flex()
            .id("ask-drawer")
            // Laid over what is beneath rather than beside it, taking no
            // clicks or scrolling meant for it.
            .absolute()
            .left_0()
            .right_0()
            .bottom(self.drawer_bottom())
            .occlude()
            .justify_end()
            .bg(theme.tab_bar)
            .border_t_1()
            .border_color(theme.border)
            .on_prepaint(move |bounds, _, _| drawer_height.set(bounds.size.height))
            // A drag that selects some of an answer's text offers to copy it
            // or attach it to the prompt, once the selection has settled.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|_, event: &MouseUpEvent, window, cx| {
                    let position = event.position;
                    cx.defer_in(window, move |this, window, cx| {
                        let text = TextSelection::selected_text(window, cx);
                        if !text.trim().is_empty() && this.drawer_open() {
                            this.selection_popover = Some((position, text));
                            cx.notify();
                        }
                    });
                }),
            )
            .children(history)
            .children(card)
            .child(handle);
        // Lets UI tests find the drawer; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(drawer).into_any_element())
    }

    /// Copies the text selected in an answer, closing the popover.
    fn copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, text)) = self.selection_popover.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        TextSelection::clear(window, cx);
        cx.notify();
    }

    /// Attaches the text selected in an answer to the prompt, closing the
    /// popover.
    fn attach_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, text)) = self.selection_popover.take() {
            self.chat_input
                .update(cx, |input, cx| input.attach_text(text, cx));
        }
        TextSelection::clear(window, cx);
        cx.notify();
    }

    /// The popover by text selected in an answer: Copy, and Attach to prompt.
    /// Pressing the mouse anywhere else closes it.
    fn render_selection_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (position, _) = self.selection_popover.as_ref()?;
        let this = cx.entity().downgrade();
        let actions = vec![
            SelectionAction::new("copy", IconName::Copy, "Copy", {
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |this, cx| this.copy_selection(window, cx))
                        .ok();
                }
            }),
            SelectionAction::new("attach", IconName::Paperclip, "Attach to prompt", {
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |this, cx| this.attach_selection(window, cx))
                        .ok();
                }
            }),
        ];
        Some(selection_popover(
            "selection",
            *position,
            actions,
            move |_, cx| {
                this.update(cx, |this, cx| {
                    this.selection_popover = None;
                    cx.notify();
                })
                .ok();
            },
            cx,
        ))
    }

    /// With `line_above`, the card draws the line between it and the card
    /// above it; otherwise whatever holds it draws that line.
    fn render_ask_card(
        &self,
        ask: &Ask,
        expanded: bool,
        line_above: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = ask.id;
        let task = &ask.task;
        let done = task.reply.is_done();
        let close = Button::new(("close-ask", id))
            .ghost()
            .xsmall()
            .icon(IconName::X)
            .tooltip(if done { "Close" } else { "Stop and close" })
            .on_click(cx.listener(move |this, _, _, cx| this.close_ask(id, cx)));

        let content: Vec<AnyElement> = if expanded {
            let open = self.file_opener(cx);
            let heading = h_flex()
                .id(("ask-heading", id))
                .flex_none()
                .gap_3()
                .px_4()
                .py_1p5()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.collapse_ask(id, cx)))
                .child(div().flex_none().child(task_title(ASK_IX - id, task, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(first_line(&task.text)),
                )
                .child(close);
            let reply_of = task_table::reply_of({
                let this = cx.entity().downgrade();
                move |cx| {
                    let this = this.upgrade()?.read(cx);
                    let ask = this.asks.iter().find(|ask| ask.id == id)?;
                    Some(&ask.task.reply)
                }
            });
            let this = cx.entity().downgrade();
            let toggle: SetLock = Rc::new(move |locked, _, cx| {
                this.update(cx, |this, cx| {
                    if let Some(ask) = this.asks.iter_mut().find(|ask| ask.id == id)
                        && ask.locked != locked
                    {
                        ask.locked = locked;
                        if ask.locked {
                            ask.table.scroll_to_end();
                        }
                        cx.notify();
                    }
                })
                .ok();
            });
            let output = ask.table.render(
                &task.reply,
                reply_of,
                TableView {
                    id: ("ask-output", id).into(),
                    scrollbar: format!("ask-output-{id}").into(),
                    table: ASK_IX - id,
                    open: Some(&open),
                    steps: Some(self.steps(ASK_IX - id, cx)),
                    lock: Some((ask.locked, toggle)),
                    padding: Edges {
                        top: px(0.),
                        right: px(16.),
                        bottom: px(12.),
                        left: px(16.),
                    },
                    max_height: None,
                },
                cx,
            );
            vec![heading.into_any_element(), output]
        } else {
            let summary = if done {
                // Over: its status and the question, opening onto its table.
                h_flex()
                    .gap_3()
                    .child(div().flex_none().child(task_title(ASK_IX - id, task, cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(first_line(&task.text)),
                    )
                    .into_any_element()
            } else {
                latest_row(id, &task.reply, cx)
            };
            let row = h_flex()
                .id(("ask-row", id))
                .flex_none()
                .gap_2()
                .px_4()
                .py_1p5()
                .when(done, |row| {
                    row.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.expand_ask(id, cx)))
                })
                .child(div().flex_1().min_w_0().child(summary))
                .child(close);
            // Lets UI tests find the row; inert in normal builds.
            vec![gpui_kit::TestSupportExt::test_support(row).into_any_element()]
        };

        let card = v_flex()
            .id(("ask-card", id))
            .flex_none()
            // Anchored to the chat input, so it rises out of it rather than
            // unrolling down onto it.
            .justify_end()
            .overflow_hidden()
            .border_color(cx.theme().border)
            .when(expanded, |card| {
                let filled = self.drawer_fill_height.clone();
                card.on_prepaint(move |bounds, _, _| filled.set(filled.get() + bounds.size.height))
            })
            .children(content);
        let card = gpui_kit::TestSupportExt::test_support(card);
        if !expanded {
            // Each question slides up from nothing, as far as its row.
            let height = SpringAnimation::new(ASK_SPRING)
                .to(ASK_ROW_HEIGHT)
                .from(px(0.));
            // Its line appears once there is more to it than the line, so the
            // line never lies on the chat input's own as it starts to rise.
            return card
                .with_spring(("ask-slide", id), height, move |this, height| {
                    this.max_h(height)
                        .when(line_above && height > px(1.5), |this| this.border_t_1())
                })
                .into_any_element();
        }
        // Open, it fills the drawer: sliding there from wherever its row is,
        // unless the drawer is being dragged to size.
        let fill = self.drawer_fill();
        if self.drawer_follows_drag() {
            return card.h(fill).into_any_element();
        }
        let height = SpringAnimation::new(ASK_SPRING).to(fill).from(px(0.));
        card.with_spring(("ask-slide", id), height, |this, height| this.h(height))
            .into_any_element()
    }

    /// A shade over everything above the chat input that fades in while the
    /// answer drawer is open over it, drawing the eye to the drawer, and back
    /// out once it closes. The stack of question rows pushes the list rather
    /// than covering it, so leaves it undimmed.
    fn render_ask_dim(&self, cx: &App) -> AnyElement {
        let dim = crate::theme::dimming(cx);
        let shade = SpringAnimation::new(ASK_DIM_SPRING)
            .to(if self.expanded().is_some() || self.ask_history_shown() {
                dim.a
            } else {
                0.
            })
            .from(0.);
        // Question rows the drawer sits on top of stay undimmed.
        let dim = div()
            .id("ask-dim")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom(self.drawer_bottom())
            .bg(black());
        // Lets UI tests find the shade; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(dim)
            .with_spring("ask-dim", shade, |this, shade| {
                this.opacity(shade.clamp(0., 1.))
            })
            .into_any_element()
    }

    /// The header pinned above the output: the latest task, unless the
    /// history is expanded, where it is listed.
    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (ix, task) = self
            .tasks
            .last()
            .filter(|_| !self.task_history.expanded)
            .map(|task| (self.tasks.len() - 1, task))?;
        let open = self.file_opener(cx);
        // Tinted like the tab the task was sent from, as the chat input's body
        // is: red for Code, purple for both, blue for Spec.
        let background = match task.mode {
            Some(mode) => cx.theme().background.blend(chat_input::mode_tint(mode, cx)),
            None => cx.theme().tab_bar,
        };
        let theme = cx.theme();
        let header = v_flex()
            .id("task-header")
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .bg(background)
            .border_b_1()
            .border_color(theme.border)
            .child(task_title(ix, task, cx))
            .child(
                div()
                    .id(("task-prompt", ix))
                    .max_h(MAX_PROMPT_HEIGHT)
                    .overflow_y_scroll()
                    .child(task_prompt(ix, task, &open, cx)),
            );
        // Lets UI tests find the header; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(header).into_any_element())
    }

    /// The queue along the bottom of the message list, as an expanding list
    /// with its controls. It only shows while something is queued.
    fn render_queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.queue.len();
        if count == 0 {
            return None;
        }
        let theme = cx.theme();
        let (border, muted, background) = (theme.border, theme.muted_foreground, theme.tab_bar);
        let expanded = self.queue_expanded;
        let next_ready = self.queue.first().is_some_and(|item| item.saved.is_some());
        let controls = {
            h_flex()
                .gap_3()
                .px_4()
                .py_1p5()
                .child(
                    Button::new("queue-toggle")
                        .ghost()
                        .small()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(format!("{count} queued"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.queue_expanded = !this.queue_expanded;
                            // It opens on the prompts queued last.
                            if this.queue_expanded {
                                this.queue_scroll.scroll_to_bottom();
                            }
                            cx.notify();
                        })),
                )
                .child(div().flex_1())
                .when(!self.working, |row| {
                    row.child(
                        Button::new("send-next")
                            .primary()
                            .small()
                            .label("Send next")
                            .disabled(!next_ready)
                            .on_click(cx.listener(|this, _, _, cx| this.send_next_clicked(cx))),
                    )
                })
                .child(
                    Switch::new("auto-send")
                        .label("Send next automatically")
                        .checked(self.auto_send)
                        .on_click(cx.listener(|this, auto_send: &bool, _, cx| {
                            this.set_auto_send(*auto_send, cx)
                        })),
                )
        };

        let list = expanded.then(|| {
            v_flex()
                .id("queue-list")
                .max_h(MAX_QUEUE_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&self.queue_scroll)
                .px_4()
                .pb_2()
                .gap_1()
                .children(self.queue.iter().enumerate().map(|(ix, item)| {
                    let id = item.id;
                    let editing = self.editing_queued == Some(id);
                    let row = h_flex()
                        .id(("queued-prompt", ix))
                        .gap_2()
                        .when(editing, |row| row.bg(theme.list_active))
                        .child(
                            div()
                                .flex_none()
                                .min_w_6()
                                .text_color(muted)
                                .child(format!("{}.", ix + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .line_clamp(2)
                                .child(item.text.clone()),
                        )
                        .when(item.saved.is_none(), |row| {
                            row.child(div().flex_none().child(Spinner::new().small()))
                        })
                        .child(
                            Button::new(("edit-queued", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Pencil)
                                .tooltip("Edit this prompt")
                                .selected(editing)
                                .disabled(item.saved.is_none())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_queued(id, window, cx)
                                })),
                        )
                        .child(
                            Button::new(("cancel-queued", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::X)
                                .tooltip("Cancel this prompt")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.cancel(id, window, cx)
                                })),
                        );
                    // Lets UI tests find the row; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(row)
                }))
        });

        Some(
            v_flex()
                .flex_none()
                .bg(background)
                .border_t_1()
                .border_color(border)
                .child(controls)
                .children(list.map(|list| {
                    scrollbar::with_scrollbar(
                        "queue-list",
                        &self.queue_scroll,
                        // Lets UI tests find the list; inert in normal builds.
                        gpui_kit::TestSupportExt::test_support(list),
                        false,
                        None,
                        cx,
                    )
                }))
                .into_any_element(),
        )
    }

    /// The latest task's output, filling the space under the header.
    fn render_output(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(task) = self.tasks.last() else {
            return div().into_any_element();
        };
        let task_ix = self.tasks.len() - 1;
        let this = cx.entity().downgrade();
        let reply_of = task_table::reply_of({
            let this = this.clone();
            move |cx| {
                let task = this.upgrade()?.read(cx).tasks.get(task_ix)?;
                Some(&task.reply)
            }
        });
        let open = self.file_opener(cx);
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| this.set_output_lock(locked, cx))
                .ok();
        });
        self.output_table.render(
            &task.reply,
            reply_of,
            TableView {
                id: "task-output".into(),
                scrollbar: "task-output".into(),
                table: task_ix,
                open: Some(&open),
                steps: None,
                lock: Some((self.output_locked, toggle)),
                padding: Edges::all(px(16.)),
                max_height: None,
            },
            cx,
        )
    }
}

impl Render for PromptMode {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The popover for text selected in an answer goes with the drawer.
        if !self.drawer_open() {
            self.selection_popover = None;
        }
        let hint = if ProjectDirectory::get(cx).is_some() {
            "Write a prompt below. Enter adds a line; Ctrl/Cmd+Enter sends it."
        } else {
            "Open a project to start prompting."
        };

        let content = if self.tasks.is_empty() {
            // The hint may shrink below one line, so it wraps in a narrow view
            // instead of running past its edges.
            let hint = div()
                .id("history-hint")
                .min_w_0()
                .text_center()
                .child(Label::new(hint));
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                // Lets UI tests find the hint; inert in normal builds.
                .child(gpui_kit::TestSupportExt::test_support(hint))
                .into_any_element()
        } else if self.task_history.expanded {
            let open_file = self.file_opener(cx);
            self.task_history.render_list(
                &self.tasks,
                |this| &this.tasks,
                |this| &mut this.task_history,
                &self.steps_shown,
                &open_file,
                cx,
            )
        } else {
            self.render_output(cx)
        };

        let history = v_flex()
            .id("history")
            .size_full()
            .child(self.task_history.render_row(
                // The latest task heads the view, so it isn't counted.
                self.tasks.len().saturating_sub(1),
                !self.tasks.is_empty(),
                |this| &mut this.task_history,
                cx,
            ))
            .children(self.render_header(cx))
            .child(content)
            .children(self.render_queue(cx));
        // Lets UI tests find the history; inert in normal builds.
        let history = gpui_kit::TestSupportExt::test_support(history);
        let history = div().relative().size_full().child(history);

        // The task view, and any file split off it.
        let body = div().relative().flex_1().min_h_0().on_prepaint({
            let body_width = self.body_width.clone();
            move |bounds, _, _| body_width.set(bounds.size.width)
        });
        // A file split off beside the task view, which can't be dragged
        // narrower than its editor's 80 columns.
        let pane = self
            .file
            .as_ref()
            .map(|file| (file.clone(), file.read(cx).min_width(window, cx)));
        // The width the pane opens at: its share of the body, or its narrowest.
        let pane_width = |file_min: Pixels| {
            if self.body_width.get() > px(0.) {
                (self.body_width.get() * FILE_SHARE).max(file_min)
            } else {
                file_min
            }
        };
        let sliding = self
            .pane_opened
            .filter(|(_, opened)| opened.elapsed() < PANE_SLIDE_TIME)
            .map(|(n, _)| n);
        if self
            .pane_closing
            .as_ref()
            .is_some_and(|closing| closing.closed.elapsed() >= PANE_SLIDE_TIME)
        {
            self.pane_closing = None;
        }
        // Records the pane's width as laid out, for it to slide closed from.
        let measured = |pane: AnyElement| {
            let pane_width = self.pane_width.clone();
            div()
                .size_full()
                .on_prepaint(move |bounds, _, _| pane_width.set(bounds.size.width))
                .child(pane)
        };
        let body = match pane {
            // Just opened, the pane grows out of the sidebar at its left, with
            // the file sliding into view from behind the sidebar's edge, over
            // the task view, which keeps its width until the slide settles, so
            // its rows aren't laid out anew each frame. Once it has, it is an
            // ordinary split that can be dragged.
            Some((file, file_min)) if sliding.is_some() && self.body_width.get() > px(0.) => {
                window.request_animation_frame();
                let width = pane_width(file_min);
                let grow = SpringAnimation::new(PANE_SPRING).to(width).from(px(0.));
                // Lets UI tests find the pane as it slides; inert in normal
                // builds.
                let pane = gpui_kit::TestSupportExt::test_support(
                    div().id(("pane-slide", sliding.unwrap_or(0))),
                )
                .relative()
                .flex_none()
                .h_full()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(width)
                        .child(measured(file.into_any_element())),
                )
                .with_spring(
                    ("pane-grow", sliding.unwrap_or(0)),
                    grow,
                    |this, width| this.w(width.max(px(0.))),
                );
                body.child(covered(history, pane, self.body_width.get(), cx))
            }
            Some((file, file_min)) => {
                let mut file_panel = resizable_panel().size_range(file_min..Pixels::MAX);
                // Not laid out yet: the split starts even, and can be dragged.
                if self.body_width.get() > px(0.) {
                    file_panel =
                        file_panel.size((self.body_width.get() * FILE_SHARE).max(file_min));
                }
                body.child(
                    h_resizable("file-split")
                        .with_state(&self.file_split)
                        .children([
                            file_panel.child(measured(file.into_any_element())),
                            resizable_panel()
                                .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                                .child(history),
                        ]),
                )
            }
            // Just closed, the pane shrinks back into the sidebar, with the
            // file sliding out of view behind the sidebar's edge, uncovering
            // the task view, already at its full width beneath it.
            None if let Some(closing) = &self.pane_closing => {
                window.request_animation_frame();
                let shrink = SpringAnimation::new(PANE_SPRING)
                    .to(px(0.))
                    .from(closing.width);
                // Lets UI tests find the pane as it slides; inert in normal
                // builds.
                let pane = gpui_kit::TestSupportExt::test_support(
                    div().id(("pane-slide-out", closing.slide)),
                )
                .relative()
                .flex_none()
                .h_full()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(closing.width)
                        .child(closing.file.clone()),
                )
                .with_spring(
                    ("pane-shrink", closing.slide),
                    shrink,
                    |this, width| this.w(width.max(px(0.))),
                );
                body.child(covered(history, pane, self.body_width.get(), cx))
            }
            None => body.child(history),
        };
        // Above the chat input: the task view, pushed up by the stack of
        // questions beneath it, with the answer drawer sliding up over both
        // and dimming them.
        let body = div()
            .id("prompt-body")
            .relative()
            .flex_1()
            .min_h_0()
            .on_prepaint({
                let body_height = self.body_height.clone();
                move |bounds, _, _| body_height.set(bounds.size.height)
            })
            // Dragging the answer drawer's top edge resizes it.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DrawerResize>, _, cx| {
                    this.drag_drawer(event.event.position.y, event.bounds, cx)
                }),
            )
            .child(
                v_flex()
                    .size_full()
                    .child(body)
                    .children(self.render_ask_stack(cx)),
            )
            .child(self.render_ask_dim(cx))
            .children(self.render_ask_drawer(cx))
            .children(self.render_selection_popover(cx));
        // Lets UI tests find the space above the chat input; inert in normal
        // builds.
        let body = gpui_kit::TestSupportExt::test_support(body);

        v_flex()
            .size_full()
            .child(body)
            .child(self.chat_input.clone())
    }
}

/// The hidden anchor `text` is sent as in `mode`: importing what `piton lsp`
/// resolved for it, with the mode's system prompt. Without `piton lsp`
/// nothing is imported, and any spec name the prompt uses fails to compile
/// with an explicit error.
fn resolve_anchor(
    text: &str,
    mode: SendMode,
    attached_text: Vec<String>,
    lsp: Option<Arc<PitonSession>>,
    project_dir: &Path,
) -> Result<HiddenAnchor> {
    let mut anchor = match lsp {
        Some(lsp) => lsp.anchor_for(text)?,
        None => HiddenAnchor::random(),
    };
    // Once a project has been prompted, each mode's system prompt can be
    // found in it and edited by hand. The defaults still apply if they
    // cannot be saved.
    system_prompts::save_missing(project_dir).ok();
    anchor.mode = Some(mode);
    anchor.attached_text = attached_text;
    anchor.system_prompt = hidden_anchor::system_prompt(mode, project_dir)?;
    Ok(anchor)
}

/// The mode an anchor was sent in: as saved with it, or for one saved before
/// modes were, as its system prompt tells.
fn anchor_mode(anchor: &HiddenAnchor) -> Option<SendMode> {
    anchor.mode.or_else(|| {
        anchor
            .system_prompt
            .as_deref()
            .and_then(hidden_anchor::mode_of)
    })
}

/// The first line of `text` with anything in it.
fn first_line(text: &str) -> SharedString {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_string()
        .into()
}

/// A task's status as a coloured label, a spinner while it is under way, and
/// the name of the hidden anchor it was compiled from once it has compiled.
fn task_title(ix: usize, task: &PromptTask, cx: &App) -> Div {
    let theme = cx.theme();
    let status = div()
        .id(("task-status", ix))
        .flex_none()
        .child(task.status.tag(cx));
    h_flex()
        .min_w_0()
        .gap_2()
        // Lets UI tests find the status; inert in normal builds.
        .child(gpui_kit::TestSupportExt::test_support(status))
        .when(task.status.is_active(), |row| {
            row.child(div().flex_none().child(Spinner::new().small()))
        })
        .when_some(task.compiled.as_ref(), |row, compiled| {
            let anchor = div()
                .id(("prompt-anchor", ix))
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .font_family(theme.mono_font_family.clone())
                .child(compiled.anchor.clone());
            // Lets UI tests find the anchor; inert in normal builds.
            row.child(gpui_kit::TestSupportExt::test_support(anchor))
        })
}

/// The prompt a task was sent as: the compiled markdown the harness received,
/// or the text as typed until it compiles. Its links to files open them.
fn task_prompt(ix: usize, task: &PromptTask, open: &OpenFile, cx: &App) -> AnyElement {
    match &task.compiled {
        Some(compiled) => {
            let prompt = div()
                .id(("compiled-prompt", ix))
                .min_w_0()
                .child(shown_markdown_view(
                    prompt_key(ix),
                    compiled.shown(),
                    Some(open),
                    cx,
                ));
            // Lets UI tests find the compiled prompt; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(prompt).into_any_element()
        }
        None => div()
            .min_w_0()
            .text_color(cx.theme().muted_foreground)
            .child(task.text.clone())
            .into_any_element(),
    }
}

/// A task in the expanded list, on one line: its title, then the start of its
/// prompt as typed.
fn task_summary(id: (&'static str, usize), ix: usize, task: &PromptTask, cx: &App) -> AnyElement {
    let summary = h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .gap_3()
        .child(div().flex_none().child(task_title(ix, task, cx)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(first_line(&task.text)),
        );
    // Lets UI tests find the task; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(summary).into_any_element()
}

/// The steps of prompt mode's table for `task_ix`, which a click shows or
/// hides.
fn steps(task_ix: usize, shown: &HashSet<usize>, entity: &Entity<PromptMode>) -> Steps {
    let this = entity.downgrade();
    let shown = shown.contains(&task_ix);
    Steps {
        shown,
        toggle: Rc::new(move |_, cx| {
            this.update(cx, |this, cx| {
                if !this.steps_shown.remove(&task_ix) {
                    this.steps_shown.insert(task_ix);
                }
                cx.notify();
            })
            .ok();
        }),
    }
}

/// A reply's latest row, the thing the harness is doing now, as the only row
/// of its task table, on a single line: the last line of its text, a tool
/// call's name and input, or the harness's latest raw output while nothing
/// else is known. A row with no status of its own shows a spinner while the
/// reply streams.
fn latest_row(id: usize, reply: &Reply, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(row) = reply.last_row() else {
        return div()
            .text_color(theme.muted_foreground)
            .child("No output.")
            .into_any_element();
    };
    let muted = |text: SharedString| {
        div()
            .min_w_0()
            .truncate()
            .text_color(theme.muted_foreground)
            .child(text)
            .into_any_element()
    };
    let raw_line = || match reply.raw_line(reply.raw_tail.len().wrapping_sub(1), cx) {
        Some(line) => div()
            .min_w_0()
            .truncate()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(line)
            .into_any_element(),
        None => muted("Waiting for the harness…".into()),
    };
    let detail = match row {
        OutputRow::Text(text) if !text.trim().is_empty() => {
            let line = text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default();
            div()
                .min_w_0()
                .truncate()
                .child(line.to_string())
                .into_any_element()
        }
        OutputRow::Tool(call) => {
            let project_dir = ProjectDirectory::get(cx);
            let input = match &call.summary {
                Some(summary) => div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .child(relative_to_project(summary, project_dir.as_deref()))
                    .into_any_element(),
                None => match ToolKind::of(&call.name).pending_input() {
                    Some(label) => muted(label.into()),
                    None => raw_line(),
                },
            };
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .when_some(call.shown_name(), |row, name| {
                    row.child(div().flex_none().font_medium().child(name.to_string()))
                })
                .child(div().flex_1().min_w_0().child(input))
                .into_any_element()
        }
        OutputRow::Error(error) => div()
            .min_w_0()
            .truncate()
            .child(first_line(error))
            .into_any_element(),
        OutputRow::Text(_) | OutputRow::Pending => raw_line(),
    };
    let status = row_status(&row, cx)
        .or_else(|| (!reply.is_done()).then(|| Spinner::new().small().into_any_element()));
    let detail = div()
        .id(("ask-row-detail", id))
        .w_full()
        .min_w_0()
        .child(detail);

    Table::new()
        // Legible at the window's base font size; tables are otherwise smaller.
        .text_base()
        .line_height(relative(1.5))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(
            TableBody::new().child(
                TableRow::new()
                    .child(
                        TableCell::new()
                            .w(KIND_WIDTH)
                            .flex_none()
                            .child(row.badge(cx)),
                    )
                    .child(
                        TableCell::new()
                            .flex_1()
                            .min_w_0()
                            // Lets UI tests find the detail; inert in normal builds.
                            .child(gpui_kit::TestSupportExt::test_support(detail)),
                    )
                    .child(
                        TableCell::new()
                            .w(STATUS_WIDTH)
                            .flex_none()
                            .children(status),
                    ),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

    use super::{OutputRow, PromptMode, PromptTask, Reply, Session, TaskStatus};
    use crate::harness::HarnessEvent;
    use crate::hidden_anchor::HiddenAnchor;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::prompt_history::{RunRecord, SavedPrompt};
    use crate::prompt_queue;
    use crate::task_table::{RAW_LINE_CHARS, ReplyPart, ToolState};

    fn tool(id: &str, name: &str) -> HarnessEvent {
        HarnessEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Opens prompt mode in a window.
    fn open(cx: &mut TestAppContext) -> (Entity<PromptMode>, AnyWindowHandle) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut prompt_mode = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| PromptMode::new(window, cx));
            prompt_mode = Some(view.clone());
            Root::new(view, window, cx)
        });
        (prompt_mode.unwrap(), window.into())
    }

    /// A queued prompt is edited in the chat input, held meanwhile, and saved
    /// back in its place; a prompt queued on purpose while the harness is free
    /// waits rather than being sent.
    #[gpui_kit::test]
    async fn queued_prompts_are_edited_in_place_and_queued_on_purpose(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-queue-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n\nbelay-config Belay:\n    codeRoot: ./src\n",
        )
        .unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        for text in ["first", "second"] {
            prompt_queue::add(HiddenAnchor::random(), text.into(), &dir).unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| {
            crate::chat_input::bind_keys(cx);
            ProjectDirectory::set(dir.clone(), cx)
        });
        cx.run_until_parked();
        let chat = prompt_mode.read_with(cx, |this, _| this.chat_input_view());
        let (first, second) = prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.queued_texts(), ["first", "second"]);
            (this.queue[0].id, this.queue[1].id)
        });

        // Edited, the first prompt is held: Send next sends nothing.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.edit_queued(first, window, cx);
                this.send_next_clicked(cx);
                assert!(this.tasks.is_empty(), "an edited prompt was sent");
                // Editing another cancels the first edit.
                this.edit_queued(second, window, cx);
            });
            window.render_frame(cx);
            assert_eq!(chat.read(cx).editor_text_for_test(cx), "second");
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| {
            assert_eq!(this.editing_queued, Some(second));
            this.set_working(true);
        });
        cx.update_window(handle, |_, window, cx| {
            chat.update(cx, |input, cx| {
                input.set_text_for_test("second, edited", window, cx)
            });
            window.press("ctrl-enter", cx);
        })
        .unwrap();
        let start = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let saved = prompt_mode.read_with(cx, |this, _| {
                this.queue[1].saved.as_ref().map(|saved| saved.text.clone())
            });
            if saved.as_deref() == Some("second, edited") {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "never saved");
            std::thread::sleep(Duration::from_millis(20));
        }
        let on_disk: Vec<String> = prompt_queue::load(&dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(on_disk, ["first", "second, edited"]);
        prompt_mode.read_with(cx, |this, _| assert_eq!(this.editing_queued, None));

        // Queued on purpose with the harness free, it waits.
        prompt_mode.update(cx, |this, cx| {
            this.set_working(false);
            this.queue.retain(|item| item.id == second);
            this.auto_send = true;
            this.queue_held = false;
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            chat.update(cx, |input, cx| {
                input.set_text_for_test("on purpose", window, cx);
                input.pick_send_option(1, window, cx);
            });
        })
        .unwrap();
        let start = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let queued = prompt_mode.read_with(cx, |this, _| {
                this.queue
                    .last()
                    .is_some_and(|item| item.saved.is_some() && item.text.as_ref() == "on purpose")
            });
            if queued {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "never queued");
            std::thread::sleep(Duration::from_millis(20));
        }
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.tasks.is_empty(), "queued on purpose, it was sent");
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Previewing from the chat input compiles the prompt against the project,
    /// and shows what the harness would receive, sending nothing.
    #[gpui_kit::test]
    async fn the_chat_input_previews_a_compiled_prompt(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| {
            crate::chat_input::bind_keys(cx);
            ProjectDirectory::set(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx)
        });
        cx.run_until_parked();
        let history_files = || {
            std::fs::read_dir(crate::hidden_anchor::history_dir(std::path::Path::new(
                env!("CARGO_MANIFEST_DIR"),
            )))
            .map(|dir| dir.count())
            .unwrap_or(0)
        };
        let before = history_files();
        let chat = prompt_mode.read_with(cx, |this, _| this.chat_input_view());
        cx.update_window(handle, |_, window, cx| {
            chat.update(cx, |input, cx| {
                input.set_text_for_test("Preview me", window, cx);
                input.pick_send_option(0, window, cx);
            });
        })
        .unwrap();
        let start = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let preview = chat.read_with(cx, |input, _| input.preview().cloned());
            match preview {
                Some(crate::chat_input::Preview::Compiled(markdown)) => {
                    assert_eq!(markdown, "Preview me");
                    break;
                }
                Some(crate::chat_input::Preview::Failed(error)) => panic!("{error}"),
                _ => {}
            }
            assert!(start.elapsed() < Duration::from_secs(20), "never compiled");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            history_files(),
            before,
            "the preview was saved to the history"
        );
    }

    /// Two folders, each an empty project, named for a test.
    fn two_projects(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dirs = [base.join("alpha"), base.join("beta")];
        for dir in &dirs {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("piton.config.pi"), "").unwrap();
        }
        let [a, b] = dirs.map(|dir| std::fs::canonicalize(dir).unwrap());
        (a, b)
    }

    /// Switching projects leaves the work of the one left running in the
    /// background: its run still writes into it, never into the project on
    /// screen, and switching back shows it just as it is, loading nothing
    /// again.
    #[gpui_kit::test]
    async fn a_project_left_keeps_its_work_running(cx: &mut TestAppContext) {
        let (a, b) = two_projects("background-work");
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.run_until_parked();
        let (task, question) = cx
            .update_window(handle, |_, _, cx| {
                prompt_mode.update(cx, |this, cx| {
                    let ix = this.push_task("Work in alpha".into(), cx);
                    this.working = true;
                    this.apply_event(ix, HarnessEvent::TextStarted, cx);
                    let question = this.start_test_question("Ask in alpha", cx);
                    (ix, question)
                })
            })
            .unwrap();

        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, cx| {
            assert!(this.tasks.is_empty() && this.asks.is_empty() && !this.working);
            assert!(this.is_working(), "alpha's work stopped counting");
            let busy = this.busy_projects();
            assert_eq!(busy.len(), 1);
            assert_eq!(busy[0].project_dir, a);
            assert_eq!(busy[0].task.as_deref(), Some("Work in alpha"));
            assert_eq!(busy[0].questions, [(question, "Ask in alpha".into())]);
            let jobs = this.running_jobs();
            assert_eq!(
                jobs.iter()
                    .map(|job| job.title.to_string())
                    .collect::<Vec<_>>(),
                ["Task · alpha", "Question · alpha"]
            );
            assert!(jobs.iter().all(|job| job.project.as_ref() == Some(&a)));

            // Alpha's run writes back into alpha, not beta on screen.
            this.in_project(&a, cx, |this, cx| {
                this.apply_event(task, HarnessEvent::TextDelta("Still going.".into()), cx);
                this.update_ask(question, |ask| ask.end(), cx);
            });
            assert!(this.tasks.is_empty() && this.asks.is_empty());
            assert_eq!(this.project_dir.as_ref(), Some(&b));
        });

        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| {
            assert_eq!(this.tasks.len(), 1, "alpha's tasks were loaded again");
            assert!(this.working);
            assert!(this.tasks[0].reply.parts.iter().any(
                |part| matches!(part, ReplyPart::Text(text) if text.contains("Still going."))
            ));
            assert_eq!(this.asks.len(), 1);
            assert!(!this.asks[0].task.status.is_active());
            // Now on screen, alpha's jobs are no longer headed with its name.
            assert_eq!(
                this.running_jobs()
                    .iter()
                    .map(|job| job.title.to_string())
                    .collect::<Vec<_>>(),
                ["Task"]
            );
        });
        std::fs::remove_dir_all(a.parent().unwrap()).ok();
    }

    /// A reply is a row per piece of text and per tool call, in the order they
    /// happened; text with nothing in it yet is left out.
    #[test]
    fn output_rows_follow_the_reply_in_order() {
        let mut reply = Reply::default();
        for event in [
            tool("t1", "Read"),
            HarnessEvent::TextStarted,
            tool("t2", "Bash"),
            HarnessEvent::TextStarted,
            HarnessEvent::TextDelta("Now the edits.".into()),
            tool("t3", "Edit"),
            HarnessEvent::ToolFinished {
                id: "t2".into(),
                is_error: true,
            },
        ] {
            reply.apply(event);
        }

        let rows: Vec<String> = reply
            .rows()
            .iter()
            .map(|row| match row {
                OutputRow::Text(text) => format!("text {text}"),
                OutputRow::Tool(call) => format!("{} {}", call.name, call.state.label()),
                OutputRow::Error(error) => format!("error {error}"),
                OutputRow::Pending => "pending".into(),
            })
            .collect();
        assert_eq!(
            rows,
            [
                "Read running",
                "Bash failed",
                "text Now the edits.",
                "Edit running"
            ]
        );
    }

    /// Until a reply is done its last row is still filling in: a pending row
    /// while nothing is known of what comes next, text with no words yet, or a
    /// tool call with no input yet. A finished reply has no such row.
    #[test]
    fn unfinished_reply_ends_in_a_row_still_filling_in() {
        let mut reply = Reply::default();
        assert_eq!(reply.rows(), [OutputRow::Pending]);

        reply.apply(HarnessEvent::TextStarted);
        assert_eq!(reply.rows(), [OutputRow::Text("")]);
        reply.apply(HarnessEvent::TextDelta("Looking.".into()));
        assert_eq!(
            reply.rows(),
            [OutputRow::Text("Looking."), OutputRow::Pending]
        );

        reply.apply(tool("t1", "Read"));
        assert!(matches!(
            reply.rows().as_slice(),
            [OutputRow::Text(_), OutputRow::Tool(call)] if call.summary.is_none()
        ));
        reply.apply(HarnessEvent::ToolInput {
            id: "t1".into(),
            summary: "a.rs".into(),
        });
        assert_eq!(reply.rows().last(), Some(&OutputRow::Pending));

        reply.apply(HarnessEvent::TextStarted);
        reply.apply(HarnessEvent::Finished {
            is_error: false,
            result: String::new(),
        });
        assert!(matches!(
            reply.rows().as_slice(),
            [OutputRow::Text("Looking."), OutputRow::Tool(call)] if call.state == ToolState::Done
        ));
    }

    /// A reply keeps only the harness's last few raw output lines, each cut
    /// short, to show while nothing else is known.
    #[test]
    fn reply_keeps_the_tail_of_its_raw_output() {
        let mut reply = Reply::default();
        for line in ["1", "2", "3", "4"] {
            reply.apply(HarnessEvent::Output(line.into()));
        }
        assert_eq!(reply.raw_tail, ["2", "3", "4"]);
        assert_eq!(reply.rows(), [OutputRow::Pending]);

        reply.apply(HarnessEvent::Output("é".repeat(RAW_LINE_CHARS * 2)));
        assert_eq!(
            reply.raw_tail.back().map(|line| line.chars().count()),
            Some(RAW_LINE_CHARS)
        );
    }

    /// A tool call's name is left out of its output when its badge names it
    /// and there is a summary; otherwise the name is kept.
    #[test]
    fn tool_name_is_shown_only_when_the_badge_does_not_say_it() {
        let call = |name: &str, summary: Option<&str>| {
            let mut reply = Reply::default();
            reply.apply(tool("t", name));
            if let Some(summary) = summary {
                reply.apply(HarnessEvent::ToolInput {
                    id: "t".into(),
                    summary: summary.into(),
                });
            }
            let shown = match reply.row(0) {
                Some(OutputRow::Tool(call)) => call.shown_name().map(str::to_string),
                row => panic!("{row:?}"),
            };
            shown
        };
        assert_eq!(call("Read", Some("/tmp/a.txt")), None);
        assert_eq!(call("Bash", Some("ls")), None);
        assert_eq!(call("WebSearch", Some("gpui")), None);
        assert_eq!(call("Bash", None), Some("Bash".to_string()));
        assert_eq!(
            call("TodoWrite", Some("plan")),
            Some("TodoWrite".to_string())
        );
        assert_eq!(
            call("mcp__backlog__list", None),
            Some("mcp__backlog__list".to_string())
        );
    }

    /// The raw output is highlighted as JSON, even a line cut off mid-object.
    #[test]
    fn raw_output_is_highlighted_as_json() {
        use gpui_kit::component::highlighter::HighlightTheme;

        let theme = HighlightTheme::default_dark();
        for line in [
            r#"{"type":"stream_event","index":0,"done":true}"#,
            r#"{"type":"stream_event","event":{"delta":{"partial_js"#,
        ] {
            let colored = crate::task_table::json_highlights(line, &theme)
                .into_iter()
                .filter(|(_, style)| style.color.is_some())
                .count();
            assert!(colored > 1, "{line}");
        }
    }

    /// A tool of a known kind says what it is getting ready to do while its
    /// input streams in; any other tool shows the raw output.
    #[test]
    fn known_tools_replace_the_raw_output_while_their_input_streams() {
        use super::ToolKind;

        for name in ["Read", "Grep", "Edit", "Write", "Bash", "WebFetch", "Task"] {
            assert!(ToolKind::of(name).pending_input().is_some(), "{name}");
        }
        assert_eq!(ToolKind::of("mcp__backlog__list").pending_input(), None);
    }

    /// Paths in the project directory are shown relative to it; anything
    /// outside it, or only sharing its name's start, is left as it is.
    #[test]
    fn project_paths_are_shown_relative() {
        use std::path::Path;

        use super::relative_to_project;

        let dir = Some(Path::new("/home/u/proj"));
        assert_eq!(
            relative_to_project("/home/u/proj/src/main.rs", dir),
            "src/main.rs"
        );
        assert_eq!(relative_to_project("/home/u/proj", dir), ".");
        assert_eq!(
            relative_to_project(
                "cd /home/u/proj/ && ls '/home/u/proj/spec' /home/u/proj2",
                dir
            ),
            "cd . && ls 'spec' /home/u/proj2"
        );
        assert_eq!(
            relative_to_project("/tmp/home/u/proj/a", dir),
            "/tmp/home/u/proj/a"
        );
        assert_eq!(
            relative_to_project("/home/u/proj/a", None),
            "/home/u/proj/a"
        );
    }

    /// A task compiles, runs, and is done once the harness finishes; a failed
    /// run adds its error as a row, and a run stopped without a result fails.
    #[test]
    fn task_status_follows_its_run() {
        let mut task = PromptTask::new("Do it".into());
        assert_eq!(task.status, TaskStatus::Compiling);
        task.set_compiled(super::Compiled::new("Prompt_0".into(), "Do it".into()));
        assert_eq!(task.status, TaskStatus::Running);
        task.apply(HarnessEvent::Finished {
            is_error: false,
            result: "All done.".into(),
        });
        task.end();
        assert_eq!(task.status, TaskStatus::Done);

        let mut failed = PromptTask::new("Do it".into());
        failed.set_compiled(super::Compiled::new("Prompt_1".into(), "Do it".into()));
        failed.apply(HarnessEvent::Failed("no harness".into()));
        failed.end();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(failed.reply.rows(), [OutputRow::Error("no harness")]);

        let mut stopped = PromptTask::new("Do it".into());
        stopped.set_compiled(super::Compiled::new("Prompt_2".into(), "Do it".into()));
        stopped.apply(tool("t1", "Bash"));
        stopped.end();
        assert_eq!(stopped.status, TaskStatus::Failed);
        assert!(matches!(
            stopped.reply.rows().as_slice(),
            [OutputRow::Tool(call), OutputRow::Error(_)] if call.state == ToolState::Failed
        ));
    }

    /// A task from the history replays its recorded output into the task it
    /// was; one with no record is shown as not recorded, with nothing pending.
    #[test]
    fn restored_tasks_replay_their_records() {
        let mut record = RunRecord {
            user_prompt: Some("Do it, compiled".into()),
            ..RunRecord::default()
        };
        for line in [
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"t1","name":"Read"}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Done."}}}"#,
            r#"{"type":"result","is_error":false,"result":"Done."}"#,
        ] {
            record.note(&HarnessEvent::Output(line.into()));
        }
        let anchor = HiddenAnchor::random();
        let name = anchor.name().to_string();
        let task = PromptTask::restore(SavedPrompt {
            anchor,
            text: "Do it".into(),
            record: Some(record),
        });
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(task.compiled.as_ref().unwrap().anchor.as_ref(), name);
        assert!(matches!(
            task.reply.rows().as_slice(),
            [OutputRow::Tool(call), OutputRow::Text("Done.")] if call.state == ToolState::Done
        ));

        let unrecorded = PromptTask::restore(SavedPrompt {
            anchor: HiddenAnchor::random(),
            text: "Old".into(),
            record: None,
        });
        assert_eq!(unrecorded.status, TaskStatus::Unrecorded);
        assert!(unrecorded.reply.done && unrecorded.compiled.is_none());
    }

    /// Tasks carry on the latest conversation in the history, passing over
    /// runs that never reported one; a session is only resumed in its own
    /// project, and one the harness could not resume is forgotten.
    #[test]
    fn sessions_resume_the_latest_conversation() {
        let saved = |lines: &[&str]| {
            let mut record = RunRecord::default();
            for line in lines {
                record.note(&HarnessEvent::Output((*line).into()));
            }
            SavedPrompt {
                anchor: HiddenAnchor::random(),
                text: "Do it".into(),
                record: Some(record),
            }
        };
        let history = [
            saved(&[r#"{"type":"system","subtype":"init","session_id":"old"}"#]),
            saved(&[r#"{"type":"system","subtype":"init","session_id":"latest"}"#]),
            saved(&["not json"]),
            SavedPrompt {
                anchor: HiddenAnchor::random(),
                text: "Old".into(),
                record: None,
            },
        ];
        let project = std::path::Path::new("/project");
        let mut session = Session::latest(&history, project);
        assert_eq!(
            Session::resume(&session, project).as_deref(),
            Some("latest")
        );
        assert_eq!(
            Session::resume(&session, std::path::Path::new("/other")),
            None
        );

        Session::forget(&mut session, "old");
        assert!(session.is_some(), "a different session was forgotten");
        Session::forget(&mut session, "latest");
        assert!(session.is_none());
        assert!(Session::latest(&history[2..], project).is_none());
    }

    /// The latest task heads the view with its status, its anchor and the
    /// compiled prompt; its output rows sit beneath, in order, wrapping inside
    /// the window.
    #[gpui_kit::test]
    async fn latest_task_heads_its_output_table(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let long = "Update `MainWindowScope` so its toolbar uses `ProjectDirectoryScope` \
                    and runs `piton build` in the selected project directory. "
            .repeat(12);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task(long.clone().into(), cx);
                this.show_compiled(ix, "Prompt_0123456789abcdef".into(), long.clone(), cx);
                for event in [
                    tool("t1", "Read"),
                    HarnessEvent::ToolInput {
                        id: "t1".into(),
                        summary: "/home/user/project/src/a/very/long/path/to/a/file.rs".repeat(8),
                    },
                    HarnessEvent::ToolFinished {
                        id: "t1".into(),
                        is_error: false,
                    },
                    HarnessEvent::TextStarted,
                    HarnessEvent::TextDelta(long.clone()),
                    tool("t2", "Bash"),
                ] {
                    this.apply_event(ix, event, cx);
                }
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("output-row", 2usize)).is_some()
                && window.try_find(("compiled-prompt", 0usize)).is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            let viewport = window.viewport_size();
            let header = window.find("task-header").bounds();
            for id in ["task-status", "prompt-anchor", "compiled-prompt"] {
                let bounds = window.find((id, 0usize)).bounds();
                assert!(
                    bounds.top() >= header.top() && bounds.top() < header.bottom(),
                    "{id} is not in the header: {bounds:?} in {header:?}"
                );
            }
            let mut above = header.bottom();
            for ix in 0..3usize {
                let bounds = window.find(("output-row", ix)).bounds();
                assert!(
                    bounds.right() <= viewport.width,
                    "row {ix} runs past the window: {bounds:?} in {viewport:?}"
                );
                assert!(
                    bounds.top() >= above,
                    "row {ix} is out of order: {bounds:?} under {above:?}"
                );
                above = bounds.bottom();
            }
            let text = window.find(("output-row", 1usize)).bounds();
            assert!(
                text.size.height > window.line_height() * 4.0,
                "the reply text did not wrap: {text:?} in {viewport:?}"
            );
        })
        .unwrap();
    }

    /// Clicking the file a file tool read opens it beside the task view.
    #[gpui_kit::test]
    async fn clicking_a_tool_file_opens_it(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-tool-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "# Notes\n").unwrap();

        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Read it".into(), cx);
                this.show_compiled(ix, "Prompt_0".into(), "Read it".into(), cx);
                this.apply_event(ix, tool("t1", "Read"), cx);
                this.apply_event(
                    ix,
                    HarnessEvent::ToolInput {
                        id: "t1".into(),
                        summary: file.display().to_string(),
                    },
                    cx,
                );
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("output-file", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("output-file", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(prompt_mode.read_with(cx, |this, _| this.file.is_some()));

        // Text the file sends to the prompt is attached to it.
        let file_view = prompt_mode.read_with(cx, |this, _| this.file.clone().unwrap());
        file_view.update(cx, |_, cx| {
            cx.emit(crate::file_view::SendToPrompt("selected text".into()))
        });
        cx.run_until_parked();
        let attached = prompt_mode.read_with(cx, |this, cx| {
            this.chat_input
                .read(cx)
                .attachments()
                .iter()
                .map(|attachment| attachment.text.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(attached, ["selected text"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With nothing sent yet, the instructions wrap inside a view too narrow
    /// for them to fit on one line.
    #[gpui_kit::test]
    async fn hint_wraps_in_a_narrow_history(cx: &mut TestAppContext) {
        let (_, handle) = open(cx);
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(180.), gpui_kit::px(600.)));

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("history-hint").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let viewport = window.viewport_size();
            let bounds = window.find("history-hint").bounds();
            assert!(
                bounds.left() >= gpui_kit::px(0.) && bounds.right() <= viewport.width,
                "the hint runs past the history: {bounds:?} in {viewport:?}"
            );
            assert!(
                bounds.size.height > window.line_height() * 1.5,
                "the hint did not wrap: {bounds:?} in {viewport:?}"
            );
        })
        .unwrap();
    }

    /// The row above the header is shown even before anything is sent, and
    /// stays shown with a single task.
    #[gpui_kit::test]
    async fn history_row_is_always_shown(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("history-toggle").is_some()
        })
        .await;

        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("only task".into(), cx);
                this.show_compiled(ix, format!("Prompt_{ix}"), "only task".into(), cx);
            });
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("task-header").is_some() && window.try_find("history-toggle").is_some()
        })
        .await;
    }

    /// The row above the header expands it into a list of every task; opening
    /// an earlier one shows its output.
    #[gpui_kit::test]
    async fn history_row_expands_into_every_task(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for text in ["first task", "second task"] {
                    let ix = this.push_task(text.into(), cx);
                    this.show_compiled(ix, format!("Prompt_{ix}"), text.into(), cx);
                    this.apply_event(ix, HarnessEvent::TextDelta(format!("Output {ix}")), cx);
                }
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("history-toggle").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.click("history-toggle", cx))
            .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("history-task", 0usize)).is_some()
                && window.try_find(("history-task", 1usize)).is_some()
                && window.try_find("task-output").is_none()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            window.click(("history-task", 0usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("output-row", 0usize)).is_some()
        })
        .await;
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.task_history.open),
            Some(0)
        );
    }

    /// Each list that expands opens scrolled to its most recent items: the
    /// previous tasks, the previous answers as their drawer slides up, and
    /// the queue, each time it is opened, however it was scrolled before.
    #[gpui_kit::test]
    async fn expanded_lists_open_on_their_latest_items(cx: &mut TestAppContext) {
        const COUNT: usize = 40;
        let dir = std::env::temp_dir().join(format!("suspense-latest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for n in 0..12 {
            prompt_queue::add(HiddenAnchor::random(), format!("queued {n}"), &dir).unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        // The project's saved history loads first, so it doesn't replace
        // what's added here.
        for _ in 0..20 {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for n in 0..COUNT {
                    let ix = this.push_task(format!("task {n}").into(), cx);
                    this.show_compiled(ix, format!("Prompt_{ix}"), format!("task {n}").into(), cx);
                    this.apply_event(ix, HarnessEvent::TextDelta(format!("Output {n}")), cx);
                    this.apply_event(
                        ix,
                        HarnessEvent::Finished {
                            is_error: false,
                            result: String::new(),
                        },
                        cx,
                    );
                }
                this.working = true;
                this.on_ask_tab = true;
                for n in 0..COUNT {
                    let id = this.push_ask(format!("question {n}").into(), cx);
                    this.update_ask(
                        id,
                        |ask| {
                            ask.apply(HarnessEvent::TextStarted);
                            ask.apply(HarnessEvent::TextDelta(format!("Answer {n}")));
                            ask.apply(HarnessEvent::Finished {
                                is_error: false,
                                result: String::new(),
                            });
                        },
                        cx,
                    );
                    this.close_ask(id, cx);
                }
                this.on_ask_tab = false;
                assert_eq!(this.queue.len(), 12);
            });
        })
        .unwrap();

        // Renders until whatever slides has settled.
        let settle = |cx: &mut TestAppContext| {
            for _ in 0..8 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
                cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(40));
            }
        };
        // Whether the row `id` is shown, and in view within `list`.
        let in_view = |cx: &mut TestAppContext, list: &'static str, id: gpui_kit::ElementId| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let list = window.find(list).bounds();
                window.try_find(id).is_some_and(|row| {
                    let row = row.bounds();
                    row.top() >= list.top() - gpui_kit::px(1.)
                        && row.bottom() <= list.bottom() + gpui_kit::px(1.)
                })
            })
            .unwrap()
        };

        for (toggle, list, item) in [
            ("history-toggle", "task-list-scroll", "history-task"),
            ("ask-history-toggle", "ask-list-scroll", "ask-history-task"),
        ] {
            if toggle == "ask-history-toggle" {
                prompt_mode.update(cx, |this, cx| {
                    this.on_ask_tab = true;
                    cx.notify();
                });
            }
            for opening in 0..2 {
                settle(cx);
                cx.update_window(handle, |_, window, cx| window.click(toggle, cx))
                    .unwrap();
                settle(cx);
                assert!(
                    in_view(cx, list, (item, COUNT - 1).into()),
                    "{list} didn't open on its latest item, opening {opening}"
                );
                assert!(!in_view(cx, list, (item, 0usize).into()));
                // Scrolled away to the top before it is closed.
                prompt_mode.update(cx, |this, _| {
                    let history = if toggle == "history-toggle" {
                        &this.task_history
                    } else {
                        &this.ask_history
                    };
                    history.rows.state().scroll_to(gpui_kit::ListOffset {
                        item_ix: 0,
                        offset_in_item: gpui_kit::px(0.),
                    });
                });
                settle(cx);
                assert!(in_view(cx, list, (item, 0usize).into()));
                cx.update_window(handle, |_, window, cx| window.click(toggle, cx))
                    .unwrap();
            }
            prompt_mode.update(cx, |this, cx| {
                this.on_ask_tab = false;
                cx.notify();
            });
        }

        for opening in 0..2 {
            settle(cx);
            cx.update_window(handle, |_, window, cx| window.click("queue-toggle", cx))
                .unwrap();
            settle(cx);
            assert!(
                in_view(cx, "queue-list", ("queued-prompt", 11usize).into()),
                "the queue didn't open on its latest prompt, opening {opening}"
            );
            prompt_mode.update(cx, |this, _| {
                this.queue_scroll
                    .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)))
            });
            settle(cx);
            cx.update_window(handle, |_, window, cx| window.click("queue-toggle", cx))
                .unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// On the Ask tab, a row of previous answers, the same as the row of
    /// previous tasks, expands into every question asked before, each
    /// opening onto its output. It shows only on the Ask tab, and dims the
    /// message list while expanded there.
    #[gpui_kit::test]
    async fn ask_tab_lists_previous_answers_like_previous_tasks(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                // Off the Ask tab, there is no row.
                assert!(this.render_ask_stack(cx).is_none());
                this.on_ask_tab = true;
                for text in ["first question", "second question"] {
                    let id = this.push_ask(text.into(), cx);
                    this.update_ask(
                        id,
                        |ask| {
                            ask.apply(HarnessEvent::TextStarted);
                            ask.apply(HarnessEvent::TextDelta(format!(
                                "Answer to {text}. {}\n\n- {}\n- {}\n\nEnd.",
                                "Words that go on. ".repeat(30),
                                "A long bullet that keeps going on and on. ".repeat(12),
                                "Short one."
                            )));
                            ask.apply(HarnessEvent::Finished {
                                is_error: false,
                                result: String::new(),
                            });
                        },
                        cx,
                    );
                    this.close_ask(id, cx);
                }
                assert!(this.asks.is_empty());
                assert_eq!(
                    this.answers.len(),
                    2,
                    "closed questions are not previous answers"
                );
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("ask-history-toggle").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click("ask-history-toggle", cx)
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(2), |window, _| {
            window.try_find(("ask-history-task", 0usize)).is_some()
                && window.try_find(("ask-history-task", 1usize)).is_some()
        })
        .await;
        assert!(prompt_mode.read_with(cx, |this, _| this.ask_history_shown()));

        // The list slides up before its answers can be clicked.
        let start = std::time::Instant::now();
        let mut last = None;
        loop {
            let height = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.find("ask-list-scroll").bounds().size.height
                })
                .unwrap();
            if last == Some(height) && height > gpui_kit::px(0.) {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the list never settled"
            );
            last = Some(height);
            std::thread::sleep(Duration::from_millis(50));
        }
        cx.update_window(handle, |_, window, cx| {
            window.click(("ask-history-task", 1usize), cx)
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window
                .try_find(("history-panel", super::ASK_HISTORY_IX + 1))
                .is_some()
        })
        .await;
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.ask_history.open),
            Some(1)
        );
        // An answer with long lines wraps them within the list, so the open
        // item ends just below its table, with no space left beneath it.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            let list = window.find("ask-list-scroll").bounds();
            let panel = window
                .find(("history-panel", super::ASK_HISTORY_IX + 1))
                .bounds();
            let end = window
                .find(("history-panel-end", super::ASK_HISTORY_IX + 1))
                .bounds();
            let row = window.find(("output-row", 0usize)).bounds();
            for (part, bounds) in [("prompt", panel), ("table", row), ("end", end)] {
                assert!(
                    bounds.right() <= list.right(),
                    "the open answer's {part} {bounds:?} is wider than its list {list:?}"
                );
            }
            assert!(
                end.bottom() - row.bottom() < gpui_kit::px(32.),
                "{:?} of space below the answer's last row",
                end.bottom() - row.bottom()
            );
            // The open answer is the last item, so its end is the list's last
            // row.
            let rows = &prompt_mode.read(cx).ask_history.rows;
            let item = rows.state().bounds_for_item(rows.count() - 1).unwrap();
            assert!(
                (item.bottom() - end.bottom()).abs() < gpui_kit::px(2.),
                "the list measured the item {item:?} taller than it is drawn"
            );
        })
        .unwrap();

        // Off the Ask tab, the answers go, and the list is undimmed.
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = false;
            cx.notify();
        });
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("ask-history-toggle").is_none()
        })
        .await;
        assert!(!prompt_mode.read_with(cx, |this, _| this.ask_history_shown()));
    }

    /// Questions saved with the project load back as previous answers, their
    /// output replayed from their records.
    #[gpui_kit::test]
    async fn saved_questions_load_as_previous_answers(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-answers-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let anchor = HiddenAnchor::random();
        let file = crate::hidden_anchor::save_ask(&anchor, "What is it?", &dir).unwrap();
        let mut record = RunRecord {
            user_prompt: Some("What is it?".into()),
            ..RunRecord::default()
        };
        record.note(&HarnessEvent::Output(
            r#"{"type":"result","is_error":false,"result":"It is a REPL."}"#.into(),
        ));
        crate::prompt_history::save_record(&file, &record).unwrap();

        let (prompt_mode, _handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        let start = std::time::Instant::now();
        while prompt_mode.read_with(cx, |this, _| this.answers.is_empty()) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no answers loaded"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.answers.len(), 1);
            assert_eq!(this.answers[0].text.as_ref(), "What is it?");
            assert_eq!(this.answers[0].status, TaskStatus::Done);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opening a project restores its queue, listed along the bottom under
    /// the task's output; it expands to list each queued prompt, and
    /// cancelling one removes its file. Once the harness is free with
    /// auto-send off, the queue waits.
    #[gpui_kit::test]
    async fn queue_is_below_the_output_and_can_be_cancelled(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for text in ["first queued", "second queued"] {
            prompt_queue::add(HiddenAnchor::random(), text.into(), &dir).unwrap();
        }

        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                assert_eq!(this.queue.len(), 2, "the saved queue was not restored");
                this.push_task("Working on this".into(), cx);
                this.working = true;
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("task-header").is_some() && window.try_find("task-output").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.click("queue-toggle", cx))
            .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("queued-prompt", 1usize)).is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            let task = window.find("task-header").bounds();
            let queued = window.find(("queued-prompt", 0usize)).bounds();
            let output = window.find("task-output").bounds();
            assert!(
                task.bottom() <= output.top() && output.bottom() <= queued.top(),
                "queue is not below the output: task {task:?}, output {output:?}, queued {queued:?}"
            );
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            window.click(("cancel-queued", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        let texts: Vec<String> = prompt_queue::load(&dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(texts, ["second queued"]);
        let queued: Vec<String> = prompt_mode.read_with(cx, |this, _| {
            this.queue
                .iter()
                .map(|item| item.text.to_string())
                .collect()
        });
        assert_eq!(queued, ["second queued"]);

        // Auto-send off, then the run ends: the queue waits for Send next.
        cx.update_window(handle, |_, window, cx| window.click("auto-send", cx))
            .unwrap();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                assert!(!this.auto_send, "the switch did not turn auto-send off");
                this.working = false;
                cx.notify();
            });
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("send-next").is_some()
        })
        .await;
        assert_eq!(prompt_mode.read_with(cx, |this, _| this.queue.len()), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A question is asked while the harness works on a task, without
    /// queueing it or touching the task, and is kept out of the history.
    #[gpui_kit::test]
    async fn asking_runs_beside_the_working_task(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;

        // No piton.config.pi, so the question fails before reaching the
        // harness.
        let dir = std::env::temp_dir().join(format!("suspense-ask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.push_task("Working on this".into(), cx);
                this.working = true;
                this.send("Why?".into(), SendMode::Ask, Vec::new(), window, cx);
                assert!(this.queue.is_empty(), "the question was queued");
                assert_eq!(this.tasks.len(), 1);
                assert_eq!(this.asks.len(), 1, "the question was not asked");
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(5), |_, cx| {
            prompt_mode
                .read(cx)
                .asks
                .first()
                .is_some_and(|ask| ask.task.status == TaskStatus::Failed)
        })
        .await;
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.working, "the task stopped working");
            assert_eq!(this.tasks[0].status, TaskStatus::Compiling);
        });
        assert!(!crate::hidden_anchor::history_dir(&dir).exists());

        // Even a question that failed is logged in the project's asks: the
        // question itself, and beside it the record of why it failed.
        let asks = dir.join(crate::hidden_anchor::APP_DIR).join("asks");
        let logged = |extension: &str| -> Vec<std::path::PathBuf> {
            std::fs::read_dir(&asks)
                .into_iter()
                .flatten()
                .filter_map(|entry| Some(entry.ok()?.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == extension))
                .collect()
        };
        let questions = logged("pi");
        assert_eq!(questions.len(), 1, "the question was not logged");
        let question = std::fs::read_to_string(&questions[0]).unwrap();
        assert!(question.contains("Why?"), "{question}");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while logged("json").is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let records = logged("json");
        assert_eq!(records.len(), 1, "no record was logged beside the question");
        let record: crate::prompt_history::RunRecord =
            serde_json::from_str(&std::fs::read_to_string(&records[0]).unwrap()).unwrap();
        assert!(record.error.is_some(), "{record:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    type Bounds = gpui_kit::Bounds<gpui_kit::Pixels>;

    /// Draws frames until `id` is laid out with `settled` holding of it and
    /// the chat input's tabs.
    fn settle(
        handle: AnyWindowHandle,
        id: impl Into<gpui_kit::ElementId> + Clone + std::fmt::Debug,
        settled: impl Fn(Bounds, Bounds) -> bool,
        cx: &mut TestAppContext,
    ) -> (Bounds, Bounds) {
        let start = std::time::Instant::now();
        loop {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let bounds = window.try_find(id.clone())?.bounds();
                    Some((bounds, window.find("chat-tabs").bounds()))
                })
                .unwrap();
            if let Some((bounds, tabs)) = found
                && settled(bounds, tabs)
            {
                return (bounds, tabs);
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{id:?} did not settle: {found:?}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// A running question rises out of the chat input rather than appearing at
    /// its full height, pushing the message list up so none of it is hidden,
    /// and undimmed. Opened onto its whole table, it becomes a drawer that
    /// slides up over the message list instead, and the shade covers all the
    /// space above the chat input.
    #[gpui_kit::test]
    async fn a_running_question_pushes_the_list_up_and_an_answer_slides_over_it(
        cx: &mut TestAppContext,
    ) {
        let (prompt_mode, handle) = open(cx);
        let find = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).map(|found| found.bounds())
            })
            .unwrap()
        };
        let history = find("history", cx).unwrap();
        let body = find("prompt-body", cx).unwrap();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.push_ask("What does the chain do?".into(), cx);
            });
        })
        .unwrap();

        let mut heights = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            heights.push(find("ask", cx).unwrap().size.height);
            std::thread::sleep(Duration::from_millis(8));
        }
        let settled = *heights.last().unwrap();
        assert!(settled > gpui_kit::px(0.), "the question never rose");
        assert!(
            heights
                .iter()
                .any(|&height| height > gpui_kit::px(0.5) && height < settled - gpui_kit::px(0.5)),
            "the question {heights:?} appeared without sliding up"
        );
        let (list, ask) = (find("history", cx).unwrap(), find("ask", cx).unwrap());
        assert!(
            (list.bottom() - ask.top()).abs() <= gpui_kit::px(1.)
                && (list.size.height + ask.size.height - history.size.height).abs()
                    <= gpui_kit::px(1.),
            "the question {ask:?} did not push the message list {list:?} up from {history:?}"
        );
        assert!(find("ask-drawer", cx).is_none());

        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.update_ask(
                    1,
                    |ask| {
                        ask.apply(HarnessEvent::TextStarted);
                        ask.apply(HarnessEvent::TextDelta("It joins Code and Spec.".into()));
                        ask.end();
                    },
                    cx,
                );
                this.expand_ask(1, cx);
            });
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let drawer = find("ask-drawer", cx).expect("the answer is not in a drawer");
        let list = find("history", cx).unwrap();
        assert_eq!(
            list, history,
            "the answer pushed the message list instead of covering it"
        );
        assert!(
            drawer.top() < list.bottom(),
            "the drawer {drawer:?} is not over the list"
        );
        assert_eq!(
            find("ask-dim", cx).unwrap(),
            body,
            "the shade does not cover the space"
        );
    }

    /// While the harness works on a question, it is a single row directly
    /// above the chat input's tabs, sliding up out of them; once it is over,
    /// it expands into its whole task table, still above the tabs.
    #[gpui_kit::test]
    async fn a_question_is_a_row_above_the_tabs_until_it_is_over(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let run = this.push_ask("What does the chain do?".into(), cx);
                assert_eq!(run, 1);
                for event in [
                    tool("t1", "Read"),
                    HarnessEvent::ToolInput {
                        id: "t1".into(),
                        summary: "src/chat_input.rs".into(),
                    },
                ] {
                    this.update_ask(run, |ask| ask.apply(event), cx);
                }
            });
        })
        .unwrap();

        // The row rises until it is whole, its bottom on the tabs' top.
        let line_height = cx
            .update_window(handle, |_, window, _| window.line_height())
            .unwrap();
        let (row, tabs) = settle(
            handle,
            ("ask-row", 1usize),
            |row, _| row.size.height > gpui_kit::px(0.),
            cx,
        );
        let (panel, _) = settle(
            handle,
            "ask",
            |panel, _| panel.top() <= row.top() + gpui_kit::px(0.5),
            cx,
        );
        assert!(
            (panel.bottom() - tabs.top()).abs() <= gpui_kit::px(1.),
            "the question {panel:?} is not right above the tabs {tabs:?}"
        );
        assert!(
            row.size.height < line_height * 4.,
            "the question {row:?} is more than a single row"
        );
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find(("ask-output", 1usize)).is_none());
        })
        .unwrap();

        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.update_ask(
                    1,
                    |ask| {
                        ask.apply(HarnessEvent::TextStarted);
                        ask.apply(HarnessEvent::TextDelta("It joins Code and Spec.".into()));
                        ask.apply(HarnessEvent::Finished {
                            is_error: false,
                            result: String::new(),
                        });
                        ask.end();
                    },
                    cx,
                );
                this.expand_ask(1, cx);
            });
        })
        .unwrap();

        let (output, tabs) = settle(
            handle,
            ("ask-output", 1usize),
            |output, _| output.size.height > line_height * 3.,
            cx,
        );
        assert!(
            output.bottom() <= tabs.top() + gpui_kit::px(1.),
            "the expanded question {output:?} is not above the tabs {tabs:?}"
        );
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find(("ask-row", 1usize)).is_none());
            assert!(window.try_find(("output-row", 1usize)).is_some());
        })
        .unwrap();
    }

    /// A finished question shows only its answer, with the steps before it
    /// collapsed behind a row that a click expands and collapses again; a
    /// task shows its whole chain.
    #[gpui_kit::test]
    async fn an_answer_collapses_its_steps_but_a_task_does_not(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let events = || {
            [
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("Let me look.".into()),
                tool("t1", "Read"),
                HarnessEvent::ToolFinished {
                    id: "t1".into(),
                    is_error: false,
                },
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("It joins Code and Spec.".into()),
            ]
        };
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("What does the chain do?".into(), cx);
                for event in events() {
                    this.apply_event(ix, event, cx);
                }
                let id = this.push_ask("What does the chain do?".into(), cx);
                this.update_ask(
                    id,
                    |ask| {
                        for event in events() {
                            ask.apply(event);
                        }
                        ask.end();
                    },
                    cx,
                );
                this.expand_ask(id, cx);
            });
        })
        .unwrap();
        let steps = ("output-steps", super::ASK_IX - 1);
        let rows = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let within = |window: &mut gpui_kit::Window, id: &'static str| {
                    let scoped = window.within(id);
                    (0..3usize)
                        .filter(|row| scoped.try_find(("output-row", *row)).is_some())
                        .count()
                };
                (
                    window.try_find(steps).is_some(),
                    within(window, "ask-drawer"),
                    within(window, "task-output"),
                )
            })
            .unwrap()
        };
        cx.wait_for(handle, Duration::from_secs(2), |window, _| {
            window.try_find(steps).is_some()
        })
        .await;

        // Counted once the drawer has slid open: only the rows in view are
        // laid out.
        let (row, _) = settle(
            handle,
            steps,
            |row, _| row.size.height > gpui_kit::px(0.),
            cx,
        );
        let _ = settle(
            handle,
            "ask-drawer",
            |panel, _| panel.top() <= row.top(),
            cx,
        );
        std::thread::sleep(Duration::from_millis(400));
        let (toggle, answer_rows, task_rows) = rows(cx);
        assert!(toggle, "the answer has no row for its steps");
        assert_eq!(answer_rows, 1, "only the answer shows");
        assert_eq!(task_rows, 3, "the task collapsed its chain");
        cx.update_window(handle, |_, window, cx| window.click(steps, cx))
            .unwrap();
        cx.run_until_parked();
        let (toggle, answer_rows, _) = rows(cx);
        assert!(toggle, "expanding the steps took away their row");
        assert_eq!(answer_rows, 3, "the steps did not expand");

        cx.update_window(handle, |_, window, cx| window.click(steps, cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(rows(cx).1, 1, "the steps did not collapse again");
    }

    /// Selecting some of an answer's text offers a popover to copy it or attach
    /// it to the prompt; either closes the popover, and attaching lists the
    /// text above the chat input.
    #[gpui_kit::test]
    async fn selected_answer_text_can_be_copied_or_attached(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let id = this.push_ask("What does the chain do?".into(), cx);
                this.update_ask(
                    id,
                    |ask| {
                        ask.apply(HarnessEvent::TextStarted);
                        ask.apply(HarnessEvent::TextDelta(
                            "It joins Code and Spec into one prompt.".into(),
                        ));
                        ask.end();
                    },
                    cx,
                );
                this.expand_ask(id, cx);
            });
        })
        .unwrap();
        let (row, _) = settle(
            handle,
            ("output-row", 0usize),
            |row, _| row.size.height > gpui_kit::px(0.),
            cx,
        );
        let _ = settle(
            handle,
            "ask-drawer",
            |drawer, _| drawer.top() <= row.top(),
            cx,
        );
        std::thread::sleep(Duration::from_millis(400));
        let select_answer = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let row = window
                    .within("ask-drawer")
                    .find(("output-row", 0usize))
                    .bounds();
                let y = row.center().y;
                window.drag(
                    gpui_kit::point(row.left() + gpui_kit::px(1.), y),
                    gpui_kit::point(row.right() - gpui_kit::px(1.), y),
                    cx,
                );
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("selection-popover").is_some()
            })
            .unwrap()
        };

        assert!(select_answer(cx), "no popover for the selected text");
        cx.update_window(handle, |_, window, cx| window.click("selection-attach", cx))
            .unwrap();
        cx.run_until_parked();
        let attached = prompt_mode.read_with(cx, |this, cx| {
            this.chat_input
                .read(cx)
                .attachments()
                .iter()
                .map(|attachment| attachment.text.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(attached.len(), 1, "{attached:?}");
        assert!(attached[0].contains("joins Code and Spec"), "{attached:?}");
        assert!(prompt_mode.read_with(cx, |this, _| this.selection_popover.is_none()));

        assert!(select_answer(cx), "no popover for the text selected again");
        cx.update_window(handle, |_, window, cx| window.click("selection-copy", cx))
            .unwrap();
        cx.run_until_parked();
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        assert!(copied.contains("joins Code and Spec"), "{copied:?}");
        assert!(prompt_mode.read_with(cx, |this, _| this.selection_popover.is_none()));
    }

    /// With a question still running, expanding the previous answers slides
    /// the list up from on top of its row, which stays in view and undimmed,
    /// rather than covering it.
    #[gpui_kit::test]
    async fn previous_answers_slide_up_from_pending_questions(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.on_ask_tab = true;
                let old = this.push_ask("An old question".into(), cx);
                this.update_ask(
                    old,
                    |ask| {
                        ask.apply(HarnessEvent::TextStarted);
                        ask.apply(HarnessEvent::TextDelta("An old answer.".into()));
                        ask.end();
                    },
                    cx,
                );
                this.close_ask(old, cx);
                let pending = this.push_ask("Still thinking?".into(), cx);
                this.update_ask(pending, |ask| ask.apply(tool("t1", "Read")), cx);
                this.ask_history.toggle();
                cx.notify();
            });
        })
        .unwrap();
        let _ = settle(
            handle,
            ("ask-row", 2usize),
            |row, _| row.size.height > gpui_kit::px(0.),
            cx,
        );
        let mut last = None;
        let drawer = loop {
            let (drawer, _) = settle(handle, "ask-drawer", |_, _| true, cx);
            if last == Some(drawer) {
                break drawer;
            }
            last = Some(drawer);
            std::thread::sleep(Duration::from_millis(50));
        };
        let (row, _) = settle(handle, ("ask-row", 2usize), |_, _| true, cx);
        assert!(
            (drawer.bottom() - row.top()).abs() <= gpui_kit::px(2.),
            "the previous answers {drawer:?} don't sit on top of the pending question {row:?}"
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let dim = window.find("ask-dim").bounds();
            assert!(
                dim.bottom() <= row.top() + gpui_kit::px(2.),
                "the shade {dim:?} covers the pending question {row:?}"
            );
        })
        .unwrap();
        let _ = row;
    }

    /// Only the output rows in view are laid out, however long the output; and
    /// a raw output line, which only changes the raw tail at the output's end,
    /// redraws nothing while that end is scrolled out of view, though the
    /// tail still takes it in.
    #[gpui_kit::test]
    async fn long_output_lays_out_only_rows_in_view(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Do it".into(), cx);
                for n in 0..300 {
                    this.apply_event(ix, HarnessEvent::TextStarted, cx);
                    this.apply_event(ix, HarnessEvent::TextDelta(format!("Paragraph {n}.")), cx);
                }
                this.scroll_output_to_top();
            });
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let laid_out = (0..300usize)
                .filter(|row| window.try_find(("output-row", *row)).is_some())
                .count();
            assert!(
                laid_out > 0 && laid_out < 100,
                "{laid_out} of 300 rows were laid out"
            );
        })
        .unwrap();

        let notified = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let _observe = cx.update(|cx| {
            let notified = notified.clone();
            cx.observe(&prompt_mode, move |_, _| notified.set(notified.get() + 1))
        });
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.apply_event(0, HarnessEvent::Output("{\"type\":\"ping\"}".into()), cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(notified.get(), 0, "an unseen raw line redrew the output");
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.tasks[0].reply.raw_tail.back().cloned()),
            Some("{\"type\":\"ping\"}".to_string())
        );

        // At the end, where the tail shows, it redraws.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, _| this.output_table.scroll_to_end());
            window.render_frame(cx);
            window.render_frame(cx);
            prompt_mode.update(cx, |this, cx| {
                this.apply_event(0, HarnessEvent::Output("{\"type\":\"ping\"}".into()), cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert!(notified.get() > 0, "a raw line in view didn't redraw");
    }

    /// The previous tasks, with one open whose text is parsed in the
    /// background, scroll without their height jumping, and keeping that
    /// text's state never has the view draw itself over and over.
    #[gpui_kit::test]
    async fn history_scrollbar_holds_still_while_scrolling(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.run_until_parked();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for t in 0..20 {
                    let ix = this.push_task(format!("task {t}").into(), cx);
                    for n in 0..6 {
                        this.apply_event(ix, HarnessEvent::TextStarted, cx);
                        let words =
                            "Long words wrap here. ".repeat(if n % 2 == 0 { 300 } else { 5 });
                        this.apply_event(ix, HarnessEvent::TextDelta(format!("P{n}. {words}")), cx);
                    }
                    this.apply_event(
                        ix,
                        HarnessEvent::Finished {
                            is_error: false,
                            result: String::new(),
                        },
                        cx,
                    );
                }
                this.task_history.toggle();
                this.task_history.open = Some(17);
                cx.notify();
            });
        })
        .unwrap();
        let scroll = prompt_mode.read_with(cx, |this, _| this.task_history.rows.scroll());
        let rows = prompt_mode.read_with(cx, |this, _| this.task_history.rows.clone());
        // Draws, letting background parses land and the rows they're in be
        // measured again, until every row's height is known.
        let frame = |cx: &mut TestAppContext| {
            for n in 0.. {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
                cx.executor().advance_clock(crate::markdown::POLL_INTERVAL);
                cx.run_until_parked();
                if n >= 3 && rows.is_settled() {
                    break;
                }
                assert!(n < 1000, "the rows were never all measured");
            }
        };
        frame(cx);
        prompt_mode.update(cx, |this, _| {
            this.task_history
                .rows
                .state()
                .scroll_to(gpui_kit::ListOffset {
                    item_ix: 0,
                    offset_in_item: gpui_kit::px(0.),
                })
        });
        frame(cx);
        let max = scroll.max_offset().y;
        assert!(max > gpui_kit::px(2000.), "{max:?}");
        let mut last = gpui_kit::px(0.);
        for _ in 0..8 {
            cx.update_window(handle, |_, window, cx| {
                let delta = gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-300.));
                window.scroll("task-list-scroll", gpui_kit::ScrollDelta::Pixels(delta), cx);
            })
            .unwrap();
            frame(cx);
            let now = scroll.max_offset().y;
            assert!((now - max).abs() < gpui_kit::px(1.), "{now:?}, not {max:?}");
            let offset = -scroll.offset().y;
            assert!(offset >= last, "scrolled back from {last:?} to {offset:?}");
            last = offset;
        }
    }

    /// Scrolling through long output of rows of differing heights, rows not yet
    /// seen are already counted at their height, so how far the output scrolls,
    /// and with it the scrollbar's thumb, holds still rather than jumping as
    /// rows come into view; and so it does once the output's width changes.
    #[gpui_kit::test]
    async fn output_scrollbar_holds_still_while_scrolling(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Do it".into(), cx);
                for n in 0..60 {
                    this.apply_event(ix, HarnessEvent::TextStarted, cx);
                    // Some over the few kilobytes parsed in the background.
                    let text = match n % 4 {
                        0 => format!("Paragraph {n}. {}", "Long words wrap here. ".repeat(300)),
                        1 => format!("Paragraph {n}. {}", "Long words wrap here. ".repeat(40)),
                        _ => format!("Paragraph {n}."),
                    };
                    this.apply_event(ix, HarnessEvent::TextDelta(text), cx);
                }
                this.scroll_output_to_top();
            });
        })
        .unwrap();
        let scroll = prompt_mode.read_with(cx, |this, _| this.output_table.scroll());
        let rows = prompt_mode.read_with(cx, |this, _| this.output_table.list().clone());
        // Draws, letting background parses land and the rows they're in be
        // measured again, until every row's height is known.
        let frame = |cx: &mut TestAppContext| {
            for n in 0.. {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
                cx.executor().advance_clock(crate::markdown::POLL_INTERVAL);
                cx.run_until_parked();
                if n >= 3 && rows.is_settled() {
                    break;
                }
                assert!(n < 1000, "the rows were never all measured");
            }
        };
        frame(cx);
        let max = scroll.max_offset().y;
        assert!(
            max > gpui_kit::px(2000.),
            "the output barely scrolls: {max:?}"
        );
        let mut y = gpui_kit::px(0.);
        while y < max {
            scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), -y));
            frame(cx);
            let now = scroll.max_offset().y;
            assert!(
                (now - max).abs() < gpui_kit::px(1.),
                "at {y:?} the output scrolls {now:?}, not {max:?}"
            );
            y += gpui_kit::px(400.);
        }

        // Scrolled by the wheel, the thumb only moves on; dragged, it stays
        // under the pointer the whole way, rows coming into view or not.
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        frame(cx);
        let thumb = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let track = window.find("task-output-scroll-track").bounds();
                (track, crate::scrollbar::thumb_for_test(&scroll, track))
            })
            .unwrap()
        };
        let mut last = thumb(cx).1.0;
        for _ in 0..10 {
            cx.update_window(handle, |_, window, cx| {
                let delta = gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-250.));
                window.scroll("task-output", gpui_kit::ScrollDelta::Pixels(delta), cx);
            })
            .unwrap();
            frame(cx);
            let (top, _) = thumb(cx).1;
            assert!(top > last, "the thumb went from {last:?} to {top:?}");
            assert!((scroll.max_offset().y - max).abs() < gpui_kit::px(1.));
            last = top;
        }
        let (track, (top, height)) = thumb(cx);
        let grab = gpui_kit::point(track.center().x, top + height / 2.);
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::{Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, PlatformInput};
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: grab,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        })
        .unwrap();
        let end = track.bottom() - height;
        for step in 1..=8 {
            let y = grab.y + (end - grab.y) * (step as f32 / 8.);
            cx.update_window(handle, |_, window, cx| {
                use gpui_kit::{Modifiers, MouseButton, MouseMoveEvent, PlatformInput};
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: gpui_kit::point(grab.x, y),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    }),
                    cx,
                );
            })
            .unwrap();
            frame(cx);
            let (_, (top, height)) = thumb(cx);
            assert!(
                (top + height / 2. - y).abs() < gpui_kit::px(1.5),
                "dragged to {y:?}, the thumb's middle is at {:?}",
                top + height / 2.
            );
            assert!((scroll.max_offset().y - max).abs() < gpui_kit::px(1.));
        }
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::{Modifiers, MouseButton, MouseUpEvent, PlatformInput};
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        })
        .unwrap();
        // Back at the top, rows scrolled past measure as they did.
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        frame(cx);
        assert!((scroll.max_offset().y - max).abs() < gpui_kit::px(1.));

        // Rows that come while the output is scrolled away from its end count
        // at their height straight away.
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.apply_event(0, HarnessEvent::TextStarted, cx);
                let text = "Late words wrap here. ".repeat(60);
                this.apply_event(0, HarnessEvent::TextDelta(text), cx);
            });
        })
        .unwrap();
        frame(cx);
        let grown = scroll.max_offset().y;
        assert!(grown > max + gpui_kit::px(40.), "{grown:?} after {max:?}");
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), -grown / 2.));
        frame(cx);
        assert!((scroll.max_offset().y - grown).abs() < gpui_kit::px(1.));
        let max = grown;

        // A narrower window rewraps the rows; once drawn, the output's height
        // is known in full again straight away.
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(500.), gpui_kit::px(700.)));
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        frame(cx);
        let narrow = scroll.max_offset().y;
        assert!(narrow > max, "rewrapped rows are no taller");
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), -narrow / 2.));
        frame(cx);
        assert!((scroll.max_offset().y - narrow).abs() < gpui_kit::px(1.));
    }

    /// However long a task's output, a frame draws only about the rows in
    /// view, and the rest are measured a few a frame. When its width changes,
    /// the rows in view stay where they are while the others are measured
    /// again; once they have been, the scrollbar holds still, and rows that
    /// come in while the output is scrolled away from its end leave the thumb
    /// where it is.
    #[gpui_kit::test]
    async fn long_output_is_measured_a_few_rows_a_frame(cx: &mut TestAppContext) {
        const ROWS: usize = 1500;
        let (prompt_mode, handle) = open(cx);
        let paragraph = |n: usize| match n % 3 {
            0 => format!("Paragraph {n}. {}", "Words wrap here. ".repeat(20)),
            _ => format!("Paragraph {n}."),
        };
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Do it".into(), cx);
                for n in 0..ROWS {
                    this.apply_event(ix, HarnessEvent::TextStarted, cx);
                    this.apply_event(ix, HarnessEvent::TextDelta(paragraph(n)), cx);
                }
            });
        })
        .unwrap();
        let (scroll, rows) = prompt_mode.read_with(cx, |this, _| {
            (this.output_table.scroll(), this.output_table.list().clone())
        });
        // A single frame, counting the rows it draws.
        let draw = |cx: &mut TestAppContext| {
            crate::task_table::rows_drawn();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            crate::task_table::rows_drawn()
        };
        let drawn = draw(cx);
        assert!(
            drawn < ROWS / 4,
            "{drawn} of {ROWS} rows were drawn in the first frame"
        );
        let mut frames = 0;
        while !rows.is_settled() {
            let drawn = draw(cx);
            assert!(drawn < ROWS / 4, "{drawn} rows were drawn in a frame");
            frames += 1;
            assert!(frames < 5000, "the rows were never all measured");
        }
        let max = scroll.max_offset().y;
        scroll.set_offset(gpui_kit::point(gpui_kit::px(0.), -max / 2.));
        draw(cx);
        draw(cx);
        let top = rows.state().logical_scroll_top().item_ix;
        assert!(top > 0, "the output didn't scroll");

        // A narrower window: the frame it narrows in draws about the rows in
        // view, not all of them.
        crate::task_table::rows_drawn();
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(600.), gpui_kit::px(700.)));
        // Resizing may draw a frame of its own.
        let drawn = crate::task_table::rows_drawn() + draw(cx);
        assert!(
            drawn < ROWS / 4,
            "{drawn} of {ROWS} rows were drawn as the width changed"
        );
        assert!(!rows.is_settled(), "every row was measured in one frame");
        let mut frames = 0;
        while !rows.is_settled() {
            draw(cx);
            assert_eq!(
                rows.state().logical_scroll_top().item_ix,
                top,
                "the rows in view moved while the rest were measured"
            );
            frames += 1;
            assert!(frames < 5000, "the rows were never all measured again");
        }

        // Measured, the thumb holds still frame after frame.
        let (offset, max) = (scroll.offset().y, scroll.max_offset().y);
        for _ in 0..5 {
            draw(cx);
            assert!((scroll.offset().y - offset).abs() < gpui_kit::px(0.5));
            assert!((scroll.max_offset().y - max).abs() < gpui_kit::px(0.5));
        }

        // Rows that come below, scrolled away from them, leave the rows above
        // where they are.
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for n in 0..20 {
                    this.apply_event(0, HarnessEvent::TextStarted, cx);
                    this.apply_event(0, HarnessEvent::TextDelta(paragraph(n)), cx);
                }
            });
        })
        .unwrap();
        while !rows.is_settled() {
            draw(cx);
        }
        assert!(
            (scroll.offset().y - offset).abs() < gpui_kit::px(0.5),
            "new rows moved the output from {offset:?} to {:?}",
            scroll.offset().y
        );
        assert!(scroll.max_offset().y > max, "the new rows aren't counted");
    }

    /// An open question fills the answer drawer to 80% of the space above the
    /// chat input; dragging the drawer's top edge resizes it, following the
    /// pointer, and the size sticks for the next question opened.
    #[gpui_kit::test]
    async fn the_answer_drawer_opens_to_most_of_the_space_and_resizes(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for question in ["What does the chain do?", "And the ribbon?"] {
                    let id = this.push_ask(question.into(), cx);
                    this.update_ask(
                        id,
                        |ask| {
                            ask.apply(HarnessEvent::TextStarted);
                            ask.apply(HarnessEvent::TextDelta("Briefly.".into()));
                            ask.end();
                        },
                        cx,
                    );
                }
                this.expand_ask(1, cx);
            });
        })
        .unwrap();
        let body = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.find("prompt-body").bounds()
            })
            .unwrap()
        };
        let share = |panel: Bounds, body: Bounds| panel.size.height / body.size.height;

        let mut last = None;
        let panel = loop {
            let (panel, _) = settle(handle, "ask-drawer", |_, _| true, cx);
            if last == Some(panel) {
                break panel;
            }
            last = Some(panel);
            std::thread::sleep(Duration::from_millis(50));
        };
        let space = body(cx);
        assert!(
            (share(panel, space) - 0.8).abs() < 0.02,
            "the drawer {panel:?} takes {} of {space:?}",
            share(panel, space)
        );

        // Dragged to half the space, it follows the pointer straight there.
        cx.update_window(handle, |_, window, cx| {
            let edge = window.find("ask-drawer-resize").bounds().center();
            let to = gpui_kit::point(edge.x, space.top() + space.size.height * 0.5);
            window.drag(edge, to, cx);
            window.render_frame(cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
        })
        .unwrap();
        let (panel, _) = settle(handle, "ask-drawer", |_, _| true, cx);
        assert!(
            (share(panel, space) - 0.5).abs() < 0.02,
            "dragged, the drawer {panel:?} takes {} of {space:?}",
            share(panel, space)
        );

        // Another question opened keeps that size.
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| this.expand_ask(2, cx))
        })
        .unwrap();
        let mut last = None;
        let panel = loop {
            let (panel, _) = settle(handle, "ask-drawer", |_, _| true, cx);
            if last == Some(panel) {
                break panel;
            }
            last = Some(panel);
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            (share(panel, space) - 0.5).abs() < 0.02,
            "reopened, the drawer {panel:?} takes {} of {space:?}",
            share(panel, space)
        );
    }

    /// Questions run at once, a row each, stacked above the tabs with the
    /// newest nearest them, and leave the message list undimmed. A finished
    /// one opens onto its whole table, which dims the list, and closes back
    /// to its row; closing a question removes only it.
    #[gpui_kit::test]
    async fn questions_stack_and_only_an_open_answer_dims(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.push_ask("First question".into(), cx);
                this.push_ask("Second question".into(), cx);
                assert!(this.asks.iter().all(|ask| ask.task.status.is_active()));
                assert!(
                    this.expanded().is_none(),
                    "a running question dims the list"
                );
            });
        })
        .unwrap();
        let (second, _) = settle(
            handle,
            ("ask-row", 2usize),
            |row, tabs| (row.bottom() - tabs.top()).abs() <= gpui_kit::px(2.),
            cx,
        );
        // The first rises above the second, whole.
        let (first, _) = settle(
            handle,
            ("ask-row", 1usize),
            |row, _| {
                row.bottom() <= second.top() + gpui_kit::px(1.)
                    && row.size.height >= second.size.height - gpui_kit::px(1.)
            },
            cx,
        );
        assert!(
            first.top() < second.top(),
            "{first:?} is not above {second:?}"
        );

        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.update_ask(
                    1,
                    |ask| {
                        ask.apply(HarnessEvent::Finished {
                            is_error: false,
                            result: "An answer.".into(),
                        });
                        ask.end();
                    },
                    cx,
                );
                this.expand_ask(1, cx);
                assert!(
                    this.expanded().is_some(),
                    "an open answer leaves the list undimmed"
                );
                this.collapse_ask(1, cx);
                assert!(this.expanded().is_none());
                this.close_ask(1, cx);
                assert_eq!(this.asks.iter().map(|ask| ask.id).collect::<Vec<_>>(), [2]);
                assert!(this.is_working(), "closing one question stopped the other");
            });
        })
        .unwrap();
    }

    /// The latest task's output has a scroll column along its right: square
    /// buttons at the top and bottom scroll it, and a button below them locks
    /// it to the bottom, where it stays as output keeps coming.
    #[gpui_kit::test]
    async fn task_output_has_a_scrollbar_that_locks_to_the_bottom(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..200).map(|n| format!("Line {n}\n\n")).collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Write a lot".into(), cx);
                this.show_compiled(ix, "Prompt_0".into(), "Write a lot".into(), cx);
                this.apply_event(ix, HarnessEvent::TextStarted, cx);
                this.apply_event(ix, HarnessEvent::TextDelta(long.clone()), cx);
            });
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(2), |window, _| {
            window.try_find("task-output-scroll-column").is_some()
        })
        .await;
        let offset = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.output_table.scroll().offset().y)
        };
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let (output, column) = (
                window.find("task-output").bounds(),
                window.find("task-output-scroll-column").bounds(),
            );
            assert!(
                column.left() >= output.right() - gpui_kit::px(1.),
                "the column {column:?} is not right of the output {output:?}"
            );
            // The table's header has the theme's bevel: a lit pixel along its
            // top and a shaded one along its bottom, inside the output.
            let (light, shade) = crate::theme::bevel_colors(crate::theme::palette(cx));
            let scale = window.scale_factor();
            let edges = |color: gpui_kit::Hsla| {
                window
                    .painted_quads()
                    .into_iter()
                    .filter(|quad| quad.background.as_solid() == Some(color))
                    .filter(|quad| {
                        let b = &quad.bounds;
                        // Not the scroll column's thumb, which has one too.
                        b.origin.x.0 >= output.left().as_f32() * scale
                            && b.origin.x.0 < column.left().as_f32() * scale
                            && b.origin.y.0 >= output.top().as_f32() * scale
                            && b.origin.y.0 < (output.top().as_f32() + 80.) * scale
                    })
                    .count()
            };
            assert_eq!(edges(light), 2, "the header's lit edges");
            assert_eq!(edges(shade), 2, "the header's shaded edges");
            // The track spans the column's full width, and every button sits
            // just inside the track's line.
            let track = window.find("task-output-scroll-track").bounds();
            assert!(track.left() == column.left() && track.right() == column.right());
            for id in [
                "task-output-scroll-up",
                "task-output-scroll-down",
                "task-output-scroll-lock",
            ] {
                let part = window.find(id).bounds();
                assert!(
                    part.left() == column.left() + gpui_kit::px(1.)
                        && part.right() == column.right(),
                    "{id} {part:?} doesn't sit inside the track's line in {column:?}"
                );
            }
            assert!(
                window.try_find("task-output-scroll-up").is_some()
                    && window.try_find("task-output-scroll-down").is_some()
                    && window.try_find("task-output-scroll-lock").is_some()
            );
        })
        .unwrap();
        assert!(
            prompt_mode.read_with(cx, |this, _| this.output_table.scroll().max_offset().y)
                > gpui_kit::px(0.),
            "the output does not scroll"
        );

        // From the top, which new output would otherwise have followed away from.
        prompt_mode.update(cx, |this, _| this.scroll_output_to_top());
        let before = offset(cx);
        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-down", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            offset(cx) < before,
            "scrolling down did not move the output"
        );
        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-up", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(offset(cx), before, "scrolling up did not move it back");

        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-lock", cx)
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                assert!(this.output_locked);
                // Scrolled away, more output brings it back to the bottom.
                this.scroll_output_to_top();
                this.apply_event(0, HarnessEvent::TextDelta(long.clone()), cx);
            });
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (offset, max) = prompt_mode.read_with(cx, |this, _| {
            (
                this.output_table.scroll().offset().y,
                this.output_table.scroll().max_offset().y,
            )
        });
        assert!(
            (offset + max).abs() <= gpui_kit::px(1.),
            "locked, the output is at {offset:?} rather than the bottom {max:?}"
        );

        // Scrolling the output by hand breaks the lock, however many wheel
        // events the gesture sends, and leaves it where it was scrolled to.
        cx.update_window(handle, |_, window, cx| {
            for _ in 0..3 {
                window.scroll(
                    "task-output",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                        gpui_kit::px(0.),
                        gpui_kit::px(120.),
                    )),
                    cx,
                );
            }
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            !prompt_mode.read_with(cx, |this, _| this.output_locked),
            "scrolling did not break the lock"
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (offset, max) = prompt_mode.read_with(cx, |this, _| {
            (
                this.output_table.scroll().offset().y,
                this.output_table.scroll().max_offset().y,
            )
        });
        assert!(
            offset + max > gpui_kit::px(1.),
            "unlocked, the output went back to the bottom: {offset:?} of {max:?}"
        );

        // The column's buttons break it too.
        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-lock", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(prompt_mode.read_with(cx, |this, _| this.output_locked));
        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-up", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!prompt_mode.read_with(cx, |this, _| this.output_locked));

        // Scrolling back to the bottom by hand locks it again, with the column's
        // button or the wheel.
        cx.update_window(handle, |_, window, cx| {
            window.click("task-output-scroll-down", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            prompt_mode.read_with(cx, |this, _| this.output_locked),
            "scrolling down to the bottom did not lock the output"
        );
        let wheel = |dy: f32, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.scroll(
                    "task-output",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                        gpui_kit::px(0.),
                        gpui_kit::px(dy),
                    )),
                    cx,
                );
                window.render_frame(cx);
                crate::double_borders::assert_none(window);
            })
            .unwrap();
            cx.run_until_parked();
        };
        wheel(200., cx);
        assert!(!prompt_mode.read_with(cx, |this, _| this.output_locked));
        wheel(-1000., cx);
        assert!(
            prompt_mode.read_with(cx, |this, _| this.output_locked),
            "wheeling down to the bottom did not lock the output"
        );
    }

    /// A Code, Chain, or Spec prompt runs `piton build` before it is sent,
    /// showing "Building" meanwhile; a build that fails says so in the task's
    /// output without stopping the prompt. A question doesn't build.
    #[gpui_kit::test]
    async fn code_and_spec_prompts_build_the_spec_first(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;

        // No piton.config.pi, so the build fails, and so does the prompt.
        let dir = std::env::temp_dir().join(format!("suspense-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        // The project's (empty) history loads first.
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Change it".into(), SendMode::Code, Vec::new(), window, cx);
                assert_eq!(this.tasks.last().unwrap().status, TaskStatus::Building);
            });
        })
        .unwrap();

        let start = std::time::Instant::now();
        while prompt_mode.read_with(cx, |this, _| this.working) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the prompt never ended"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        prompt_mode.read_with(cx, |this, _| {
            let task = this.tasks.last().unwrap();
            assert!(
                task.reply.parts.iter().any(|part| matches!(
                    part,
                    ReplyPart::Error(error) if error.starts_with("piton build failed")
                )),
                "the failed build is not in the output"
            );
            // It went on past the build: the prompt itself then failed.
            assert_eq!(task.status, TaskStatus::Failed);
        });

        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Why?".into(), SendMode::Ask, Vec::new(), window, cx);
                assert_ne!(this.asks[0].task.status, TaskStatus::Building);
            });
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A finished task's note is written from what it asked and what it did,
    /// and added to the project's commit notes; a task that changed nothing
    /// adds none.
    #[gpui_kit::test]
    async fn finished_tasks_add_commit_notes(cx: &mut TestAppContext) {
        fn summarize(
            _: &std::path::Path,
            asked: &str,
            result: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok((!result.contains("nothing")).then(|| format!("Do what was asked: {asked}")))
        }
        let dir = std::env::temp_dir().join(format!("suspense-notes-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        cx.update(|cx| {
            super::add_commit_note(
                summarize,
                dir.clone(),
                "Add a ribbon".into(),
                "Added it.".into(),
                cx,
            );
            super::add_commit_note(
                summarize,
                dir.clone(),
                "Look around".into(),
                "Changed nothing.".into(),
                cx,
            );
        });
        let start = std::time::Instant::now();
        while crate::commit_notes::load(&dir).is_empty() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no note was added"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        cx.run_until_parked();
        let notes: Vec<String> = crate::commit_notes::load(&dir)
            .into_iter()
            .map(|note| note.text)
            .collect();
        assert_eq!(notes, ["Do what was asked: Add a ribbon"]);
        assert!(cx.update(|cx| {
            cx.try_global::<crate::commit_notes::NotesVersion>()
                .is_some()
        }));
        std::fs::remove_dir_all(&dir).ok();
    }
}
