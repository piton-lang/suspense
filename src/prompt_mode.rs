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
//! straight away, beside any task the harness is working on, and never
//! queues. It slides up out of the chat input, above its tabs and over the
//! message list, which dims behind it, as a single row of its task table
//! while the harness works on it, expanding into the whole table once it is
//! over.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

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

use crate::chat_input::{ChatInput, SendMode, Submit};
use crate::file_link::{self, OpenFile};
use crate::file_view::{CloseFile, FileView, OpenDefinition};
use crate::harness::{self, HarnessEvent};
use crate::hidden_anchor::{self, HiddenAnchor};
use crate::markdown;
use crate::piton_lsp::PitonSession;
use crate::project_directory::ProjectDirectory;
use crate::prompt_history::{self, RunRecord, SavedPrompt};
use crate::prompt_queue::{self, QueuedPrompt};
use crate::shell_format;

/// The share of the width an opened file takes from the task view.
const FILE_SHARE: f32 = 0.5;

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

/// The tallest a question's output gets once it has expanded, before it
/// scrolls.
const MAX_ASK_OUTPUT_HEIGHT: Pixels = px(320.);

/// How far a question slides up out of the chat input: enough for its single
/// row while the harness works on it, and for its heading and output once it
/// has expanded.
const ASK_ROW_HEIGHT: Pixels = px(80.);
const MAX_ASK_HEIGHT: Pixels = px(320. + 80.);

/// How a question slides up and expands: critically damped, so it settles
/// without bouncing.
const ASK_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);

/// How dark the message list gets behind an open question, and how quickly
/// it fades there and back: slower than the slide, so the dimming is seen.
const ASK_DIM: f32 = 0.45;
const ASK_DIM_SPRING: SpringConfig = SpringConfig::new(120., 22., 1.);

/// Stands in for a question's task index in its element ids.
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
}

/// A sent prompt once compiled.
struct Compiled {
    /// The name of the hidden anchor it was compiled from.
    anchor: SharedString,
    markdown: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TaskStatus {
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
            Self::Compiling => "Compiling",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Unrecorded => "Not recorded",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Compiling | Self::Running)
    }

    /// A label in the status's colour, with the status spelled out. Uses the
    /// palette colours, like the output badges, as the theme's solid status
    /// colours are unreadable in dark mode.
    fn tag(self) -> Tag {
        Tag::color(match self {
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
        }
    }

    /// A task from the history, as it ended: its run's output is replayed
    /// through the harness's parser.
    fn restore(saved: SavedPrompt) -> Self {
        let mut task = Self::new(saved.text.into());
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
            Self::Pending => return Skeleton::new().w(px(84.)).h_6().rounded_md().into_any_element(),
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

pub struct PromptMode {
    /// Every task sent, oldest first; the last heads the view.
    tasks: Vec<PromptTask>,
    /// The latest task's output, which follows new rows while scrolled to
    /// the bottom.
    output_scroll: ScrollHandle,
    chat_input: Entity<ChatInput>,
    /// The harness is working on the latest task.
    working: bool,
    /// The project changed while the harness worked; its history loads once
    /// the run is over.
    history_stale: bool,
    _history_load: Task<()>,
    /// The header is expanded into the list of every task.
    history_expanded: bool,
    /// The task opened in the expanded list.
    open_task: Option<usize>,
    task_list_scroll: ScrollHandle,
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
    file_split: Entity<ResizableState>,
    /// The width the task view and any file share, as last laid out.
    body_width: Rc<Cell<Pixels>>,
    _pending: Task<()>,
    /// The conversation the tasks share, each resuming the last.
    session: Option<Session>,
    /// The question asked from the Ask tab, run apart from the tasks.
    ask: Option<PromptTask>,
    /// Counts the questions asked, so a replaced one's run changes nothing.
    ask_run: usize,
    ask_scroll: ScrollHandle,
    _ask_pending: Task<()>,
    /// The conversation questions share, apart from the tasks'.
    ask_session: Option<Session>,
    _file_subscriptions: Vec<Subscription>,
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
            cx.observe_global::<ProjectDirectory>(|this, cx| {
                this.load_queue(cx);
                this.load_history(cx);
            }),
        ];

        let mut this = Self {
            tasks: Vec::new(),
            output_scroll: ScrollHandle::new(),
            chat_input,
            working: false,
            history_stale: false,
            _history_load: Task::ready(()),
            history_expanded: false,
            open_task: None,
            task_list_scroll: ScrollHandle::new(),
            queue: Vec::new(),
            next_queue_id: 0,
            queue_expanded: false,
            auto_send: true,
            queue_held: false,
            file: None,
            file_split: cx.new(|_| ResizableState::default()),
            body_width: Rc::default(),
            _pending: Task::ready(()),
            session: None,
            ask: None,
            ask_run: 0,
            ask_scroll: ScrollHandle::new(),
            _ask_pending: Task::ready(()),
            ask_session: None,
            _file_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.load_queue(cx);
        this.load_history(cx);
        this
    }

    /// Whether the harness is working on a prompt or a question, compiling or
    /// running it.
    pub fn is_working(&self) -> bool {
        self.working || self.ask.as_ref().is_some_and(|ask| ask.status.is_active())
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
        }
        let file = cx.new(|cx| FileView::new(path, position, window, cx));
        self._file_subscriptions = vec![
            cx.subscribe(&file, |this, _, _: &CloseFile, cx| {
                this.file = None;
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
        if following && ix + 1 == self.tasks.len() {
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
                this.open_task = None;
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

    fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.history_expanded = !self.history_expanded;
        if self.history_expanded {
            // The latest tasks are the likeliest to be looked for.
            self.task_list_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Sends `text` to the harness as a new task: a queued prompt with the
    /// anchor it was saved with, else with a freshly resolved one.
    fn start(&mut self, text: String, sending: Sending, cx: &mut Context<Self>) {
        let Some(project_dir) = ProjectDirectory::get(cx) else {
            return;
        };
        let task_ix = self.push_task(text.clone().into(), cx);
        self.working = true;
        self.chat_input
            .update(cx, |input, cx| input.set_busy(true, cx));

        let resume = Session::resume(&self.session, &project_dir);
        let lsp = self.chat_input.read(cx).lsp();
        let compile = cx.background_spawn({
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
        });

        self._pending = cx.spawn(async move |this, cx| {
            // What came of the prompt, saved beside it in the history once it
            // is over.
            let mut record = RunRecord::default();
            let mut prompt_file = None;
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
                        resume.clone(),
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
        let resume = Session::resume(&self.ask_session, &project_dir);
        let lsp = self.chat_input.read(cx).lsp();
        let compile = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let anchor = resolve_anchor(&text, SendMode::Ask, lsp, &project_dir)?;
                let file = hidden_anchor::save_ask(&anchor, &text, &project_dir)?;
                let compiled = hidden_anchor::compile(&anchor, &file, &project_dir)?;
                anyhow::Ok((anchor.name().to_string(), compiled))
            }
        });

        // Replacing the run drops any earlier question's, which stops it.
        self._ask_pending = cx.spawn(async move |this, cx| {
            match compile.await {
                Ok((anchor, compiled)) => {
                    let prompt = compiled.user_prompt;
                    let mut events = harness::send(
                        prompt.clone(),
                        compiled.system_prompt,
                        resume.clone(),
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
                    if let Some(resume) = resume.filter(|_| !started) {
                        this.update(cx, |this, _| {
                            Session::forget(&mut this.ask_session, &resume)
                        })
                        .ok();
                    }
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.update_ask(run, |ask| ask.fail(format!("{err:#}")), cx)
                    })
                    .ok();
                }
            }
            this.update(cx, |this, cx| this.update_ask(run, PromptTask::end, cx))
                .ok();
        });
    }

    /// Opens a question for `text`, compiling, in place of any other.
    /// Returns its run.
    fn push_ask(&mut self, text: SharedString, cx: &mut Context<Self>) -> usize {
        self.ask_run += 1;
        self.ask = Some(PromptTask::new(text));
        self.ask_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
        self.ask_run
    }

    /// Updates the open question, if it is still the one of `run`.
    fn update_ask(
        &mut self,
        run: usize,
        update: impl FnOnce(&mut PromptTask),
        cx: &mut Context<Self>,
    ) {
        if let Some(ask) = self.ask.as_mut().filter(|_| self.ask_run == run) {
            update(ask);
            cx.notify();
        }
    }

    /// Closes the question, stopping its run if it is still under way.
    fn close_ask(&mut self, cx: &mut Context<Self>) {
        self.ask = None;
        self.ask_run += 1;
        self._ask_pending = Task::ready(());
        cx.notify();
    }

    /// The open question, sliding up out of the chat input above its tabs.
    /// While the harness works on it, its task table is a single row: the
    /// latest thing the harness did. Once it is over, it expands into the
    /// whole table, headed by the question.
    fn render_ask(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.ask.as_ref()?;
        let done = ask.reply.is_done();
        let close = Button::new("close-ask")
            .ghost()
            .xsmall()
            .icon(IconName::X)
            .tooltip(if done { "Close" } else { "Stop and close" })
            .on_click(cx.listener(|this, _, _, cx| this.close_ask(cx)));
        let content: Vec<AnyElement> = if done {
            let open = self.file_opener(cx);
            let heading = h_flex()
                .flex_none()
                .gap_3()
                .px_4()
                .py_1p5()
                .child(div().flex_none().child(task_title(ASK_IX, ask, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(first_line(&ask.text)),
                )
                .child(close);
            let output = div()
                .id("ask-output")
                .min_h_0()
                .max_h(MAX_ASK_OUTPUT_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&self.ask_scroll)
                .px_4()
                .pb_3()
                .child(output_table(ASK_IX, &ask.reply, Some(&open), cx));
            vec![
                heading.into_any_element(),
                // Lets UI tests find the output; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(output).into_any_element(),
            ]
        } else {
            let row = h_flex()
                .id("ask-row")
                .flex_none()
                .gap_2()
                .px_4()
                .py_1p5()
                .child(div().flex_1().min_w_0().child(latest_row(&ask.reply, cx)))
                .child(close);
            // Lets UI tests find the row; inert in normal builds.
            vec![gpui_kit::TestSupportExt::test_support(row).into_any_element()]
        };

        let theme = cx.theme();
        // Each question slides up from nothing; expanding carries on from
        // wherever the row is.
        let height = SpringAnimation::new(ASK_SPRING)
            .to(if done { MAX_ASK_HEIGHT } else { ASK_ROW_HEIGHT })
            .from(px(0.));
        let panel = v_flex()
            .id("ask")
            // Laid over the bottom of the message list rather than beside it,
            // so the list neither jumps nor reflows as the question rises,
            // and takes no clicks or scrolling meant for the question.
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            // Anchored to the chat input, so it rises out of it rather than
            // unrolling down onto it.
            .justify_end()
            .overflow_hidden()
            .bg(theme.tab_bar)
            .border_t_1()
            .border_color(theme.border)
            .children(content);
        let panel = gpui_kit::TestSupportExt::test_support(panel)
            .with_spring(("ask-slide", self.ask_run), height, |this, height| {
                this.max_h(height)
            });
        Some(panel.into_any_element())
    }

    /// A shade over the message list that fades in while a question is open,
    /// drawing the eye to it, and back out once it closes. It takes no
    /// clicks, so the list stays usable behind it.
    fn render_ask_dim(&self) -> AnyElement {
        let shade = SpringAnimation::new(ASK_DIM_SPRING)
            .to(if self.ask.is_some() { ASK_DIM } else { 0. })
            .from(0.);
        let dim = div().id("ask-dim").absolute().inset_0().bg(black());
        // Lets UI tests find the shade; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(dim)
            .with_spring("ask-dim", shade, |this, shade| {
                this.opacity(shade.clamp(0., 1.))
            })
            .into_any_element()
    }

    /// The small row above the header that expands it into the list of every
    /// task. It is always shown, and does nothing until a task has been sent.
    fn render_history_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let label = match self.tasks.len().saturating_sub(1) {
            0 => "No previous tasks".to_string(),
            1 => "1 previous task".to_string(),
            n => format!("{n} previous tasks"),
        };
        h_flex()
            .flex_none()
            .px_2()
            .py_0p5()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("history-toggle")
                    .ghost()
                    .xsmall()
                    .icon(if self.history_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(if self.history_expanded {
                        "Back to the latest task".to_string()
                    } else {
                        label
                    })
                    .disabled(self.tasks.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_history(cx))),
            )
            .into_any_element()
    }

    /// The header pinned above the output: the latest task, unless the
    /// history is expanded, where it is listed.
    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (ix, task) = self
            .tasks
            .last()
            .filter(|_| !self.history_expanded)
            .map(|task| (self.tasks.len() - 1, task))?;
        let open = self.file_opener(cx);
        let theme = cx.theme();
        let header = v_flex()
            .id("task-header")
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .bg(theme.tab_bar)
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
                .children(list)
                .into_any_element(),
        )
    }

    /// The latest task's output, filling the space under the header.
    fn render_output(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(task) = self.tasks.last() else {
            return div().into_any_element();
        };
        let output = div()
            .id("task-output")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.output_scroll)
            .p_4()
            .child(output_table(
                self.tasks.len() - 1,
                &task.reply,
                Some(&self.file_opener(cx)),
                cx,
            ));
        // Lets UI tests find the output; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(output).into_any_element()
    }

    /// Every task, oldest first, as an accordion: each is headed by its
    /// status, anchor and prompt, and opens onto its prompt and output.
    fn render_task_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let open_file = self.file_opener(cx);
        let accordion = self.tasks.iter().enumerate().fold(
            Accordion::new("task-list").h_auto(),
            |accordion, (ix, task)| {
                let open = self.open_task == Some(ix);
                accordion.item(|item| {
                    item.open(open)
                        .title(task_summary(ix, task, cx))
                        // Closed tasks are not laid out.
                        .when(open, |item| {
                            item.child(
                                v_flex()
                                    .gap_3()
                                    .pt_1()
                                    .child(task_prompt(ix, task, &open_file, cx))
                                    .child(output_table(ix, &task.reply, Some(&open_file), cx)),
                            )
                        })
                })
            },
        );
        let accordion = accordion.on_toggle_click(move |open, _, cx| {
            this.update(cx, |this, cx| {
                this.open_task = open.first().copied();
                cx.notify();
            })
            .ok();
        });
        let list = div()
            .id("task-list-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.task_list_scroll)
            .p_4()
            .child(accordion);
        // Lets UI tests find the list; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(list).into_any_element()
    }
}

impl Render for PromptMode {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        } else if self.history_expanded {
            self.render_task_list(cx)
        } else {
            self.render_output(cx)
        };

        let history = v_flex()
            .id("history")
            .size_full()
            .child(self.render_history_row(cx))
            .children(self.render_header(cx))
            .child(content)
            .children(self.render_queue(cx));
        // Lets UI tests find the history; inert in normal builds.
        let history = gpui_kit::TestSupportExt::test_support(history);
        let history = div()
            .relative()
            .size_full()
            .child(history)
            .child(self.render_ask_dim());

        // The task view, and any file split off it, sit above the chat input,
        // with any question rising over them out of it.
        let body = div().relative().flex_1().min_h_0().on_prepaint({
            let body_width = self.body_width.clone();
            move |bounds, _, _| body_width.set(bounds.size.width)
        });
        let body = match &self.file {
            Some(file) => {
                let mut file_panel = resizable_panel().size_range(MIN_SPLIT_WIDTH..Pixels::MAX);
                // Not laid out yet: the split starts even, and can be dragged.
                if self.body_width.get() > px(0.) {
                    file_panel = file_panel.size(self.body_width.get() * FILE_SHARE);
                }
                body.child(
                    h_resizable("file-split")
                        .with_state(&self.file_split)
                        .children([
                            file_panel.child(file.clone()),
                            resizable_panel()
                                .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                                .child(history),
                        ]),
                )
            }
            None => body.child(history),
        };
        let body = body.children(self.render_ask(cx));

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
    anchor.system_prompt = Some(hidden_anchor::system_prompt(mode, project_dir)?);
    Ok(anchor)
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
fn task_summary(ix: usize, task: &PromptTask, cx: &App) -> AnyElement {
    let summary = h_flex()
        .id(("history-task", ix))
        .flex_1()
        .min_w_0()
        .gap_3()
        .child(div().flex_none().child(task_title(ix, task, cx)))
        .child(div().flex_1().min_w_0().truncate().child(first_line(&task.text)));
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
    let body = TableBody::new().children(rows.iter().enumerate().map(|(row_ix, row)| {
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
                        Skeleton::new().w(relative(0.6)).h_4().rounded_md().into_any_element()
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
    }));

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
fn latest_row(reply: &Reply, cx: &App) -> AnyElement {
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
    let status = row_status(row, cx).or_else(|| {
        (!reply.is_done()).then(|| Spinner::new().small().into_any_element())
    });
    let detail = div()
        .id("ask-row-detail")
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
        OutputRow, PromptMode, PromptTask, RAW_LINE_CHARS, Reply, Session, TaskStatus, ToolState,
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
        assert_eq!(reply.rows(), [OutputRow::Text("Looking."), OutputRow::Pending]);

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
        assert_eq!(Session::resume(&session, project).as_deref(), Some("latest"));
        assert_eq!(Session::resume(&session, std::path::Path::new("/other")), None);

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
        assert_eq!(prompt_mode.read_with(cx, |this, _| this.open_task), Some(0));
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
                assert!(this.ask.is_some(), "the question was not asked");
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(5), |_, cx| {
            prompt_mode
                .read(cx)
                .ask
                .as_ref()
                .is_some_and(|ask| ask.status == TaskStatus::Failed)
        })
        .await;
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.working, "the task stopped working");
            assert_eq!(this.tasks[0].status, TaskStatus::Compiling);
        });
        assert!(!crate::hidden_anchor::history_dir(&dir).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    type Bounds = gpui_kit::Bounds<gpui_kit::Pixels>;

    /// Draws frames until `id` is laid out with `settled` holding of it and
    /// the chat input's tabs.
    fn settle(
        handle: AnyWindowHandle,
        id: &'static str,
        settled: impl Fn(Bounds, Bounds) -> bool,
        cx: &mut TestAppContext,
    ) -> (Bounds, Bounds) {
        let start = std::time::Instant::now();
        loop {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let bounds = window.try_find(id)?.bounds();
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
                "{id} did not settle: {found:?}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// A question rises over the message list rather than appearing at its
    /// full height, and a shade covering the list comes with it; the list
    /// keeps its size underneath.
    #[gpui_kit::test]
    async fn a_question_slides_up_over_the_dimmed_message_list(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let find = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).map(|found| found.bounds())
            })
            .unwrap()
        };
        let history = find("history", cx).unwrap();
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

        let dim = find("ask-dim", cx).unwrap();
        assert_eq!(dim, history, "the shade does not cover the message list");
        assert_eq!(
            find("history", cx).unwrap(),
            history,
            "the message list moved for the question"
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
            "ask-row",
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
            assert!(window.try_find("ask-output").is_none());
        })
        .unwrap();

        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let run = this.ask_run;
                this.update_ask(
                    run,
                    |ask| {
                        ask.apply(HarnessEvent::TextStarted);
                        ask.apply(HarnessEvent::TextDelta("It joins Code and Spec.".into()));
                        ask.apply(HarnessEvent::Finished {
                            is_error: false,
                            result: String::new(),
                        });
                    },
                    cx,
                );
            });
        })
        .unwrap();

        let (output, tabs) = settle(
            handle,
            "ask-output",
            |output, _| output.size.height > line_height * 3.,
            cx,
        );
        assert!(
            output.bottom() <= tabs.top() + gpui_kit::px(1.),
            "the expanded question {output:?} is not above the tabs {tabs:?}"
        );
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("ask-row").is_none());
            assert!(window.try_find(("output-row", 1usize)).is_some());
        })
        .unwrap();
    }
}
