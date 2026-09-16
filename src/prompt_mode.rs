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

use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::accordion::Accordion;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_kit::component::input::Rope;
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{
    ActiveTheme as _, ColorName, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::component::{Disableable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_input::{self, ChatInput, SendMode, Submit, TabChanged};
use crate::commit_notes;
use crate::file_link::{self, OpenFile};
use crate::file_view::{CloseFile, FileView, OpenDefinition};
use crate::harness::{self, HarnessEvent};
use crate::hidden_anchor::{self, HiddenAnchor};
use crate::markdown;
use crate::piton_build;
use crate::piton_lsp::PitonSession;
use crate::project_directory::ProjectDirectory;
use crate::prompt_history::{self, RunRecord, SavedPrompt};
use crate::prompt_queue::{self, QueuedPrompt};
use crate::scroll_column::{self, SetLock};
use crate::shell_format;
use crate::system_prompts;

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

/// The widths of the output table's badge and status columns; the detail
/// column takes the rest.
const KIND_WIDTH: Pixels = px(130.);
const STATUS_WIDTH: Pixels = px(110.);

/// How many lines of the harness's latest raw output stand in for output
/// that has yet to arrive, and how much of each line is kept.
const RAW_TAIL_LINES: usize = 3;
const RAW_LINE_CHARS: usize = 400;

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
const ASK_DIM: f32 = 0.45;
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
}

/// A prompt on its way to the harness.
enum Sending {
    /// Typed and sent straight away, in a mode.
    Now(SendMode),
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

    /// A label in the status's colour, with the status spelled out. Uses the
    /// palette colours, like the output badges, as the theme's solid status
    /// colours are unreadable in dark mode.
    fn tag(self) -> Tag {
        Tag::color(match self {
            Self::Building => ColorName::Blue,
            Self::Compiling => ColorName::Cyan,
            Self::Running => ColorName::Yellow,
            Self::Done => ColorName::Green,
            Self::Failed => ColorName::Red,
            Self::Unrecorded => ColorName::Gray,
        })
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
            task.reply.done = true;
            task.status = TaskStatus::Unrecorded;
            return task;
        };
        if let Some(markdown) = record.user_prompt {
            task.set_compiled(Compiled {
                anchor: saved.anchor.name().to_string().into(),
                markdown,
            });
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
        self.reply.parts.push(ReplyPart::Error(error));
        self.status = TaskStatus::Failed;
    }

    /// The run is over. One that ended without a result, as when the harness
    /// was stopped, failed, and nothing still running in it finished.
    fn end(&mut self) {
        if self.reply.done {
            return;
        }
        self.reply.done = true;
        self.reply.settle_tools(ToolState::Failed);
        if self.status != TaskStatus::Failed {
            self.fail("The harness stopped without a result.".into());
        }
    }
}

/// A harness reply as it streams in.
#[derive(Default)]
pub(crate) struct Reply {
    parts: Vec<ReplyPart>,
    /// The harness's last few raw output lines, newest last, each cut short.
    raw_tail: VecDeque<String>,
    done: bool,
}

enum ReplyPart {
    Text(String),
    Tool(ToolCall),
    Error(String),
}

#[derive(Debug, PartialEq)]
struct ToolCall {
    id: String,
    name: String,
    summary: Option<String>,
    state: ToolState,
}

impl ToolCall {
    /// The tool's name, for the output column. Left out when the badge already
    /// says what the tool is and there is a summary to show instead; a tool
    /// the badge only calls "Tool" keeps its name.
    fn shown_name(&self) -> Option<&str> {
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolState {
    Running,
    Done,
    Failed,
}

impl ToolState {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// The general type of a tool call, shown as its badge.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolKind {
    Read,
    Edit,
    Command,
    Web,
    Agent,
    Other,
}

impl ToolKind {
    fn of(tool_name: &str) -> Self {
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
    fn pending_input(self) -> Option<&'static str> {
        match self {
            Self::Read => Some("Choosing what to read…"),
            Self::Edit => Some("Writing the edit…"),
            Self::Command => Some("Writing the command…"),
            Self::Web => Some("Preparing the request…"),
            Self::Agent => Some("Briefing the agent…"),
            Self::Other => None,
        }
    }

    fn color(self) -> ColorName {
        match self {
            Self::Read => ColorName::Sky,
            Self::Edit => ColorName::Amber,
            Self::Command => ColorName::Violet,
            Self::Web => ColorName::Teal,
            Self::Agent => ColorName::Pink,
            Self::Other => ColorName::Gray,
        }
    }
}

/// A row of a task's output table.
#[derive(Debug, PartialEq)]
enum OutputRow<'a> {
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
    fn badge(&self) -> AnyElement {
        let (tag, icon, label) = match self {
            Self::Text(_) => (Tag::secondary(), IconName::MessageSquare, "Reply"),
            Self::Tool(call) => {
                let kind = ToolKind::of(&call.name);
                (Tag::color(kind.color()), kind.icon(), kind.label())
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
    fn is_partial(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Tool(call) => call.summary.is_none() && call.state == ToolState::Running,
            Self::Error(_) => false,
            Self::Pending => true,
        }
    }
}

impl Reply {
    /// Folds a harness event into the reply. Returns an error to show when the
    /// run failed.
    pub(crate) fn apply(&mut self, event: HarnessEvent) -> Option<String> {
        match event {
            HarnessEvent::Output(mut line) => {
                if let Some((end, _)) = line.char_indices().nth(RAW_LINE_CHARS) {
                    line.truncate(end);
                }
                if self.raw_tail.len() == RAW_TAIL_LINES {
                    self.raw_tail.pop_front();
                }
                self.raw_tail.push_back(line);
            }
            HarnessEvent::TextStarted => self.parts.push(ReplyPart::Text(String::new())),
            HarnessEvent::TextDelta(delta) => match self.parts.last_mut() {
                Some(ReplyPart::Text(text)) => text.push_str(&delta),
                _ => self.parts.push(ReplyPart::Text(delta)),
            },
            HarnessEvent::ToolStarted { id, name } => self.parts.push(ReplyPart::Tool(ToolCall {
                id,
                name,
                summary: None,
                state: ToolState::Running,
            })),
            HarnessEvent::ToolInput { id, summary } => {
                if let Some(call) = self.tool_mut(&id) {
                    call.summary = Some(summary);
                }
            }
            HarnessEvent::ToolFinished { id, is_error } => {
                if let Some(call) = self.tool_mut(&id) {
                    call.state = if is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
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
                    return Some(if result.is_empty() {
                        "The harness reported an error.".into()
                    } else {
                        result
                    });
                }
                // Nothing streamed (e.g. an older harness): show the result.
                if !self.has_text() && !result.is_empty() {
                    self.parts.push(ReplyPart::Text(result));
                }
            }
            HarnessEvent::Failed(error) => {
                self.done = true;
                self.settle_tools(ToolState::Failed);
                return Some(error);
            }
            HarnessEvent::Session(_) => {}
        }
        None
    }

    /// Gives tool calls still running when the run ended the state they ended
    /// in, so none is left reading "running".
    fn settle_tools(&mut self, state: ToolState) {
        for part in &mut self.parts {
            if let ReplyPart::Tool(call) = part
                && call.state == ToolState::Running
            {
                call.state = state;
            }
        }
    }

    fn tool_mut(&mut self, tool_id: &str) -> Option<&mut ToolCall> {
        self.parts.iter_mut().rev().find_map(|part| match part {
            ReplyPart::Tool(call) if call.id == tool_id => Some(call),
            _ => None,
        })
    }

    /// Whether the run has ended, with a result or without.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Adds an error row to the end of the output.
    pub(crate) fn push_error(&mut self, error: String) {
        self.parts.push(ReplyPart::Error(error));
    }

    fn has_text(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ReplyPart::Text(text) if !text.trim().is_empty()))
    }

    /// The rows of the output table, in the order they happened. Text with
    /// nothing in it is left out, unless it is the last thing in a reply still
    /// streaming. Until the reply is done, it ends in a row that is still
    /// filling in, adding a pending row if the last is already complete. Rows
    /// only change or are added at the end as the reply streams, so a row
    /// keeps its index.
    fn rows(&self) -> Vec<OutputRow<'_>> {
        let last = self.parts.len().saturating_sub(1);
        let mut rows: Vec<_> = self
            .parts
            .iter()
            .enumerate()
            .filter_map(|(ix, part)| match part {
                ReplyPart::Text(text) if text.trim().is_empty() && (self.done || ix != last) => {
                    None
                }
                ReplyPart::Text(text) => Some(OutputRow::Text(text)),
                ReplyPart::Tool(call) => Some(OutputRow::Tool(call)),
                ReplyPart::Error(error) => Some(OutputRow::Error(error)),
            })
            .collect();
        if !self.done && !rows.last().is_some_and(OutputRow::is_partial) {
            rows.push(OutputRow::Pending);
        }
        rows
    }
}

/// A row saying how many previous items there are, which expands into a
/// scrollable accordion of every one of them, oldest first, each headed by
/// its status and prompt and opening onto its prompt and output. Previous
/// tasks and previous answers are each one of these.
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
    scroll: ScrollHandle,
    /// Counts the times it was expanded.
    opened: usize,
    /// Its tables collapse the steps leading up to the answer.
    collapse_steps: bool,
}

impl HistoryList {
    fn toggle(&mut self) {
        self.expanded = !self.expanded;
        if self.expanded {
            self.opened += 1;
            // The latest are the likeliest to be looked for.
            self.scroll.scroll_to_bottom();
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

    /// Every item, oldest first, as an accordion that scrolls.
    fn render_list(
        &self,
        tasks: &[PromptTask],
        select: fn(&mut PromptMode) -> &mut HistoryList,
        open_file: &OpenFile,
        steps_shown: &HashSet<usize>,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        let this = cx.entity().downgrade();
        let accordion = tasks.iter().enumerate().fold(
            Accordion::new(self.list).h_auto(),
            |accordion, (ix, task)| {
                let open = self.open == Some(ix);
                let task_ix = self.id_base + ix;
                accordion.item(|item| {
                    item.open(open)
                        .title(task_summary((self.item, ix), task_ix, task, cx))
                        // Closed items are not laid out.
                        .when(open, |item| {
                            item.child(
                                v_flex()
                                    .gap_3()
                                    .pt_1()
                                    .child(task_prompt(task_ix, task, open_file, cx))
                                    .child(output_table(
                                        task_ix,
                                        &task.reply,
                                        Some(open_file),
                                        self.collapse_steps
                                            .then(|| steps(task_ix, steps_shown, cx)),
                                        cx,
                                    )),
                            )
                        })
                })
            },
        );
        let accordion = accordion.on_toggle_click(move |open, _, cx| {
            this.update(cx, |this, cx| {
                select(this).open = open.first().copied();
                cx.notify();
            })
            .ok();
        });
        let list = div()
            .id(SharedString::from(format!("{}-scroll", self.list)))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_4()
            .child(accordion);
        // Lets UI tests find the list; inert in normal builds.
        let list = gpui_kit::TestSupportExt::test_support(list);
        scroll_column::with_scroll_column(self.list, &self.scroll, list, true, None, cx)
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
    scroll: ScrollHandle,
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

pub struct PromptMode {
    /// Every task sent, oldest first; the last heads the view.
    tasks: Vec<PromptTask>,
    /// The latest task's output, which follows new rows while scrolled to
    /// the bottom, or always while locked there.
    output_scroll: ScrollHandle,
    output_locked: bool,
    queue_scroll: ScrollHandle,
    chat_input: Entity<ChatInput>,
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
                    this.send(submit.text.clone(), submit.mode, window, cx)
                },
            ),
            cx.subscribe(&chat_input, |this, input, _: &TabChanged, cx| {
                this.on_ask_tab = input.read(cx).mode() == SendMode::Ask;
                cx.notify();
            }),
            cx.observe_global::<ProjectDirectory>(|this, cx| {
                this.load_queue(cx);
                this.load_history(cx);
                this.load_answers(cx);
            }),
        ];

        let mut this = Self {
            tasks: Vec::new(),
            output_scroll: ScrollHandle::new(),
            output_locked: false,
            queue_scroll: ScrollHandle::new(),
            chat_input,
            working: false,
            history_stale: false,
            _history_load: Task::ready(()),
            task_history: HistoryList {
                toggle: "history-toggle",
                list: "task-list",
                item: "history-task",
                singular: "previous task",
                plural: "previous tasks",
                back: "Back to the latest task",
                collapse_steps: false,
                id_base: 0,
                expanded: false,
                open: None,
                scroll: ScrollHandle::new(),
                opened: 0,
            },
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
            steps_shown: HashSet::new(),
            drawer_share: DRAWER_SHARE,
            drawer_dragged: None,
            body_height: Rc::default(),
            drawer_height: Rc::default(),
            drawer_fill_height: Rc::default(),
            answers: Vec::new(),
            ask_history: HistoryList {
                toggle: "ask-history-toggle",
                list: "ask-list",
                item: "ask-history-task",
                singular: "previous answer",
                plural: "previous answers",
                back: "Back to the questions",
                collapse_steps: true,
                id_base: ASK_HISTORY_IX,
                expanded: false,
                open: None,
                scroll: ScrollHandle::new(),
                opened: 0,
            },
            _ask_history_load: Task::ready(()),
            on_ask_tab: false,
            ask_session: None,
            _file_subscriptions: Vec::new(),
            summarize: commit_notes::summarize,
            _subscriptions: subscriptions,
        };
        this.load_queue(cx);
        this.load_history(cx);
        this.load_answers(cx);
        this
    }

    /// Whether the harness is working on a prompt or a question, compiling or
    /// running it.
    pub fn is_working(&self) -> bool {
        self.working || self.asks.iter().any(|ask| ask.task.status.is_active())
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

    #[cfg(test)]
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
            cx.subscribe(&file, |this, _, _: &CloseFile, cx| {
                // It slides back into the sidebar from the width it had.
                if let Some(file) = this.file.take() {
                    this.pane_closing = Some(PaneClosing {
                        file,
                        width: this.pane_width.get(),
                        slide: this.pane_opened.map_or(0, |(n, _)| n),
                        closed: Instant::now(),
                    });
                }
                cx.notify();
            }),
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
        self.output_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
        self.tasks.len() - 1
    }

    /// Applies a harness event to the task at `ix`.
    fn apply_event(&mut self, ix: usize, event: HarnessEvent, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.get_mut(ix) else {
            return;
        };
        // Follows new output only while already scrolled to the bottom.
        let following =
            self.output_scroll.offset().y <= -self.output_scroll.max_offset().y + px(1.);
        task.apply(event);
        if (following || self.output_locked) && ix + 1 == self.tasks.len() {
            self.output_scroll.scroll_to_bottom();
        }
        cx.notify();
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
            task.set_compiled(Compiled {
                anchor: anchor.into(),
                markdown,
            });
        }
        cx.notify();
    }

    /// Sends `text` in `mode` now if the harness is free, or queues it. A
    /// question is always asked now.
    fn send(&mut self, text: String, mode: SendMode, window: &mut Window, cx: &mut Context<Self>) {
        if ProjectDirectory::get(cx).is_none() {
            window.push_notification(
                Notification::error("Open a project before sending a prompt.")
                    .title("No project open"),
                cx,
            );
            return;
        }
        if mode == SendMode::Ask {
            self.ask(text, cx);
        } else if self.working {
            self.enqueue(text, mode, window, cx);
        } else {
            self.start(text, Sending::Now(mode), cx);
        }
    }

    /// Replaces the queue with the one saved for the current project. A
    /// restored queue waits to be sent.
    fn load_queue(&mut self, cx: &mut Context<Self>) {
        let saved = ProjectDirectory::get(cx)
            .map(|project_dir| prompt_queue::load(&project_dir))
            .unwrap_or_default();
        self.queue = saved
            .into_iter()
            .map(|saved| {
                self.next_queue_id += 1;
                QueueItem {
                    id: self.next_queue_id,
                    text: saved.text.clone().into(),
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
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            self._history_load = Task::ready(());
            return;
        };
        let load = cx.background_spawn(async move {
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
                if this.working {
                    this.history_stale = true;
                    return;
                }
                this.tasks = tasks;
                this.session = session;
                this.task_history.open = None;
                this.output_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
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
        mode: SendMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        self.next_queue_id += 1;
        let id = self.next_queue_id;
        self.queue.push(QueueItem {
            id,
            text: text.clone().into(),
            saved: None,
        });
        cx.notify();

        let lsp = self.chat_input.read(cx).lsp();
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let anchor = resolve_anchor(&text, mode, lsp, &project_dir)?;
                prompt_queue::add(anchor, text, &project_dir)
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = save.await;
            this.update_in(cx, |this, window, cx| {
                this.queue_saved(id, saved, &project_dir, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn queue_saved(
        &mut self,
        id: usize,
        saved: Result<QueuedPrompt>,
        project_dir: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.queue.iter().position(|item| item.id == id) else {
            // Cancelled while it was saved. A queue left behind by switching
            // projects keeps it, for when that project is opened again.
            if let Ok(saved) = saved
                && ProjectDirectory::get(cx).as_deref() == Some(project_dir)
            {
                prompt_queue::remove(&saved.file).ok();
            }
            return;
        };
        match saved {
            Ok(saved) => {
                self.queue[ix].saved = Some(saved);
                self.auto_send_next(cx);
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

    /// Sends the first queued prompt, if the harness is free and it is saved.
    fn send_next(&mut self, cx: &mut Context<Self>) {
        // Without a project it could not be sent, and must stay queued.
        if self.working
            || ProjectDirectory::get(cx).is_none()
            || !self.queue.first().is_some_and(|item| item.saved.is_some())
        {
            return;
        }
        let item = self.queue.remove(0);
        if self.queue.is_empty() {
            self.queue_held = false;
        }
        let Some(saved) = item.saved else { return };
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
            self.output_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Replaces the previous answers with the questions saved in the current
    /// project, read in the background. Questions asked now carry on the
    /// conversation the last of them left off, unless one has been asked
    /// since.
    fn load_answers(&mut self, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            self.answers.clear();
            self._ask_history_load = Task::ready(());
            return;
        };
        let load = cx.background_spawn(async move {
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
            })
            .ok();
        });
    }

    /// Sends `text` to the harness as a new task: a queued prompt with the
    /// anchor it was saved with, else with a freshly resolved one.
    fn start(&mut self, text: String, sending: Sending, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let task_ix = self.push_task(text.clone().into(), cx);
        self.tasks[task_ix].mode = match &sending {
            Sending::Now(mode) => Some(*mode),
            Sending::Queued(queued) => anchor_mode(&queued.anchor),
        };
        self.working = true;
        self.chat_input
            .update(cx, |input, cx| input.set_busy(true, cx));

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
                    Sending::Now(mode) => resolve_anchor(&text, mode, lsp, &project_dir)?,
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
                            this.show_compiled(task_ix, anchor, prompt, cx)
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
                                if let HarnessEvent::Session(id) = &event {
                                    started = true;
                                    this.session = Some(Session {
                                        project_dir: project_dir.clone(),
                                        id: id.clone(),
                                    });
                                }
                                this.apply_event(task_ix, event, cx)
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(resume) = resume.filter(|_| !started) {
                        this.update(cx, |this, _| Session::forget(&mut this.session, &resume))
                            .ok();
                    }
                }
                Err(err) => {
                    let error = format!("{err:#}");
                    record.error = Some(error.clone());
                    this.update(cx, |this, _| {
                        if let Some(task) = this.tasks.get_mut(task_ix) {
                            task.fail(error);
                        }
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
                if let Some(task) = this.tasks.get_mut(task_ix) {
                    task.end();
                    if let Err(err) = saved {
                        // Shown without failing the task: the run itself is
                        // unaffected.
                        task.reply.parts.push(ReplyPart::Error(format!(
                            "Could not save this task to the history: {err:#}"
                        )));
                    }
                }
                this.working = false;
                if this.history_stale {
                    this.load_history(cx);
                }
                this.chat_input
                    .update(cx, |input, cx| input.set_busy(false, cx));
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
                // Deferred: starting the next run replaces this task.
                let prompt_mode = cx.entity();
                cx.defer(move |cx| prompt_mode.update(cx, |this, cx| this.auto_send_next(cx)));
                cx.notify();
            })
            .ok();
        });
    }

    /// Asks `text` straight away, beside any task the harness is working on,
    /// in place of any question still open. It is saved apart from the
    /// history, so it never becomes one of the tasks.
    fn ask(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
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
                let (anchor, resolve_error) =
                    match resolve_anchor(&text, SendMode::Ask, lsp, &project_dir) {
                        Ok(anchor) => (anchor, None),
                        Err(err) => {
                            let mut anchor = HiddenAnchor::random();
                            anchor.mode = Some(SendMode::Ask);
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
                    let compiled = Compiled {
                        anchor: anchor.into(),
                        markdown: prompt,
                    };
                    if this
                        .update(cx, |this, cx| {
                            this.update_ask(run, |ask| ask.set_compiled(compiled), cx)
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
                                if let HarnessEvent::Session(id) = &event {
                                    started = true;
                                    this.ask_session = Some(Session {
                                        project_dir: project_dir.clone(),
                                        id: id.clone(),
                                    });
                                }
                                this.update_ask(run, |ask| ask.apply(event), cx)
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(resume) = resume.filter(|_| !started && !fork) {
                        this.update(cx, |this, _| {
                            Session::forget(&mut this.ask_session, &resume)
                        })
                        .ok();
                    }
                }
                Err(err) => {
                    let error = format!("{err:#}");
                    log.record.error = Some(error.clone());
                    this.update(cx, |this, cx| {
                        this.update_ask(run, |ask| ask.fail(error), cx)
                    })
                    .ok();
                }
            }
            // Once over, it opens onto its whole task table, in place of any
            // other.
            this.update(cx, |this, cx| {
                this.update_ask(run, PromptTask::end, cx);
                if this.asks.iter().any(|ask| ask.id == run) {
                    this.expand_ask(run, cx);
                }
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
            scroll: ScrollHandle::new(),
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
                ask.scroll.scroll_to_bottom();
            }
            cx.notify();
        }
    }

    /// Opens the finished question `id` onto its whole task table, closing
    /// any other down to its row.
    fn expand_ask(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(ask) = self.asks.iter().find(|ask| ask.id == id) {
            ask.scroll.set_offset(point(px(0.), px(0.)));
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
        steps(task_ix, &self.steps_shown, cx)
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
        ((body * self.drawer_share - rest) / open as f32).max(MIN_DRAWER_FILL)
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
            .map(|ask| self.render_ask_card(ask, false, cx))
            .collect();
        if history_row.is_none() && cards.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let stack = v_flex()
            .id("ask")
            .flex_none()
            .bg(theme.tab_bar)
            .border_t_1()
            .border_color(theme.border)
            .children(history_row)
            .children(cards);
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
                |this| &mut this.ask_history,
                &open_file,
                &self.steps_shown,
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
            .map(|ask| self.render_ask_card(ask, true, cx));
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
            .bottom_0()
            .occlude()
            .justify_end()
            .bg(theme.tab_bar)
            .border_t_1()
            .border_color(theme.border)
            .on_prepaint(move |bounds, _, _| drawer_height.set(bounds.size.height))
            .children(history)
            .children(card)
            .child(handle);
        // Lets UI tests find the drawer; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(drawer).into_any_element())
    }

    fn render_ask_card(&self, ask: &Ask, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
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
            if ask.locked {
                ask.scroll.scroll_to_bottom();
            }
            let output = div()
                .id(("ask-output", id))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&ask.scroll)
                .px_4()
                .pb_3()
                .child(output_table(
                    ASK_IX - id,
                    &task.reply,
                    Some(&open),
                    Some(self.steps(ASK_IX - id, cx)),
                    cx,
                ));
            // Lets UI tests find the output; inert in normal builds.
            let output = gpui_kit::TestSupportExt::test_support(output);
            let this = cx.entity().downgrade();
            let toggle: SetLock = Rc::new(move |locked, _, cx| {
                this.update(cx, |this, cx| {
                    if let Some(ask) = this.asks.iter_mut().find(|ask| ask.id == id)
                        && ask.locked != locked
                    {
                        ask.locked = locked;
                        if ask.locked {
                            ask.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                    }
                })
                .ok();
            });
            vec![
                heading.into_any_element(),
                scroll_column::with_scroll_column(
                    format!("ask-output-{id}"),
                    &ask.scroll,
                    output,
                    true,
                    Some((ask.locked, toggle)),
                    cx,
                ),
            ]
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
            .border_t_1()
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
            return card
                .with_spring(("ask-slide", id), height, |this, height| this.max_h(height))
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
    fn render_ask_dim(&self) -> AnyElement {
        let shade = SpringAnimation::new(ASK_DIM_SPRING)
            .to(if self.expanded().is_some() || self.ask_history_shown() {
                ASK_DIM
            } else {
                0.
            })
            .from(0.);
        let dim = div().id("ask-dim").absolute().inset_0().bg(black());
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
                    let row = h_flex()
                        .id(("queued-prompt", ix))
                        .gap_2()
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
                    scroll_column::with_scroll_column(
                        "queue-list",
                        &self.queue_scroll,
                        list,
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
        if self.output_locked {
            self.output_scroll.scroll_to_bottom();
        }
        let output = div()
            .id("task-output")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.output_scroll)
            .p_4()
            .child(output_table(
                self.tasks.len() - 1,
                &task.reply,
                Some(&self.file_opener(cx)),
                // A task's whole chain is of interest, so none of it collapses.
                None,
                cx,
            ));
        // Lets UI tests find the output; inert in normal builds.
        let output = gpui_kit::TestSupportExt::test_support(output);
        let this = cx.entity().downgrade();
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| this.set_output_lock(locked, cx))
                .ok();
        });
        scroll_column::with_scroll_column(
            "task-output",
            &self.output_scroll,
            output,
            true,
            Some((self.output_locked, toggle)),
            cx,
        )
    }
}

impl Render for PromptMode {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                |this| &mut this.task_history,
                &open_file,
                &self.steps_shown,
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
            // the file sliding into view from behind the sidebar's edge, and
            // the task view giving way beside it. Once it has, it is an ordinary
            // split that can be dragged.
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
                body.child(
                    h_flex()
                        .size_full()
                        .child(pane)
                        .child(div().flex_1().min_w_0().h_full().child(history)),
                )
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
            // file sliding out of view behind the sidebar's edge, and the task
            // view growing back beside it.
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
                body.child(
                    h_flex()
                        .size_full()
                        .child(pane)
                        .child(div().flex_1().min_w_0().h_full().child(history)),
                )
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
            .child(self.render_ask_dim())
            .children(self.render_ask_drawer(cx));
        // Lets UI tests find the space above the chat input; inert in normal
        // builds.
        let body = gpui_kit::TestSupportExt::test_support(body);

        v_flex()
            .size_full()
            .child(body)
            .child(self.chat_input.clone())
    }
}

/// Markdown whose headings are sized from the window's base font size; left
/// to its default, the smaller headings come out below the body text. With
/// `open`, a link to a file opens it in the editor.
fn markdown_view(
    id: impl Into<ElementId>,
    text: &str,
    open: Option<&OpenFile>,
    cx: &App,
) -> TextView {
    let view = TextView::markdown(id, markdown::without_inline_code(text)).style(TextViewStyle {
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

/// The hidden anchor `text` is sent as in `mode`: importing what `piton lsp`
/// resolved for it, with the mode's system prompt. Without `piton lsp`
/// nothing is imported, and any spec name the prompt uses fails to compile
/// with an explicit error.
fn resolve_anchor(
    text: &str,
    mode: SendMode,
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
fn relative_to_project(text: &str, project_dir: Option<&Path>) -> String {
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

/// A task's status as a coloured label, a spinner while it is under way, and
/// the name of the hidden anchor it was compiled from once it has compiled.
fn task_title(ix: usize, task: &PromptTask, cx: &App) -> Div {
    let theme = cx.theme();
    let status = div()
        .id(("task-status", ix))
        .flex_none()
        .child(task.status.tag());
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
                .child(markdown_view(
                    ("prompt", ix),
                    &compiled.markdown,
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

/// A task's output as a table: a row for each piece of text, tool call and
/// error, in order, with its kind as a badge, what it was, and a tool call's
/// state spelled out rather than only coloured. While the reply streams, its
/// last row shows the harness's latest raw output in place of what is not yet
/// known of it. With `open`, files
/// the output links to, and those its file tools worked on, open when clicked.
pub(crate) fn output_table(
    task_ix: usize,
    reply: &Reply,
    open: Option<&OpenFile>,
    steps: Option<Steps>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let rows = reply.rows();
    // An unfinished reply always has a row, so only a finished one is empty.
    if rows.is_empty() {
        return div()
            .text_color(theme.muted_foreground)
            .child("No output.")
            .into_any_element();
    }

    let header = TableHeader::new().child(
        TableRow::new()
            .child(TableHead::new().w(KIND_WIDTH).flex_none().child("Type"))
            .child(TableHead::new().flex_1().min_w_0().child("Output"))
            .child(TableHead::new().w(STATUS_WIDTH).flex_none().child("Status")),
    );
    let project_dir = ProjectDirectory::get(cx);
    let render_row = |row_ix: usize, row: &OutputRow| {
        let detail = match row {
            OutputRow::Text(text) if text.trim().is_empty() => raw_tail(reply, cx),
            OutputRow::Pending => raw_tail(reply, cx),
            OutputRow::Text(text) => div()
                .w_full()
                .min_w_0()
                .child(markdown_view(
                    ElementId::NamedInteger(format!("output-text-{task_ix}").into(), row_ix as u64),
                    text,
                    open,
                    cx,
                ))
                .into_any_element(),
            OutputRow::Tool(call) => {
                let summary = call
                    .summary
                    .as_deref()
                    .map(|summary| relative_to_project(summary, project_dir.as_deref()));
                match summary {
                    // A command is laid out across lines and highlighted, so it
                    // can be read rather than cut off.
                    Some(command) if ToolKind::of(&call.name) == ToolKind::Command => div()
                        .w_full()
                        .min_w_0()
                        .child(TextView::markdown(
                            ElementId::NamedInteger(
                                format!("output-command-{task_ix}").into(),
                                row_ix as u64,
                            ),
                            code_block("bash", &shell_format::format_command(&command)),
                        ))
                        .into_any_element(),
                    // A known tool whose input is on its way shows only a
                    // skeleton, with nothing else beside it.
                    None if row.is_partial()
                        && ToolKind::of(&call.name).pending_input().is_some() =>
                    {
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
                    .child(row.badge()),
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
    };

    // With `steps`, a finished reply's rows up to its last tool call collapse
    // behind a row that shows or hides them, leaving the answer after them.
    let collapsed = steps.filter(|_| reply.is_done()).and_then(|steps| {
        let last_tool = rows
            .iter()
            .rposition(|row| matches!(row, OutputRow::Tool(_)))?;
        (last_tool + 1 < rows.len()).then_some((steps, last_tool + 1))
    });
    let body = match collapsed {
        None => TableBody::new().children(
            rows.iter()
                .enumerate()
                .map(|(row_ix, row)| render_row(row_ix, row)),
        ),
        Some((steps, count)) => {
            let label = match (steps.shown, count) {
                (false, 1) => "Show 1 step".to_string(),
                (false, count) => format!("Show {count} steps"),
                (true, 1) => "Hide 1 step".to_string(),
                (true, count) => format!("Hide {count} steps"),
            };
            let toggle = steps.toggle;
            let toggle_row = h_flex()
                .id(("output-steps", task_ix))
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
            let toggle_row = TableRow::new().child(
                TableCell::new()
                    .flex_1()
                    .min_w_0()
                    .child(gpui_kit::TestSupportExt::test_support(toggle_row)),
            );
            let shown = if steps.shown { 0..count } else { 0..0 };
            TableBody::new()
                .child(toggle_row)
                .children(shown.map(|row_ix| render_row(row_ix, &rows[row_ix])))
                .children(
                    rows.iter()
                        .enumerate()
                        .skip(count)
                        .map(|(row_ix, row)| render_row(row_ix, row)),
                )
        }
    };

    Table::new()
        // Legible at the window's base font size; tables are otherwise smaller.
        .text_base()
        .line_height(relative(1.5))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(header)
        .child(body)
        .into_any_element()
}

/// Whether a table's steps up to its answer are shown, and how to show or
/// hide them.
pub(crate) struct Steps {
    pub shown: bool,
    pub toggle: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// The steps of prompt mode's table for `task_ix`, which a click shows or
/// hides.
fn steps(task_ix: usize, shown: &HashSet<usize>, cx: &Context<PromptMode>) -> Steps {
    let this = cx.entity().downgrade();
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

/// A tool call's state in the status column, as an icon and spelled out
/// rather than only coloured. Other rows have none.
fn row_status(row: &OutputRow, cx: &App) -> Option<AnyElement> {
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

/// A reply's latest row, the thing the harness is doing now, as the only row
/// of its task table, on a single line: the last line of its text, a tool
/// call's name and input, or the harness's latest raw output while nothing
/// else is known. A row with no status of its own shows a spinner while the
/// reply streams.
fn latest_row(id: usize, reply: &Reply, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let rows = reply.rows();
    let Some(row) = rows.last() else {
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
    let raw_line = || match reply.raw_tail.back() {
        Some(line) => div()
            .min_w_0()
            .truncate()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(highlighted_json(line, cx))
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
    let status = row_status(row, cx)
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
                            .child(row.badge()),
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
            let line: AnyElement = match reply.raw_tail.get(ix) {
                Some(line) => highlighted_json(line, cx).into_any_element(),
                None if ix == 0 => "Waiting for the harness…".into_any_element(),
                // A no-break space keeps a line with nothing in it a line tall.
                None => "\u{a0}".into_any_element(),
            };
            div().w_full().min_w_0().truncate().child(line)
        }))
        .into_any_element()
}

/// A line of the harness's raw output, highlighted as JSON. A line cut short
/// is still highlighted as far as it parses.
fn highlighted_json(line: &str, cx: &App) -> StyledText {
    let styles = json_highlights(line, &cx.theme().highlight_theme);
    StyledText::new(SharedString::from(line.to_string())).with_highlights(styles)
}

fn json_highlights(
    line: &str,
    theme: &HighlightTheme,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut highlighter = SyntaxHighlighter::new("json");
    highlighter.update(None, &Rope::from(line), None);
    highlighter.styles(&(0..line.len()), theme)
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

    use super::{
        OutputRow, PromptMode, PromptTask, RAW_LINE_CHARS, Reply, ReplyPart, Session, TaskStatus,
        ToolState,
    };
    use crate::harness::HarnessEvent;
    use crate::hidden_anchor::HiddenAnchor;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::prompt_history::{RunRecord, SavedPrompt};
    use crate::prompt_queue;

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
        let call = |name: &str, summary: Option<&str>| super::ToolCall {
            id: "t".into(),
            name: name.into(),
            summary: summary.map(Into::into),
            state: ToolState::Done,
        };
        assert_eq!(call("Read", Some("/tmp/a.txt")).shown_name(), None);
        assert_eq!(call("Bash", Some("ls")).shown_name(), None);
        assert_eq!(call("WebSearch", Some("gpui")).shown_name(), None);
        assert_eq!(call("Bash", None).shown_name(), Some("Bash"));
        assert_eq!(
            call("TodoWrite", Some("plan")).shown_name(),
            Some("TodoWrite")
        );
        assert_eq!(
            call("mcp__backlog__list", None).shown_name(),
            Some("mcp__backlog__list")
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
            let colored = super::json_highlights(line, &theme)
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
        task.set_compiled(super::Compiled {
            anchor: "Prompt_0".into(),
            markdown: "Do it".into(),
        });
        assert_eq!(task.status, TaskStatus::Running);
        task.apply(HarnessEvent::Finished {
            is_error: false,
            result: "All done.".into(),
        });
        task.end();
        assert_eq!(task.status, TaskStatus::Done);

        let mut failed = PromptTask::new("Do it".into());
        failed.set_compiled(super::Compiled {
            anchor: "Prompt_1".into(),
            markdown: "Do it".into(),
        });
        failed.apply(HarnessEvent::Failed("no harness".into()));
        failed.end();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(failed.reply.rows(), [OutputRow::Error("no harness")]);

        let mut stopped = PromptTask::new("Do it".into());
        stopped.set_compiled(super::Compiled {
            anchor: "Prompt_2".into(),
            markdown: "Do it".into(),
        });
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
                            ask.apply(HarnessEvent::TextDelta(format!("Answer to {text}")));
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
            window.try_find(("panel", 1usize)).is_some()
        })
        .await;
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.ask_history.open),
            Some(1)
        );

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
                this.send("Why?".into(), SendMode::Ask, window, cx);
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

        let (toggle, answer_rows, task_rows) = rows(cx);
        assert!(toggle, "the answer has no row for its steps");
        assert_eq!(answer_rows, 1, "only the answer shows");
        assert_eq!(task_rows, 3, "the task collapsed its chain");

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
    async fn task_output_has_a_scroll_column_that_locks_to_the_bottom(cx: &mut TestAppContext) {
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
            prompt_mode.read_with(cx, |this, _| this.output_scroll.offset().y)
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
            assert!(
                window.try_find("task-output-scroll-up").is_some()
                    && window.try_find("task-output-scroll-down").is_some()
                    && window.try_find("task-output-scroll-lock").is_some()
            );
        })
        .unwrap();
        assert!(
            prompt_mode.read_with(cx, |this, _| this.output_scroll.max_offset().y)
                > gpui_kit::px(0.),
            "the output does not scroll"
        );

        // From the top, which new output would otherwise have followed away from.
        prompt_mode.update(cx, |this, _| {
            this.output_scroll
                .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)))
        });
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
                this.output_scroll
                    .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
                this.apply_event(0, HarnessEvent::TextDelta(long.clone()), cx);
            });
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (offset, max) = prompt_mode.read_with(cx, |this, _| {
            (
                this.output_scroll.offset().y,
                this.output_scroll.max_offset().y,
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
                this.output_scroll.offset().y,
                this.output_scroll.max_offset().y,
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
                this.send("Change it".into(), SendMode::Code, window, cx);
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
                this.send("Why?".into(), SendMode::Ask, window, cx);
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
