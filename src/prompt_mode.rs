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
//! question, and never queues. While the Ask tab is selected, the body splits
//! to show every question and its answer as a chat (see [`ask_pane`]); on any
//! other tab, the questions still running are rows stacked above the chat
//! input's tabs.

mod ask_pane;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::channel::oneshot;
use futures::{FutureExt as _, StreamExt as _};
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::base::TextSelection;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{Table, TableBody, TableCell, TableRow};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::activity::{Job, JobKind};
use crate::attached_image::{self, AttachedImage};
use crate::chat_input::{
    self, ChatInput, FocusActiveEditor, Lanes, NewConversation, PreviewPrompt, QueuedEdit,
    SendMode, Submit, TabChanged,
};
use crate::commit_notes;
use crate::conversations;
use crate::file_link::OpenFile;
use crate::file_view::{ChangedOnDisk, CloseFile, FileView, OpenDefinition, SendToPrompt};
use crate::harness::{self, HarnessEvent};
use crate::hidden_anchor::{self, Attached, CodeTask, HiddenAnchor};
use crate::markdown;
use crate::markdown::{MarkdownKey, MarkdownKind, MarkdownStates};
use crate::measured_list::{MeasuredList, RenderRow};
use crate::mode_guard;
use crate::piton_build;
use crate::piton_lsp::PitonSession;
use crate::project_directory::ProjectDirectory;
use crate::prompt_history::{self, RunRecord, SavedPrompt};
use crate::prompt_queue::{self, QueuedPrompt};
use crate::prompt_title;
use crate::raw_prompt::{Given, RawPrompt, RawPromptView};
use crate::referenced_spec;
use crate::scrollbar::{self, SetLock};
use crate::selection_popover::{SelectionAction, selection_popover};
use crate::shell_paths;
use crate::subagents::Subagents;
use crate::system_prompts;
use crate::task_snapshot;
use crate::task_table::{
    self, KIND_WIDTH, Layout as TableLayout, OutputRow, Reply, STATUS_WIDTH, Steps, TableSync,
    TableView, TaskTable, ToolKind, relative_to_project, row_status, shown_markdown_view,
};
use crate::theme::Hue;
use crate::understanding::{self, Understanding};
use crate::usage::{self, Conversation, PlanLimits, ProjectUsage, UsageReport};
use ask_pane::{ASK_SPLIT_SHARE, AskPane, AskSplitResize};

/// How long the file pane takes to slide in from the sidebar, or back into it
/// when closed, and how it moves: critically damped, so it settles without
/// bouncing.
const PANE_SLIDE_TIME: Duration = Duration::from_millis(450);
const PANE_SPRING: SpringConfig = SpringConfig::new(300., 35., 1.);

/// The narrowest the task view can be dragged beside the referenced spec
/// sidebar.
const MIN_SPLIT_WIDTH: Pixels = px(160.);

/// The tallest the expanded queue gets before it scrolls.
const MAX_QUEUE_HEIGHT: Pixels = px(240.);

/// The tallest the stack of question rows grows before it scrolls.
const MAX_ASK_STACK_HEIGHT: Pixels = MAX_QUEUE_HEIGHT;

/// The most question rows the stack draws as they are, every one in view,
/// before it holds more than fit and draws only those in view. Rows slide up
/// from nothing as they are asked, so a stack that fits is drawn whole.
const PLAIN_ASK_ROWS: usize = 6;

/// The tallest the compiled prompt in the header gets before it scrolls.
const MAX_PROMPT_HEIGHT: Pixels = px(160.);

/// How far a question slides up out of the chat input while it is a single
/// row.
const ASK_ROW_HEIGHT: Pixels = px(80.);

/// How a running question's row slides up out of the chat input: critically
/// damped, so it settles without bouncing.
const ASK_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);

/// Where a saved question's task index starts in its element ids, apart
/// from the tasks' and the questions asked since.
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
    /// Sent to the other mode, the name of the task it was sent from, known
    /// from the moment it is queued, before it is saved.
    sent_from: Option<String>,
    /// The images attached to it, by their paths from the project directory,
    /// known from the moment it is queued.
    images: Vec<String>,
    /// It starts a new conversation rather than carrying on the tasks',
    /// as its hidden anchor records once saved.
    new_conversation: bool,
    /// The mode it was sent in, known from the moment it is queued, which
    /// sets the lane it waits for.
    mode: Option<SendMode>,
    /// Its mark of when it was queued, which orders it on disk: see
    /// [`prompt_queue::stamp`].
    queued_at: u128,
}

/// How a task's Send to Spec or Send to Code button stands. Only
/// [`Self::Offered`] is enabled; the others are greyed out and disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OtherModeState {
    /// It can be sent there.
    Offered,
    /// A prompt sent there from it waits in the queue, or a task sent from
    /// it is building, compiling, or running.
    Sending,
    /// A task sent there from it finished as Done.
    Sent,
    /// It was marked done by hand, from the button's menu.
    MarkedDone,
    /// It was itself sent from the other mode (its [`SentAs::sent_from`] is
    /// set), so it has been through both the code and the spec and never
    /// offers to be sent back, for good. Its button opens no menu.
    Complete,
}

impl OtherModeState {
    /// The button's tooltip, sending to `to`.
    fn tooltip(self, to: SendMode) -> String {
        let to = to.label();
        match self {
            Self::Offered => format!("Send to {to}"),
            Self::Sending => format!("Being sent to {to}"),
            Self::Sent => format!("Sent to {to}"),
            Self::MarkedDone => "Marked as done".to_string(),
            // Sent here from the mode it would be sent back to.
            Self::Complete => format!("Complete, sent here from {to}"),
        }
    }
}

/// The menu right-clicking a Send to Spec or Send to Code button opens,
/// where it was right-clicked.
struct MarkMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    /// What had focus before, given back if the menu closes without anything
    /// else taking it.
    previous_focus: Option<FocusHandle>,
    _dismissed: Subscription,
}

/// The one item of a Send to Spec or Send to Code button's menu, for a task
/// `marked_done` or not.
fn mark_label(marked_done: bool) -> &'static str {
    if marked_done {
        "Mark as not done"
    } else {
        "Mark as done"
    }
}

/// A prompt on its way to the harness.
enum Sending {
    /// Typed and sent straight away, in a mode, with anything attached,
    /// whether it is sent sliced, for a Code task sent to Spec, the code
    /// task it was sent from, and, for a task sent to the other mode, the
    /// name of the task it was sent from; for a chain or its code step,
    /// whether a post-build spec update follows it; and, for one sent after
    /// another, resent or a chain's next step, the name of the prompt it is
    /// named after.
    Now(
        SendMode,
        Attached,
        bool,
        Option<CodeTask>,
        Option<String>,
        bool,
        Option<String>,
    ),
    /// Out of the queue, saved with its anchor.
    Queued(QueuedPrompt),
}

/// A prompt sent to the harness, and what the harness did with it.
struct PromptTask {
    /// The name of its hidden anchor, known from the moment it is sent, and
    /// what a task sent to the other mode from it knows it by.
    name: SharedString,
    /// The prompt as typed, shown until it compiles.
    text: SharedString,
    compiled: Option<Compiled>,
    reply: Reply,
    status: TaskStatus,
    /// The mode it was sent in, when known, which tints its header.
    mode: Option<SendMode>,
    /// The spec files its prompt imports from, once it has compiled.
    imported: Vec<PathBuf>,
    /// The spec location it was sent against, once it has compiled.
    spec_dir: Option<PathBuf>,
    /// The files its run references, for the referenced spec sidebar.
    references: referenced_spec::References,
    /// The constraints the harness has taken from the spec for it, as its
    /// understanding file last read; a question has none.
    understanding: Understanding,
    /// Follows its understanding file while it runs.
    _understanding_watch: Task<()>,
    /// The subagents its run started, for the right sidebar.
    subagents: Subagents,
    /// How it was sent, to send it again the same way.
    sent: SentAs,
    /// Its spec slices are shown, rather than collapsed.
    slices_open: bool,
    /// Cancels it while it is under way; none for a task not sent from here.
    cancel: Option<Cancel>,
    /// The harness it was sent to and the system prompt that harness
    /// received, once sent; none for one from the history saved before these
    /// were kept.
    given: Option<Given>,
    /// Marked done by hand for sending to the other mode, from its Send to
    /// Spec or Send to Code button's menu: its button is disabled as though
    /// it had been sent there, and the batch actions leave it out. Kept in
    /// its history record.
    marked_done: bool,
    /// The conversation its run was in, once the harness has said.
    session: Option<SharedString>,
    /// In a git repository, the files that changed while it ran, once its
    /// run is over and both snapshots were taken.
    changed: Option<ChangedFiles>,
    /// Its changed files are listed, rather than collapsed to their heading.
    changed_open: bool,
    /// It started a new conversation rather than carrying one on, as its
    /// hidden anchor recorded; false for one saved before that was.
    new_conversation: bool,
    /// When it was sent, in seconds since the Unix epoch, once known.
    asked_at: Option<u64>,
    /// Sends it more while it runs, where its harness can be fed more.
    feed: Option<harness::Feed>,
}

/// Asks for a file's changes while a task ran, between the snapshots taken
/// as it started and ended, to be opened in the diff view.
pub struct OpenSnapshotDiff {
    pub top: PathBuf,
    pub path: PathBuf,
    pub from: Option<PathBuf>,
    pub before: String,
    pub after: String,
}

impl EventEmitter<OpenSnapshotDiff> for PromptMode {}

/// The files that changed while a task ran, between the snapshots of the
/// working tree taken as it started and ended.
#[derive(Clone, Debug)]
struct ChangedFiles {
    /// The repository's top, which the paths are from.
    top: PathBuf,
    before: String,
    after: String,
    /// Each file, and whether the agent's tool calls reported changing it.
    files: Vec<(task_snapshot::Change, bool)>,
}

impl ChangedFiles {
    /// The project's own files between `before` and `after` in the
    /// repository `top`, never what the application keeps in `.suspense` or
    /// its draft, each marked as the agent's where it is among `edited`; none
    /// where git can't say.
    fn read(top: PathBuf, before: String, after: String, edited: &[PathBuf]) -> Option<Self> {
        let changes =
            task_snapshot::project_own(task_snapshot::changes(&top, &before, &after).ok()?);
        let by_agent = |path: &Path| {
            let full = shell_paths::normalize(&top.join(path));
            edited.iter().any(|edited| *edited == full)
        };
        let files = changes
            .into_iter()
            .map(|change| {
                let agent = by_agent(&change.path)
                    || change.from.as_deref().is_some_and(|from| by_agent(from));
                (change, agent)
            })
            .collect();
        Some(Self {
            top,
            before,
            after,
            files,
        })
    }
}

/// Cancels a task under way, from its Cancel button: see
/// [`PromptMode::cancel_task`].
struct Cancel {
    /// Set once it is cancelled, for what compiles it, off the main thread.
    cancelled: Arc<AtomicBool>,
    /// Wakes what is sending it, to end it straight away.
    signal: Option<oneshot::Sender<()>>,
    /// Stops its run, once it has one.
    stop: Option<harness::Stop>,
}

impl Cancel {
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(stop) = &self.stop {
            stop.stop();
        }
        if let Some(signal) = self.signal.take() {
            signal.send(()).ok();
        }
    }
}

/// How a prompt was sent, besides its text.
#[derive(Clone, Debug, Default, PartialEq)]
struct SentAs {
    /// Its mode, unknown for a prompt saved before modes were.
    mode: Option<SendMode>,
    attached_text: Vec<String>,
    /// Its attached images, by their paths from the project directory.
    attached_images: Vec<String>,
    sliced: bool,
    /// Sent to Spec from a Code task, that code task, and for a chain's code
    /// step, the chain's spec step, told to it again whenever it is resent.
    code_task: Option<CodeTask>,
    /// Sent to the other mode, from Code or from Spec, the name of the task
    /// it was sent from (see [`PromptTask::name`]), kept whenever it is
    /// resent. That task no longer offers to be sent there while this one is
    /// under way or once it is done.
    sent_from: Option<String>,
    /// A chain, or its code step, followed by a post-build spec update.
    post_build_update: bool,
}

impl SentAs {
    fn of(anchor: &HiddenAnchor) -> Self {
        Self {
            mode: anchor_mode(anchor),
            attached_text: anchor.attached_text.clone(),
            attached_images: anchor.attached_images.clone(),
            sliced: anchor.sliced,
            code_task: anchor.code_task.clone(),
            sent_from: anchor.sent_from.clone(),
            post_build_update: anchor.post_build_update,
        }
    }

    /// What was attached to it.
    fn attached(&self) -> Attached {
        Attached {
            text: self.attached_text.clone(),
            images: self.attached_images.clone(),
        }
    }
}

/// A sent prompt once compiled.
struct Compiled {
    /// The name of the hidden anchor it was compiled from.
    anchor: SharedString,
    markdown: String,
    /// The markdown as it is shown, once worked out.
    shown: std::cell::OnceCell<SharedString>,
    /// What is shown, split into the blocks the latest task's header draws
    /// one at a time.
    blocks: std::cell::OnceCell<Arc<[SharedString]>>,
    /// The spec slices it was sent with, as they are shown, how many, and in
    /// blocks, apart from the prompt, which is shown first.
    slices: std::cell::OnceCell<Option<Slices>>,
}

/// A sliced prompt's slices, as they are shown.
struct Slices {
    shown: SharedString,
    count: usize,
    blocks: Arc<[SharedString]>,
}

impl Compiled {
    fn new(anchor: SharedString, markdown: String) -> Self {
        Self {
            anchor,
            markdown,
            shown: std::cell::OnceCell::new(),
            blocks: std::cell::OnceCell::new(),
            slices: std::cell::OnceCell::new(),
        }
    }

    /// The spec slices it was sent with, if it was sliced.
    fn slices(&self) -> Option<&Slices> {
        self.slices
            .get_or_init(|| {
                let (slices, count) = markdown::split_slices(&self.markdown).slices?;
                let shown = markdown::without_inline_code(&slices);
                Some(Slices {
                    blocks: markdown::blocks(&shown)
                        .into_iter()
                        .map(SharedString::from)
                        .collect(),
                    shown: shown.into(),
                    count,
                })
            })
            .as_ref()
    }

    /// What is shown, in blocks: see [`markdown::blocks`].
    fn blocks(&self) -> Arc<[SharedString]> {
        self.blocks
            .get_or_init(|| {
                markdown::blocks(&self.shown())
                    .into_iter()
                    .map(SharedString::from)
                    .collect()
            })
            .clone()
    }

    /// The markdown as it is shown: what was typed, and any text attached,
    /// without the spec slices, which are shown apart.
    fn shown(&self) -> SharedString {
        self.shown
            .get_or_init(|| {
                let prompt = markdown::split_slices(&self.markdown).prompt;
                markdown::without_inline_code(&prompt).into()
            })
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
    /// Cancelled while under way, from its Cancel button.
    Cancelled,
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
            Self::Cancelled => "Cancelled",
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
                Self::Cancelled => Hue::Orange,
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
            name: HiddenAnchor::random_name().into(),
            text,
            compiled: None,
            reply: Reply::default(),
            status: TaskStatus::Compiling,
            mode: None,
            imported: Vec::new(),
            spec_dir: None,
            references: referenced_spec::References::default(),
            understanding: Understanding::default(),
            _understanding_watch: Task::ready(()),
            subagents: Subagents::default(),
            sent: SentAs::default(),
            slices_open: false,
            cancel: None,
            given: None,
            marked_done: false,
            session: None,
            changed: None,
            changed_open: false,
            new_conversation: false,
            asked_at: None,
            feed: None,
        }
    }

    /// A task from the history, as it ended: its run's output is replayed
    /// through the harness's parser. A record holding only the task's mark
    /// leaves it a task without a record, marked.
    #[cfg(test)]
    fn restore(saved: SavedPrompt) -> Self {
        Self::restore_in(saved, None)
    }

    /// A task from the history of the project `project_dir`, whose files are
    /// read from there, and, in a git repository, with the files that changed
    /// while it ran. Called off the UI thread, since that asks git.
    fn restore_in(saved: SavedPrompt, project_dir: Option<&Path>) -> Self {
        let snapshots = saved.record.as_ref().and_then(|record| {
            Some((
                record.snapshot_before.clone()?,
                record.snapshot_after.clone()?,
            ))
        });
        let mut task = Self::restore_replayed(saved, project_dir);
        if let (Some(dir), Some((before, after))) = (project_dir, snapshots)
            && let Some(top) = task_snapshot::repo_top(dir)
        {
            let edited = task.references.edited_paths();
            task.changed = ChangedFiles::read(top, before, after, &edited);
        }
        task
    }

    fn restore_replayed(saved: SavedPrompt, project_dir: Option<&Path>) -> Self {
        let mut task = Self::new(saved.text.into());
        task.references = referenced_spec::References::new(project_dir.map(Path::to_path_buf));
        task.name = saved.anchor.name().to_string().into();
        task.mode = anchor_mode(&saved.anchor);
        task.sent = SentAs::of(&saved.anchor);
        task.new_conversation = saved.anchor.new_conversation.unwrap_or(false);
        task.asked_at = (saved.sent_at > 0).then_some(saved.sent_at);
        task.marked_done = saved
            .record
            .as_ref()
            .is_some_and(|record| record.marked_done);
        let Some(record) = saved.record.filter(|record| !record.holds_only_the_mark()) else {
            task.reply.stop();
            task.status = TaskStatus::Unrecorded;
            return task;
        };
        task.given = record.harness().map(|harness| Given {
            harness,
            system_prompt: record.system_prompt.clone(),
            instructions: record.instructions.clone(),
            resumed: record.resumed,
        });
        if let Some(markdown) = record.user_prompt.clone() {
            task.set_compiled(Compiled::new(
                saved.anchor.name().to_string().into(),
                markdown,
            ));
        }
        for event in record.events() {
            task.apply(event);
        }
        if record.cancelled {
            task.cancel();
        } else if let Some(error) = record.error {
            task.apply(HarnessEvent::Failed(error));
        }
        task.end();
        task
    }

    /// Compiled, it runs, unless it was cancelled meanwhile.
    fn set_compiled(&mut self, compiled: Compiled) {
        self.compiled = Some(compiled);
        if self.status.is_active() {
            self.status = TaskStatus::Running;
        }
    }

    /// Folds a harness event into the task's output and status.
    fn apply(&mut self, event: HarnessEvent) {
        self.apply_event(event);
        self.settle_outcome();
    }

    /// Once its run is over, heads its final summary with what it did: the
    /// spec, code, or other files its tool calls wrote or edited, and with
    /// none, whether it answered.
    fn settle_outcome(&mut self) {
        if !self.reply.done {
            return;
        }
        let project_dir = self.references.project_dir().to_path_buf();
        let root = |key: &str| {
            crate::hidden_anchor::config_value(&project_dir, key)
                .ok()
                .map(|dir| shell_paths::normalize(&project_dir.join(dir)))
        };
        let spec_dir = self.spec_dir.clone().or_else(|| root("root"));
        let code_dir = root("codeRoot");
        let tags = crate::task_table::outcome_tags(
            &self.references.edited_paths(),
            &project_dir,
            spec_dir.as_deref(),
            code_dir.as_deref(),
            self.reply.final_output().is_some(),
        );
        self.reply.set_outcome(tags);
    }

    fn apply_event(&mut self, event: HarnessEvent) {
        if let HarnessEvent::Session(id) = &event {
            self.session.get_or_insert_with(|| id.clone().into());
        }
        self.references.apply(&event);
        self.subagents.apply(&event);
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
        // A task cancelled stays cancelled, whatever its harness says after.
        if self.status != TaskStatus::Cancelled {
            self.status = TaskStatus::Failed;
        }
    }

    /// Cancels it, keeping the output it had so far: tool calls still running
    /// were cancelled with it.
    fn cancel(&mut self) {
        self.status = TaskStatus::Cancelled;
        self.subagents.end();
        self.reply.cancel();
    }

    /// Whether it can be cancelled: it is under way, sent from here.
    fn can_cancel(&self) -> bool {
        self.status.is_active() && self.cancel.is_some()
    }

    /// The run is over. One that ended without a result, as when the harness
    /// was stopped, failed, and nothing still running in it finished.
    fn end(&mut self) {
        self.end_run();
        self.settle_outcome();
    }

    fn end_run(&mut self) {
        self.subagents.end();
        // Cancelled, what it printed before it stopped is kept, and any call
        // in it still running was cancelled with it.
        if self.status == TaskStatus::Cancelled {
            self.reply.cancel();
            return;
        }
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
/// tasks are one of these.
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
    /// Its items can be selected, each by a checkbox in its heading, as
    /// previous tasks can.
    selectable: bool,
    /// The items selected, by index, kept while the list is closed and opened
    /// again until they are cleared or sent.
    selected: BTreeSet<usize>,
    /// The filter chips on, which hide the tasks matching none of them; kept
    /// while the list is closed and opened again, not saved.
    filters: Vec<TaskFilter>,
    /// While any filter is on, how many items it shows, which the row reads.
    shown: Cell<Option<usize>>,
    /// The item whose checkbox was clicked last, the far end of a
    /// shift-click's range.
    last_selected: Option<usize>,
    /// Its items are grouped by the conversation they ran in, each group
    /// headed by a divider, as previous tasks are.
    by_session: bool,
}

/// How many items a history list was laid out with, which was open, and what
/// of the open one's prompt and table.
#[derive(Default)]
struct HistoryLaidOut {
    count: usize,
    open: Option<usize>,
    compiled: bool,
    /// Whether the open item's spec slices were shown.
    slices_open: bool,
    table: TableSync,
    markdown: u64,
    /// The items that began a conversation, each heading a group.
    groups: Vec<usize>,
    /// Each place's step in its chain, which sets it in and heads a chain.
    chains: Vec<Option<ChainStep>>,
    /// The item listed at each place.
    order: Vec<usize>,
    /// Which items the filters show, none while no filter is on.
    visible: Option<Vec<bool>>,
    /// The open item's changed files, how many and whether listed, which
    /// its end shows.
    changed: Option<(usize, bool)>,
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
        selectable: bool,
        by_session: bool,
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
            selectable,
            selected: BTreeSet::new(),
            filters: Vec::new(),
            shown: Cell::new(None),
            last_selected: None,
            by_session,
        }
    }

    /// The previous tasks, whose tables show whole, which can be selected to
    /// be sent to the other mode together, grouped by conversation.
    fn tasks() -> Self {
        Self::new(
            ("history-toggle", "task-list", "history-task"),
            ("previous task", "previous tasks", "Back to the latest task"),
            0,
            false,
            true,
            true,
        )
    }

    /// Where the items of `tasks` that began a conversation are listed, as
    /// `layout` lists them: the first, and each whose conversation isn't the
    /// one listed before it. One whose conversation isn't known, as when it
    /// had no record, stays in the group before it.
    fn session_groups(tasks: &[PromptTask], layout: &ChainLayout) -> Vec<usize> {
        let mut last = None;
        (0..tasks.len())
            .filter(|&place| match &tasks[layout.order[place]].session {
                Some(session) if last != Some(session) => {
                    last = Some(session);
                    true
                }
                _ => place == 0,
            })
            // A divider never falls between a chain's steps: it goes above
            // the chain.
            .map(|place| layout.steps[place].map_or(place, |step| step.start))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The checkbox of `item` was clicked: it is selected or deselected, or,
    /// with `range` (a shift-click), it and every item between it and the one
    /// clicked last are selected.
    fn click_checkbox(&mut self, item: usize, range: bool) {
        match self.last_selected.filter(|_| range) {
            Some(last) => self.selected.extend(last.min(item)..=last.max(item)),
            None => {
                if !self.selected.remove(&item) {
                    self.selected.insert(item);
                }
            }
        }
        self.last_selected = Some(item);
    }

    /// Deselects every item.
    fn clear_selection(&mut self) {
        self.selected.clear();
        self.last_selected = None;
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
    /// At its right, `actions`, such as the batch actions on the selected
    /// previous tasks, whose clicks are theirs and not the row's.
    fn render_row(
        &self,
        count: usize,
        enabled: bool,
        select: fn(&mut PromptMode) -> &mut HistoryList,
        actions: Vec<AnyElement>,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        self.render_row_as(
            self.toggle.into(),
            self.expanded,
            count,
            enabled,
            select,
            actions,
            cx,
        )
    }

    /// The row as it reads while `expanded`, or not, whichever it is, its
    /// toggle's element id `toggle`, with `actions` at its right.
    #[allow(clippy::too_many_arguments)]
    fn render_row_as(
        &self,
        toggle: ElementId,
        expanded: bool,
        count: usize,
        enabled: bool,
        select: fn(&mut PromptMode) -> &mut HistoryList,
        actions: Vec<AnyElement>,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        let theme = cx.theme();
        let label = if expanded {
            self.back.to_string()
        } else {
            match count {
                0 => format!("No {}", self.plural),
                1 => format!("1 {}", self.singular),
                n => format!("{n} {}", self.plural),
            }
        };
        let label = match self.shown.get() {
            Some(shown) => format!("{label} · {shown} shown"),
            None => label,
        };
        h_flex()
            .flex_none()
            .px_2()
            .py_0p5()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new(toggle)
                    .ghost()
                    .xsmall()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .label(label)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        select(this).toggle();
                        this.follow_task_history();
                        cx.notify();
                    })),
            )
            .when(!actions.is_empty(), |row| {
                row.child(
                    h_flex()
                        .ml_auto()
                        .flex_none()
                        .gap_1()
                        .items_center()
                        .children(actions),
                )
            })
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
        tasks_of_mut: fn(&mut PromptMode) -> &mut Vec<PromptTask>,
        select: fn(&mut PromptMode) -> &mut HistoryList,
        steps_shown: &HashSet<usize>,
        open_file: &OpenFile,
        visible: Option<Vec<bool>>,
        latest: Option<usize>,
        cx: &mut Context<PromptMode>,
    ) -> AnyElement {
        let count = tasks.len();
        let (id_base, collapse_steps) = (self.id_base, self.collapse_steps);
        let shows = |item: usize| {
            visible
                .as_ref()
                .is_none_or(|visible| visible.get(item) != Some(&false))
        };
        // A chain's steps are listed together beneath its parent row, where
        // its first step was sent; rows are laid out by where each item is
        // listed.
        let layout = Rc::new(if self.by_session {
            chain_layout(tasks)
        } else {
            ChainLayout::identity(count)
        });
        let shows_at = |place: usize| shows(layout.order[place]);
        // An open item filtered out is closed.
        let open = self.open.filter(|open| *open < count && shows(*open));
        let open_at = open.map(|open| layout.place[open]);
        if visible
            .as_ref()
            .is_some_and(|visible| !visible.contains(&true))
        {
            // Lets UI tests find it; inert in normal builds.
            return gpui_kit::TestSupportExt::test_support(div().id("no-filtered-tasks"))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("No previous tasks match these filters")
                .into_any_element();
        }
        // An open Freeform task is shown as a chat, with no table.
        let freeform_open =
            id_base == 0 && open.is_some_and(|open| tasks[open].mode == Some(SendMode::Freeform));
        let no_reply = Reply::default();
        let table_layout = open.map(|open| {
            let task_ix = id_base + open;
            let reply = if freeform_open {
                &no_reply
            } else {
                &tasks[open].reply
            };
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
            if laid_out.open != open_at {
                if let Some(was) = laid_out.open {
                    let table = laid_out.table.layout().map_or(0, |layout| layout.items());
                    self.rows.splice(was + 1..was + 1 + Self::block(table), 0);
                    self.rows.remeasure(was..was + 1);
                }
                if let Some(open) = open_at {
                    self.rows.splice(open + 1..open + 1, Self::block(0));
                    self.rows.remeasure(open..open + 1);
                }
                laid_out.open = open_at;
                laid_out.compiled = open.is_some_and(|open| tasks[open].compiled.is_some());
                laid_out.slices_open = open.is_some_and(|open| tasks[open].slices_open);
                laid_out.table = TableSync::default();
            }
            if let (Some(open), Some(at), Some((task_ix, reply, layout))) =
                (open, open_at, table_layout)
            {
                let compiled = tasks[open].compiled.is_some();
                let slices_open = tasks[open].slices_open;
                let prompt_changed = MarkdownStates::changed_since(&mut laid_out.markdown, cx)
                    .into_iter()
                    .any(|key| {
                        matches!(key.kind, MarkdownKind::Prompt | MarkdownKind::Slices)
                            && key.table == task_ix
                    });
                if compiled != laid_out.compiled
                    || slices_open != laid_out.slices_open
                    || prompt_changed
                {
                    self.rows.remeasure(at + 1..at + 2);
                    laid_out.compiled = compiled;
                    laid_out.slices_open = slices_open;
                }
                // "No output." stands in the header's place, for as long as
                // there is none.
                let empty = reply.row_count() == 0;
                // An open Freeform task's reply stands there, measured again
                // each time, as its steps show and hide.
                if freeform_open
                    || laid_out.table.layout().is_some_and(|was| was.items() == 0) != empty
                {
                    self.rows.remeasure(at + 2..at + 3);
                }
                laid_out
                    .table
                    .update(&self.rows, at + 3, task_ix, reply, layout, cx);
                let changed = tasks[open]
                    .changed
                    .as_ref()
                    .map(|changed| (changed.files.len(), tasks[open].changed_open));
                if laid_out.changed != changed {
                    let end = at + 3 + laid_out.table.layout().map_or(0, |layout| layout.items());
                    self.rows.remeasure(end..end + 1);
                    laid_out.changed = changed;
                }
            }
        }
        // An item that begins a conversation is headed by a divider, so its
        // heading is measured again whenever that changes, as when a running
        // task reports a new one.
        let groups = if self.by_session {
            Self::session_groups(tasks, &layout)
        } else {
            Vec::new()
        };
        // A session's divider heads the first of its tasks shown; a session
        // with none shown has none.
        let groups: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter_map(|(ix, &start)| {
                let end = groups.get(ix + 1).copied().unwrap_or(count);
                (start..end).find(|&place| shows_at(place))
            })
            .collect();
        {
            let mut laid_out = self.laid_out.borrow_mut();
            if laid_out.groups != groups {
                let table = laid_out.table.layout().map_or(0, |layout| layout.items());
                let row_of = |item: usize| match open_at {
                    Some(open) if item > open => item + Self::block(table),
                    _ => item,
                };
                let changed: BTreeSet<usize> = laid_out
                    .groups
                    .iter()
                    .chain(&groups)
                    .filter(|item| laid_out.groups.contains(item) != groups.contains(item))
                    .copied()
                    .collect();
                for item in changed.into_iter().filter(|&item| item < count) {
                    let row = row_of(item);
                    self.rows.remeasure(row..row + 1);
                }
                laid_out.groups = groups.clone();
            }
        }
        // A chain's steps are set in beneath its parent row, so when one
        // gains a step, or a task becomes a step, every row is measured again.
        let chains = Rc::new(layout.steps.clone());
        {
            let mut laid_out = self.laid_out.borrow_mut();
            if laid_out.chains != *chains
                || laid_out.order != layout.order
                || laid_out.visible != visible
            {
                let rows = self.rows.count();
                self.rows.remeasure(0..rows);
                laid_out.chains = chains.to_vec();
                laid_out.order = layout.order.clone();
                laid_out.visible = visible.clone();
            }
        }
        // Which places are hidden by the filters, laid out as nothing; and
        // which head their chain, its first step shown carrying its parent.
        let hidden: Rc<Vec<bool>> = Rc::new((0..count).map(|place| !shows_at(place)).collect());
        let heads: Rc<Vec<bool>> = Rc::new(
            (0..count)
                .map(|place| {
                    chains[place].is_some_and(|step| {
                        !hidden[place] && (step.start..place).all(|earlier| hidden[earlier])
                    })
                })
                .collect(),
        );
        if self.scroll_to_latest.take() {
            self.rows.scroll_to_end();
        }

        let theme = cx.theme();
        let (border, hover, muted) = (theme.border, theme.list_hover, theme.muted_foreground);
        let (table_background, radius) = (theme.tokens.table, theme.radius);
        let item_id = self.item;
        // Which items are selected, when they can be.
        let selected = self.selectable.then(|| self.selected.clone());
        let this = cx.entity().downgrade();
        let open_file = open_file.clone();
        // Its table tightens as the list narrows, counting the list's padding.
        let density = task_table::Density::of_list(&self.rows, px(32.));
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
                density,
                cx,
            );
            (layout.items(), rows)
        });
        let open_rows = open_at.zip(table.as_ref().map(|(items, _)| *items));
        let chain_color = chat_input::mode_color(SendMode::Both, cx);
        let render_chains = chains.clone();
        let heading_chains = chains.clone();
        let (heading_heads, render_heads) = (heads.clone(), heads.clone());
        let heading_hidden = hidden.clone();
        let render_layout = layout.clone();
        let render_row = move |ix: usize, window: &mut Window, cx: &mut App| -> AnyElement {
            let layout = &render_layout;
            let Some(entity) = this.upgrade() else {
                return div().into_any_element();
            };
            let row = Self::row_at(ix, open_rows);
            // Everything an open item shows is set in from the list's edges,
            // as wide as the list, however long its lines, so they wrap and it
            // is laid out as tall as it is measured.
            let room = density.inset(window.rem_size() * 0.75);
            let inset = || div().w_full().px(room);
            match row {
                HistoryRow::Heading(place) => {
                    let Some(&item) = layout.order.get(place) else {
                        return div().into_any_element();
                    };
                    let Some(task) = tasks_of(entity.read(cx)).get(item) else {
                        return div().into_any_element();
                    };
                    // Filtered out, it takes no room.
                    if heading_hidden.get(place) == Some(&true) {
                        return div().into_any_element();
                    }
                    let step = heading_chains.get(place).copied().flatten();
                    let is_open = open == Some(item);
                    let task_ix = id_base + item;
                    let checkbox = selected.as_ref().map(|selected| {
                        let this = this.clone();
                        history_checkbox(task_ix, selected.contains(&item), move |window, cx| {
                            // A shift-click selects the range from the one
                            // clicked last.
                            let range = window.modifiers().shift;
                            this.update(cx, |this, cx| {
                                select(this).click_checkbox(item, range);
                                cx.notify();
                            })
                            .ok();
                        })
                    });
                    let this_for_resend = this.clone();
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
                        .children(checkbox)
                        .map(|heading| match step {
                            // A step is labelled by what it did, in its
                            // mode's colour; its prompt is its chain's, shown
                            // on the parent row.
                            Some(step) => heading.child(chain_step_summary(
                                (item_id, item),
                                task_ix,
                                task,
                                step,
                                cx,
                            )),
                            None => heading.child(task_summary((item_id, item), task_ix, task, cx)),
                        })
                        .children(entity.read(cx).other_mode_state(task).map(|(to, state)| {
                            let this = this_for_resend.clone();
                            let menu = this_for_resend.clone();
                            let name = task.name.clone();
                            other_mode_button(
                                ("send-to-other-previous", task_ix),
                                to,
                                state,
                                move |_, window, cx| {
                                    // The button's click isn't the heading's.
                                    cx.stop_propagation();
                                    this.update(cx, |this, cx| {
                                        this.send_to_other_mode(tasks_of, item, window, cx)
                                    })
                                    .ok();
                                },
                                move |event, window, cx| {
                                    menu.update(cx, |this, cx| {
                                        this.open_mark_menu(
                                            name.clone(),
                                            event.position,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                                },
                                cx,
                            )
                        }))
                        // A task running in the other lane, beside the latest,
                        // can be cancelled from here.
                        .when(id_base == 0 && task.can_cancel(), |heading| {
                            let this = this_for_resend.clone();
                            heading.child(cancel_button(("cancel-previous", task_ix)).on_click(
                                move |_, _, cx| {
                                    // The button's click isn't the heading's.
                                    cx.stop_propagation();
                                    this.update(cx, |this, cx| this.cancel_task(item, cx)).ok();
                                },
                            ))
                        })
                        .child({
                            let this = this_for_resend.clone();
                            resend_button(("resend-previous", task_ix)).on_click(
                                move |_, window, cx| {
                                    // The button's click isn't the heading's.
                                    cx.stop_propagation();
                                    this.update(cx, |this, cx| {
                                        this.resend(tasks_of, item, window, cx)
                                    })
                                    .ok();
                                },
                            )
                        })
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
                    let group = groups.binary_search(&place).ok();
                    // Only touched by the colour of the mode it was sent in,
                    // far fainter than the latest task's header; hovered, the
                    // heading's own colour shows over it.
                    let hint = task.mode.map(|mode| Hsla {
                        a: HISTORY_MODE_HINT,
                        ..chat_input::mode_color(mode, cx)
                    });
                    let trigger = div()
                        .when_some(hint, |heading, hint| heading.bg(hint))
                        .child(trigger);
                    // An open item's line is below its end instead.
                    div()
                        .w_full()
                        .when(!is_open, |item| item.border_b_1().border_color(border))
                        .children(group.map(|group| {
                            h_flex()
                                .id(("history-session", task_ix))
                                .gap_2()
                                .px_3()
                                .pt(if group == 0 { px(6.) } else { px(14.) })
                                .pb_1()
                                .border_b_1()
                                .border_color(border)
                                .text_xs()
                                .text_color(muted)
                                .child(format!("Session {}", group + 1))
                        }))
                        .map(|heading| match step.filter(|_| heading_heads[place]) {
                            // A chain's first step heads it with its parent
                            // row, then sits set in beneath it as the others do.
                            Some(step) => {
                                let members = layout.members(step);
                                let all = tasks_of(entity.read(cx));
                                let steps: Vec<&PromptTask> =
                                    members.iter().filter_map(|&ix| all.get(ix)).collect();
                                let task_ix = id_base + members[0];
                                heading
                                    .child(chain_parent(
                                        task_ix,
                                        &steps,
                                        latest.is_some_and(|latest| members.contains(&latest)),
                                        cx,
                                    ))
                                    .child(
                                        div().w_full().pl(CHAIN_INDENT).child(
                                            div()
                                                .w_full()
                                                .border_l_2()
                                                .border_color(chain_color.opacity(0.6))
                                                .child(trigger),
                                        ),
                                    )
                            }
                            None => heading.child(trigger),
                        })
                        .into_any_element()
                }
                HistoryRow::Prompt(place) => {
                    let item = layout.order[place];
                    let task_ix = id_base + item;
                    let (shown, slices) = tasks_of(entity.read(cx))
                        .get(item)
                        .and_then(|task| task.compiled.as_ref())
                        .map(|compiled| {
                            let task = &tasks_of(entity.read(cx))[item];
                            let slices = compiled
                                .slices()
                                .filter(|_| task.slices_open)
                                .map(|slices| slices.shown.clone());
                            (Some(compiled.shown()), slices)
                        })
                        .unwrap_or_default();
                    // Its markdown's state is kept, so it's parsed once.
                    if let Some(shown) = shown {
                        MarkdownStates::prepare(prompt_key(task_ix), &shown, cx);
                    }
                    if let Some(slices) = slices {
                        MarkdownStates::prepare(slices_key(task_ix), &slices, cx);
                    }
                    let toggle: Rc<dyn Fn(&mut Window, &mut App)> = {
                        let this = this.clone();
                        Rc::new(move |_, cx| {
                            this.update(cx, |this, cx| {
                                if let Some(task) = tasks_of_mut(this).get_mut(item) {
                                    task.slices_open = !task.slices_open;
                                    cx.notify();
                                }
                            })
                            .ok();
                        })
                    };
                    let Some(task) = tasks_of(entity.read(cx)).get(item) else {
                        return div().into_any_element();
                    };
                    // A Freeform task's prompt is a message, as in its chat.
                    let prompt = if freeform_open {
                        entity
                            .update(cx, |this, cx| this.freeform_prompt(item, cx))
                            .unwrap_or_else(|| div().into_any_element())
                    } else {
                        task_prompt(task_ix, task, &open_file, Some(toggle), cx)
                    };
                    // Lets UI tests find the panel; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(
                        inset()
                            .id(("history-panel", task_ix))
                            .pt_1()
                            .pb_3()
                            .child(prompt),
                    )
                    .into_any_element()
                }
                HistoryRow::TableHeader(place) => {
                    let item = layout.order[place];
                    // A Freeform task's reply, as in its chat.
                    if freeform_open {
                        return entity
                            .update(cx, |this, cx| this.freeform_reply(item, cx))
                            .unwrap_or_else(|| div().into_any_element());
                    }
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
                                .child(task_table::output_header(density, cx)),
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
                HistoryRow::End(place) => {
                    let item = layout.order[place];
                    let has_table = open_rows.is_some_and(|(_, rows)| rows > 0);
                    // A previous task's changed files, beneath its table;
                    // a question has none.
                    let changed = (id_base == 0)
                        .then(|| {
                            entity.update(cx, |this, cx| this.changed_files_of(item, item, cx))
                        })
                        .flatten();
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
                            })
                            .children(changed.map(|changed| div().pt_2().child(changed))),
                    )
                    .into_any_element()
                }
            }
        };
        // Every row of a chain's step is set in beneath its parent row, a
        // line in the Chain colour down its left joining it to the chain.
        let render: RenderRow = Rc::new(move |ix, window, cx| {
            let element = render_row(ix, window, cx);
            let item = match Self::row_at(ix, open_rows) {
                HistoryRow::Heading(item)
                | HistoryRow::Prompt(item)
                | HistoryRow::TableHeader(item)
                | HistoryRow::TableRow(item, _)
                | HistoryRow::End(item) => item,
            };
            match render_chains.get(item).copied().flatten() {
                // The first step's row starts with the parent, not set in.
                Some(_)
                    if render_heads[item]
                        && matches!(Self::row_at(ix, open_rows), HistoryRow::Heading(_)) =>
                {
                    element
                }
                Some(_) => div()
                    .w_full()
                    .pl(CHAIN_INDENT)
                    .child(
                        div()
                            .w_full()
                            .border_l_2()
                            .border_color(chain_color.opacity(0.6))
                            .child(element),
                    )
                    .into_any_element(),
                None => element,
            }
        });
        let items = self.rows.element(render);
        let list = div()
            .id(SharedString::from(format!("{}-scroll", self.list)))
            .size_full()
            .p_4()
            .child(items)
            .child(task_table::follow_density(&self.rows, density, px(32.)));
        // Lets UI tests find the list; inert in normal builds.
        let list = gpui_kit::TestSupportExt::test_support(list);
        scrollbar::with_scrollbar(self.list, &self.rows, list, true, None, cx)
    }
}

/// The task view beside a sidebar sliding out or back, pushed rather than
/// covered: the view takes whatever width the pane leaves it, so as the pane
/// grows or shrinks each frame, the tab bar, the tab's contents, and the chat
/// input narrow or widen with it, their right edge against its left.
fn pushed_by(history: Div, pane: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .size_full()
        .overflow_hidden()
        .child(div().flex_1().min_w_0().h_full().child(history))
        .child(
            div()
                .flex_none()
                .h_full()
                .bg(cx.theme().background)
                .child(pane),
        )
}

/// A sidebar sliding out from, or back behind, the task view's right edge,
/// its width following `spring`: `contents` at their full `width`, pinned to
/// the pane's left, so they come into view from behind the edge as the pane
/// grows.
fn slide_pane(
    id: (&'static str, usize),
    spring_id: (&'static str, usize),
    spring: SpringAnimation<Pixels>,
    width: Pixels,
    contents: AnyElement,
) -> AnyElement {
    // Lets UI tests find the pane as it slides; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(div().id(id))
        .relative()
        .flex_none()
        .h_full()
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(width)
                .child(contents),
        )
        .with_spring(spring_id, spring, |this, width| this.w(width.max(px(0.))))
        .into_any_element()
}

/// Where a task's compiled prompt's markdown is kept.
/// Where a previous task's spec slices are shown, whole.
fn slices_key(task_ix: usize) -> MarkdownKey {
    MarkdownKey {
        kind: MarkdownKind::Slices,
        table: task_ix,
        row: usize::MAX,
    }
}

fn prompt_key(task_ix: usize) -> MarkdownKey {
    MarkdownKey {
        kind: MarkdownKind::Prompt,
        table: task_ix,
        row: 0,
    }
}

/// The latest task's compiled prompt in its header: a virtualized list of its
/// markdown blocks, as tall as they are up to [`MAX_PROMPT_HEIGHT`], so a long
/// prompt, spec slices and all, lays out only what is in view.
struct HeaderPrompt {
    rows: MeasuredList,
    /// The task, blocks, and whether its slices were open, the list was last
    /// made for.
    shown: RefCell<Option<(usize, Arc<[SharedString]>, bool)>>,
    /// Which parsed markdown the list has seen, to measure a block again once
    /// it has parsed.
    markdown: Cell<u64>,
}

impl Default for HeaderPrompt {
    fn default() -> Self {
        Self {
            rows: MeasuredList::new(task_table::OVERDRAW),
            shown: RefCell::default(),
            markdown: Cell::new(0),
        }
    }
}

impl HeaderPrompt {
    fn key(kind: MarkdownKind, task_ix: usize, block: usize) -> MarkdownKey {
        MarkdownKey {
            kind,
            table: task_ix,
            row: block,
        }
    }

    /// The prompt's blocks, then, for a sliced prompt, the row that shows or
    /// hides its slices, and its slices' blocks while they are shown.
    fn element(
        &self,
        task_ix: usize,
        compiled: &Compiled,
        slices_open: bool,
        toggle: Rc<dyn Fn(&mut Window, &mut App)>,
        open: OpenFile,
        cx: &App,
    ) -> AnyElement {
        let blocks = compiled.blocks();
        let slices = compiled
            .slices()
            .map(|slices| (slices.count, slices.blocks.clone()));
        let slice_rows = match &slices {
            Some((_, blocks)) if slices_open => 1 + blocks.len(),
            Some(_) => 1,
            None => 0,
        };
        let same = self
            .shown
            .borrow()
            .as_ref()
            .is_some_and(|(ix, shown, was)| {
                *ix == task_ix && Arc::ptr_eq(shown, &blocks) && *was == slices_open
            });
        if !same {
            self.rows.reset(blocks.len() + slice_rows);
            *self.shown.borrow_mut() = Some((task_ix, blocks.clone(), slices_open));
            self.markdown.set(MarkdownStates::latest(cx));
        } else {
            let mut seen = self.markdown.get();
            for key in MarkdownStates::changed_since(&mut seen, cx) {
                if key.table != task_ix {
                    continue;
                }
                let row = match key.kind {
                    MarkdownKind::PromptBlock => key.row,
                    MarkdownKind::Slices => blocks.len() + 1 + key.row,
                    _ => continue,
                };
                self.rows.remeasure(row..row + 1);
            }
            self.markdown.set(seen);
        }
        let render: RenderRow = Rc::new(move |row, _, cx| {
            let (kind, block, text) = if let Some(text) = blocks.get(row) {
                (MarkdownKind::PromptBlock, row, text.clone())
            } else {
                let Some((count, slice_blocks)) = &slices else {
                    return div().into_any_element();
                };
                if row == blocks.len() {
                    let toggle = toggle.clone();
                    return div()
                        .pt_2()
                        .child(task_table::slices_row(
                            ("prompt-slices", task_ix),
                            slices_open,
                            *count,
                            move |window, cx| toggle(window, cx),
                            cx,
                        ))
                        .into_any_element();
                }
                let block = row - blocks.len() - 1;
                let Some(text) = slice_blocks.get(block) else {
                    return div().into_any_element();
                };
                (MarkdownKind::Slices, block, text.clone())
            };
            let key = Self::key(kind, task_ix, block);
            // Its markdown's state is kept, so it's parsed once.
            MarkdownStates::prepare(key, &text, cx);
            div()
                .min_w_0()
                .when(row > 0, |row| row.pt_2())
                .child(shown_markdown_view(key, text, Some(&open), cx))
                .into_any_element()
        });
        let height = self.rows.total_height().min(MAX_PROMPT_HEIGHT);
        let prompt = div()
            .id(("compiled-prompt", task_ix))
            .min_w_0()
            .h(height)
            .child(self.rows.element(render));
        // Lets UI tests find the compiled prompt; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(prompt).into_any_element()
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
    /// Set when it is stopped, so its record says it was cancelled.
    stopped: Arc<AtomicBool>,
    /// Its run, stopped when the question is stopped.
    _run: Task<()>,
}

/// A question's run as far as it went, saved beside the question in
/// `.suspense/asks` when dropped: once the run is over, or when the question
/// is stopped.
struct AskLog {
    file: Option<PathBuf>,
    record: RunRecord,
    stopped: Arc<AtomicBool>,
}

impl Drop for AskLog {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let mut record = std::mem::take(&mut self.record);
        record.cancelled |= self.stopped.load(Ordering::SeqCst);
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
    /// How many tokens its context holds, as of its latest reply, once known.
    context: Option<u64>,
    /// The system prompt its latest run was sent, which a Freeform prompt
    /// carrying it on keeps, so the harness's cache holds; none when it was
    /// sent none, or its run wasn't recorded with one.
    system_prompt: Option<String>,
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

    /// The latest conversation in a project's history, with its context as
    /// that run left it; none if that is the conversation `left` for a new
    /// one, so the next run starts fresh.
    fn latest(history: &[SavedPrompt], project_dir: &Path, left: Option<&str>) -> Option<Self> {
        let latest = history.iter().rev().find_map(|saved| {
            let (mut id, mut context) = (None, None);
            for event in saved
                .record
                .as_ref()?
                .output
                .iter()
                .flat_map(harness::parse)
            {
                match event {
                    HarnessEvent::Session(session) => id = Some(session),
                    HarnessEvent::Usage { context: tokens } => context = Some(tokens),
                    _ => {}
                }
            }
            Some(Self {
                project_dir: project_dir.to_path_buf(),
                id: id?,
                context,
                system_prompt: saved.record.as_ref()?.system_prompt.clone(),
            })
        })?;
        (Some(latest.id.as_str()) != left).then_some(latest)
    }

    /// A task's run in the conversation `id` is over, holding `context` as of
    /// its latest reply: the tasks carry that one on from now, whichever
    /// other task's was carried on before, as a task of the other lane may
    /// have run beside it in a copy.
    fn finished(
        session: &mut Option<Self>,
        id: String,
        context: Option<u64>,
        project_dir: &Path,
        sent: Option<&str>,
    ) {
        if session.as_ref().is_some_and(|session| session.id == id) {
            return;
        }
        *session = Some(Self {
            project_dir: project_dir.to_path_buf(),
            id,
            context,
            system_prompt: sent.map(str::to_string),
        });
    }

    /// Follows a run's events: the conversation it reported, and how much
    /// context that holds as it replies. A run started before the
    /// conversation was left for a new one, which `current` is no longer,
    /// changes nothing; the conversation it reports is returned to be kept
    /// as left, so the history doesn't carry it on either, unless a run since
    /// has begun one of its own. That covers a run whose conversation wasn't
    /// known yet when it was left.
    #[must_use]
    fn follow(
        session: &mut Option<Self>,
        current: bool,
        run: &mut Option<String>,
        event: &HarnessEvent,
        project_dir: &Path,
        sent: Option<&str>,
    ) -> Option<String> {
        match event {
            HarnessEvent::Session(id) => {
                *run = Some(id.clone());
                if !current {
                    return session.is_none().then(|| id.clone());
                }
                // Carrying on the same conversation keeps what it holds until
                // the run says otherwise.
                let context = session
                    .as_ref()
                    .filter(|session| session.id == *id)
                    .and_then(|session| session.context);
                *session = Some(Self {
                    project_dir: project_dir.to_path_buf(),
                    id: id.clone(),
                    context,
                    system_prompt: sent.map(str::to_string),
                });
            }
            HarnessEvent::Usage { context } if current => {
                if let Some(session) = session
                    .as_mut()
                    .filter(|session| Some(&session.id) == run.as_ref())
                {
                    session.context = Some(*context);
                }
            }
            _ => {}
        }
        None
    }
}

/// A file pane sliding closed.
/// The referenced spec sidebar, just closed, sliding back behind the task
/// view's right edge.
struct RefsClosing {
    files: Vec<referenced_spec::Referenced>,
    subagents: Vec<referenced_spec::SubagentGroup>,
    understanding: Understanding,
    /// The width it slides closed from.
    width: Pixels,
    /// Which opening of the sidebar this closes, so each slide animates afresh.
    slide: usize,
    closed: Instant,
}

/// What was chosen for a file edited on disk while it had unsaved changes.
#[derive(Clone, Copy)]
enum ConflictChoice {
    KeepMine,
    Reload,
    Merge,
}

/// A file open in a tab of the body.
struct FileTab {
    view: Entity<FileView>,
    _subscriptions: Vec<Subscription>,
}

/// The latest task as the previous tasks were opened over it: which it was,
/// among how many, and whether its output was locked to its bottom.
struct OutputLeft {
    task: Option<SharedString>,
    tasks: usize,
    locked: bool,
}

/// A filter chip of the previous tasks: the mode a task was sent in, or where
/// it stands with the other mode.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TaskFilter {
    Mode(SendMode),
    CanSendSpec,
    CanSendCode,
    Sent,
    MarkedDone,
}

impl TaskFilter {
    /// Every chip, in the bar's order: the modes, then the other mode.
    const ALL: [TaskFilter; 8] = [
        TaskFilter::Mode(SendMode::Code),
        TaskFilter::Mode(SendMode::Both),
        TaskFilter::Mode(SendMode::Spec),
        TaskFilter::Mode(SendMode::Freeform),
        TaskFilter::CanSendSpec,
        TaskFilter::CanSendCode,
        TaskFilter::Sent,
        TaskFilter::MarkedDone,
    ];

    fn label(self) -> &'static str {
        match self {
            TaskFilter::Mode(SendMode::Code) => "Code",
            TaskFilter::Mode(SendMode::Both) => "Chain",
            TaskFilter::Mode(SendMode::Spec) => "Spec",
            TaskFilter::Mode(_) => "Freeform",
            TaskFilter::CanSendSpec => "Can send to Spec",
            TaskFilter::CanSendCode => "Can send to Code",
            TaskFilter::Sent => "Sent",
            TaskFilter::MarkedDone => "Marked done",
        }
    }

    fn is_mode(self) -> bool {
        matches!(self, TaskFilter::Mode(_))
    }
}

/// Where a task stands in the chain it is a step of: where the chain's first
/// step is listed, this step's place in it, and how many steps it has so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChainStep {
    start: usize,
    pos: usize,
    len: usize,
}

/// How the previous tasks are listed: oldest first, but with each chain's
/// steps together where its first step was sent, as the PromptEditorScope
/// says, whatever tasks of the other lane ran between them.
#[derive(Clone, Debug, Default, PartialEq)]
struct ChainLayout {
    /// The task listed at each place.
    order: Vec<usize>,
    /// Where each task is listed.
    place: Vec<usize>,
    /// The step of a chain listed at each place, if it is one.
    steps: Vec<Option<ChainStep>>,
}

impl ChainLayout {
    /// `count` items listed as they are, none of them chained.
    fn identity(count: usize) -> Self {
        Self {
            order: (0..count).collect(),
            place: (0..count).collect(),
            steps: vec![None; count],
        }
    }

    /// The tasks of the chain listed from `step.start`, in the order they
    /// ran.
    fn members(&self, step: ChainStep) -> &[usize] {
        &self.order[step.start..(step.start + step.len).min(self.order.len())]
    }

    /// The step of a chain that the task at `ix` is, if it is one.
    fn step_of(&self, ix: usize) -> Option<ChainStep> {
        self.steps[*self.place.get(ix)?]
    }
}

/// How `tasks` are listed, each chain's steps together. A chain is a Chain
/// task, then the Code step it sent once it was done, sent from it, then,
/// with a post-build spec update, the Spec follow-up that code step sent,
/// sent from it. Its steps are found by which step each was sent from, not by
/// being next to each other, since tasks of the other lane may run between
/// them: each is the first task after the step before it sent from it in its
/// mode. A task sent or resent from a step by hand, later, is a task of its
/// own.
fn chain_layout(tasks: &[PromptTask]) -> ChainLayout {
    let count = tasks.len();
    // The tasks told what another did, by the name of the one they were sent
    // from, oldest first.
    let mut sent_from: HashMap<&str, Vec<usize>> = HashMap::new();
    for (ix, task) in tasks.iter().enumerate() {
        if let Some(from) = task.sent.sent_from.as_deref()
            && task.sent.code_task.is_some()
        {
            sent_from.entry(from).or_default().push(ix);
        }
    }
    let mut claimed = vec![false; count];
    let next = |claimed: &[bool], from: usize, mode: SendMode| {
        sent_from
            .get(tasks[from].name.as_ref())?
            .iter()
            .copied()
            .find(|&ix| ix > from && !claimed[ix] && tasks[ix].sent.mode == Some(mode))
    };
    let mut chains: Vec<Option<Vec<usize>>> = vec![None; count];
    for ix in 0..count {
        if tasks[ix].sent.mode != Some(SendMode::Both) {
            continue;
        }
        let mut chain = vec![ix];
        if let Some(code) = next(&claimed, ix, SendMode::Code) {
            claimed[code] = true;
            chain.push(code);
            if tasks[code].sent.post_build_update
                && let Some(update) = next(&claimed, code, SendMode::Spec)
            {
                claimed[update] = true;
                chain.push(update);
            }
        }
        chains[ix] = Some(chain);
    }
    let mut layout = ChainLayout {
        order: Vec::with_capacity(count),
        place: vec![0; count],
        steps: Vec::with_capacity(count),
    };
    for ix in (0..count).filter(|&ix| !claimed[ix]) {
        match &chains[ix] {
            Some(chain) => {
                let start = layout.order.len();
                for (pos, &step) in chain.iter().enumerate() {
                    layout.order.push(step);
                    layout.steps.push(Some(ChainStep {
                        start,
                        pos,
                        len: chain.len(),
                    }));
                }
            }
            None => {
                layout.order.push(ix);
                layout.steps.push(None);
            }
        }
    }
    for (place, &ix) in layout.order.iter().enumerate() {
        layout.place[ix] = place;
    }
    layout
}

/// A chain's parent row in the previous tasks, heading its `steps`: tinted
/// the Chain colour, a joined chain icon, the prompt's first line, the
/// chain's status as a whole, and how many steps it has; `latest` when a step
/// of it is the latest task. It has nothing to click.
fn chain_parent(task_ix: usize, steps: &[&PromptTask], latest: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = chat_input::mode_color(SendMode::Both, cx);
    let text = steps
        .first()
        .map(|task| first_line(&task.text))
        .unwrap_or_default();
    let count = steps.len();
    // Lets UI tests find the parent; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(h_flex().id(("history-chain", task_ix)))
        .w_full()
        .gap_3()
        .py_2()
        .px_3()
        .font_medium()
        .bg(Hsla {
            a: CHAIN_HINT,
            ..color
        })
        .child(
            Icon::new(IconName::Link)
                .small()
                .flex_none()
                .text_color(color),
        )
        .child(div().flex_none().child(chain_status(steps).tag(cx)))
        .child(div().flex_1().min_w_0().truncate().child(text))
        .when(latest, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Latest"),
            )
        })
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if count == 1 {
                    "1 step".to_string()
                } else {
                    format!("{count} steps")
                }),
        )
        .into_any_element()
}

/// A chain step's summary in its heading: what it did, "Spec", "Code", or
/// "Spec follow-up", in that step's mode colour, then its status, with no
/// prompt, which its parent row shows.
/// What a chain's step at `pos` did, and the mode it did it in: the spec
/// step and follow-up Spec's, the code step Code's.
fn chain_step_label(pos: usize) -> (&'static str, SendMode) {
    match pos {
        0 => ("Spec", SendMode::Spec),
        1 => ("Code", SendMode::Code),
        _ => ("Spec follow-up", SendMode::Spec),
    }
}

fn chain_step_summary(
    id: (&'static str, usize),
    ix: usize,
    task: &PromptTask,
    step: ChainStep,
    cx: &App,
) -> AnyElement {
    let (label, mode) = chain_step_label(step.pos);
    let summary = h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .gap_3()
        .child(
            div()
                .flex_none()
                .text_sm()
                .font_semibold()
                .text_color(chat_input::mode_color(mode, cx))
                .child(label),
        )
        .child(div().min_w_0().child(task_title(ix, task, false, cx)));
    // Lets UI tests find the task; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(summary).into_any_element()
}

/// A chain's status as a whole: running while any step is, failed or
/// cancelled if any step was, and otherwise its last step's.
fn chain_status(steps: &[&PromptTask]) -> TaskStatus {
    let statuses = steps.iter().map(|task| task.status);
    if let Some(active) = statuses.clone().find(|status| status.is_active()) {
        return active;
    }
    for ended in [TaskStatus::Failed, TaskStatus::Cancelled] {
        if statuses.clone().any(|status| status == ended) {
            return ended;
        }
    }
    steps
        .last()
        .map_or(TaskStatus::Unrecorded, |task| task.status)
}

/// How strongly a chain's parent row is tinted with the Chain colour, a
/// little more than a single task's heading.
const CHAIN_HINT: f32 = 0.12;

/// How far a chain's steps are set in beneath its parent row.
const CHAIN_INDENT: Pixels = px(14.);

/// How strongly a previous task's heading is tinted with its mode's colour:
/// only a hint of it.
const HISTORY_MODE_HINT: f32 = 0.07;

/// Scrolling past an end of the latest task, `up` into the previous tasks,
/// or of the previous tasks back down to it, sliding one in for the other.
struct ScrollSlide {
    /// Counts the slides, so each springs from its start.
    n: usize,
    up: bool,
    started: Instant,
    /// How far toward the barrier the glow had grown as it crossed over,
    /// fading out as the slide begins; none for a slide it didn't start.
    glow: f32,
    /// How far the stretch had pulled the view as it crossed over, down
    /// for up, the slide starting from there.
    pull: Pixels,
}

/// A scroll pushing into the stretch at an end: which way, and how far past
/// it, as of `at`, draining away once the scrolling pauses.
#[derive(Clone, Copy)]
struct ScrollPush {
    up: bool,
    amount: f32,
    at: Instant,
}

impl Default for ScrollPush {
    fn default() -> Self {
        Self {
            up: false,
            amount: 0.,
            at: Instant::now(),
        }
    }
}

impl ScrollPush {
    /// How far it has built up at `now`: all of it while the scroll goes
    /// on, draining only once it has stopped, [`SCROLL_HOLD`] after its last
    /// step, to nothing by [`SCROLL_DRAIN`] after it.
    fn held(&self, now: Instant) -> f32 {
        let since = now.duration_since(self.at);
        if since <= SCROLL_HOLD {
            return self.amount;
        }
        let drained =
            (since - SCROLL_HOLD).as_secs_f32() / (SCROLL_DRAIN - SCROLL_HOLD).as_secs_f32();
        self.amount * (1. - drained).max(0.)
    }

    /// How far the stretch pulls the view past its end at `now`, down while
    /// scrolling up: nearly as far as it built up at first, less the further
    /// it goes, as a rubber band gives, and about [`STRETCH_PULL`] by the
    /// barrier.
    fn pull(&self, now: Instant) -> Pixels {
        let held = self.held(now);
        if held <= 0. {
            return px(0.);
        }
        let pull = (STRETCH_GIVE * held / (STRETCH_GIVE + held)).min(STRETCH_PULL);
        px(if self.up { pull } else { -pull })
    }
}

/// Whether each wheel event over the latest task or the previous tasks is
/// written to standard error with what came of it, for seeing how a
/// platform's scrolling reaches the stretch: set `SUSPENSE_SCROLL_LOG`.
static SCROLL_LOG: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("SUSPENSE_SCROLL_LOG").is_some());

/// How far the stretch pulls the view by the barrier, and never more.
const STRETCH_PULL: f32 = 64.;

/// How quickly the stretch stiffens, as a rubber band does: it gives all of
/// a scroll at first, less and less the further it goes, and still gives a
/// little right up to the barrier, reaching [`STRETCH_PULL`] just as the
/// build-up reaches [`SCROLL_BARRIER`].
const STRETCH_GIVE: f32 = STRETCH_PULL * SCROLL_BARRIER / (SCROLL_BARRIER - STRETCH_PULL);

/// How far a scroll must push past an end to cross over.
const SCROLL_BARRIER: f32 = 768.;

/// How long a scroll's steps may be apart and it still goes on: until then,
/// nothing it built up against an end drains.
const SCROLL_HOLD: Duration = Duration::from_millis(200);

/// How long after a scroll's last step what it built up against an end has
/// drained away.
const SCROLL_DRAIN: Duration = Duration::from_millis(500);

/// How quickly the glow fades as the view crosses over.
const GLOW_FADE: Duration = Duration::from_millis(150);

/// How long the wheel must rest before a scroll counts as a new one, after a
/// slide or a fling, which then pulls into the stretch at an end again:
/// barely a pause. A trackpad touch starting is a new scroll at once.
const SCROLL_REST: Duration = Duration::from_millis(80);

/// The gap an item dragged over the item at `ix`, whose bounds are
/// `bounds`, would be dropped into, the pointer at `at`: the gap before it
/// over its first half, the one after it over its second, halves taken
/// `along` the way the items run. None while the pointer is off it.
fn drag_gap(ix: usize, bounds: Bounds<Pixels>, at: Point<Pixels>, vertical: bool) -> Option<usize> {
    if !bounds.contains(&at) {
        return None;
    }
    let after = if vertical {
        at.y > bounds.center().y
    } else {
        at.x > bounds.center().x
    };
    Some(ix + after as usize)
}

/// The item a drag from `from` lands at, dropped in `gap`, and whether that
/// moves it at all: the gaps either side of it are where it already is.
fn gap_target(from: usize, gap: usize) -> Option<usize> {
    (gap != from && gap != from + 1).then(|| if gap > from { gap - 1 } else { gap })
}

/// The insertion indicator of a drag reorder: a 2 pixel accent line across
/// the gap, drawn over the items, with a small open circle at its leading
/// end, taking no mouse. `vertical` for a line between tabs side by side;
/// `after` for one along the item's far edge rather than its near one.
fn drop_indicator(vertical: bool, after: bool, cx: &App) -> AnyElement {
    let accent = cx.theme().accent;
    let (line, dot) = (px(2.), px(6.));
    let circle = div()
        .absolute()
        .size(dot)
        .rounded_full()
        .border_2()
        .border_color(accent)
        .bg(cx.theme().background);
    let bar = div().absolute().bg(accent).rounded_full();
    let edge = -line / 2.;
    if vertical {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(line)
            .map(|this| {
                if after {
                    this.right(edge)
                } else {
                    this.left(edge)
                }
            })
            .child(bar.top(dot).bottom_0().left_0().right_0())
            .child(circle.top_0().left(-(dot - line) / 2.))
            .into_any_element()
    } else {
        div()
            .absolute()
            .left_0()
            .right_0()
            .h(line)
            .map(|this| {
                if after {
                    this.bottom(edge)
                } else {
                    this.top(edge)
                }
            })
            .child(bar.left(dot).right_0().top_0().bottom_0())
            .child(circle.left_0().top(-(dot - line) / 2.))
            .into_any_element()
    }
}

/// A file tab's close button: a square this big, drawn this far into the
/// tab's right padding, leaving 6 of the tab's 12.
const CLOSE_TAB_SIZE: Pixels = px(16.);
const CLOSE_TAB_PULL: Pixels = px(6.);

/// A file's tab being dragged along the body's tab bar to a new place, shown
/// under the pointer as the file's name.
#[derive(Clone)]
struct FileTabDrag {
    view: EntityId,
    name: SharedString,
}

impl Render for FileTabDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.drag_border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_sm()
            .shadow_md()
            .child(self.name.clone())
    }
}

/// A queued prompt being dragged to a new place in the queue, shown under
/// the pointer as a line of its text.
#[derive(Clone)]
struct QueuedDrag {
    id: usize,
    text: SharedString,
}

impl Render for QueuedDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.drag_border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_sm()
            .shadow_md()
            .line_clamp(1)
            .child(self.text.clone())
    }
}

/// The work that belongs to one open project (see the OpenProjectsScope):
/// swapped into [`PromptMode`] while the project is on screen, or while a run
/// of it writes back from the background, and kept aside otherwise.
struct ProjectSession {
    project_dir: Option<PathBuf>,
    tasks: Vec<PromptTask>,
    working: Lanes,
    history_stale: bool,
    _history_load: Task<()>,
    task_history: HistoryList,
    queue: Vec<QueueItem>,
    queue_expanded: bool,
    auto_send: bool,
    queue_held: bool,
    _pending: [Task<()>; 2],
    feeding: Arc<futures::lock::Mutex<()>>,
    session: Option<Session>,
    session_epoch: u64,
    new_conversation_pending: bool,
    asks: Vec<Ask>,
    steps_shown: HashSet<usize>,
    /// The chat segments whose steps are shown, by their prompt's element id
    /// and their place in its reply, in the Ask conversation and the
    /// Freeform chat.
    chat_steps_shown: HashSet<(usize, usize)>,
    answers: Vec<PromptTask>,
    _ask_history_load: Task<()>,
    ask_pane: AskPane,
    /// The Freeform chat the task view shows while the latest task was sent
    /// in Freeform, as the MessageList's freeform says.
    freeform_pane: AskPane,
    ask_new_pending: bool,
    ask_session: Option<Session>,
    ask_session_epoch: u64,
    usage: ProjectUsage,
    // How the project was left on screen, kept for when it's back.
    output_table: TaskTable,
    output_locked: bool,
    /// The latest task, and whether its output was locked, as the previous
    /// tasks were opened over it, for it to come back as it was left.
    output_left: Option<OutputLeft>,
    header_prompt: HeaderPrompt,
    queue_scroll: ScrollHandle,
    ask_rows: MeasuredList,
    ask_row_ids: RefCell<Vec<usize>>,
    files: Vec<FileTab>,
    selected_file: Option<usize>,
    latest_settled: Option<(usize, SharedString)>,
    mode_tab: SendMode,
}

/// The rows of the questions still running, stacked above the chat input's
/// tabs off the Ask tab, kept on the newest as rows slide up into it, until
/// scrolled away from it.
fn ask_rows() -> MeasuredList {
    let rows = MeasuredList::new(task_table::OVERDRAW);
    rows.state().set_follow_mode(FollowMode::Tail);
    rows
}

impl ProjectSession {
    fn new(project_dir: Option<PathBuf>, _cx: &mut App) -> Self {
        Self {
            project_dir,
            tasks: Vec::new(),
            working: Lanes::NONE,
            history_stale: false,
            _history_load: Task::ready(()),
            task_history: HistoryList::tasks(),
            queue: Vec::new(),
            queue_expanded: false,
            auto_send: true,
            queue_held: false,
            _pending: [Task::ready(()), Task::ready(())],
            feeding: Arc::default(),
            session: None,
            session_epoch: 0,
            new_conversation_pending: false,
            asks: Vec::new(),
            steps_shown: HashSet::new(),
            chat_steps_shown: HashSet::new(),
            answers: Vec::new(),
            _ask_history_load: Task::ready(()),
            ask_pane: AskPane::new(),
            freeform_pane: AskPane::new(),
            ask_new_pending: false,
            ask_session: None,
            ask_session_epoch: 0,
            usage: ProjectUsage::default(),
            output_table: TaskTable::new(),
            output_locked: false,
            output_left: None,
            header_prompt: HeaderPrompt::default(),
            queue_scroll: ScrollHandle::new(),
            ask_rows: ask_rows(),
            ask_row_ids: RefCell::default(),
            files: Vec::new(),
            selected_file: None,
            latest_settled: None,
            // A project opened for the first time starts on Chain.
            mode_tab: SendMode::Both,
        }
    }
}

/// What is running in an open project, for showing it busy.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectActivity {
    pub project_dir: PathBuf,
    /// Each task the harness is working on, one in each lane at most, by
    /// its index, with its first line.
    pub tasks: Vec<(usize, SharedString)>,
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
    /// When the wheel last scrolled the latest task or the previous tasks,
    /// telling a new scroll from one carried on.
    last_wheel: Rc<Cell<Option<Instant>>>,
    /// Whether the scroll going on is carried on from a slide, or is a
    /// fling's momentum, stopping at an end rather than pulling into the
    /// stretch, until a new scroll.
    scroll_held_back: Rc<Cell<bool>>,
    /// Whether the scroll going on has crossed over, and is spent: all of it
    /// that follows is swallowed, scrolling neither view, until a new scroll.
    scroll_spent: Rc<Cell<bool>>,
    /// While a queued prompt is dragged: which, and the gap between prompts
    /// it would be dropped into, if any.
    queue_gap: Rc<Cell<Option<(usize, Option<usize>)>>>,
    /// While a file's tab is dragged: which, and the gap between file tabs
    /// it would be dropped into, if any.
    tab_gap: Rc<Cell<Option<(EntityId, Option<usize>)>>>,
    /// How far a scroll has pushed into the stretch at an end, toward the
    /// barrier it must get past to cross over.
    scroll_push: Rc<Cell<ScrollPush>>,
    output_locked: bool,
    /// The latest task, and whether its output was locked, as the previous
    /// tasks were opened over it, for it to come back as it was left.
    output_left: Option<OutputLeft>,
    queue_scroll: ScrollHandle,
    chat_input: Entity<ChatInput>,
    /// The queued prompt being edited in the chat input, by its id.
    editing_queued: Option<usize>,
    /// The lanes the harness is working on a task in, as the
    /// PromptSendingScope says: a task of each lane runs beside the other's.
    working: Lanes,
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
    /// The files open in the body's tabs, after Chat, in the order they
    /// were opened.
    files: Vec<FileTab>,
    /// The file whose tab is selected, or none while Chat is.
    selected_file: Option<usize>,
    /// The task last seen heading the view while under way, by its index and
    /// name: once nothing is under way, the one that finished most recently,
    /// which heads it then.
    latest_settled: Option<(usize, SharedString)>,
    /// The chat input's tab the project on screen was on, as it was left:
    /// filled in as it is switched away from, and selected as it comes back.
    mode_tab: SendMode,
    /// Files edited on disk while they had unsaved changes, waiting to be
    /// asked about, one after another.
    disk_conflicts: Vec<WeakEntity<FileView>>,
    /// The file being asked about, while it is.
    conflict_asked: Option<EntityId>,
    /// The tab bar's sideways scrolling, once its tabs don't fit.
    tabs_scroll: ScrollHandle,
    /// Whether the referenced spec sidebar shows, as of the last frame.
    refs_shown: bool,
    /// When the referenced spec sidebar last slid out, and how many times it
    /// has.
    refs_opened: Option<(usize, Instant)>,
    /// The referenced spec sidebar, just closed, sliding back in.
    refs_closing: Option<RefsClosing>,
    /// Scrolling past an end of the latest task or the previous tasks, the
    /// one sliding in for the other, until it settles.
    scroll_slide: Option<ScrollSlide>,
    /// How tall the space between the previous tasks row and the queue was
    /// last laid out, which a scroll's slide moves across.
    slide_height: Rc<Cell<Pixels>>,
    /// The referenced spec sidebar's width, as last laid out once settled, so
    /// it opens at the width it was dragged to.
    refs_width: Rc<Cell<Pixels>>,
    /// The heights the referenced spec sidebar's panels were dragged to,
    /// kept while the application runs, as its width is.
    refs_panels: referenced_spec::PanelHeights,
    refs_split: Entity<ResizableState>,
    refs_scroll: ScrollHandle,
    understanding_scroll: ScrollHandle,
    subagents_scroll: ScrollHandle,
    /// The run of each lane's task, by [`lane_slot`].
    _pending: [Task<()>; 2],
    /// Taken by each message sent to the running task while it is compiled
    /// and sent, so messages reach it in the order they were sent.
    feeding: Arc<futures::lock::Mutex<()>>,
    /// The conversation the tasks share, each resuming the last.
    session: Option<Session>,
    /// Counts the times the tasks' conversation was left for a new one, so a
    /// run started before can't bring it back, nor can the history.
    session_epoch: u64,
    /// New conversation was pressed with nothing queued: the next task sent
    /// or queued starts the new conversation, and records so.
    new_conversation_pending: bool,
    /// The questions asked from the Ask tab, oldest first, each run at once
    /// and apart from the tasks.
    asks: Vec<Ask>,
    next_ask_id: usize,
    /// The popover offering to copy text selected in the Ask conversation, or
    /// attach it to the prompt: where the selecting drag ended, and what it selected.
    selection_popover: Option<(Point<Pixels>, String)>,
    /// The menu a task's Send to Spec or Send to Code button opens when
    /// right-clicked, while it is open.
    mark_menu: Option<MarkMenu>,
    /// The answers' tables whose steps were expanded, by task index.
    steps_shown: HashSet<usize>,
    /// The chat segments whose steps are shown, by their prompt's element id
    /// and their place in its reply, in the Ask conversation and the
    /// Freeform chat.
    chat_steps_shown: HashSet<(usize, usize)>,
    /// The question rows in the stack, drawn only as they come into view,
    /// and the questions they were last laid out for, oldest first.
    ask_rows: MeasuredList,
    ask_row_ids: RefCell<Vec<usize>>,
    /// The questions saved with the project, oldest first, loaded as it
    /// opens; those asked since are among [`Self::asks`].
    answers: Vec<PromptTask>,
    _ask_history_load: Task<()>,
    /// The Ask conversation, shown beside the tab's contents on the Ask tab.
    ask_pane: AskPane,
    /// The Freeform chat the task view shows while the latest task was sent
    /// in Freeform, as the MessageList's freeform says.
    freeform_pane: AskPane,
    /// New conversation was pressed on the Ask tab with nothing asked since:
    /// the pane marks, at its bottom, that the next question starts one.
    ask_new_pending: bool,
    /// The share of the width above the chat input the Ask conversation
    /// takes, kept while the application runs, for every project.
    ask_split_share: f32,
    /// The chat input's Ask tab is selected.
    on_ask_tab: bool,
    /// The conversation questions share, apart from the tasks'.
    ask_session: Option<Session>,
    /// As [`Self::session_epoch`], for the questions' conversation.
    ask_session_epoch: u64,
    /// What the project's tasks and questions have reported spending since
    /// it was opened.
    usage: ProjectUsage,
    /// The plan limits each harness last reported, the user's rather than a
    /// project's.
    limits: PlanLimits,
    _file_subscriptions: Vec<Subscription>,
    /// Compiling the prompt for the chat input's preview.
    _preview: Task<()>,
    /// The latest task's compiled prompt in its header, drawn a block at a
    /// time.
    header_prompt: HeaderPrompt,
    /// Writes a finished task's commit note: [`commit_notes::summarize`],
    /// replaced in tests.
    summarize: Summarize,
    /// Asks the harness for a prompt's title, to name it by:
    /// [`prompt_title::ask`], replaced in tests.
    titler: prompt_title::Titler,
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
                    let text = submit.text.clone();
                    // Its images are saved in the project's data as it is
                    // sent or queued.
                    let Some(attached) = this.save_attached(
                        &text,
                        submit.attached_text.clone(),
                        &submit.attached_images,
                        window,
                        cx,
                    ) else {
                        return;
                    };
                    // Queued on purpose, it waits in the queue even while the
                    // harness is free; a question never queues.
                    if submit.to_task && submit.mode != SendMode::Ask {
                        this.send_to_task(text, submit.mode, attached, window, cx);
                    } else if submit.queue
                        && submit.mode != SendMode::Ask
                        && this.project_dir.is_some()
                    {
                        let sliced = this.chat_input.read(cx).slices();
                        let post_build_update = submit.mode == SendMode::Both
                            && this.chat_input.read(cx).post_build_update();
                        this.enqueue(
                            text,
                            true,
                            submit.mode,
                            attached,
                            sliced,
                            None,
                            None,
                            post_build_update,
                            None,
                            window,
                            cx,
                        );
                    } else {
                        this.send_attached(text, submit.mode, attached, window, cx)
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
            cx.subscribe(&chat_input, |this, _, _: &NewConversation, cx| {
                this.new_conversation(cx)
            }),
            cx.subscribe_in(
                &chat_input,
                window,
                |this, _, _: &FocusActiveEditor, window, cx| this.focus_open_file(window, cx),
            ),
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
            last_wheel: Rc::default(),
            scroll_held_back: Rc::default(),
            scroll_spent: Rc::default(),
            queue_gap: Rc::default(),
            tab_gap: Rc::default(),
            scroll_push: Rc::default(),
            output_locked: false,
            output_left: None,
            queue_scroll: ScrollHandle::new(),
            chat_input,
            editing_queued: None,
            working: Lanes::NONE,
            history_stale: false,
            _history_load: Task::ready(()),
            task_history: HistoryList::tasks(),
            queue: Vec::new(),
            next_queue_id: 0,
            queue_expanded: false,
            auto_send: true,
            queue_held: false,
            files: Vec::new(),
            selected_file: None,
            latest_settled: None,
            mode_tab: SendMode::Both,
            disk_conflicts: Vec::new(),
            conflict_asked: None,
            tabs_scroll: ScrollHandle::new(),
            refs_shown: false,
            refs_opened: None,
            refs_closing: None,
            scroll_slide: None,
            slide_height: Rc::default(),
            refs_width: Rc::new(Cell::new(referenced_spec::WIDTH)),
            refs_panels: referenced_spec::PanelHeights::default(),
            refs_split: cx.new(|_| ResizableState::default()),
            refs_scroll: ScrollHandle::new(),
            understanding_scroll: ScrollHandle::new(),
            subagents_scroll: ScrollHandle::new(),
            _pending: [Task::ready(()), Task::ready(())],
            feeding: Arc::default(),
            session: None,
            session_epoch: 0,
            new_conversation_pending: false,
            asks: Vec::new(),
            next_ask_id: 0,
            selection_popover: None,
            mark_menu: None,
            steps_shown: HashSet::new(),
            chat_steps_shown: HashSet::new(),
            ask_rows: ask_rows(),
            ask_row_ids: RefCell::default(),
            answers: Vec::new(),
            _ask_history_load: Task::ready(()),
            ask_pane: AskPane::new(),
            freeform_pane: AskPane::new(),
            ask_new_pending: false,
            ask_split_share: ASK_SPLIT_SHARE,
            on_ask_tab: false,
            ask_session: None,
            ask_session_epoch: 0,
            usage: ProjectUsage::default(),
            limits: PlanLimits::default(),
            _file_subscriptions: Vec::new(),
            _preview: Task::ready(()),
            header_prompt: HeaderPrompt::default(),
            summarize: commit_notes::summarize,
            #[cfg(not(test))]
            titler: prompt_title::ask,
            // Tests name their prompts at random, unless they say otherwise.
            #[cfg(test)]
            titler: prompt_title::never,
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
        swap(&mut self.feeding, &mut other.feeding);
        swap(&mut self.session, &mut other.session);
        swap(&mut self.session_epoch, &mut other.session_epoch);
        swap(
            &mut self.new_conversation_pending,
            &mut other.new_conversation_pending,
        );
        swap(&mut self.asks, &mut other.asks);
        swap(&mut self.steps_shown, &mut other.steps_shown);
        swap(&mut self.chat_steps_shown, &mut other.chat_steps_shown);
        swap(&mut self.freeform_pane, &mut other.freeform_pane);
        swap(&mut self.answers, &mut other.answers);
        swap(&mut self._ask_history_load, &mut other._ask_history_load);
        swap(&mut self.ask_pane, &mut other.ask_pane);
        swap(&mut self.ask_new_pending, &mut other.ask_new_pending);
        swap(&mut self.ask_session, &mut other.ask_session);
        swap(&mut self.ask_session_epoch, &mut other.ask_session_epoch);
        swap(&mut self.usage, &mut other.usage);
        swap(&mut self.output_table, &mut other.output_table);
        swap(&mut self.output_locked, &mut other.output_locked);
        swap(&mut self.output_left, &mut other.output_left);
        swap(&mut self.header_prompt, &mut other.header_prompt);
        swap(&mut self.queue_scroll, &mut other.queue_scroll);
        swap(&mut self.ask_rows, &mut other.ask_rows);
        swap(&mut self.ask_row_ids, &mut other.ask_row_ids);
        swap(&mut self.files, &mut other.files);
        swap(&mut self.selected_file, &mut other.selected_file);
        swap(&mut self.latest_settled, &mut other.latest_settled);
        swap(&mut self.mode_tab, &mut other.mode_tab);
    }

    /// Follows the project on screen: the work of the one left keeps running
    /// in the background, and the one switched to comes back as it was left,
    /// or, the first time it is opened, is loaded from its data.
    fn project_changed(&mut self, cx: &mut Context<Self>) {
        let dir = ProjectDirectory::get(cx);
        if dir == self.project_dir {
            return;
        }
        // The tab the project left was on, kept for when it's back.
        self.mode_tab = self.chat_input.read(cx).project_mode();
        let mut left = ProjectSession::new(None, cx);
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
        // The project switched to shows just as it was left, sliding nothing
        // in: its tabs as they were, and the referenced spec sidebar as it
        // has it.
        self.refs_shown = false;
        self.refs_shown = self.refs_wanted();
        self.refs_opened = None;
        self.refs_closing = None;
        self.scroll_slide = None;
        self.selection_popover = None;
        let working = self.working;
        let mode_tab = self.mode_tab;
        self.chat_input.update(cx, |input, cx| {
            input.set_busy(working, cx);
            input.set_project_mode(mode_tab, cx);
        });
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
            working: Lanes,
            asks: &[Ask],
        ) -> Option<ProjectActivity> {
            let running = running_tasks(tasks, working);
            let questions: Vec<(usize, SharedString)> = asks
                .iter()
                .filter(|ask| ask.task.status.is_active())
                .map(|ask| (ask.id, first_line(&ask.task.text)))
                .collect();
            if running.is_empty() && questions.is_empty() {
                return None;
            }
            Some(ProjectActivity {
                project_dir: project_dir?.clone(),
                tasks: running,
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
        let tasks = running_tasks(&self.tasks, self.working)
            .into_iter()
            .map(|(ix, text)| Job {
                kind: JobKind::Task(ix),
                title: "Task".into(),
                detail: Some(text),
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
        let mut jobs: Vec<Job> = tasks.chain(questions).collect();
        for busy in self
            .busy_projects()
            .into_iter()
            .filter(|busy| Some(&busy.project_dir) != self.project_dir.as_ref())
        {
            let project = Some(busy.project_dir.clone());
            jobs.extend(busy.tasks.into_iter().map(|(ix, task)| Job {
                kind: JobKind::Task(ix),
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
    pub fn stop_test_question(&mut self, id: usize, cx: &mut Context<Self>) {
        self.stop_ask(id, cx);
    }

    /// Brings the running task at `ix` into view: the latest task's output,
    /// out from behind the previous tasks and scrolled to its end, or a task
    /// running in the other lane opened among the previous tasks.
    pub fn reveal_running_task(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tasks.len() || Some(ix) == self.latest_ix() {
            return self.reveal_task(cx);
        }
        if !self.task_history.expanded {
            self.task_history.toggle();
            self.follow_task_history();
        }
        self.task_history.filters.clear();
        self.task_history.open = Some(ix);
        cx.notify();
    }

    /// Brings the latest task's output into view: out from behind the
    /// previous tasks, scrolled to its end.
    pub fn reveal_task(&mut self, cx: &mut Context<Self>) {
        self.task_history.expanded = false;
        self.output_left = None;
        self.output_table.scroll_to_end();
        cx.notify();
    }

    /// Whether the harness is working on a task or a question in any open
    /// project.
    pub fn is_working(&self) -> bool {
        self.working.any()
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
        self.working = if working { Lanes::ALL } else { Lanes::NONE };
    }

    /// Moves keyboard focus into the chat input.
    pub fn focus_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_input
            .update(cx, |chat_input, cx| chat_input.focus(window, cx));
    }

    /// Moves keyboard focus into the editor of the file in the selected tab,
    /// and nowhere while Chat is selected.
    fn focus_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.open_file_view() {
            view.update(cx, |view, cx| view.focus_editor(window, cx));
        }
    }

    /// Inserts a harness mention into the chat input and focuses it.
    pub fn insert_mention(&mut self, mention: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_input.update(cx, |chat_input, cx| {
            chat_input.insert_mention(mention, window, cx)
        });
    }

    /// The file in the selected tab, if a file's tab is selected.
    pub fn open_file_view(&self) -> Option<Entity<FileView>> {
        self.selected_file
            .and_then(|ix| self.files.get(ix))
            .map(|tab| tab.view.clone())
    }

    /// Every file open in a tab, in the tabs' order.
    #[cfg(test)]
    pub fn open_file_views(&self) -> Vec<Entity<FileView>> {
        self.files.iter().map(|tab| tab.view.clone()).collect()
    }

    pub fn chat_input_view(&self) -> Entity<ChatInput> {
        self.chat_input.clone()
    }

    /// Opens a file in a tab of its own, or selects the tab it already has.
    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, None, window, cx)
    }

    /// Opens a file clicked in a prompt or the output in a tab.
    fn file_opener(&self, cx: &Context<Self>) -> OpenFile {
        let this = cx.entity().downgrade();
        Arc::new(move |path, window, cx| {
            this.update(cx, |this, cx| this.open_file(path, window, cx))
                .ok();
        })
    }

    /// Opens a file with the cursor at `position`: selecting the tab it
    /// already has, the cursor moved there, or else in a new tab just after
    /// the one selected, which is then selected.
    fn open_file_at(
        &mut self,
        path: PathBuf,
        position: Option<lsp_types::Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self
            .files
            .iter()
            .position(|tab| tab.view.read(cx).path() == path)
        {
            if let Some(position) = position {
                self.files[ix]
                    .view
                    .update(cx, |view, cx| view.go_to(position, window, cx));
            }
            return self.select_tab(Some(ix), cx);
        }
        let ix = self.selected_file.map_or(0, |ix| ix + 1);
        let tab = self.file_tab(path, position, window, cx);
        self.files.insert(ix, tab);
        self.select_tab(Some(ix), cx);
    }

    /// An editor for the file at `path`, wired to the chat.
    fn file_tab(
        &mut self,
        path: PathBuf,
        position: Option<lsp_types::Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FileTab {
        let view = cx.new(|cx| FileView::new(path, position, window, cx));
        let subscriptions = vec![
            // Its tab shows whether it has unsaved changes.
            cx.observe(&view, |_, _, cx| cx.notify()),
            // Text selected in the file and sent to the prompt is attached to it.
            cx.subscribe(&view, |this, _, SendToPrompt(text): &SendToPrompt, cx| {
                let text = text.clone();
                this.chat_input
                    .update(cx, |input, cx| input.attach_text(text, cx));
            }),
            cx.subscribe(&view, |this, view, _: &CloseFile, cx| {
                this.close_file_tab(view.entity_id(), cx)
            }),
            // Edited on disk with unsaved changes, it asks what to do.
            cx.subscribe_in(
                &view,
                window,
                |this, view, _: &ChangedOnDisk, window, cx| {
                    this.disk_conflicts.push(view.downgrade());
                    this.ask_next_conflict(window, cx);
                },
            ),
            cx.subscribe_in(
                &view,
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
        FileTab {
            view,
            _subscriptions: subscriptions,
        }
    }

    /// Asks about the next file edited on disk while it had unsaved changes,
    /// unless one is being asked about: keep the changes, reload, or merge.
    /// A file of a project not on screen waits until it is.
    fn ask_next_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conflict_asked.is_some() {
            return;
        }
        let on_screen: Vec<EntityId> = self.files.iter().map(|tab| tab.view.entity_id()).collect();
        self.disk_conflicts.retain(|view| {
            view.upgrade()
                .is_some_and(|view| view.read(cx).has_conflict())
        });
        let Some(ix) = self
            .disk_conflicts
            .iter()
            .position(|view| on_screen.contains(&view.entity_id()))
        else {
            return;
        };
        let Some(view) = self.disk_conflicts.remove(ix).upgrade() else {
            return;
        };
        let id = view.entity_id();
        self.conflict_asked = Some(id);
        let name: SharedString = view.read(cx).path().file_name().map_or_else(
            || view.read(cx).title(),
            |name| name.to_string_lossy().into_owned().into(),
        );
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let choose = |choice: ConflictChoice| {
                let this = this.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    this.update(cx, |this, cx| this.answer_conflict(id, choice, window, cx))
                        .ok();
                }
            };
            let on_close = choose(ConflictChoice::KeepMine);
            alert
                .title(SharedString::from(format!("{name} changed on disk")))
                .description(SharedString::from(format!(
                    "{name} was changed outside the editor while it has unsaved changes here."
                )))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("conflict-keep-mine")
                                .outline()
                                .label("Keep Mine")
                                .on_click(choose(ConflictChoice::KeepMine)),
                        )
                        .child(
                            Button::new("conflict-reload")
                                .outline()
                                .label("Reload")
                                .on_click(choose(ConflictChoice::Reload)),
                        )
                        .child(
                            Button::new("conflict-merge")
                                .primary()
                                .label("Merge")
                                .on_click(choose(ConflictChoice::Merge)),
                        ),
                )
                // <Escape>, or closing it otherwise, keeps the changes.
                .on_close(on_close)
        });
    }

    /// Acts on what was chosen for the file `id` edited on disk, closes the
    /// dialog, and asks about the next.
    fn answer_conflict(
        &mut self,
        id: EntityId,
        choice: ConflictChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.conflict_asked != Some(id) {
            return;
        }
        self.conflict_asked = None;
        window.close_dialog(cx);
        if let Some(ix) = self.file_tab_index(id) {
            let view = self.files[ix].view.clone();
            match choice {
                ConflictChoice::KeepMine => view.update(cx, |view, cx| view.keep_mine(cx)),
                ConflictChoice::Reload => {
                    self.select_tab(Some(ix), cx);
                    view.update(cx, |view, cx| view.reload_from_disk(window, cx));
                }
                ConflictChoice::Merge => {
                    self.select_tab(Some(ix), cx);
                    view.update(cx, |view, cx| view.start_merge(window, cx));
                }
            }
        }
        self.ask_next_conflict(window, cx);
    }

    /// Selects the tab of the file at `ix`, or Chat for none.
    fn select_tab(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        self.selected_file = file.filter(|ix| *ix < self.files.len());
        // Scrolled to, should it be past the edge of the tab bar.
        self.tabs_scroll
            .scroll_to_item(self.selected_file.map_or(0, |ix| ix + 1));
        cx.notify();
    }

    /// Where the tab of the file `view` is among the file tabs.
    fn file_tab_index(&self, view: EntityId) -> Option<usize> {
        self.files
            .iter()
            .position(|tab| tab.view.entity_id() == view)
    }

    /// Moves the tab of the file `view` to place `to` among the file tabs,
    /// those between moving over to make room; Chat stays first. The
    /// selected tab stays selected wherever it ends up.
    fn move_file_tab(&mut self, view: EntityId, to: usize, cx: &mut Context<Self>) {
        let Some(from) = self.file_tab_index(view) else {
            return;
        };
        if from == to || to >= self.files.len() {
            return;
        }
        let selected = self.selected_file.map(|ix| self.files[ix].view.entity_id());
        let tab = self.files.remove(from);
        self.files.insert(to, tab);
        let selected = selected.and_then(|view| self.file_tab_index(view));
        self.select_tab(selected, cx);
    }

    /// Closes the tab of the file `view`, with nothing asked. Closing the
    /// selected tab selects the one to its right, else the one to its left.
    fn close_file_tab(&mut self, view: EntityId, cx: &mut Context<Self>) {
        let Some(ix) = self
            .files
            .iter()
            .position(|tab| tab.view.entity_id() == view)
        else {
            return;
        };
        self.files.remove(ix);
        let selected = match self.selected_file {
            Some(selected) if selected == ix => {
                if ix < self.files.len() {
                    Some(ix)
                } else {
                    ix.checked_sub(1)
                }
            }
            Some(selected) if selected > ix => Some(selected - 1),
            selected => selected,
        };
        self.select_tab(selected, cx);
    }

    /// A file or folder was renamed to `to`, or deleted: each file open in a
    /// tab, when it is that file or inside that folder, follows it to its new
    /// path in the same tab, unsaved changes and all, or its tab closes
    /// without asking.
    pub fn file_moved(
        &mut self,
        from: &Path,
        to: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let moved: Vec<(EntityId, PathBuf)> = self
            .files
            .iter()
            .filter_map(|tab| {
                let view = tab.view.read(cx);
                let within = view.path().strip_prefix(from).ok()?;
                let moved = match to {
                    Some(to) if within.as_os_str().is_empty() => to.to_path_buf(),
                    Some(to) => to.join(within),
                    None => PathBuf::new(),
                };
                Some((tab.view.entity_id(), moved))
            })
            .collect();
        for (view, moved) in moved {
            match to {
                Some(_) => {
                    let Some(ix) = self.file_tab_index(view) else {
                        continue;
                    };
                    self.files[ix]
                        .view
                        .update(cx, |view, cx| view.follow_rename(moved, window, cx));
                    cx.notify();
                }
                None => self.close_file_tab(view, cx),
            }
        }
    }

    /// The body's tab bar: Chat, with a spinner while a task or question
    /// runs, then a tab for each open file, with its name, a dot while it has
    /// unsaved changes, and a button closing it.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        // While a file's tab is dragged: which, and the gap it would land in.
        if !cx.has_active_drag() {
            self.tab_gap.set(None);
        }
        let (tab_dragging, tab_gap) = match self.tab_gap.get() {
            Some((view, gap)) => (Some(view), gap),
            None => (None, None),
        };
        let files_len = self.files.len();
        let views: Rc<Vec<EntityId>> =
            Rc::new(self.files.iter().map(|tab| tab.view.entity_id()).collect());
        let select = this.clone();
        let file_tabs = self.files.iter().enumerate().map(|(ix, tab)| {
            let file = tab.view.read(cx);
            let name: SharedString = file
                .path()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into();
            let title = file.title();
            let view = tab.view.clone();
            let dragged = view.entity_id();
            let close =
                // A 16 pixel square, drawn 6 pixels into the tab's right
                // padding, so the icon sits as far from its right edge as the
                // name does from its left.
                gpui_kit::TestSupportExt::test_support(div().id(("close-file-tab-box", ix)))
                    .flex_none()
                    .relative()
                    .w(CLOSE_TAB_SIZE - CLOSE_TAB_PULL)
                    .h(CLOSE_TAB_SIZE)
                    .child(div().absolute().top_0().left_0().child(
                    Button::new(("close-file-tab", ix))
                        .ghost()
                        .xsmall()
                        .size(CLOSE_TAB_SIZE)
                        .icon(IconName::X)
                        .tooltip("Close file")
                        // The press closing it neither selects the tab nor drags it.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            view.update(cx, |view, cx| view.close(window, cx));
                        }),
                ));
            Tab::new()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                })
                // Dragged along the bar, it is selected, and dropped on
                // another file's tab takes its place.
                .on_drag(
                    FileTabDrag {
                        view: dragged,
                        name: name.clone(),
                    },
                    {
                        let this = this.clone();
                        move |drag, _, _, cx| {
                            this.update(cx, |this, cx| {
                                this.tab_gap.set(Some((drag.view, None)));
                                if let Some(ix) = this.file_tab_index(drag.view) {
                                    this.select_tab(Some(ix), cx);
                                }
                            })
                            .ok();
                            cx.new(|_| drag.clone())
                        }
                    },
                )
                // The one dragged stays in its place, dimmed.
                .when(tab_dragging == Some(dragged), |tab| tab.opacity(0.5))
                // Over its first half, the gap before it; over its second,
                // the one after. Chat stays first: the gap before the first
                // file's tab is never one.
                .on_drag_move({
                    let (tab_gap_cell, views) = (self.tab_gap.clone(), views.clone());
                    move |event: &DragMoveEvent<FileTabDrag>, window, cx| {
                        let Some(at) = drag_gap(ix, event.bounds, event.event.position, false)
                        else {
                            return;
                        };
                        let view = event.drag(cx).view;
                        let gap = views
                            .iter()
                            .position(|v| *v == view)
                            .and_then(|from| gap_target(from, at))
                            .filter(|_| at > 0)
                            .map(|_| at);
                        if tab_gap_cell.get() != Some((view, gap)) {
                            tab_gap_cell.set(Some((view, gap)));
                            window.refresh();
                        }
                    }
                })
                .on_drop({
                    let this = this.clone();
                    move |drag: &FileTabDrag, _, cx| {
                        this.update(cx, |this, cx| {
                            let gap = this.tab_gap.take().and_then(|(_, gap)| gap);
                            let from = this.file_tab_index(drag.view);
                            if let Some(to) =
                                from.zip(gap).and_then(|(from, gap)| gap_target(from, gap))
                            {
                                this.move_file_tab(drag.view, to, cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .relative()
                .when(tab_gap == Some(ix), |tab| {
                    tab.child(drop_indicator(true, false, cx))
                })
                .when(ix + 1 == files_len && tab_gap == Some(files_len), |tab| {
                    tab.child(drop_indicator(true, true, cx))
                })
                // The close button sits just after the name, or its unsaved
                // dot, rather than across the tab's own padding.
                // Lets UI tests find the tab's contents; inert in normal
                // builds.
                .child(
                    gpui_kit::TestSupportExt::test_support(h_flex().id(("file-tab", ix)))
                        .flex_none()
                        .gap_1()
                        .child(
                            gpui_kit::TestSupportExt::test_support(div().id(("file-tab-name", ix)))
                                .child(name),
                        )
                        .when(file.is_dirty(), |this| {
                            this.child(
                                div()
                                    .id(("file-tab-unsaved", ix))
                                    .flex_none()
                                    .size_2()
                                    .rounded_full()
                                    .bg(muted),
                            )
                        })
                        .child(close),
                )
        });
        let bar = TabBar::new("body-tabs")
            .track_scroll(&self.tabs_scroll)
            .selected_index(self.selected_file.map_or(0, |ix| ix + 1))
            .on_click(move |ix, _, cx| {
                select
                    .update(cx, |this, cx| this.select_tab(ix.checked_sub(1), cx))
                    .ok();
            })
            // The spinner sits within the tab's padding, beside its label, as
            // a file tab's unsaved dot does; a suffix would touch its edge.
            .child(
                Tab::new().child(
                    h_flex()
                        .gap_1p5()
                        .child("Chat")
                        .when(self.chat_running(), |this| {
                            this.child(Spinner::new().xsmall())
                        }),
                ),
            )
            .children(file_tabs);
        // Lets UI tests find the tab bar; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(div().id("body-tabs-row"))
            .flex_none()
            .w_full()
            .min_w_0()
            .child(bar)
    }

    /// Adds a task for `text`, compiling, as the latest. Returns its index.
    fn push_task(&mut self, text: SharedString, cx: &mut Context<Self>) -> usize {
        let mut task = PromptTask::new(text);
        task.references = referenced_spec::References::new(self.project_dir.clone());
        self.tasks.push(task);
        // A new task's output starts at its top.
        self.scroll_output_to_top();
        self.settle_latest();
        cx.notify();
        self.tasks.len() - 1
    }

    /// The task heading the view as the latest: of the tasks under way, in
    /// either lane, the one sent most recently; while none is, the one that
    /// finished most recently, or else the one sent most recently.
    fn latest_ix(&self) -> Option<usize> {
        latest_of(&self.tasks, self.latest_settled.as_ref())
    }

    /// The latest task.
    fn latest_task(&self) -> Option<&PromptTask> {
        self.latest_ix().map(|ix| &self.tasks[ix])
    }

    /// Notes which task heads the view, for once nothing is under way, and
    /// when another has come to head it, shows its output as a task just
    /// sent shows its own: from its top.
    fn settle_latest(&mut self) {
        let Some(ix) = self.latest_ix() else {
            self.latest_settled = None;
            return;
        };
        let name = self.tasks[ix].name.clone();
        let changed = self
            .latest_settled
            .as_ref()
            .is_some_and(|(was, was_name)| *was != ix || *was_name != name);
        if changed && ix + 1 != self.tasks.len() {
            self.scroll_output_to_top();
        }
        self.latest_settled = Some((ix, name));
    }

    /// Applies a harness event to the task at `ix`.
    fn apply_event(&mut self, ix: usize, event: HarnessEvent, cx: &mut Context<Self>) {
        if ix >= self.tasks.len() {
            return;
        }
        if self.in_background {
            self.tasks[ix].apply(event);
            self.settle_latest();
            return;
        }
        let latest = Some(ix) == self.latest_ix();
        // A raw output line only changes the raw tail at the end of the
        // output, so there is nothing to redraw while that is out of sight.
        // The Freeform chat's status line shows the latest raw line too.
        let freeform = self.tasks[ix].mode == Some(SendMode::Freeform);
        let unseen = matches!(event, HarnessEvent::Output(_))
            && (!latest
                || self.task_history.expanded
                || (!freeform && !self.output_table.end_in_view()));
        // Follows new output only while already scrolled to the bottom.
        let scroll = self.output_table.scroll();
        let following = scroll.offset().y <= -scroll.max_offset().y + px(1.);
        self.tasks[ix].apply(event);
        // A task finishing may leave another heading the view.
        self.settle_latest();
        if unseen {
            return;
        }
        if (following || self.output_locked) && latest {
            self.output_table.scroll_to_end();
        }
        cx.notify();
    }

    /// A wheel scroll `up`, or down, carries on past the latest task's
    /// top into the previous tasks, opening them on the latest, or past their
    /// bottom back to the latest task, shown as it was left. Returns whether
    /// it did.
    fn scroll_past(&mut self, up: bool, cx: &mut Context<Self>) -> bool {
        if !self.can_scroll_past(up) {
            return false;
        }
        self.task_history.toggle();
        self.follow_task_history();
        let n = self.scroll_slide.as_ref().map_or(0, |slide| slide.n + 1);
        self.scroll_slide = Some(ScrollSlide {
            n,
            up,
            started: Instant::now(),
            glow: 0.,
            pull: px(0.),
        });
        cx.notify();
        true
    }

    /// Keeps the latest task's place as the previous tasks open over it, and
    /// puts it back as they close: locked, at its bottom and still following
    /// its output; unlocked, where it was in its output. Its place is gone
    /// once another task is the latest, and it comes back at its top,
    /// unlocked.
    fn follow_task_history(&mut self) {
        let latest = self.latest_task().map(|task| task.name.clone());
        if self.task_history.expanded {
            if self.output_left.is_none() {
                self.output_left = Some(OutputLeft {
                    task: latest,
                    tasks: self.tasks.len(),
                    locked: self.output_locked,
                });
            }
            return;
        }
        let Some(left) = self.output_left.take() else {
            return;
        };
        if left.task != latest || left.tasks != self.tasks.len() {
            self.output_locked = false;
            self.scroll_output_to_top();
        } else if left.locked {
            self.output_locked = true;
            self.output_table.scroll_to_end();
        } else {
            // Its list kept its place while hidden.
            self.output_locked = false;
        }
    }

    /// Whether a scroll `up`, or down, is at the end it would cross over
    /// from: the latest task's top, with previous tasks to open, or the
    /// previous tasks' bottom.
    fn can_scroll_past(&self, up: bool) -> bool {
        self.to_end(up).is_some_and(|to_end| to_end <= 1.)
    }

    /// How far a scroll `up`, or down, has to go to reach the end it would
    /// cross over from, as [`Self::can_scroll_past`] says; none when there
    /// is nothing to cross over to that way.
    ///
    /// It goes by where the list laid its end row out, not by the heights it
    /// has guessed for rows not yet measured, so a view resting at its end
    /// is always at it, however it got there.
    fn to_end(&self, up: bool) -> Option<f32> {
        if self.task_history.expanded {
            (!up).then(|| f32::from(self.task_history.rows.to_bottom()))
        } else {
            let latest_freeform = self
                .latest_task()
                .is_some_and(|task| task.mode == Some(SendMode::Freeform));
            (up && self.tasks.len() >= 2).then(|| {
                f32::from(if latest_freeform {
                    self.freeform_to_top()
                } else {
                    self.output_table.list().to_top()
                })
            })
        }
    }

    /// Whether a scroll's crossing over is still sliding.
    fn scroll_sliding(&self) -> bool {
        self.scroll_slide
            .as_ref()
            .is_some_and(|slide| slide.started.elapsed() < PANE_SLIDE_TIME)
    }

    /// A soft radial glow in the middle of the space between the previous
    /// tasks row and the queue, in the selected mode's colour, growing
    /// larger and slightly brighter as a scroll's build-up nears the
    /// barrier, and fading out quickly as the view crosses over. None while
    /// nothing is built up. It takes no clicks or scrolls.
    fn render_scroll_glow(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let building = self.scroll_push.get().held(Instant::now()) / SCROLL_BARRIER;
        let fading = self
            .scroll_slide
            .as_ref()
            .map(|slide| {
                let faded = slide.started.elapsed().as_secs_f32() / GLOW_FADE.as_secs_f32();
                slide.glow * (1. - faded).max(0.)
            })
            .unwrap_or(0.);
        let (progress, strength) = if building > 0. {
            (building.min(1.), 1.)
        } else if fading > 0. {
            (fading, fading)
        } else {
            return None;
        };
        // It drains, or fades, on its own.
        window.request_animation_frame();
        let height = self.slide_height.get().max(px(1.));
        let radius = height * (0.12 + 0.3 * progress);
        let color = chat_input::mode_color(self.chat_input.read(cx).mode(), cx);
        let glow = Hsla {
            a: (0.05 + 0.1 * progress) * strength,
            ..color
        };
        // Lets UI tests find the glow; inert in normal builds.
        Some(
            gpui_kit::TestSupportExt::test_support(div().id("scroll-glow"))
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(2.))
                        .rounded_full()
                        .bg(glow)
                        .shadow(vec![BoxShadow {
                            color: glow,
                            offset: point(px(0.), px(0.)),
                            blur_radius: radius,
                            spread_radius: radius * 0.45,
                            inset: false,
                        }]),
                )
                .into_any_element(),
        )
    }

    /// Over the latest task's output or the previous tasks, hears the wheel
    /// before they do, so a scroll reaching an end carries on into the
    /// stretch there and past it, as [`Self::scroll_past`] says. A fling's
    /// momentum, a scroll carried on from a slide, and a sideways one never
    /// do.
    fn scroll_past_ends(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let last = self.last_wheel.clone();
        let held_back = self.scroll_held_back.clone();
        let spent = self.scroll_spent.clone();
        let push = self.scroll_push.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    let now = Instant::now();
                    // While it slides, the wheel scrolls neither view, and a
                    // scroll then is carried on rather than new.
                    if this
                        .upgrade()
                        .is_some_and(|this| this.read(cx).scroll_sliding())
                    {
                        last.set(Some(now));
                        held_back.set(true);
                        spent.set(true);
                        push.set(ScrollPush::default());
                        cx.stop_propagation();
                        return;
                    }
                    // A new scroll pulls into the stretch again; once the
                    // fingers lift, what follows is a fling's momentum.
                    if event.touch_phase == TouchPhase::Started
                        || last
                            .get()
                            .is_none_or(|at| now.duration_since(at) >= SCROLL_REST)
                    {
                        held_back.set(false);
                        spent.set(false);
                    }
                    if event.touch_phase == TouchPhase::Ended {
                        held_back.set(true);
                    }
                    last.set(Some(now));
                    // A scroll that has crossed over is spent: what more of it
                    // follows scrolls neither view, until a new scroll.
                    if spent.get() {
                        if *SCROLL_LOG {
                            eprintln!("scroll: {:?} spent, swallowed", event.delta);
                        }
                        push.set(ScrollPush::default());
                        cx.stop_propagation();
                        return;
                    }
                    let delta = event.delta.pixel_delta(window.line_height());
                    if delta.y.abs() <= delta.x.abs() {
                        return;
                    }
                    let up = delta.y > px(0.);
                    let distance = f32::from(delta.y.abs());
                    let mut held = push.get();
                    held.amount = held.held(now);
                    let before = held.amount;
                    let mut reaching = false;
                    let pushing = if held.amount > 0. {
                        if held.up != up {
                            // Scrolling back takes it down, and scrolls the
                            // view once it is gone.
                            held.amount -= distance;
                            held.amount > 0.
                        } else {
                            // Momentum adds nothing, but the end still holds.
                            if !held_back.get() {
                                held.amount += distance;
                            }
                            true
                        }
                    } else if let Some(to_end) = this
                        .upgrade()
                        .filter(|_| !held_back.get())
                        .and_then(|this| this.read(cx).to_end(up))
                        .filter(|to_end| *to_end < distance || *to_end <= 1.)
                    {
                        // A scroll at an end starts pulling into the stretch;
                        // one reaching it, with whatever of it goes past,
                        // the view scrolling to its end first.
                        reaching = to_end > 1.;
                        held = ScrollPush {
                            up,
                            amount: if reaching {
                                distance - to_end
                            } else {
                                distance
                            },
                            at: now,
                        };
                        true
                    } else {
                        false
                    };
                    if *SCROLL_LOG {
                        let to_end = this.upgrade().and_then(|this| this.read(cx).to_end(up));
                        eprintln!(
                            "scroll: {:?} {} {distance:.0}px phase={:?} held_back={} \
                             to_end={to_end:?} built_up={before:.0}->{:.0} pushing={pushing} \
                             reaching={reaching}",
                            event.delta,
                            if up { "up" } else { "down" },
                            event.touch_phase,
                            held_back.get(),
                            if pushing { held.amount } else { 0. },
                        );
                    }
                    if !pushing {
                        push.set(ScrollPush::default());
                        return;
                    }
                    held.at = now;
                    // The view's own scroll holds at its end while it is
                    // pulled.
                    if !reaching {
                        cx.stop_propagation();
                    }
                    if held.amount >= SCROLL_BARRIER {
                        // The step that crosses over is used up by it, all of
                        // it, and the rest of its scroll is spent.
                        cx.stop_propagation();
                        let pull = held.pull(now);
                        push.set(ScrollPush::default());
                        held_back.set(true);
                        spent.set(true);
                        this.update(cx, |this, cx| {
                            if this.scroll_past(held.up, cx) {
                                if let Some(slide) = this.scroll_slide.as_mut() {
                                    slide.glow = 1.;
                                    slide.pull = pull;
                                }
                            }
                        })
                        .ok();
                        return;
                    }
                    push.set(held);
                    this.update(cx, |_, cx| cx.notify()).ok();
                });
            },
        )
        .absolute()
        .size_full()
    }

    fn scroll_output_to_top(&self) {
        // The project's own output list, on screen or not.
        self.output_table.scroll_to_top();
    }

    /// Opens the raw prompt modal on task `ix`: what the harness was given
    /// for it, once it has compiled.
    fn open_raw_prompt(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.get(ix) else {
            return;
        };
        let Some(compiled) = &task.compiled else {
            return;
        };
        let prompt = RawPrompt::new(
            task.mode,
            compiled.anchor.clone(),
            &compiled.markdown,
            task.given.as_ref(),
        )
        .with_images(&task.sent.attached_images);
        RawPromptView::open(prompt, window, cx);
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
        let sliced = input.read(cx).slices();
        let (text, mode, attached_text) = (
            preview.text.clone(),
            preview.mode,
            preview.attached_text.clone(),
        );
        let compile = cx.background_spawn(async move {
            // A Freeform prompt is sent as it is, with nothing to compile.
            if mode == SendMode::Freeform {
                return Ok(hidden_anchor::freeform(&text, &attached_text));
            }
            let anchor = resolve_anchor(
                &text,
                mode,
                attached_text.into(),
                sliced,
                None,
                lsp,
                &project_dir,
            )?;
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
        self.send_attached(text, mode, attached_text.into(), window, cx)
    }

    /// Sends `text` in `mode`, with what is `attached` to it, as
    /// [`Self::send`] does; its images are already saved in the project's
    /// data.
    pub fn send_attached(
        &mut self,
        text: String,
        mode: SendMode,
        attached: Attached,
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
        let sliced = self.chat_input.read(cx).slices();
        // Only a chain goes on to a post-build spec update.
        let post_build_update =
            mode == SendMode::Both && self.chat_input.read(cx).post_build_update();
        self.send_as(
            text,
            mode,
            attached,
            sliced,
            None,
            None,
            post_build_update,
            None,
            window,
            cx,
        );
    }

    /// What is attached to a prompt as it is sent or queued: `attached_text`,
    /// and `images`, each saved in the project's data unless it already is.
    /// None, the prompt put back in the chat input as it was, when an image
    /// couldn't be saved.
    fn save_attached(
        &mut self,
        text: &str,
        attached_text: Vec<String>,
        images: &[AttachedImage],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Attached> {
        let Some(project_dir) = self.project_dir.clone().filter(|_| !images.is_empty()) else {
            // With no project open, sending says so.
            return Some(attached_text.into());
        };
        match attached_image::save_all(images, &project_dir) {
            Ok(images) => Some(Attached {
                text: attached_text,
                images,
            }),
            Err(err) => {
                window.push_notification(
                    Notification::error(format!("{err:#}")).title("Could not attach the images"),
                    cx,
                );
                let (text, images) = (text.to_string(), images.to_vec());
                self.chat_input.update(cx, |input, cx| {
                    input.take_back(text, attached_text, images, window, cx)
                });
                None
            }
        }
    }

    /// Sends `text` in `mode`, sliced or not, as [`Self::send`] does; in Spec,
    /// told what `code_task`, the Code task it was sent from, if any, did;
    /// sent to the other mode, knowing `sent_from`, the name of the task it
    /// was sent from; a chain or its code step followed by a post-build spec
    /// update or not; named after the prompt named `named_after`, if any, as
    /// a resend is.
    #[allow(clippy::too_many_arguments)]
    fn send_as(
        &mut self,
        text: String,
        mode: SendMode,
        attached: Attached,
        sliced: bool,
        code_task: Option<CodeTask>,
        sent_from: Option<String>,
        post_build_update: bool,
        named_after: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_dir.is_none() {
            return;
        }
        if mode == SendMode::Ask {
            self.ask(text, attached, sliced, named_after, cx);
        } else if self.working.queues(mode) {
            self.enqueue(
                text,
                false,
                mode,
                attached,
                sliced,
                code_task,
                sent_from,
                post_build_update,
                named_after,
                window,
                cx,
            );
        } else {
            self.start(
                text,
                Sending::Now(
                    mode,
                    attached,
                    sliced,
                    code_task,
                    sent_from,
                    post_build_update,
                    named_after,
                ),
                cx,
            );
        }
    }

    /// Sends the task at `ix` of `tasks_of` again, as it was sent. One of
    /// unknown mode goes from the selected tab.
    fn resend(
        &mut self,
        tasks_of: fn(&PromptMode) -> &Vec<PromptTask>,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resend_in(tasks_of, ix, None, window, cx);
    }

    /// Sends the task at `ix` of `tasks_of` again as [`Self::resend`] does,
    /// but in the other mode ([`SendMode::other`]): a Code task to Spec, a
    /// Spec task to Code. A task with no other mode isn't sent.
    /// Cancels the task at `ix` straight away, if it is under way: a build or
    /// compile is abandoned before it runs, and a run stopped, its harness's
    /// process ended. It keeps its output, reads "Cancelled", and the queue
    /// carries on after it as after a failure; questions run on.
    fn cancel_task(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.get_mut(ix).filter(|task| task.can_cancel()) else {
            return;
        };
        task.cancel();
        if let Some(cancel) = task.cancel.as_mut() {
            cancel.cancel();
        }
        cx.notify();
    }

    fn send_to_other_mode(
        &mut self,
        tasks_of: fn(&PromptMode) -> &Vec<PromptTask>,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(to) = tasks_of(self)
            .get(ix)
            .and_then(|task| self.offers_other_mode(task))
        else {
            return;
        };
        self.resend_in(tasks_of, ix, Some(to), window, cx);
    }

    /// The mode `task` offers to be sent to, from its Send to Spec or Send to
    /// Code button, as [`other_mode`] says, unless it has already been sent
    /// there, is being sent, or was marked done (see
    /// [`Self::other_mode_state`]).
    fn offers_other_mode(&self, task: &PromptTask) -> Option<SendMode> {
        self.other_mode_state(task)
            .filter(|&(_, state)| state == OtherModeState::Offered)
            .map(|(to, _)| to)
    }

    /// The mode `task`'s Send to Spec or Send to Code button sends it to, and
    /// how the button stands; none for a task that has no such button, as
    /// [`other_mode`] says. A task itself sent from the other mode is
    /// complete, before anything else; then a task sent there that finished
    /// as Done, then one on its way, then the task's own mark.
    fn other_mode_state(&self, task: &PromptTask) -> Option<(SendMode, OtherModeState)> {
        let to = other_mode(task)?;
        let state = match self.sent_to_other_mode(task) {
            _ if task.sent.sent_from.is_some() => OtherModeState::Complete,
            Some(sent) => sent,
            None if task.marked_done => OtherModeState::MarkedDone,
            None => OtherModeState::Offered,
        };
        Some((to, state))
    }

    /// Whether `task` has been sent to the other mode, and that hasn't come
    /// to nothing: [`OtherModeState::Sent`] once a task sent from it is done,
    /// else [`OtherModeState::Sending`] while a prompt sent from it waits in
    /// the queue, or a task sent from it is building, compiling, or running.
    /// One that failed, was cancelled, or wasn't recorded doesn't count, and
    /// nor does one no longer in the history.
    fn sent_to_other_mode(&self, task: &PromptTask) -> Option<OtherModeState> {
        let from = |sent_from: &Option<String>| sent_from.as_deref() == Some(task.name.as_ref());
        let sent_from_it = || self.tasks.iter().filter(|sent| from(&sent.sent.sent_from));
        if sent_from_it().any(|sent| sent.status == TaskStatus::Done) {
            Some(OtherModeState::Sent)
        } else if self.queue.iter().any(|item| from(&item.sent_from))
            || sent_from_it().any(|sent| sent.status.is_active())
        {
            Some(OtherModeState::Sending)
        } else {
            None
        }
    }

    /// Marks the task named `name` done by hand for sending to the other
    /// mode, or not, from its Send to Spec or Send to Code button's menu,
    /// sending nothing. The mark is saved in its history record, straight
    /// away unless its run is still to save the record, which then saves it
    /// (see [`Self::send_as`]).
    fn set_marked_done(&mut self, name: &str, marked_done: bool, cx: &mut Context<Self>) {
        let Some(task) = self
            .tasks
            .iter_mut()
            .find(|task| task.name.as_ref() == name)
        else {
            return;
        };
        if task.marked_done == marked_done {
            return;
        }
        task.marked_done = marked_done;
        // Its run, while it has one, saves the mark with its record.
        let saved_by_its_run = task.cancel.is_some();
        if !saved_by_its_run && let Some(project_dir) = self.project_dir.clone() {
            let name = name.to_string();
            cx.background_spawn(async move {
                if let Some(file) = prompt_history::history_file(&project_dir, &name) {
                    prompt_history::save_marked_done(&file, marked_done).ok();
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Opens the menu right-clicking the Send to Spec or Send to Code button
    /// of the task named `name` opens, at `position`, in place of any open:
    /// "Mark as done", or "Mark as not done" on a task marked so. A task
    /// that is [`OtherModeState::Complete`] opens none, since it can't be
    /// marked not done.
    fn open_mark_menu(
        &mut self,
        name: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(marked_done) = self
            .tasks
            .iter()
            .find(|task| task.name == name)
            .filter(|task| {
                !matches!(
                    self.other_mode_state(task),
                    Some((_, OtherModeState::Complete))
                )
            })
            .map(|task| task.marked_done)
        else {
            return;
        };
        let this = cx.entity().downgrade();
        let view = PopupMenu::build(window, cx, move |menu, _, _| {
            let this = this.clone();
            let name = name.clone();
            menu.item(
                PopupMenuItem::new(mark_label(marked_done))
                    .icon(if marked_done {
                        IconName::RotateCcw
                    } else {
                        IconName::Check
                    })
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| this.set_marked_done(&name, !marked_done, cx))
                            .ok();
                    }),
            )
        });
        let previous_focus = window.focused(cx).filter(|focused| {
            self.mark_menu
                .as_ref()
                .is_none_or(|open| *focused != open.view.focus_handle(cx))
        });
        let previous_focus = previous_focus.or_else(|| {
            self.mark_menu
                .as_ref()
                .and_then(|open| open.previous_focus.clone())
        });
        let dismissed =
            cx.subscribe_in(&view, window, |this, view, _: &DismissEvent, window, cx| {
                let Some(open) = this.mark_menu.take_if(|open| open.view == *view) else {
                    return;
                };
                // Given back only if nothing else took the focus.
                let menu_focused = view.focus_handle(cx).contains_focused(window, cx);
                if (window.focused(cx).is_none() || menu_focused)
                    && let Some(previous) = &open.previous_focus
                {
                    window.focus(previous, cx);
                }
                cx.notify();
            });
        view.focus_handle(cx).focus(window, cx);
        self.mark_menu = Some(MarkMenu {
            view,
            position,
            previous_focus,
            _dismissed: dismissed,
        });
        cx.notify();
    }

    /// The menu a Send to Spec or Send to Code button's right-click opened,
    /// where it was right-clicked, while it is open.
    fn render_mark_menu(&self) -> Option<AnyElement> {
        let open = self.mark_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(open.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(open.view.clone()),
            )
            .with_priority(gpui_kit::base::POPUP_PRIORITY)
            .into_any_element(),
        )
    }

    /// The selected previous tasks a batch action sends to `to`, oldest
    /// first: those sent in the mode it is the other of, and not already sent
    /// there. Chain tasks, those whose mode isn't known, and those complete,
    /// having been sent from the other mode themselves, are never among
    /// them.
    fn selected_for(&self, to: SendMode) -> Vec<usize> {
        self.task_history
            .selected
            .iter()
            .copied()
            .filter(|&ix| {
                self.tasks
                    .get(ix)
                    .and_then(|task| self.offers_other_mode(task))
                    == Some(to)
            })
            .collect()
    }

    /// Sends each selected previous task that can go to `to` there, as
    /// [`Self::send_to_other_mode`] does for one, oldest first, so they queue
    /// in the order they were first sent; then deselects them, leaving the
    /// list as it was.
    fn send_selected_to(&mut self, to: SendMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_dir.is_none() {
            window.push_notification(
                Notification::error("Open a project before sending a prompt.")
                    .title("No project open"),
                cx,
            );
            return;
        }
        let sending = self.selected_for(to);
        // The first starts at once if its lane is free, and so marks the lane
        // busy before the next is sent, which queues behind it.
        for &ix in &sending {
            self.send_to_other_mode(|this| &this.tasks, ix, window, cx);
        }
        for ix in sending {
            self.task_history.selected.remove(&ix);
        }
        cx.notify();
    }

    /// The previous tasks' batch actions, at the right of their row while any
    /// is selected: how many are, "Send N to Spec" and "Send N to Code", each
    /// shown while the selection holds a task it sends, then "Clear".
    fn render_selection_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let count = self
            .task_history
            .selected
            .iter()
            .filter(|&&ix| ix < self.tasks.len())
            .count();
        if count == 0 {
            return Vec::new();
        }
        let theme = cx.theme();
        let mut actions = vec![
            gpui_kit::TestSupportExt::test_support(
                div()
                    .id("history-selected-count")
                    .flex_none()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{count} selected")),
            )
            .into_any_element(),
        ];
        for (id, to) in [
            ("history-send-selected-to-spec", SendMode::Spec),
            ("history-send-selected-to-code", SendMode::Code),
        ] {
            let sending = self.selected_for(to).len();
            if sending == 0 {
                continue;
            }
            let color = chat_input::mode_color(to, cx);
            let button = Button::new("send-selected")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::ArrowRightLeft).text_color(color))
                .label(format!("Send {sending} to {}", to.label()))
                .text_color(color)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.send_selected_to(to, window, cx);
                }));
            // Lets UI tests find and click the button; inert in normal builds.
            actions.push(
                gpui_kit::TestSupportExt::test_support(div().id(id).flex_none().child(button))
                    .into_any_element(),
            );
        }
        let clear = Button::new("clear-selected")
            .ghost()
            .xsmall()
            .label("Clear")
            .on_click(cx.listener(|this, _, _, cx| {
                this.task_history.clear_selection();
                cx.notify();
            }));
        actions.push(
            gpui_kit::TestSupportExt::test_support(
                div().id("history-clear-selected").flex_none().child(clear),
            )
            .into_any_element(),
        );
        actions
    }

    /// Sends the task at `ix` of `tasks_of` again, as it was sent, in `mode`
    /// when given, rather than the mode it was sent in. A Code task sent to
    /// Spec is sent with what it did (see [`code_task_of`]); a task sent from
    /// Code, resent to Spec, is told the same code task again; a Spec task
    /// sent to Code is told nothing of one. Sent in another mode, it knows the
    /// task it was sent from; resent, it keeps the one it was sent from, if
    /// any.
    fn resend_in(
        &mut self,
        tasks_of: fn(&PromptMode) -> &Vec<PromptTask>,
        ix: usize,
        mode: Option<SendMode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = tasks_of(self).get(ix) else {
            return;
        };
        let (text, sent) = (task.text.to_string(), task.sent.clone());
        // Named after it, rather than titled again.
        let named_after = Some(task.name.to_string());
        let code_task = match mode {
            Some(SendMode::Spec) if sent.mode == Some(SendMode::Code) => Some(code_task_of(task)),
            Some(_) => None,
            None => sent.code_task.clone(),
        };
        let sent_from = match mode {
            Some(_) => Some(task.name.to_string()),
            None => sent.sent_from.clone(),
        };
        // Resent, a chain or its code step goes on as it did.
        let post_build_update = mode.is_none() && sent.post_build_update;
        let mode = mode
            .or(sent.mode)
            .unwrap_or_else(|| self.chat_input.read(cx).mode());
        let code_task = code_task.filter(|_| hands_on(mode));
        if self.project_dir.is_none() {
            window.push_notification(
                Notification::error("Open a project before sending a prompt.")
                    .title("No project open"),
                cx,
            );
            return;
        }
        self.send_as(
            text,
            mode,
            sent.attached(),
            sent.sliced,
            code_task,
            sent_from,
            post_build_update,
            named_after,
            window,
            cx,
        );
    }

    /// Whether the latest task is running with a harness that can be fed
    /// more, its output shown rather than the previous tasks.
    fn can_send_to_task(&self) -> bool {
        !self.task_history.expanded
            && self.latest_task().is_some_and(|task| {
                task.status.is_active() && task.feed.as_ref().is_some_and(harness::Feed::is_open)
            })
    }

    /// Sends `text`, with the text attached to it, to the task running, as
    /// more for it to do: compiled as a hidden anchor, sliced as the Slice
    /// toggle says, without building the spec first or a system prompt, and
    /// kept only as part of the task. One that doesn't compile goes back into
    /// the chat input; one whose task is over by the time it is sent is
    /// queued as a task of its own, in `mode`.
    fn send_to_task(
        &mut self,
        text: String,
        mode: SendMode,
        attached: Attached,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project_dir) = self.project_dir.clone() else {
            window.push_notification(
                Notification::error("Open a project before sending a prompt.")
                    .title("No project open"),
                cx,
            );
            return;
        };
        let sliced = self.chat_input.read(cx).slices();
        // Only the latest task, whose output is shown, is sent to.
        let Some(feed) = self
            .latest_task()
            .and_then(|task| task.feed.clone())
            .filter(harness::Feed::is_open)
        else {
            self.enqueue(
                text, false, mode, attached, sliced, None, None, false, None, window, cx,
            );
            return;
        };
        let lsp = self.chat_input.read(cx).lsp();
        let feeding = self.feeding.clone();
        let sent = cx.background_spawn({
            let (text, attached) = (text.clone(), attached.clone());
            let project_dir = project_dir.clone();
            async move {
                let _turn = feeding.lock().await;
                // From the Freeform tab it goes just as it was typed.
                let compiled = if mode == SendMode::Freeform {
                    let mut compiled = hidden_anchor::freeform(&text, &attached.text);
                    compiled.images = hidden_anchor::image_files(&attached.images, &project_dir);
                    Ok(compiled)
                } else {
                    message_anchor(&text, attached, sliced, lsp)
                        .and_then(|anchor| hidden_anchor::preview(&anchor, &text, &project_dir))
                };
                match compiled {
                    Ok(compiled) => match feed.send(text, compiled.user_prompt, &compiled.images) {
                        Ok(()) => ToTask::Sent,
                        Err(_) => ToTask::Over,
                    },
                    Err(err) => ToTask::Failed(format!("{err:#}")),
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let sent = sent.await;
            this.update_in(cx, |this, window, cx| match sent {
                ToTask::Sent => {}
                ToTask::Over => {
                    this.in_project(&project_dir, cx, |this, cx| {
                        this.enqueue(
                            text, false, mode, attached, sliced, None, None, false, None, window,
                            cx,
                        )
                    });
                }
                ToTask::Failed(error) => {
                    window.push_notification(
                        Notification::error(error).title("Could not send to the task"),
                        cx,
                    );
                    let images = attached_image::load_saved(&attached.images, Some(&project_dir));
                    this.chat_input.update(cx, |input, cx| {
                        input.take_back(text, attached.text, images, window, cx)
                    });
                }
            })
            .ok();
        })
        .detach();
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
                    sent_from: saved.anchor.sent_from.clone(),
                    images: saved.anchor.attached_images.clone(),
                    new_conversation: saved.anchor.new_conversation == Some(true),
                    mode: anchor_mode(&saved.anchor),
                    queued_at: prompt_queue::queued_at(&saved.file).unwrap_or_default(),
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
        if self.working.any() {
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
            // Tasks sent now carry on the conversation the history left off,
            // unless it was left for a new one.
            let left = conversations::left(&project_dir, conversations::Kind::Tasks);
            let session = Session::latest(&history, &project_dir, left.as_deref());
            let tasks = history
                .into_iter()
                .map(|saved| PromptTask::restore_in(saved, Some(&project_dir)))
                .collect::<Vec<_>>();
            (tasks, session)
        });
        self._history_load = cx.spawn(async move |this, cx| {
            let (tasks, session) = load.await;
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    if this.working.any() {
                        this.history_stale = true;
                        return;
                    }
                    this.tasks = tasks;
                    let count = this.tasks.len();
                    this.task_history.selected.retain(|&ix| ix < count);
                    // The history's conversation, unless it was left for a new
                    // one here.
                    if this.session_epoch == 0 {
                        this.session = session;
                    }
                    this.task_history.open = None;
                    this.scroll_output_to_top();
                    cx.notify();
                });
            })
            .ok();
        });
    }

    /// Sends the next queued prompt of each free lane if auto send is on and
    /// the queue is not being held after a restore.
    fn auto_send_next(&mut self, cx: &mut Context<Self>) {
        if self.auto_send && !self.queue_held {
            // Each prompt sent keeps its lane busy, so this ends.
            while self.send_next(cx) {}
        }
    }

    /// Adds `text` to the end of the queue, then resolves its hidden anchor,
    /// with the system prompt of `mode`, told what `code_task` did if it was
    /// sent from one, knowing `sent_from`, the task it was sent from if sent
    /// to the other mode, and whether a chain goes on to a post-build spec
    /// update, and saves it with the project in the background.
    #[allow(clippy::too_many_arguments)]
    fn enqueue(
        &mut self,
        text: String,
        wait: bool,
        mode: SendMode,
        attached: Attached,
        sliced: bool,
        code_task: Option<CodeTask>,
        sent_from: Option<String>,
        post_build_update: bool,
        named_after: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_at(
            false,
            text,
            wait,
            mode,
            attached,
            sliced,
            code_task,
            sent_from,
            post_build_update,
            named_after,
            cx,
        );
    }

    /// Queues a prompt as [`Self::enqueue`] does, at the end of the queue, or
    /// at its `head`, ahead of every prompt waiting, as a chain's next step
    /// waits for its lane.
    #[allow(clippy::too_many_arguments)]
    fn enqueue_at(
        &mut self,
        head: bool,
        text: String,
        wait: bool,
        mode: SendMode,
        attached: Attached,
        sliced: bool,
        code_task: Option<CodeTask>,
        sent_from: Option<String>,
        post_build_update: bool,
        named_after: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        // Named as it is queued, and known by that name from then on.
        let naming = prompt_title::start(&project_dir, &text, named_after.as_deref(), self.titler);
        self.next_queue_id += 1;
        let id = self.next_queue_id;
        // The first task after New conversation, pressed with nothing
        // queued, is the one to start the new conversation.
        let new_conversation = std::mem::take(&mut self.new_conversation_pending);
        // Its place on disk is taken now, not once it has compiled, so prompts
        // queued in a row, as a batch sent to the other mode is, come back in
        // the order they were queued; at the head, before the first.
        let queued_at = match self.queue.first().filter(|_| head) {
            Some(first) => first.queued_at.saturating_sub(1),
            None => prompt_queue::stamp(),
        };
        let item = QueueItem {
            id,
            text: text.clone().into(),
            wait,
            saved: None,
            sent_from: sent_from.clone(),
            images: attached.images.clone(),
            new_conversation,
            mode: Some(mode),
            queued_at,
        };
        if head {
            self.queue.insert(0, item);
            if !self.in_background {
                self.sync_editing_position(cx);
            }
        } else {
            self.queue.push(item);
        }
        cx.notify();

        let lsp = self.chat_input.read(cx).lsp();
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let mut anchor =
                    resolve_anchor(&text, mode, attached, sliced, code_task, lsp, &project_dir)?;
                anchor.rename(naming.wait());
                anchor.sent_from = sent_from;
                anchor.post_build_update = post_build_update;
                anchor.new_conversation = Some(new_conversation);
                prompt_queue::add_at(queued_at, anchor, text, &project_dir)
            }
        });
        cx.spawn(async move |this, cx| {
            let saved = save.await;
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| this.queue_saved(id, saved, cx));
            })
            .ok();
        })
        .detach();
    }

    fn queue_saved(&mut self, id: usize, saved: Result<QueuedPrompt>, cx: &mut Context<Self>) {
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
                // Marked to start a new conversation while it was saved.
                self.keep_new_conversation(ix);
                if !self.queue[ix].wait {
                    self.auto_send_next(cx);
                }
            }
            Err(err) => {
                self.queue.remove(ix);
                if !self.in_background {
                    self.sync_editing_position(cx);
                }
                notify(
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
        // Its images are read back from the project's data, to be shown and
        // sent again.
        let images =
            attached_image::load_saved(&saved.anchor.attached_images, self.project_dir.as_deref());
        let mode = anchor_mode(&saved.anchor).unwrap_or(SendMode::Both);
        // Another edit in progress simply gives way: the chat input puts back
        // what it set aside before setting it aside again.
        self.editing_queued = Some(id);
        self.chat_input.update(cx, |input, cx| {
            input.begin_editing(ix + 1, text, mode, attached_text, images, window, cx)
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
            attached_images,
        } = edit
        else {
            self.auto_send_next(cx);
            cx.notify();
            return;
        };
        let Some(project_dir) = self
            .project_dir
            .clone()
            .filter(|_| self.queue.iter().any(|item| item.id == id))
        else {
            return;
        };
        // Its images are saved as it is, the same image once.
        let images = match attached_image::save_all(attached_images, &project_dir) {
            Ok(images) => images,
            Err(err) => {
                window.push_notification(
                    Notification::error(format!("{err:#}"))
                        .title("Could not save the queued prompt"),
                    cx,
                );
                self.auto_send_next(cx);
                cx.notify();
                return;
            }
        };
        let Some(item) = self.queue.iter_mut().find(|item| item.id == id) else {
            return;
        };
        let Some(old) = item.saved.take() else {
            return;
        };
        let old_text = std::mem::replace(&mut item.text, text.clone().into());
        let old_images = std::mem::replace(&mut item.images, images.clone());
        cx.notify();
        let lsp = self.chat_input.read(cx).lsp();
        let sliced = self.chat_input.read(cx).slices();
        let (text, mode) = (text.clone(), *mode);
        let attached = Attached {
            text: attached_text.clone(),
            images,
        };
        // Sent to the other mode, it is still sent from its task while it
        // stays in the mode it was sent to.
        let same_mode = anchor_mode(&old.anchor) == Some(mode);
        let sent_from = old.anchor.sent_from.clone().filter(|_| same_mode);
        // A chain edited in place keeps its post-build spec update; one moved
        // to the Chain tab takes the toggle's.
        let post_build_update = if same_mode {
            old.anchor.post_build_update
        } else {
            mode == SendMode::Both && self.chat_input.read(cx).post_build_update()
        };
        item.sent_from = sent_from.clone();
        // Saved in another tab, it waits for that mode's lane.
        let old_mode = item.mode.replace(mode);
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                // Sent to Spec from a Code task, it is still told what that
                // did while it stays in Spec, as a chain's code step is told
                // what its spec step did while it stays in Code.
                let code_task = old.anchor.code_task.clone().filter(|_| same_mode);
                match resolve_anchor(&text, mode, attached, sliced, code_task, lsp, &project_dir) {
                    Ok(mut anchor) => {
                        anchor.sent_from = sent_from;
                        anchor.post_build_update = post_build_update;
                        anchor.new_conversation = old.anchor.new_conversation;
                        prompt_queue::replace(old.file.clone(), anchor, text)
                            .map_err(|err| (old, err))
                    }
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
                        Ok(saved) => {
                            item.saved = Some(saved);
                            // Marked to start a new conversation while it
                            // saved.
                            if let Some(ix) = this.queue.iter().position(|item| item.id == id) {
                                this.keep_new_conversation(ix);
                            }
                        }
                        Err((old, err)) => {
                            // It stays as it was.
                            item.text = old_text;
                            item.images = old_images;
                            item.sent_from = old.anchor.sent_from.clone();
                            item.mode = old_mode;
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

    /// The place in the queue of the next prompt that can be sent: the first
    /// whose lane is free, as the QueueListScope says. Each lane sends its
    /// own prompts first to last, so a prompt that must wait, for its lane,
    /// for being saved, or for being edited, holds back those of its lane
    /// after it; a Freeform prompt, needing both lanes, holds back both.
    fn next_sendable(&self) -> Option<usize> {
        // Without a project it could not be sent, and must stay queued.
        self.project_dir.as_ref()?;
        let mut held = self.working;
        for (ix, item) in self.queue.iter().enumerate() {
            let lanes = Lanes::of(item.mode);
            let ready = item.saved.is_some()
                && (self.in_background || Some(item.id) != self.editing_queued);
            if ready && !held.overlaps(lanes) {
                return Some(ix);
            }
            held = held.with(lanes, true);
            if held == Lanes::ALL {
                return None;
            }
        }
        None
    }

    /// Sends the first queued prompt whose lane is free, once it is saved.
    /// Returns whether it sent one.
    fn send_next(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(ix) = self.next_sendable() else {
            return false;
        };
        let item = self.queue.remove(ix);
        if self.queue.is_empty() {
            self.queue_held = false;
        }
        let Some(saved) = item.saved else {
            return false;
        };
        if !self.in_background {
            self.sync_editing_position(cx);
        }
        self.start(item.text.to_string(), Sending::Queued(saved), cx);
        true
    }

    /// "Send next" was clicked: sends the first queued prompt, and releases a
    /// restored queue to auto send.
    fn send_next_clicked(&mut self, cx: &mut Context<Self>) {
        self.queue_held = false;
        self.send_next(cx);
    }

    /// Toggles whether the queued prompt `id` starts a new conversation rather
    /// than carrying on the tasks', saving it with the project.
    fn toggle_queued_new_conversation(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(ix) = self.queue.iter().position(|item| item.id == id) else {
            return;
        };
        self.queue[ix].new_conversation = !self.queue[ix].new_conversation;
        self.keep_new_conversation(ix);
        cx.notify();
    }

    /// Moves the queued prompt `id` to place `to` in the queue, as dropping
    /// it there does, walking it a neighbour at a time so each step renames
    /// just two files. It moves only once it and every prompt it passes are
    /// saved; a step that can't be saved puts back those already taken.
    fn move_queued(&mut self, id: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.queue.iter().position(|item| item.id == id) else {
            return;
        };
        if to == from
            || to >= self.queue.len()
            || !self.queue[from.min(to)..=from.max(to)]
                .iter()
                .all(|item| item.saved.is_some())
        {
            return;
        }
        let toward = |at: usize, end: usize| if end > at { at + 1 } else { at - 1 };
        let mut at = from;
        while at != to {
            let next = toward(at, to);
            if let Err(err) = self.swap_queued(at, next) {
                while at != from {
                    let back = toward(at, from);
                    self.swap_queued(at, back).ok();
                    at = back;
                }
                window.push_notification(
                    Notification::error(format!("{err:#}")).title("Could not move the prompt"),
                    cx,
                );
                cx.notify();
                return;
            }
            at = next;
        }
        self.sync_editing_position(cx);
        cx.notify();
    }

    /// Trades the saved queued prompts at `a` and `b` places, files and all.
    fn swap_queued(&mut self, a: usize, b: usize) -> Result<()> {
        let (low, high) = (a.min(b), a.max(b));
        let (head, tail) = self.queue.split_at_mut(high);
        let (Some(a), Some(b)) = (head[low].saved.as_mut(), tail[0].saved.as_mut()) else {
            anyhow::bail!("the prompt is still being saved");
        };
        prompt_queue::swap(a, b)?;
        self.queue.swap(low, high);
        // Each place keeps its mark, as the files traded theirs.
        let (first, second) = (self.queue[low].queued_at, self.queue[high].queued_at);
        self.queue[low].queued_at = second;
        self.queue[high].queued_at = first;
        Ok(())
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
        } else if item.new_conversation {
            // The new conversation passes to the task that now runs next.
            self.mark_new_conversation();
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
            self.auto_send_next(cx);
        }
        cx.notify();
    }

    /// Starts a new conversation for the selected tab, the questions' on the
    /// Ask tab and the tasks' on any other, so its next run starts fresh
    /// rather than carrying on. The conversation left is kept with the
    /// project, so reopening it doesn't carry it on either. A run of it under
    /// way finishes in the conversation left, which is kept as left once the
    /// run says which it is, if that wasn't known yet. Does nothing while
    /// there is nothing to carry on and no run of it is under way.
    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        if self.context().is_none() && !self.conversation_running() {
            return;
        }
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        let (left, kind) = if self.on_ask_tab {
            self.ask_session_epoch += 1;
            // The Ask conversation marks at once that the next question
            // starts a new one.
            self.ask_new_pending = true;
            (self.ask_session.take(), conversations::Kind::Questions)
        } else {
            self.session_epoch += 1;
            self.mark_new_conversation();
            (self.session.take(), conversations::Kind::Tasks)
        };
        if let Some(left) = left {
            Self::keep_left(project_dir, kind, left.id, cx);
        }
        cx.notify();
    }

    /// Makes the next task to run start a new conversation: the first in the
    /// queue, whose hidden anchor then records so, or with nothing queued,
    /// the next task sent or queued.
    fn mark_new_conversation(&mut self) {
        if self.queue.is_empty() {
            self.new_conversation_pending = true;
            return;
        }
        self.queue[0].new_conversation = true;
        self.keep_new_conversation(0);
    }

    /// Saves the queued prompt at `ix` again if whether it starts a new
    /// conversation changed since its hidden anchor was saved.
    fn keep_new_conversation(&mut self, ix: usize) {
        let item = &mut self.queue[ix];
        let new_conversation = item.new_conversation;
        let Some(saved) = item.saved.as_mut() else {
            return;
        };
        if saved.anchor.new_conversation.unwrap_or(false) == new_conversation {
            return;
        }
        saved.anchor.new_conversation = Some(new_conversation);
        // Unsaved, it still starts the new conversation while the
        // application runs.
        if let Err(err) = prompt_queue::rewrite(saved) {
            eprintln!("could not keep that the queued prompt starts a new conversation: {err:#}");
        }
    }

    /// Keeps with the project, in the background, that the conversation `id`
    /// of `kind` was left for a new one.
    fn keep_left(project_dir: PathBuf, kind: conversations::Kind, id: String, cx: &mut App) {
        cx.background_spawn(async move {
            if let Err(err) = conversations::leave(&project_dir, kind, &id) {
                eprintln!("could not keep the conversation left: {err:#}");
            }
        })
        .detach();
    }

    /// Whether the referenced spec sidebar should show: while a task from the
    /// Code, Chain, or Spec tab runs, once it has anything to show. A
    /// Freeform prompt references nothing.
    fn refs_wanted(&self) -> bool {
        self.latest_task().is_some_and(|task| {
            self.working.any()
                && task.status.is_active()
                && !matches!(task.mode, Some(SendMode::Ask | SendMode::Freeform))
        })
            // It waits for something to show, and once out stays out until
            // the run is over.
            && (self.refs_shown || self.refs_have_contents())
    }

    /// Whether the running task has anything for the sidebar to show: a
    /// spec file referenced, a constraint understood, or a subagent started.
    fn refs_have_contents(&self) -> bool {
        self.latest_task().is_some_and(|task| {
            !task.understanding.rows.is_empty() || !task.subagents.list.is_empty()
        }) || !self.referenced_files().is_empty()
    }

    /// The spec files the running task references.
    fn referenced_files(&self) -> Vec<referenced_spec::Referenced> {
        let (Some(project_dir), Some(task)) = (self.project_dir.as_deref(), self.latest_task())
        else {
            return Vec::new();
        };
        referenced_spec::collect(
            &task.imported,
            &task.references,
            project_dir,
            task.spec_dir.as_deref(),
        )
    }

    /// The understanding of the running task, as last read.
    /// The subagents the running task started, shown by the mode that
    /// started them. While a chain runs, those of each of its steps so far,
    /// a group per step labelled by what it did, in that step's own mode:
    /// the spec step and follow-up Spec's, the code step Code's.
    fn running_subagents(&self, cx: &App) -> Vec<referenced_spec::SubagentGroup> {
        let Some(latest) = self.latest_ix() else {
            return Vec::new();
        };
        let group = |ix: usize, step: Option<ChainStep>| {
            let task = &self.tasks[ix];
            let (label, mode, started_by) = match step {
                Some(step) => {
                    let (label, mode) = chain_step_label(step.pos);
                    (Some(label.into()), Some(mode), format!("the {label} step"))
                }
                None => (
                    None,
                    task.mode,
                    task.mode.map_or("the task".to_string(), |mode| {
                        format!("the {} task", mode.label())
                    }),
                ),
            };
            referenced_spec::SubagentGroup {
                label,
                started_by: started_by.into(),
                color: mode.map(|mode| chat_input::mode_color(mode, cx)),
                agents: task.subagents.clone(),
            }
        };
        let layout = chain_layout(&self.tasks);
        match layout.step_of(latest) {
            Some(step) => layout.members(step)[..=step.pos]
                .iter()
                .enumerate()
                .map(|(pos, &ix)| group(ix, Some(ChainStep { pos, ..step })))
                .collect(),
            None => vec![group(latest, None)],
        }
    }

    fn running_understanding(&self) -> Understanding {
        self.tasks
            .last()
            .map(|task| task.understanding.clone())
            .unwrap_or_default()
    }

    /// Follows the understanding `file` of the task at `ix` while it runs,
    /// reading it a few times a second off the UI thread, and showing it
    /// again whenever what it says, or whether its links' files exist,
    /// changes.
    fn watch_understanding(
        ix: usize,
        file: PathBuf,
        project_dir: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut last: Option<Option<Vec<understanding::Read>>> = None;
            loop {
                let read = cx
                    .background_spawn({
                        let (file, project_dir) = (file.clone(), project_dir.clone());
                        async move { understanding::read(&file, &project_dir) }
                    })
                    .await;
                let fresh = last.as_ref() != Some(&read);
                last = Some(read.clone());
                let running = this
                    .update(cx, |this, cx| {
                        this.in_project(&project_dir, cx, |this, cx| {
                            let Some(task) = this.tasks.get_mut(ix) else {
                                return false;
                            };
                            if fresh && task.understanding.update(read) {
                                cx.notify();
                            }
                            task.status.is_active()
                        })
                    })
                    .ok()
                    .flatten()
                    .unwrap_or(false);
                if !running {
                    break;
                }
                cx.background_executor()
                    .timer(understanding::POLL_INTERVAL)
                    .await;
            }
        })
    }

    /// The task view, with the chat input beneath it, and the referenced spec
    /// sidebar at the right of both while a task runs: sliding out from behind its right edge as the task starts,
    /// over the task view, which keeps its width until the slide settles; then
    /// a split that can be dragged; and sliding back once the run is over.
    fn with_referenced_spec(
        &mut self,
        history: Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let shown = self.refs_wanted();
        if shown != self.refs_shown {
            self.refs_shown = shown;
            let slide = self.refs_opened.map_or(0, |(n, _)| n);
            if shown {
                self.refs_opened = Some((slide + 1, Instant::now()));
                self.refs_closing = None;
                // Opens at the width last dragged to.
                self.refs_split = cx.new(|_| ResizableState::default());
            } else {
                self.refs_closing = Some(RefsClosing {
                    files: self.referenced_files(),
                    subagents: self.running_subagents(cx),
                    understanding: self.running_understanding(),
                    width: self.refs_width.get(),
                    slide,
                    closed: Instant::now(),
                });
            }
        }
        if self
            .refs_closing
            .as_ref()
            .is_some_and(|closing| closing.closed.elapsed() >= PANE_SLIDE_TIME)
        {
            self.refs_closing = None;
        }
        let host = div().relative().size_full();
        let contents = |files: &[referenced_spec::Referenced],
                        subagents: &[referenced_spec::SubagentGroup],
                        understanding: &Understanding,
                        this: &Self,
                        cx: &mut Context<Self>| {
            referenced_spec::render(
                files,
                subagents,
                understanding,
                referenced_spec::Layout {
                    files_scroll: &this.refs_scroll,
                    subagents_scroll: &this.subagents_scroll,
                    understanding_scroll: &this.understanding_scroll,
                    heights: &this.refs_panels,
                },
                this.file_opener(cx),
                cx,
            )
        };
        let sliding = self
            .refs_opened
            .filter(|(_, opened)| opened.elapsed() < PANE_SLIDE_TIME)
            .map(|(n, _)| n);
        if shown {
            let files = self.referenced_files();
            let subagents = self.running_subagents(cx);
            let understanding = self.running_understanding();
            // Rows just added or changed fade back.
            if understanding.fading() {
                window.request_animation_frame();
            }
            let width = self.refs_width.get();
            if let Some(n) = sliding {
                window.request_animation_frame();
                let grow = SpringAnimation::new(PANE_SPRING).to(width).from(px(0.));
                let pane = slide_pane(
                    ("refs-slide", n),
                    ("refs-grow", n),
                    grow,
                    width,
                    contents(&files, &subagents, &understanding, self, cx),
                );
                return host.child(pushed_by(history, pane, cx));
            }
            let refs_width = self.refs_width.clone();
            let sidebar = div()
                .size_full()
                .on_prepaint(move |bounds, _, _| refs_width.set(bounds.size.width))
                .child(contents(&files, &subagents, &understanding, self, cx));
            return host.child(
                h_resizable("refs-split")
                    .with_state(&self.refs_split)
                    .with_handle_appearance(crate::hit_areas::resize_edges("refs-split"))
                    .children([
                        resizable_panel()
                            .size_range(MIN_SPLIT_WIDTH..Pixels::MAX)
                            .child(history),
                        resizable_panel()
                            .size(width)
                            .size_range(referenced_spec::MIN_WIDTH..Pixels::MAX)
                            .child(sidebar),
                    ]),
            );
        }
        if let Some(closing) = &self.refs_closing {
            window.request_animation_frame();
            let (width, n) = (closing.width, closing.slide);
            let shrink = SpringAnimation::new(PANE_SPRING).to(px(0.)).from(width);
            let files = closing.files.clone();
            let subagents = closing.subagents.clone();
            let understanding = closing.understanding.clone();
            let pane = slide_pane(
                ("refs-slide-out", n),
                ("refs-shrink", n),
                shrink,
                width,
                contents(&files, &subagents, &understanding, self, cx),
            );
            return host.child(pushed_by(history, pane, cx));
        }
        host.child(history)
    }

    /// Whether any run the Chat tab shows is under way: a task on Code,
    /// Chain, or Spec, or a question on Ask.
    fn chat_running(&self) -> bool {
        self.working.any() || self.asks.iter().any(|ask| ask.task.status.is_active())
    }

    /// Whether a run of the selected tab's conversation is under way: a task
    /// on Code, Chain, or Spec, any question on Ask.
    fn conversation_running(&self) -> bool {
        if self.on_ask_tab {
            self.asks.iter().any(|ask| ask.task.status.is_active())
        } else {
            self.working.any()
        }
    }

    /// How much context the selected tab's next prompt carries on with:
    /// `None` when it starts a new conversation, and none that the project's
    /// runs know of counts as nothing yet.
    fn context(&self) -> Option<u64> {
        let session = if self.on_ask_tab {
            &self.ask_session
        } else {
            &self.session
        };
        let project_dir = self.project_dir.as_deref()?;
        session
            .as_ref()
            .filter(|session| session.project_dir == project_dir)
            .map(|session| session.context.unwrap_or(0))
    }

    /// Follows what run `run` of the project on hand, of `agent`, reports
    /// of its usage.
    fn follow_usage(
        &mut self,
        run: Option<usize>,
        agent: crate::agent::Agent,
        event: &HarnessEvent,
    ) {
        let now = usage::now();
        if let Some(run) = run {
            self.usage.follow(run, agent, event, now);
        }
        self.limits.follow(agent, event, now);
    }

    /// The agent's usage as the chat input shows it: the plan limits of the
    /// harness picked, the selected tab's conversation, and the project.
    fn usage_report(&self) -> UsageReport {
        let now = usage::now();
        let agent = crate::agent::current();
        let (conversation, epoch, session) = if self.on_ask_tab {
            (
                Conversation::Questions,
                self.ask_session_epoch,
                &self.ask_session,
            )
        } else {
            (Conversation::Tasks, self.session_epoch, &self.session)
        };
        let (limits, limits_reported) = self.limits.of(agent, now);
        let project_dir = self.project_dir.as_deref();
        let context = session
            .as_ref()
            .filter(|session| Some(session.project_dir.as_path()) == project_dir)
            .and_then(|session| session.context);
        let (mut harness, mut model, mut reported) = self.usage.source();
        // Limits reported since the project's own figures say where they came
        // from.
        if limits_reported > reported {
            if harness != Some(agent) {
                model = None;
            }
            harness = Some(agent);
            reported = limits_reported;
        }
        UsageReport {
            limits,
            conversation: Some(conversation),
            context,
            conversation_spend: self.usage.conversation(conversation, epoch),
            project_spend: self.usage.project(),
            runs: self.usage.runs(),
            harness,
            model,
            reported,
        }
    }

    #[cfg(test)]
    pub fn output_locked(&self) -> bool {
        self.output_locked
    }

    /// Locks the latest task's output to the bottom, or unlocks it.
    pub(crate) fn set_output_lock(&mut self, locked: bool, cx: &mut Context<Self>) {
        if self.output_locked == locked {
            return;
        }
        self.output_locked = locked;
        if self.output_locked {
            self.output_table.scroll_to_end();
        }
        cx.notify();
    }

    /// Replaces the saved questions with those saved in the current project,
    /// read in the background. Questions asked now carry on the
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
            let left = conversations::left(&project_dir, conversations::Kind::Questions);
            let session = Session::latest(&saved, &project_dir, left.as_deref());
            let answers = saved
                .into_iter()
                .map(|saved| PromptTask::restore_in(saved, Some(&project_dir)))
                .collect::<Vec<_>>();
            (answers, session)
        });
        self._ask_history_load = cx.spawn(async move |this, cx| {
            let (answers, session) = load.await;
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    // Those asked since are listed as they were asked.
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
                    if this.ask_session.is_none() && this.ask_session_epoch == 0 {
                        this.ask_session = session;
                    }
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
        // Sent, the Freeform chat goes to its bottom to follow it.
        self.lock_freeform_chat();
        self.tasks[task_ix].sent = match &sending {
            Sending::Now(mode, attached, sliced, code_task, sent_from, post_build_update, _) => {
                SentAs {
                    mode: Some(*mode),
                    attached_text: attached.text.clone(),
                    attached_images: attached.images.clone(),
                    // A Freeform prompt is never sliced, whatever the toggle says.
                    sliced: *sliced && *mode != SendMode::Freeform,
                    code_task: code_task.clone().filter(|_| hands_on(*mode)),
                    sent_from: sent_from.clone(),
                    post_build_update: *post_build_update,
                }
            }
            Sending::Queued(queued) => SentAs::of(&queued.anchor),
        };
        // Known by its anchor's name from now on, before the anchor is
        // resolved, so a task sent to the other mode from it knows it by that.
        // One sent now is named once its title comes, while the spec builds,
        // and until then by the random name it would be given without one.
        let naming = match &sending {
            Sending::Queued(queued) => {
                self.tasks[task_ix].name = queued.anchor.name().to_string().into();
                None
            }
            Sending::Now(.., named_after) => {
                let naming =
                    prompt_title::start(&project_dir, &text, named_after.as_deref(), self.titler);
                Some(cx.background_spawn(async move { naming.wait() }))
            }
        };
        let known = self.tasks[task_ix].name.to_string();
        self.tasks[task_ix].mode = self.tasks[task_ix].sent.mode;
        // It runs in its own lane, beside a task of the other lane if one is
        // running, as the PromptSendingScope says.
        let lanes = Lanes::of(self.tasks[task_ix].mode);
        let beside = self.working.any();
        self.working = self.working.with(lanes, true);
        if !self.in_background {
            let working = self.working;
            self.chat_input
                .update(cx, |input, cx| input.set_busy(working, cx));
        }

        // Whether it starts a new conversation is part of the prompt: a queued
        // one's anchor says, and one sent now is the first after New
        // conversation, if nothing else has been since.
        let new_conversation = match &sending {
            Sending::Queued(queued) => queued.anchor.new_conversation == Some(true),
            Sending::Now(..) => std::mem::take(&mut self.new_conversation_pending),
        };
        let resume = Session::resume(&self.session, &project_dir).filter(|_| !new_conversation);
        let new_conversation = resume.is_none();
        let epoch = self.session_epoch;
        let lsp = self.chat_input.read(cx).lsp();
        // A prompt that works on the code or the spec is sent against a
        // freshly built spec. A Freeform prompt is a task too, but is sent
        // just as it was typed: no build, compile, slices, system prompt, or
        // understanding file.
        let freeform = self.tasks[task_ix].mode == Some(SendMode::Freeform);
        let is_task = self.tasks[task_ix]
            .mode
            .is_none_or(|mode| mode != SendMode::Ask);
        let builds = is_task && !freeform;
        // Code may not change the spec, nor Spec the code.
        let guarded = self.tasks[task_ix].mode;
        if builds {
            self.tasks[task_ix].status = TaskStatus::Building;
        }
        // What the task asked, for its commit note.
        let asked = text.clone();
        let summarize = self.summarize;
        // Its Cancel button ends it wherever it is: see `cancel_task`.
        let cancelled = Arc::new(AtomicBool::new(false));
        let (signal, on_cancel) = oneshot::channel();
        self.tasks[task_ix].cancel = Some(Cancel {
            cancelled: cancelled.clone(),
            signal: Some(signal),
            stop: None,
        });
        let on_cancel = on_cancel.shared();
        // Resolves once the task is cancelled, and never otherwise.
        let until_cancelled = move || {
            let on_cancel = on_cancel.clone();
            async move {
                if on_cancel.await.is_err() {
                    std::future::pending::<()>().await
                }
            }
        };
        let is_cancelled = {
            let cancelled = cancelled.clone();
            move || cancelled.load(Ordering::SeqCst)
        };
        let build = builds.then(|| {
            let project_dir = project_dir.clone();
            cx.background_spawn(async move {
                // What the build writes is kept by the guard of a task
                // running beside it.
                let _writing = mode_guard::Writing::start(None, &project_dir);
                piton_build::build(&project_dir)
            })
        });
        // The system prompt it is sent with: the project's, the same for
        // every prompt of the conversation whatever the mode, so the
        // harness's prompt cache holds. A Freeform prompt keeps the one the
        // conversation it carries on was sent, and one starting a
        // conversation has none.
        let conversation_system = resume
            .as_ref()
            .and_then(|_| self.session.as_ref()?.system_prompt.clone());
        let system = {
            let project_dir = project_dir.clone();
            cx.background_spawn(async move {
                if freeform {
                    return conversation_system;
                }
                hidden_anchor::project_system_prompt(&project_dir)
                    .ok()
                    .flatten()
            })
        };
        let compile = {
            let project_dir = project_dir.clone();
            move |name: String| async move {
                let anchor = match sending {
                    // Out of the queue first, so it is never sent twice.
                    Sending::Queued(queued) => {
                        prompt_queue::remove(&queued.file)?;
                        let mut anchor = queued.anchor;
                        // Its instructions as its mode's template now gives
                        // them, not as they were queued: one queued before
                        // they were sent apart from the system prompt would
                        // otherwise send the fluency with it.
                        if let Some(mode) = anchor.mode.filter(|mode| *mode != SendMode::Freeform) {
                            anchor.system_prompt = hidden_anchor::instructions_for(
                                mode,
                                anchor.code_task.is_some(),
                                &project_dir,
                            )?;
                        }
                        anchor
                    }
                    Sending::Now(
                        mode,
                        attached,
                        sliced,
                        code_task,
                        sent_from,
                        post_build_update,
                        _,
                    ) => {
                        let mut anchor = resolve_anchor(
                            &text,
                            mode,
                            attached,
                            sliced,
                            code_task,
                            lsp,
                            &project_dir,
                        )?;
                        anchor.rename(name);
                        anchor.sent_from = sent_from;
                        anchor.post_build_update = post_build_update;
                        anchor
                    }
                };
                // The history records whether it started a new conversation.
                let mut anchor = anchor;
                anchor.new_conversation = Some(new_conversation);
                let file = hidden_anchor::save(&anchor, &text, &project_dir)?;
                // Cancelled by now, it is saved to the history as cancelled,
                // but never compiled, nor run.
                if cancelled.load(Ordering::SeqCst) {
                    return Ok((anchor.name().to_string(), file, None));
                }
                if freeform {
                    let mut compiled = hidden_anchor::freeform(&text, &anchor.attached_text);
                    compiled.images =
                        hidden_anchor::image_files(&anchor.attached_images, &project_dir);
                    return anyhow::Ok((
                        anchor.name().to_string(),
                        file,
                        Some((Ok(compiled), (Vec::new(), None))),
                    ));
                }
                let compiled = hidden_anchor::compile(&anchor, &file, &project_dir);
                let imported = (
                    hidden_anchor::spec_files(&anchor.imports, &project_dir),
                    hidden_anchor::spec_dir(&project_dir),
                );
                anyhow::Ok((anchor.name().to_string(), file, Some((compiled, imported))))
            }
        };

        self._pending[lane_slot(lanes)] = cx.spawn(async move |this, cx| {
            // A build cancelled is abandoned: the task goes on without it.
            let build = match build {
                Some(build) => {
                    match futures::future::select(build, std::pin::pin!(until_cancelled())).await
                    {
                        futures::future::Either::Left((built, _)) => Some(built),
                        futures::future::Either::Right(_) => None,
                    }
                }
                None => None,
            };
            if let Some(built) = build {
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
            // Named by now, unless it was cancelled first, when it keeps
            // the random name it was known by.
            let name = match naming {
                Some(naming) => {
                    match futures::future::select(naming, std::pin::pin!(until_cancelled()))
                        .await
                    {
                        futures::future::Either::Left((name, _)) => Some(name),
                        futures::future::Either::Right(_) => None,
                    }
                }
                None => None,
            };
            let name = name.unwrap_or(known);
            let named = this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    if let Some(task) = this.tasks.get_mut(task_ix) {
                        task.name = name.clone().into();
                    }
                    cx.notify();
                });
            });
            if named.is_err() {
                return;
            }
            let compile = cx.background_spawn(compile(name));
            // What came of the prompt, saved beside it in the history once it
            // is over.
            let mut record = RunRecord::default();
            let mut prompt_file = None;
            // The harness's final summary, once the run finished without error.
            let mut finished: Option<String> = None;
            let compiled = compile.await.and_then(|(anchor, file, compiled)| {
                prompt_file = Some(file);
                Ok(match compiled {
                    Some((compiled, imported)) => Some((anchor, compiled?, imported)),
                    None => None,
                })
            });
            // Cancelled before it could run, it never runs.
            let compiled = match compiled {
                Ok(Some(compiled)) if !is_cancelled() => Ok(Some(compiled)),
                Ok(_) => Ok(None),
                Err(_) if is_cancelled() => Ok(None),
                Err(err) => Err(err),
            };
            match compiled {
                Ok(None) => {}
                Ok(Some((anchor, compiled, imported))) => {
                    let prompt = compiled.user_prompt.clone();
                    record.user_prompt = Some(prompt.clone());
                    // A Code, Chain, or Spec task keeps its understanding
                    // beside its history record; a question has none.
                    let understanding_file = prompt_file
                        .as_deref()
                        .filter(|_| builds)
                        .map(understanding::path);
                    let shown_path = understanding_file.as_deref().map(|file| {
                        file.strip_prefix(&project_dir)
                            .unwrap_or(file)
                            .display()
                            .to_string()
                    });
                    // Its instructions head its message, its understanding
                    // file filled in, then, for one sent from Code, what the
                    // code task did; the system prompt is the project's.
                    let instructions = compiled.instructions_as_sent(shown_path.as_deref());
                    let message =
                        system_prompts::with_instructions(instructions.as_deref(), &prompt);
                    let sent_system = system.await;
                    // The harness the run goes to, which its usage came from.
                    let agent = crate::agent::current();
                    // What else the harness is given, kept for the raw
                    // prompt, with the task and in its record.
                    record.sent_to(agent, sent_system.as_deref());
                    record.instructions = instructions.clone();
                    record.resumed = resume.is_some();
                    let given = Given {
                        harness: agent,
                        system_prompt: sent_system.clone(),
                        instructions,
                        resumed: resume.is_some(),
                    };
                    let mut usage_run = None;
                    // Whatever the harness does, what the task's mode may
                    // not change is put back.
                    let guard = match guarded {
                        Some(mode) => {
                            let dir = project_dir.clone();
                            cx.background_spawn(async move { mode_guard::Guard::start(mode, &dir) })
                                .await
                        }
                        None => None,
                    };
                    // Told, too, not to read the spec's source, as a Code
                    // task, so it reads the compiled reference instead.
                    let unread = guarded.and_then(|mode| {
                        mode_guard::unread(mode, &crate::project_tree::Locations::read(&project_dir))
                    });
                    let protected = harness::Protected {
                        root: guard.as_ref().map(mode_guard::Guard::root),
                        unread,
                    };
                    // What it may change, the guard of a task running beside
                    // it keeps rather than putting back.
                    let writing = {
                        let dir = project_dir.clone();
                        cx.background_spawn(async move {
                            mode_guard::Writing::start(guarded, &dir)
                        })
                        .await
                    };
                    // In a git repository, the working tree as the harness
                    // starts the task, to compare with how its run leaves it.
                    let snapshot_name = anchor.clone();
                    let snapshot_before = {
                        let (dir, name) = (project_dir.clone(), snapshot_name.clone());
                        cx.background_spawn(async move {
                            task_snapshot::repo_top(&dir)?;
                            task_snapshot::take(&dir, &name, "before").ok()
                        })
                        .await
                    };
                    record.snapshot_before = snapshot_before.clone();
                    let harness::Run {
                        mut events,
                        feed,
                        stop,
                    } = harness::send_task(
                        message,
                        sent_system.clone(),
                        compiled.images.clone(),
                        // Beside a task of the other lane, it carries on a copy
                        // of the conversation, so neither writes over the other.
                        resume.clone().map(|session| harness::Resume {
                            session,
                            fork: beside,
                        }),
                        project_dir.clone(),
                        protected,
                    );
                    if this
                        .update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, cx| {
                                usage_run =
                                    Some(this.usage.start_run(Conversation::Tasks, epoch));
                                // More can be sent to it while it runs.
                                if let Some(task) = this.tasks.get_mut(task_ix) {
                                    task.feed = feed;
                                }
                                // Cancelled, its run is stopped.
                                if let Some(cancel) = this
                                    .tasks
                                    .get_mut(task_ix)
                                    .and_then(|task| task.cancel.as_mut())
                                {
                                    cancel.stop = Some(stop.clone());
                                }
                                // Cancelled meanwhile, it stops at once.
                                if is_cancelled() {
                                    stop.stop();
                                }
                                if let Some(file) = understanding_file {
                                    let watch = Self::watch_understanding(
                                        task_ix,
                                        file,
                                        project_dir.clone(),
                                        cx,
                                    );
                                    if let Some(task) = this.tasks.get_mut(task_ix) {
                                        task._understanding_watch = watch;
                                    }
                                }
                                if let Some(task) = this.tasks.get_mut(task_ix) {
                                    (task.imported, task.spec_dir) = imported;
                                    task.given = Some(given);
                                }
                                this.show_compiled(task_ix, anchor, prompt, cx)
                            });
                        })
                        .is_err()
                    {
                        return;
                    }
                    let mut started = false;
                    // The conversation this run reported, and what it held as
                    // of its latest reply.
                    let mut run_session = None;
                    let mut run_context = None;
                    loop {
                        // What the harness printed before it was cancelled is
                        // taken first, and kept; then, cancelled, the run is
                        // over, not waiting on the harness.
                        let event = match futures::future::select(
                            events.next(),
                            std::pin::pin!(until_cancelled()),
                        )
                        .await
                        {
                            futures::future::Either::Left((Some(event), _)) => event,
                            futures::future::Either::Left((None, _))
                            | futures::future::Either::Right(_) => break,
                        };
                        // Nothing it says after it was cancelled ends it
                        // otherwise.
                        if is_cancelled()
                            && matches!(
                                event,
                                HarnessEvent::Failed(_)
                                    | HarnessEvent::Finished { .. }
                                    | HarnessEvent::Answered { .. }
                            )
                        {
                            continue;
                        }
                        record.note(&event);
                        if let HarnessEvent::Usage { context } = &event {
                            run_context = Some(*context);
                        }
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
                                    if let HarnessEvent::Session(_) = &event {
                                        started = true;
                                    }
                                    this.follow_usage(usage_run, agent, &event);
                                    if let Some(left) = Session::follow(
                                        &mut this.session,
                                        this.session_epoch == epoch,
                                        &mut run_session,
                                        &event,
                                        &project_dir,
                                        sent_system.as_deref(),
                                    ) {
                                        Self::keep_left(
                                            project_dir.clone(),
                                            conversations::Kind::Tasks,
                                            left,
                                            cx,
                                        );
                                    }
                                    this.apply_event(task_ix, event, cx)
                                });
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(guard) = guard {
                        let what = guard.what;
                        let put_back = cx.background_spawn(async move { guard.finish() }).await;
                        if !put_back.is_empty() {
                            let files = put_back
                                .iter()
                                .map(|file| format!("- {}", file.display()))
                                .collect::<Vec<_>>()
                                .join("\n");
                            let mode = guarded.map_or("", SendMode::label);
                            // Passive: it fails nothing, and opens onto
                            // the files put back.
                            let summary = format!(
                                "Put back {} {what} {} this {mode} task changed",
                                put_back.len(),
                                if put_back.len() == 1 { "file" } else { "files" },
                            );
                            let details = format!(
                                "A {mode} task may not change the {what}, so what it changed there was put back as it was:\n{files}"
                            );
                            this.update(cx, |this, cx| {
                                this.in_project(&project_dir, cx, |this, cx| {
                                    if let Some(task) = this.tasks.get_mut(task_ix) {
                                        task.reply.push_notice(summary, details);
                                    }
                                    cx.notify();
                                });
                            })
                            .ok();
                        }
                    }
                    // What it last changed is kept by the guards beside it.
                    cx.background_spawn(async move { drop(writing) }).await;
                    // And as its run leaves it, whatever was put back, with
                    // the files that changed between the two.
                    if let Some(before) = snapshot_before {
                        let edited = this
                            .update(cx, |this, cx| {
                                this.in_project(&project_dir, cx, |this, _| {
                                    this.tasks
                                        .get(task_ix)
                                        .map(|task| task.references.edited_paths())
                                })
                            })
                            .ok()
                            .flatten()
                            .flatten()
                            .unwrap_or_default();
                        let dir = project_dir.clone();
                        let (after, changed) = cx
                            .background_spawn(async move {
                                let after =
                                    task_snapshot::take(&dir, &snapshot_name, "after").ok();
                                let changed = after.clone().and_then(|after| {
                                    ChangedFiles::read(
                                        task_snapshot::repo_top(&dir)?,
                                        before,
                                        after,
                                        &edited,
                                    )
                                });
                                (after, changed)
                            })
                            .await;
                        record.snapshot_after = after;
                        this.update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, cx| {
                                if let Some(task) = this.tasks.get_mut(task_ix) {
                                    task.changed = changed;
                                }
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                    // Cancelled, the conversation it carried on is still carried
                    // on by the next task, however far it got.
                    if let Some(resume) = resume.filter(|_| !started && !is_cancelled()) {
                        this.update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, _| {
                                Session::forget(&mut this.session, &resume)
                            });
                        })
                        .ok();
                    }
                    // The tasks carry on the conversation of whichever task
                    // finished last, as the HarnessIntegrationScope says, as
                    // tasks of both lanes may have run at once.
                    if let Some(id) = run_session.clone() {
                        this.update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, _| {
                                if this.session_epoch == epoch {
                                    Session::finished(
                                        &mut this.session,
                                        id,
                                        run_context,
                                        &project_dir,
                                        sent_system.as_deref(),
                                    );
                                }
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

            record.cancelled = is_cancelled();
            // Marked done by hand while it ran, it is saved so; marked or
            // unmarked while this saves, it is saved again below.
            record.marked_done = this
                .update(cx, |this, cx| {
                    this.in_project(&project_dir, cx, |this, _| {
                        this.tasks
                            .get(task_ix)
                            .is_some_and(|task| task.marked_done)
                    })
                })
                .ok()
                .flatten()
                .unwrap_or(false);
            let marked_done = record.marked_done;
            let record_file = prompt_file.clone();
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
                    // Nothing more can be sent to it; a message on its way is
                    // queued as a task instead.
                    task.feed = None;
                    task.cancel = None;
                    task.end();
                    if task.marked_done != marked_done
                        && saved.is_ok()
                        && let Some(file) = record_file
                    {
                        let marked_done = task.marked_done;
                        cx.background_spawn(async move {
                            prompt_history::save_marked_done(&file, marked_done).ok();
                        })
                        .detach();
                    }
                    if let Err(err) = saved {
                        // Shown without failing the task: the run itself is
                        // unaffected.
                        task.reply.push_error(format!(
                            "Could not save this task to the history: {err:#}"
                        ));
                    }
                }
                this.working = this.working.with(lanes, false);
                if this.history_stale {
                    this.load_history(cx);
                }
                if !this.in_background {
                    let working = this.working;
                    this.chat_input
                        .update(cx, |input, cx| input.set_busy(working, cx));
                }
                // A Code, Chain, Spec, or Freeform task that finished well
                // adds a note to the next commit, written in the background;
                // one cancelled adds none.
                if is_task
                    && !is_cancelled()
                    && let Some(result) = finished.take()
                    && this
                        .tasks
                        .get(task_ix)
                        .is_some_and(|task| task.status == TaskStatus::Done)
                {
                    add_commit_note(summarize, project_dir.clone(), asked, result, cx);
                }
                // A chain step done goes on to the next step of its chain,
                // in its own lane, ahead of that lane's queue: see
                // `chain_next`.
                let next = this
                    .tasks
                    .get(task_ix)
                    .filter(|task| task.status == TaskStatus::Done && !is_cancelled())
                    .and_then(chain_next);
                // Deferred: starting the next run replaces this task. Only this
                // project's queues send, whichever project is on screen.
                let prompt_mode = cx.entity();
                let dir = project_dir.clone();
                cx.defer(move |cx| {
                    prompt_mode.update(cx, |this, cx| {
                        this.in_project(&dir, cx, |this, cx| {
                            if let Some((text, sending)) = next {
                                this.chain_on(text, sending, cx);
                            }
                            this.auto_send_next(cx)
                        });
                    })
                });
                cx.notify();
                });
            })
            .ok();
        });
    }

    /// Sends a chain's next step, `sending`, as [`chain_next`] makes it: at
    /// once while its lane is free, and otherwise at the head of the queue,
    /// ahead of every prompt waiting for its lane, where it shows and can be
    /// cancelled.
    fn chain_on(&mut self, text: String, sending: Sending, cx: &mut Context<Self>) {
        let Sending::Now(
            mode,
            attached,
            sliced,
            code_task,
            sent_from,
            post_build_update,
            named_after,
        ) = sending
        else {
            return self.start(text, sending, cx);
        };
        if !self.working.queues(mode) {
            let sending = Sending::Now(
                mode,
                attached,
                sliced,
                code_task,
                sent_from,
                post_build_update,
                named_after,
            );
            return self.start(text, sending, cx);
        }
        self.enqueue_at(
            true,
            text,
            false,
            mode,
            attached,
            sliced,
            code_task,
            sent_from,
            post_build_update,
            named_after,
            cx,
        );
    }

    /// Asks `text` straight away, beside any task the harness is working on,
    /// in place of any question still open. It is saved apart from the
    /// history, so it never becomes one of the tasks.
    fn ask(
        &mut self,
        text: String,
        attached: Attached,
        sliced: bool,
        named_after: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(project_dir) = self.project_dir.clone() else {
            return;
        };
        // Titled while its anchor is resolved.
        let naming = prompt_title::start(&project_dir, &text, named_after.as_deref(), self.titler);
        let run = self.push_ask(text.clone().into(), cx);
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == run) {
            ask.task.sent = SentAs {
                mode: Some(SendMode::Ask),
                attached_text: attached.text.clone(),
                attached_images: attached.images.clone(),
                sliced,
                code_task: None,
                sent_from: None,
                post_build_update: false,
            };
        }
        // Questions run at once. One that starts while another carries on the
        // conversation carries it on as a copy, so they don't write over
        // each other.
        let fork = self
            .asks
            .iter()
            .any(|ask| ask.id != run && ask.task.status.is_active());
        let resume = Session::resume(&self.ask_session, &project_dir);
        let new_conversation = resume.is_none();
        let epoch = self.ask_session_epoch;
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == run) {
            ask.task.new_conversation = new_conversation;
            ask.task.asked_at = Some(ask_pane::now_secs());
        }
        // Asked, the new conversation New conversation marked has begun, and
        // the Ask conversation goes to the bottom to follow it.
        self.ask_new_pending = false;
        self.ask_pane.locked = true;
        // The project's system prompt, as every task is sent.
        let system = {
            let project_dir = project_dir.clone();
            cx.background_spawn(async move {
                hidden_anchor::project_system_prompt(&project_dir)
                    .ok()
                    .flatten()
            })
        };
        let stopped = self
            .asks
            .iter()
            .find(|ask| ask.id == run)
            .map(|ask| ask.stopped.clone())
            .unwrap_or_default();
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
                    attached.clone(),
                    sliced,
                    None,
                    lsp,
                    &project_dir,
                ) {
                    Ok(anchor) => (anchor, None),
                    Err(err) => {
                        let mut anchor = HiddenAnchor::random();
                        anchor.mode = Some(SendMode::Ask);
                        anchor.attach(attached);
                        (anchor, Some(err))
                    }
                };
                // Saved under its name, with whether it started a new
                // conversation.
                let mut anchor = anchor;
                anchor.rename(naming.wait());
                anchor.new_conversation = Some(new_conversation);
                let file = hidden_anchor::save_ask(&anchor, &text, &project_dir);
                let compiled = match (resolve_error, &file) {
                    (Some(err), _) => Err(err),
                    (None, Ok(file)) => hidden_anchor::compile(&anchor, file, &project_dir),
                    (None, Err(_)) => Err(anyhow::anyhow!("the question was not saved")),
                };
                (
                    anchor.name().to_string(),
                    file,
                    compiled.map(|compiled| (anchor.name().to_string(), compiled)),
                )
            }
        });

        // Stopping the question drops its run, which stops it.
        let task = cx.spawn(async move |this, cx| {
            let (name, file, compiled) = compile.await;
            // Known by its name, as resending it names the next after it.
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    this.update_ask(run, |task| task.name = name.into(), cx)
                });
            })
            .ok();
            // What came of the question, saved beside it once the run is over,
            // or once it is stopped.
            let mut log = AskLog {
                file: file.as_ref().ok().cloned(),
                record: RunRecord::default(),
                stopped,
            };
            let compiled = match file {
                Ok(_) => compiled,
                Err(err) => Err(err),
            };
            match compiled {
                Ok((anchor, compiled)) => {
                    let prompt = compiled.user_prompt.clone();
                    log.record.user_prompt = Some(prompt.clone());
                    // Ask's instructions head its message, a question having
                    // no understanding file; its system prompt is the
                    // project's, the same as the tasks', so a conversation it
                    // forks keeps its cache.
                    let instructions = compiled.instructions_as_sent(None);
                    let message =
                        system_prompts::with_instructions(instructions.as_deref(), &prompt);
                    let sent_system = system.await;
                    // The harness the question goes to, which its usage came
                    // from.
                    let agent = crate::agent::current();
                    log.record.sent_to(agent, sent_system.as_deref());
                    log.record.instructions = instructions;
                    log.record.resumed = resume.is_some();
                    let mut usage_run = None;
                    // A question is told not to read the spec's source, so it
                    // reads the compiled reference instead.
                    let protected = harness::Protected {
                        root: None,
                        unread: mode_guard::unread(
                            SendMode::Ask,
                            &crate::project_tree::Locations::read(&project_dir),
                        ),
                    };
                    let mut events = harness::send_kept_off(
                        message,
                        sent_system.clone(),
                        compiled.images,
                        resume
                            .clone()
                            .map(|session| harness::Resume { session, fork }),
                        project_dir.clone(),
                        protected,
                    );
                    let compiled = Compiled::new(anchor.into(), prompt);
                    if this
                        .update(cx, |this, cx| {
                            this.in_project(&project_dir, cx, |this, cx| {
                                usage_run =
                                    Some(this.usage.start_run(Conversation::Questions, epoch));
                                this.update_ask(run, |ask| ask.set_compiled(compiled), cx)
                            });
                        })
                        .is_err()
                    {
                        return;
                    }
                    let mut started = false;
                    // The conversation this run reported.
                    let mut run_session = None;
                    while let Some(event) = events.next().await {
                        log.record.note(&event);
                        if this
                            .update(cx, |this, cx| {
                                let dir = project_dir.clone();
                                this.in_project(&dir, cx, |this, cx| {
                                    if let HarnessEvent::Session(_) = &event {
                                        started = true;
                                    }
                                    this.follow_usage(usage_run, agent, &event);
                                    if let Some(left) = Session::follow(
                                        &mut this.ask_session,
                                        this.ask_session_epoch == epoch,
                                        &mut run_session,
                                        &event,
                                        &project_dir,
                                        sent_system.as_deref(),
                                    ) {
                                        Self::keep_left(
                                            project_dir.clone(),
                                            conversations::Kind::Questions,
                                            left,
                                            cx,
                                        );
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
            this.update(cx, |this, cx| {
                this.in_project(&project_dir, cx, |this, cx| {
                    this.update_ask(run, PromptTask::end, cx);
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
            stopped: Arc::default(),
            _run: Task::ready(()),
        });
        cx.notify();
        self.next_ask_id
    }

    /// Updates the question `id`.
    fn update_ask(
        &mut self,
        id: usize,
        update: impl FnOnce(&mut PromptTask),
        cx: &mut Context<Self>,
    ) {
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == id) {
            update(&mut ask.task);
            cx.notify();
        }
    }

    /// Off the Ask tab, the questions still running, stacked above the chat
    /// input's tabs, the newest nearest them, each sliding up out of the
    /// input as it is asked and pushing the task view up to make room, so
    /// nothing of it is hidden. Each is a row: its first line, the latest
    /// thing the harness did, and Stop. A question leaves the stack once it
    /// is over; on the Ask tab there is no stack, its questions being in the
    /// Ask conversation.
    fn render_ask_stack(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ids: Vec<usize> = if self.on_ask_tab {
            Vec::new()
        } else {
            self.asks
                .iter()
                .filter(|ask| ask.task.status.is_active())
                .map(|ask| ask.id)
                .collect()
        };
        self.sync_ask_rows(&ids);
        if ids.is_empty() {
            return None;
        }
        let entity = cx.entity().downgrade();
        let render: RenderRow = Rc::new(move |ix, _, cx| {
            let Some(entity) = entity.upgrade() else {
                return div().into_any_element();
            };
            entity.update(cx, |this, cx| {
                let id = this.ask_row_ids.borrow().get(ix).copied();
                match id.and_then(|id| this.asks.iter().find(|ask| ask.id == id)) {
                    // The stack's own top border is the line above the first.
                    Some(ask) => this.render_running_question(ask, ix > 0, cx),
                    None => div().into_any_element(),
                }
            })
        });
        // As tall as its rows, until it scrolls.
        let total = self.ask_rows.total_height();
        let height = total.min(MAX_ASK_STACK_HEIGHT);
        let list = div()
            .id("ask-rows")
            .h(height)
            .child(self.ask_rows.element(render));
        // Lets UI tests find the rows; inert in normal builds.
        let list = gpui_kit::TestSupportExt::test_support(list);
        let content = if ids.len() <= PLAIN_ASK_ROWS {
            // Few enough to all be in view: drawn as they are.
            v_flex()
                .children(
                    self.asks
                        .iter()
                        .filter(|ask| ids.contains(&ask.id))
                        .enumerate()
                        .map(|(ix, ask)| self.render_running_question(ask, ix > 0, cx)),
                )
                .into_any_element()
        } else if total > MAX_ASK_STACK_HEIGHT + px(0.5) {
            scrollbar::with_scrollbar("ask-rows", &self.ask_rows, list, false, None, cx)
        } else {
            list.into_any_element()
        };
        let theme = cx.theme();
        let stack = v_flex()
            .id("ask")
            .flex_none()
            .bg(theme.tab_bar)
            .border_t_1()
            .border_color(theme.border)
            .child(content);
        // Lets UI tests find the questions; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(stack).into_any_element())
    }

    /// Makes the stack's list hold a row for each of `ids`, oldest first,
    /// keeping the rows already measured, and keeping it on the newest as
    /// questions are asked.
    fn sync_ask_rows(&self, ids: &[usize]) {
        let mut laid_out = self.ask_row_ids.borrow_mut();
        if laid_out.as_slice() == ids {
            return;
        }
        // The rows from the first that differs on are made anew.
        let kept = laid_out.iter().zip(ids).take_while(|(a, b)| a == b).count();
        let asked = ids.len() > laid_out.len() && kept == laid_out.len();
        self.ask_rows.splice(kept..laid_out.len(), ids.len() - kept);
        *laid_out = ids.to_vec();
        if asked {
            self.ask_rows.scroll_to_end();
        }
    }

    /// A running question's row in the stack: its first line, the latest
    /// thing the harness did, and Stop, sliding up out of the chat input as
    /// it is asked. Clicking it shows it in the Ask conversation. With
    /// `line_above`, it draws the line between it and the row above it.
    fn render_running_question(
        &self,
        ask: &Ask,
        line_above: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = ask.id;
        let stop = Button::new(("stop-ask-row", id))
            .ghost()
            .xsmall()
            .icon(IconName::CircleStop)
            .tooltip("Stop this question, leaving the others running")
            .on_click(cx.listener(move |this, _, _, cx| {
                // The button's click isn't the row's.
                cx.stop_propagation();
                this.stop_ask(id, cx)
            }));
        let row = h_flex()
            .id(("ask-row", id))
            .flex_none()
            .gap_3()
            .px_4()
            .py_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.reveal_question(id, window, cx)))
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.4))
                    .truncate()
                    .child(first_line(&ask.task.text)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(latest_row(id, &ask.task.reply, cx)),
            )
            .child(stop);
        // Lets UI tests find the row; inert in normal builds.
        let row = gpui_kit::TestSupportExt::test_support(row);
        let card = v_flex()
            .id(("ask-card", id))
            .flex_none()
            // Anchored to the chat input, so it rises out of it rather than
            // unrolling down onto it.
            .justify_end()
            .overflow_hidden()
            .border_color(cx.theme().border)
            .child(row);
        let card = gpui_kit::TestSupportExt::test_support(card);
        // Each question slides up from nothing, as far as its row.
        let height = SpringAnimation::new(ASK_SPRING)
            .to(ASK_ROW_HEIGHT)
            .from(px(0.));
        // Its line appears once there is more to it than the line, so the
        // line never lies on the chat input's own as it starts to rise.
        card.with_spring(("ask-slide", id), height, move |this, height| {
            this.max_h(height)
                .when(line_above && height > px(1.5), |this| this.border_t_1())
        })
        .into_any_element()
    }

    /// Copies the text selected in the Ask conversation, closing the popover.
    fn copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, text)) = self.selection_popover.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        TextSelection::clear(window, cx);
        cx.notify();
    }

    /// Attaches the text selected in the Ask conversation to the prompt, closing the
    /// popover.
    fn attach_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, text)) = self.selection_popover.take() {
            self.chat_input
                .update(cx, |input, cx| input.attach_text(text, cx));
        }
        TextSelection::clear(window, cx);
        cx.notify();
    }

    /// The popover by text selected in the Ask conversation: Copy, and Attach to prompt.
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

    /// The header pinned above the output: the latest task, unless the
    /// history is expanded, where it is listed.
    fn render_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.task_history.expanded {
            return None;
        }
        self.render_latest_header(cx)
    }

    /// The latest task's header, whether or not the previous tasks cover it,
    /// as while they slide in over it or away from it.
    fn render_latest_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ix = self.latest_ix()?;
        let task = &self.tasks[ix];
        // A Freeform task is a chat, with no header.
        if task.mode == Some(SendMode::Freeform) {
            return None;
        }
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
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(task_title(ix, task, true, cx))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(task.can_cancel(), |row| {
                                row.child(cancel_button(("cancel-latest", ix)).on_click(
                                    cx.listener(move |this, _, _, cx| this.cancel_task(ix, cx)),
                                ))
                            })
                            .children(self.other_mode_state(task).map(|(to, state)| {
                                let name = task.name.clone();
                                other_mode_button(
                                    ("send-to-other-latest", ix),
                                    to,
                                    state,
                                    cx.listener(move |this, _, window, cx| {
                                        this.send_to_other_mode(|this| &this.tasks, ix, window, cx)
                                    }),
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        this.open_mark_menu(
                                            name.clone(),
                                            event.position,
                                            window,
                                            cx,
                                        )
                                    }),
                                    cx,
                                )
                            }))
                            .child(resend_button(("resend-latest", ix)).on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.resend(|this| &this.tasks, ix, window, cx)
                                },
                            )))
                            .child(raw_prompt_button(
                                ("raw-prompt-latest", ix),
                                task.compiled.is_some(),
                                cx.listener(move |this, _, window, cx| {
                                    // Nothing else in the header opens or
                                    // closes with it.
                                    cx.stop_propagation();
                                    this.open_raw_prompt(ix, window, cx)
                                }),
                            )),
                    ),
            )
            .child(match &task.compiled {
                Some(compiled) => {
                    let this = cx.entity().downgrade();
                    let toggle: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
                        this.update(cx, |this, cx| {
                            if let Some(task) = this.tasks.get_mut(ix) {
                                task.slices_open = !task.slices_open;
                                cx.notify();
                            }
                        })
                        .ok();
                    });
                    let prompt = self.header_prompt.element(
                        ix,
                        compiled,
                        task.slices_open,
                        toggle,
                        open,
                        cx,
                    );
                    // Its images beneath it.
                    v_flex()
                        .gap_2()
                        .child(prompt)
                        .children(attached_image::prompt_thumbnails(
                            ("header-images", ix),
                            &task.sent.attached_images,
                            self.project_dir.as_deref(),
                            attached_image::PROMPT_THUMBNAIL,
                            cx,
                        ))
                        .into_any_element()
                }
                None => div()
                    .id(("task-prompt", ix))
                    .max_h(MAX_PROMPT_HEIGHT)
                    .overflow_y_scroll()
                    .child(task_prompt(ix, task, &open, None, cx))
                    .into_any_element(),
            });
        // Lets UI tests find the header; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(header).into_any_element())
    }

    /// What a queued prompt's row says of the lane it waits for, and the mode
    /// whose colour it reads in: its mode, "Chain" for one sent from the
    /// Chain tab, and "Spec follow-up" for a chain's last step, sent from its
    /// code step.
    fn queued_label(&self, item: &QueueItem) -> Option<(&'static str, SendMode)> {
        Some(match item.mode? {
            SendMode::Both => ("Chain", SendMode::Both),
            SendMode::Spec
                if item.sent_from.as_deref().is_some_and(|from| {
                    self.tasks.iter().any(|task| {
                        task.name.as_ref() == from
                            && task.sent.mode == Some(SendMode::Code)
                            && task.sent.code_task.is_some()
                    })
                }) =>
            {
                ("Spec follow-up", SendMode::Spec)
            }
            mode => (mode.label(), mode),
        })
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
        // A prompt waits for "Send next" once its lane is free.
        let lane_free = self
            .queue
            .iter()
            .any(|item| !self.working.overlaps(Lanes::of(item.mode)));
        let next_ready = self.next_sendable().is_some();
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
                .when(lane_free, |row| {
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

        // Where each prompt is and whether it is saved, for telling where a
        // dragged prompt may be dropped: it moves only past saved prompts.
        let saved: Rc<Vec<(usize, bool)>> = Rc::new(
            self.queue
                .iter()
                .map(|item| (item.id, item.saved.is_some()))
                .collect(),
        );
        let droppable = |saved: &[(usize, bool)], dragged: usize, to: usize| {
            saved
                .iter()
                .position(|&(id, _)| id == dragged)
                .is_some_and(|from| {
                    from != to && saved[from.min(to)..=from.max(to)].iter().all(|&(_, ok)| ok)
                })
        };
        // While a prompt is dragged: which, and the gap it would land in,
        // shown as a line there; nothing once no drag is under way.
        if !cx.has_active_drag() {
            self.queue_gap.set(None);
        }
        let (dragging, gap) = match self.queue_gap.get() {
            Some((id, gap)) => (Some(id), gap),
            None => (None, None),
        };
        let queue_len = count;
        let list = expanded.then(|| {
            let queue_gap = self.queue_gap.clone();
            v_flex()
                .id("queue-list")
                // Off the list, no gap is shown.
                .on_drag_move(move |event: &DragMoveEvent<QueuedDrag>, window, cx| {
                    if !event.bounds.contains(&event.event.position)
                        && let Some((id, Some(_))) = queue_gap.get()
                    {
                        queue_gap.set(Some((id, None)));
                        window.refresh();
                        let _ = cx;
                    }
                })
                .max_h(MAX_QUEUE_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&self.queue_scroll)
                .px_4()
                .pb_2()
                .children(self.queue.iter().enumerate().map(|(ix, item)| {
                    let id = item.id;
                    let editing = self.editing_queued == Some(id);
                    // Prompts sharing a context are joined by a line between
                    // their switches: one that carries on joins the one
                    // above it, the first never does.
                    let joins_above = ix > 0 && !item.new_conversation;
                    let joins_below = self.queue.get(ix + 1).is_some_and(|next| !next.new_conversation);
                    let segment = |joined: bool| {
                        div()
                            .flex_1()
                            .w(px(1.))
                            .when(joined, |line| line.bg(border))
                    };
                    // As tall as the row, its padding included, so the line
                    // meets the next row's across the gap between them.
                    let new_conversation = v_flex()
                        .flex_none()
                        .self_stretch()
                        .my(px(-2.))
                        .items_center()
                        .child(segment(joins_above))
                        .child(
                            Switch::new(("new-conversation-queued", ix))
                                .xsmall()
                                .checked(item.new_conversation)
                                .tooltip(if item.new_conversation {
                                    "Starts a new conversation; click to carry on the conversation instead"
                                } else {
                                    "Carries on the conversation; click to start a new one instead"
                                })
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle_queued_new_conversation(id, cx)
                                })),
                        )
                        .child(segment(joins_below));
                    let row = h_flex()
                        .id(("queued-prompt", ix))
                        .gap_2()
                        .py_0p5()
                        .relative()
                        .when(editing, |row| row.bg(theme.list_active))
                        // The one dragged stays in its place, dimmed.
                        .when(dragging == Some(id), |row| row.opacity(0.5))
                        // Over its first half, the gap before it; over its
                        // second, the one after, where the prompt may go.
                        .on_drag_move({
                            let (queue_gap, saved) = (self.queue_gap.clone(), saved.clone());
                            move |event: &DragMoveEvent<QueuedDrag>, window, cx| {
                                let Some(at) =
                                    drag_gap(ix, event.bounds, event.event.position, true)
                                else {
                                    return;
                                };
                                let dragged = event.drag(cx).id;
                                let from = saved.iter().position(|&(id, _)| id == dragged);
                                let gap = from
                                    .and_then(|from| gap_target(from, at))
                                    .filter(|&to| droppable(&saved, dragged, to))
                                    .map(|_| at);
                                if queue_gap.get() != Some((dragged, gap)) {
                                    queue_gap.set(Some((dragged, gap)));
                                    window.refresh();
                                }
                            }
                        })
                        .on_drop(cx.listener(move |this, drag: &QueuedDrag, window, cx| {
                            let gap = this.queue_gap.take().and_then(|(_, gap)| gap);
                            let from = this.queue.iter().position(|item| item.id == drag.id);
                            if let Some(to) = from.zip(gap).and_then(|(from, gap)| gap_target(from, gap)) {
                                this.move_queued(drag.id, to, window, cx);
                            }
                            cx.notify();
                        }))
                        .when(gap == Some(ix), |row| row.child(drop_indicator(false, false, cx)))
                        .when(ix + 1 == queue_len && gap == Some(queue_len), |row| {
                            row.child(drop_indicator(false, true, cx))
                        })
                        .when(item.saved.is_some(), |row| {
                            let queue_gap = self.queue_gap.clone();
                            row.cursor_grab().on_drag(
                                QueuedDrag {
                                    id,
                                    text: item.text.clone().into(),
                                },
                                move |drag, _, _, cx| {
                                    queue_gap.set(Some((drag.id, None)));
                                    cx.new(|_| drag.clone())
                                },
                            )
                        })
                        .child(
                            Icon::new(IconName::GripVertical)
                                .xsmall()
                                .flex_none()
                                .text_color(if item.saved.is_some() {
                                    muted
                                } else {
                                    transparent_black()
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .min_w_6()
                                .text_color(muted)
                                .child(format!("{}.", ix + 1)),
                        )
                        // Which lane it waits for, in its mode's colour.
                        .children(self.queued_label(item).map(|(label, mode)| {
                            div()
                                .id(("queued-lane", ix))
                                .flex_none()
                                .text_sm()
                                .font_semibold()
                                .text_color(chat_input::mode_color(mode, cx))
                                .child(label)
                        }))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().min_w_0().line_clamp(2).child(item.text.clone()))
                                .children(attached_image::prompt_thumbnails(
                                    ("queued-images", ix),
                                    &item.images,
                                    self.project_dir.as_deref(),
                                    attached_image::QUEUED_THUMBNAIL,
                                    cx,
                                )),
                        )
                        .when(item.saved.is_none(), |row| {
                            row.child(div().flex_none().child(Spinner::new().small()))
                        })
                        .child(new_conversation)
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

    /// The previous tasks, expanded in place of the latest task.
    fn render_task_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let open_file = self.file_opener(cx);
        let list = self.task_history.render_list(
            &self.tasks,
            |this| &this.tasks,
            |this| &mut this.tasks,
            |this| &mut this.task_history,
            &self.steps_shown,
            &open_file,
            self.task_visibility(),
            self.latest_ix(),
            cx,
        );
        // The filters stay put above the list as it scrolls.
        v_flex()
            .size_full()
            .child(self.render_task_filters(cx))
            .child(div().flex_1().min_h_0().child(list))
            .into_any_element()
    }

    /// Whether `task` matches the filter chip `filter`.
    fn matches_filter(&self, task: &PromptTask, filter: TaskFilter) -> bool {
        match filter {
            TaskFilter::Mode(mode) => task.mode == Some(mode),
            TaskFilter::CanSendSpec => self.offers_other_mode(task) == Some(SendMode::Spec),
            TaskFilter::CanSendCode => self.offers_other_mode(task) == Some(SendMode::Code),
            TaskFilter::Sent => self.other_mode_state(task).is_some_and(|(_, state)| {
                matches!(state, OtherModeState::Sent | OtherModeState::Sending)
            }),
            TaskFilter::MarkedDone => task.marked_done,
        }
    }

    /// How many previous tasks the filter chip `filter` matches on its own,
    /// whatever other chips are on; the latest task, heading the view, isn't
    /// one of them.
    fn filter_count(&self, filter: TaskFilter) -> usize {
        let latest = self.latest_ix();
        self.tasks
            .iter()
            .enumerate()
            .filter(|(ix, _)| Some(*ix) != latest)
            .filter(|(_, task)| self.matches_filter(task, filter))
            .count()
    }

    /// Which previous tasks the filters on show: a task shows when it
    /// matches a chip on in each group that has one on. None while no chip
    /// is on, when every task shows.
    fn task_visibility(&self) -> Option<Vec<bool>> {
        let filters = &self.task_history.filters;
        if filters.is_empty() {
            return None;
        }
        let group = |task: &PromptTask, modes: bool| {
            let on: Vec<_> = filters.iter().filter(|f| f.is_mode() == modes).collect();
            on.is_empty() || on.iter().any(|&&f| self.matches_filter(task, f))
        };
        Some(
            self.tasks
                .iter()
                .map(|task| group(task, true) && group(task, false))
                .collect(),
        )
    }

    /// Turns the filter chip `filter` on or off; an open task it hides
    /// closes.
    fn toggle_task_filter(&mut self, filter: TaskFilter, cx: &mut Context<Self>) {
        let filters = &mut self.task_history.filters;
        match filters.iter().position(|&on| on == filter) {
            Some(ix) => {
                filters.remove(ix);
            }
            None => filters.push(filter),
        }
        self.close_filtered_out();
        cx.notify();
    }

    fn close_filtered_out(&mut self) {
        if let (Some(open), Some(visible)) = (self.task_history.open, self.task_visibility())
            && visible.get(open) == Some(&false)
        {
            self.task_history.open = None;
        }
    }

    /// The filter bar along the top of the expanded previous tasks: a chip
    /// for each mode, then for where a task stands with the other mode,
    /// each counting the tasks it would show, and Clear while any is on.
    fn render_task_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, bar, accent) = (
            theme.muted_foreground,
            theme.border,
            theme.tab_bar,
            theme.accent,
        );
        let chips = TaskFilter::ALL.iter().enumerate().map(|(ix, &filter)| {
            let on = self.task_history.filters.contains(&filter);
            let count = self.filter_count(filter);
            let color = match filter {
                TaskFilter::Mode(mode) => chat_input::mode_color(mode, cx),
                _ => accent,
            };
            // Lets UI tests find the chip; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(h_flex().id(("task-filter", ix)))
                .flex_none()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_md()
                .border_1()
                .text_xs()
                .cursor_pointer()
                .border_color(if on { color } else { border })
                .when(on, |chip| chip.bg(color.opacity(0.18)))
                .hover(|chip| chip.bg(theme.list_hover))
                // A gap between the two groups.
                .when(ix == 4, |chip| chip.ml_3())
                .child(filter.label())
                .child(div().text_color(muted).child(count.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_task_filter(filter, cx)))
        });
        // Lets UI tests find the bar; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(h_flex().id("task-filters"))
            .flex_none()
            .w_full()
            .flex_wrap()
            .gap_1()
            .px_3()
            .py_1p5()
            .bg(bar)
            .border_b_1()
            .border_color(border)
            .children(chips)
            .when(!self.task_history.filters.is_empty(), |row| {
                row.child(
                    div().ml_auto().child(
                        Button::new("clear-task-filters")
                            .ghost()
                            .xsmall()
                            .label("Clear")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.task_history.filters.clear();
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    /// A scroll's crossing over as it slides: the previous tasks stacked
    /// above the latest task's header and output, one column as tall as two
    /// of the space they share, moving down to bring the previous tasks in,
    /// or up to bring the latest task back, clipped to that space.
    fn render_scroll_slide(
        &self,
        slide: &ScrollSlide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = self.slide_height.get();
        // From wherever the stretch had pulled the view.
        let (from, to) = if slide.up {
            (-height + slide.pull, px(0.))
        } else {
            (slide.pull, -height)
        };
        let spring = SpringAnimation::new(PANE_SPRING).to(to).from(from);
        let column = v_flex()
            .absolute()
            .left_0()
            .right_0()
            .h(height * 2.)
            .child(
                v_flex()
                    .h(height)
                    .flex_none()
                    .child(self.render_task_list(cx)),
            )
            .child(
                v_flex()
                    .h(height)
                    .flex_none()
                    .children(self.render_latest_header(cx))
                    .child(v_flex().flex_1().min_h_0().child(self.render_output(cx))),
            )
            .with_spring(("scroll-slide", slide.n), spring, |this, top| this.top(top));
        // Lets UI tests find the slide; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(div().id(("scroll-slide-view", slide.n)))
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(column)
            .children(self.render_scroll_glow(window, cx))
            .child(self.scroll_past_ends(cx))
            .into_any_element()
    }

    /// The latest task's output, filling the space under the header.
    fn render_output(&self, cx: &mut Context<Self>) -> AnyElement {
        // A Freeform task is shown as a chat, not a table.
        if let Some(chat) = self.render_freeform_chat(cx) {
            return chat;
        }
        let Some(task_ix) = self.latest_ix() else {
            return div().into_any_element();
        };
        let task = &self.tasks[task_ix];
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
        let table = self.output_table.render(
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
        );
        // In a git repository, the files it changed, beneath its table.
        let changed = self.changed_files_of(task_ix, id_of_latest(task_ix), cx);
        match changed {
            Some(changed) => v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(table))
                .child(
                    div()
                        .id("latest-changed-files")
                        .flex_none()
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .child(changed),
                )
                .into_any_element(),
            None => table,
        }
    }
}

impl PromptMode {
    /// The files that changed while the task at `ix` ran, when
    /// both its snapshots were taken: a heading with their count, which
    /// shows and hides them, each a row that opens its diff between the
    /// snapshots. Its element ids are keyed by `key`.
    fn changed_files_of(
        &self,
        ix: usize,
        key: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let entity = cx.entity().downgrade();
        let task = self.tasks.get(ix)?;
        let changed = task.changed.clone()?;
        let open = task.changed_open;
        render_changed_files(
            key,
            &changed,
            open,
            {
                let entity = entity.clone();
                Rc::new(move |cx: &mut App| {
                    entity
                        .update(cx, |this, cx| {
                            if let Some(task) = this.tasks.get_mut(ix) {
                                task.changed_open = !task.changed_open;
                            }
                            cx.notify();
                        })
                        .ok();
                })
            },
            Rc::new(move |file: usize, cx: &mut App| {
                entity
                    .update(cx, |this, cx| {
                        let Some(changed) =
                            this.tasks.get(ix).and_then(|task| task.changed.clone())
                        else {
                            return;
                        };
                        let Some((change, _)) = changed.files.get(file) else {
                            return;
                        };
                        cx.emit(OpenSnapshotDiff {
                            top: changed.top.clone(),
                            path: change.path.clone(),
                            from: change.from.clone(),
                            before: changed.before.clone(),
                            after: changed.after.clone(),
                        });
                    })
                    .ok();
            }),
            cx,
        )
        .into()
    }
}

/// Element ids of the latest task's changed files, apart from any previous
/// task's.
fn id_of_latest(ix: usize) -> usize {
    usize::MAX / 2 + ix
}

/// A task's changed files, as [`PromptMode::changed_files_of`] says, with
/// `toggle` showing or hiding them and `open` opening one's diff.
#[allow(clippy::type_complexity)]
fn render_changed_files(
    key: usize,
    changed: &ChangedFiles,
    open: bool,
    toggle: Rc<dyn Fn(&mut App)>,
    open_file: Rc<dyn Fn(usize, &mut App)>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, hover, border) = (theme.muted_foreground, theme.list_hover, theme.border);
    let palette = crate::theme::palette(cx);
    let count = changed.files.len();
    // A run that changed nothing says so, with nothing to expand.
    if count == 0 {
        return gpui_kit::TestSupportExt::test_support(div().id(("changed-files", key)))
            .w_full()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(border)
            .text_sm()
            .text_color(muted)
            .child("No files changed")
            .into_any_element();
    }
    let heading =
        gpui_kit::TestSupportExt::test_support(h_flex().id(("changed-files-toggle", key)))
            .gap_1()
            .px_3()
            .py_1()
            .text_sm()
            .font_medium()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| toggle(cx))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(muted),
            )
            .child("Files changed")
            .child(div().text_color(muted).child(format!("{count}")));
    let rows = open.then(|| {
        v_flex()
            .pb_1()
            .children(
                changed
                    .files
                    .iter()
                    .enumerate()
                    .map(|(ix, (change, by_agent))| {
                        use task_snapshot::ChangeKind;
                        let color = match change.kind {
                            ChangeKind::Added => crate::git_status::Status::Added.color(cx),
                            ChangeKind::Modified | ChangeKind::Renamed => {
                                crate::git_status::Status::Modified.color(cx)
                            }
                            ChangeKind::Deleted => Hue::Red.of(palette),
                        };
                        let open_file = open_file.clone();
                        let by_agent = *by_agent;
                        gpui_kit::TestSupportExt::test_support(h_flex().id(("changed-file", ix)))
                            .gap_2()
                            .px_3()
                            .py_0p5()
                            .text_sm()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(move |_, _, cx| open_file(ix, cx))
                            .child(
                                div()
                                    .flex_none()
                                    .w_4()
                                    .font_semibold()
                                    .text_color(color)
                                    .child(change.kind.letter()),
                            )
                            .when(!by_agent, |row| row.text_color(muted))
                            .children(change.from.as_ref().map(|from| {
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    .text_color(muted)
                                    .child(from.display().to_string())
                                    .child("→")
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(change.path.display().to_string()),
                            )
                            .when(!by_agent, |row| {
                                row.child(
                                    gpui_kit::TestSupportExt::test_support(
                                        div().id(("changed-file-elsewhere", ix)),
                                    )
                                    .flex_none()
                                    .child(Icon::new(IconName::Clock).xsmall().text_color(muted))
                                    .tooltip(|window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(
                                            "Changed while the task ran, but not by the agent",
                                        )
                                        .build(window, cx)
                                    }),
                                )
                            })
                    }),
            )
            .into_any_element()
    });
    v_flex()
        .id(("changed-files", key))
        .w_full()
        .border_t_1()
        .border_color(border)
        .child(heading)
        .children(rows)
        .into_any_element()
}

impl Render for PromptMode {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A task cancelled or failed may leave another heading the view.
        self.settle_latest();
        // The chat input shows how much context its next prompt carries on.
        let context = self.context();
        if self.chat_input.read(cx).context() != context {
            self.chat_input
                .update(cx, |input, cx| input.set_context(context, cx));
        }
        // And the agent's usage, as its runs report it.
        let usage = self.usage_report();
        if *self.chat_input.read(cx).usage() != usage {
            self.chat_input
                .update(cx, |input, cx| input.set_usage(usage, cx));
        }
        // New conversation waits for a run of the conversation to finish.
        let running = self.conversation_running();
        if self.chat_input.read(cx).conversation_running() != running {
            self.chat_input
                .update(cx, |input, cx| input.set_conversation_running(running, cx));
        }
        // The task running can be sent more while its output shows.
        let can_send_to_task = self.can_send_to_task();
        if self.chat_input.read(cx).can_send_to_task() != can_send_to_task {
            self.chat_input.update(cx, |input, cx| {
                input.set_can_send_to_task(can_send_to_task, cx)
            });
        }
        // The popover for text selected in the Ask conversation goes with it.
        if !self.on_ask_tab {
            self.selection_popover = None;
        }
        let hint = if ProjectDirectory::get(cx).is_some() {
            "Write a prompt below. Enter adds a line; Ctrl/Cmd+Enter sends it."
        } else {
            "Open a project to start prompting."
        };

        // While any filter is on, the row says how many tasks show.
        self.task_history.shown.set(
            // The previous tasks shown; the latest, heading the view, isn't one.
            self.task_visibility().map(|visible| {
                let previous = visible.len().saturating_sub(1);
                visible[..previous].iter().filter(|&&shows| shows).count()
            }),
        );
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
        } else if let Some(slide) = self
            .scroll_slide
            .as_ref()
            .filter(|_| self.scroll_sliding() && self.slide_height.get() > px(0.))
        {
            window.request_animation_frame();
            self.render_scroll_slide(slide, window, cx)
        } else {
            let view = if self.task_history.expanded {
                self.render_task_list(cx)
            } else {
                self.render_output(cx)
            };
            // Pulled past its end by the stretch, the header and the view
            // together, leaving an empty band, clipped to the space.
            let pull = self.scroll_push.get().pull(Instant::now());
            v_flex()
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(
                    v_flex()
                        .relative()
                        .top(pull)
                        .flex_1()
                        .min_h_0()
                        .children(self.render_header(cx))
                        .child(v_flex().relative().flex_1().min_h_0().child(view)),
                )
                .children(self.render_scroll_glow(window, cx))
                .child(self.scroll_past_ends(cx))
                .into_any_element()
        };
        // Measured each frame, for a scroll's slide to move across.
        let content = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_prepaint({
                let slide_height = self.slide_height.clone();
                move |bounds, _, _| slide_height.set(bounds.size.height)
            })
            .child(content);

        let history = v_flex()
            .id("history")
            .size_full()
            .child(self.task_history.render_row(
                // The latest task heads the view, so it isn't counted.
                self.tasks.len().saturating_sub(1),
                !self.tasks.is_empty(),
                |this| &mut this.task_history,
                self.render_selection_actions(cx),
                cx,
            ))
            .child(content)
            .children(self.render_queue(cx));
        // Lets UI tests find the history; inert in normal builds.
        let history = gpui_kit::TestSupportExt::test_support(history);
        let history = div().relative().size_full().child(history);

        // The selected tab's contents: the task view, or a file.
        let shown = match self.open_file_view() {
            Some(file) => div()
                .size_full()
                .overflow_hidden()
                .child(file)
                .into_any_element(),
            None => history.into_any_element(),
        };
        let body = div().relative().flex_1().min_h_0().child(shown);
        // Above the chat input: the task view, pushed up by the stack of
        // questions still running beneath it.
        let body = div()
            .id("prompt-body")
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .size_full()
                    .child(body)
                    .children(self.render_ask_stack(cx)),
            )
            .children(self.render_mark_menu());
        // Lets UI tests find the space above the chat input; inert in normal
        // builds.
        let body = gpui_kit::TestSupportExt::test_support(body);
        // The tab bar along the top, the selected tab's contents beneath it.
        let left = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_tabs(cx))
            .child(body);
        // On the Ask tab, the Ask conversation beside them, the edge between
        // the two resizing the split.
        let top = if self.on_ask_tab {
            let split = h_flex()
                .id("ask-split")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<AskSplitResize>, _, cx| {
                        this.drag_ask_split(event.event.position.x, event.bounds, cx)
                    }),
                )
                .child(left)
                .child(self.render_ask_pane(cx))
                .children(self.render_selection_popover(cx));
            gpui_kit::TestSupportExt::test_support(split).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .child(left)
                .into_any_element()
        };

        // The chat input along the bottom, beneath both sides of the split,
        // with the referenced spec sidebar beside them all, never reaching
        // over the chat input.
        let column = v_flex()
            .size_full()
            .child(top)
            .child(self.chat_input.clone());
        self.with_referenced_spec(column, window, cx)
    }
}

/// The hidden anchor `text` is sent as in `mode`, sliced or not: importing
/// what `piton lsp` resolved for it, with the mode's system prompt. Without `piton lsp`
/// nothing is imported, and any spec name the prompt uses fails to compile
/// with an explicit error.
///
/// A Freeform prompt is saved as an anchor too, so it joins the history and
/// the queue as any task does, but one that imports nothing, has no system
/// prompt, and is never sliced, whatever the Slice toggle says: it is sent
/// just as it was typed (see [`hidden_anchor::freeform`]), never compiled.
fn resolve_anchor(
    text: &str,
    mode: SendMode,
    attached: Attached,
    sliced: bool,
    code_task: Option<CodeTask>,
    lsp: Option<Arc<PitonSession>>,
    project_dir: &Path,
) -> Result<HiddenAnchor> {
    if mode == SendMode::Freeform {
        let mut anchor = HiddenAnchor::random();
        anchor.mode = Some(mode);
        anchor.attach(attached);
        return Ok(anchor);
    }
    let mut anchor = match lsp {
        Some(lsp) => lsp.anchor_for(text)?,
        None => HiddenAnchor::random(),
    };
    // Once a project has been prompted, each mode's system prompt can be
    // found in it and edited by hand. The defaults still apply if they
    // cannot be saved.
    system_prompts::save_missing(project_dir).ok();
    anchor.mode = Some(mode);
    anchor.sliced = sliced;
    anchor.attach(attached);
    // A Code task sent to Spec is also told what the code task did, and a
    // chain's code step what its spec step did; in any other mode, the task
    // it was handed on from is nothing to it. Its mode's instructions go at
    // the top of its message, not in the system prompt, which is the
    // project's, the same for every prompt.
    anchor.code_task = code_task.filter(|_| hands_on(mode));
    anchor.system_prompt =
        hidden_anchor::instructions_for(mode, anchor.code_task.is_some(), project_dir)?;
    Ok(anchor)
}

/// Which of `tasks` heads the view as the latest task: the one sent most
/// recently of those under way, in either lane; while none is, the one that
/// finished most recently, `settled`, the task last seen heading the view
/// while under way, by its index and name; or else the one sent most
/// recently.
fn latest_of(tasks: &[PromptTask], settled: Option<&(usize, SharedString)>) -> Option<usize> {
    if let Some(active) = tasks.iter().rposition(|task| task.status.is_active()) {
        return Some(active);
    }
    settled
        .filter(|(ix, name)| tasks.get(*ix).is_some_and(|task| task.name == *name))
        .map(|(ix, _)| *ix)
        .or_else(|| tasks.len().checked_sub(1))
}

/// The tasks of `tasks` the harness is working on while `working`, by their
/// index, with each one's first line, oldest first.
fn running_tasks(tasks: &[PromptTask], working: Lanes) -> Vec<(usize, SharedString)> {
    if !working.any() {
        return Vec::new();
    }
    let latest = latest_of(tasks, None);
    tasks
        .iter()
        .enumerate()
        // The latest, or one of the other lane still under way beside it.
        .filter(|(ix, task)| {
            task.status.is_active() && (task.cancel.is_some() || Some(*ix) == latest)
        })
        .map(|(ix, task)| (ix, first_line(&task.text)))
        .collect()
}

/// Where a lane's run is kept among [`PromptMode::_pending`]: the code
/// lane's, where a Freeform task, which keeps both busy, runs too, then the
/// spec lane's.
fn lane_slot(lanes: Lanes) -> usize {
    if lanes.code { 0 } else { 1 }
}

/// Shows `notification` in the window, from where there is none at hand.
fn notify(notification: Notification, cx: &mut App) {
    cx.defer(move |cx| {
        if let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) {
            handle
                .update(cx, |_, window, cx| {
                    window.push_notification(notification, cx)
                })
                .ok();
        }
    });
}

/// Whether a task sent in `mode` can be told what the task it was handed on
/// from did: a Spec task sent from Code, or a chain's code step, told what
/// its spec step did.
fn hands_on(mode: SendMode) -> bool {
    matches!(mode, SendMode::Spec | SendMode::Code)
}

/// The step a chain goes on to once `task`, a step of it, is done, and how
/// it is sent. A Chain task writes the spec; then the same prompt goes to
/// Code, with what attached to it, sliced as it was, built against the spec
/// just written, and told what the spec step did. With a post-build spec
/// update, the code step done goes on to Spec, just as a Code task sent to
/// Spec does, told what the code step did. Each step knows the one it was
/// sent from.
fn chain_next(task: &PromptTask) -> Option<(String, Sending)> {
    let sent = &task.sent;
    let (mode, code_task, post_build_update) = match sent.mode? {
        SendMode::Both => (
            SendMode::Code,
            CodeTask {
                prompt: task.text.to_string(),
                result: task.reply.final_output(),
            },
            sent.post_build_update,
        ),
        SendMode::Code if sent.code_task.is_some() && sent.post_build_update => {
            (SendMode::Spec, code_task_of(task), false)
        }
        _ => return None,
    };
    Some((
        task.text.to_string(),
        Sending::Now(
            mode,
            sent.attached(),
            sent.sliced,
            Some(code_task),
            Some(task.name.to_string()),
            post_build_update,
            // Each step is named after the chain's title.
            Some(task.name.to_string()),
        ),
    ))
}

/// The hidden anchor a message sent to a running task is compiled as: as
/// [`resolve_anchor`] resolves a prompt's, but with no mode or system prompt,
/// since the run already has the one it began with.
fn message_anchor(
    text: &str,
    attached: Attached,
    sliced: bool,
    lsp: Option<Arc<PitonSession>>,
) -> Result<HiddenAnchor> {
    let mut anchor = match lsp {
        Some(lsp) => lsp.anchor_for(text)?,
        None => HiddenAnchor::random(),
    };
    anchor.sliced = sliced;
    anchor.attach(attached);
    Ok(anchor)
}

/// What came of a message sent to the task running.
enum ToTask {
    Sent,
    /// Its task was over before it could be sent.
    Over,
    /// It didn't compile, for this reason.
    Failed(String),
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
/// the name of the hidden anchor it was compiled from once it has compiled;
/// with `origin`, as in the latest task's header, beneath the name, that a
/// task sent to Spec from a Code task was sent from Code.
fn task_title(ix: usize, task: &PromptTask, origin: bool, cx: &App) -> Div {
    let theme = cx.theme();
    // A Freeform prompt is sent as it is, with no hidden anchor to name.
    let named = task
        .compiled
        .as_ref()
        .filter(|_| task.sent.mode != Some(SendMode::Freeform));
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
        .when_some(named, |row, compiled| {
            let anchor = div()
                .id(("prompt-anchor", ix))
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .font_family(theme.mono_font_family.clone())
                .child(compiled.anchor.clone());
            // Lets UI tests find the anchor; inert in normal builds.
            let anchor = gpui_kit::TestSupportExt::test_support(anchor);
            let from_code = (origin && task.sent.code_task.is_some()).then(|| {
                // Lets UI tests find it; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(
                    div()
                        .id(("prompt-sent-from", ix))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if task.sent.mode == Some(SendMode::Code) {
                            SENT_FROM_CHAIN
                        } else {
                            SENT_FROM_CODE
                        }),
                )
            });
            match from_code {
                Some(from_code) => row.child(v_flex().min_w_0().child(anchor).child(from_code)),
                None => row.child(anchor),
            }
        })
}

/// What a task sent to Spec from a Code task says beneath its anchor's name.
const SENT_FROM_CODE: &str = "Sent from Code";

/// What a chain's code step says beneath its anchor's name.
const SENT_FROM_CHAIN: &str = "Sent from Chain";

/// The button that sends a prompt again, as it was sent.
fn resend_button(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::RotateCcw)
        .tooltip("Send this prompt again, as it was sent")
}

/// The button, `id`, beside the latest task's Resend button that opens the
/// raw prompt modal with `on_click`; disabled until the task has compiled, as
/// until then nothing has been sent.
fn raw_prompt_button(
    id: impl Into<ElementId>,
    compiled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let button = Button::new("raw-prompt-button")
        .ghost()
        .xsmall()
        .icon(IconName::Code)
        .tooltip(if compiled {
            "View the raw prompt"
        } else {
            "No prompt sent yet"
        })
        .disabled(!compiled)
        .on_click(on_click);
    // Lets UI tests find and click the button; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(div().id(id).flex_none().child(button))
        .into_any_element()
}

/// The button that cancels the task under way.
fn cancel_button(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::CircleStop)
        .tooltip("Cancel this task")
}

/// What a Code task sent to Spec tells it of the code task: its prompt as
/// typed, and its final output, the reply text after its last tool call (see
/// [`crate::task_table::Reply::final_output`]). Only a task that finished has
/// one: one that failed, was cancelled, is still under way, or wasn't
/// recorded left none.
fn code_task_of(task: &PromptTask) -> CodeTask {
    CodeTask {
        prompt: task.text.to_string(),
        result: (task.status == TaskStatus::Done)
            .then(|| task.reply.final_output())
            .flatten(),
    }
}

/// The mode `task` can be sent to instead of the one it was sent in: Spec
/// for a Code task, Code for a Spec task. Chain tasks, questions, and prompts
/// whose mode isn't known have none.
fn other_mode(task: &PromptTask) -> Option<SendMode> {
    task.sent.mode.and_then(SendMode::other)
}

/// The button, `id`, that sends a task again in the other mode, `to`,
/// tinted that mode's colour, beside its Resend button: enabled while it is
/// [`OtherModeState::Offered`], else greyed out and disabled, its tooltip
/// saying why. Right-clicked, enabled or not, it calls `on_right_click`,
/// which opens its menu; the right-click goes no further.
fn other_mode_button(
    id: impl Into<ElementId>,
    to: SendMode,
    state: OtherModeState,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_right_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let enabled = state == OtherModeState::Offered;
    let tooltip: SharedString = state.tooltip(to).into();
    let color = if enabled {
        chat_input::mode_color(to, cx)
    } else {
        cx.theme().muted_foreground
    };
    let button = Button::new("send-to-other-mode")
        .ghost()
        .xsmall()
        .icon(Icon::new(IconName::ArrowRightLeft).text_color(color))
        .tooltip(tooltip.clone())
        .disabled(!enabled)
        .on_click(on_click);
    // Lets UI tests find and click the button; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(
        div()
            .id(id)
            .flex_none()
            // Says what its tooltip does, whether it is enabled or not.
            .aria_label(tooltip)
            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                // Not the heading's, nor anything else's beneath it.
                cx.stop_propagation();
                on_right_click(event, window, cx);
            })
            .child(button),
    )
    .into_any_element()
}

/// The checkbox, `checked` or not, at the left of the heading of the history
/// item `task_ix`, before its status, which selects or deselects it with
/// `on_click` without opening or closing it.
fn history_checkbox(
    task_ix: usize,
    checked: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let checkbox = Checkbox::new(("history-checkbox", task_ix))
        .checked(checked)
        .on_click(move |_, window, cx| {
            // The checkbox's click isn't the heading's.
            cx.stop_propagation();
            on_click(window, cx);
        });
    // Lets UI tests find and click the checkbox; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(
        div()
            .id(("history-select", task_ix))
            .flex_none()
            .child(checkbox),
    )
    .into_any_element()
}

/// The prompt a task was sent as: the compiled markdown the harness received,
/// or the text as typed until it compiles. Its links to files open them.
fn task_prompt(
    ix: usize,
    task: &PromptTask,
    open: &OpenFile,
    toggle_slices: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> AnyElement {
    match &task.compiled {
        Some(compiled) => {
            // What was typed first, then the slices it was sent with,
            // collapsed until shown.
            let slices = compiled
                .slices()
                .zip(toggle_slices)
                .map(|(slices, toggle)| {
                    v_flex()
                        .pt_2()
                        .gap_2()
                        .child(task_table::slices_row(
                            ("prompt-slices", ix),
                            task.slices_open,
                            slices.count,
                            move |window, cx| toggle(window, cx),
                            cx,
                        ))
                        .when(task.slices_open, |column| {
                            column.child(shown_markdown_view(
                                slices_key(ix),
                                slices.shown.clone(),
                                Some(open),
                                cx,
                            ))
                        })
                });
            let prompt = div()
                .id(("compiled-prompt", ix))
                .min_w_0()
                .child(shown_markdown_view(
                    prompt_key(ix),
                    compiled.shown(),
                    Some(open),
                    cx,
                ))
                // Its images beneath the prompt as typed, then its slices.
                .children(task_images(ix, task, cx))
                .children(slices);
            // Lets UI tests find the compiled prompt; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(prompt).into_any_element()
        }
        None => div()
            .min_w_0()
            .text_color(cx.theme().muted_foreground)
            .child(task.text.clone())
            .children(task_images(ix, task, cx))
            .into_any_element(),
    }
}

/// The images attached to task `ix`, as thumbnails beneath its prompt.
fn task_images(ix: usize, task: &PromptTask, cx: &App) -> Option<AnyElement> {
    let project_dir = ProjectDirectory::get(cx);
    attached_image::prompt_thumbnails(
        ("prompt-images", ix),
        &task.sent.attached_images,
        project_dir.as_deref(),
        attached_image::PROMPT_THUMBNAIL,
        cx,
    )
    .map(|row| div().pt_2().child(row).into_any_element())
}

/// A task in the expanded list, on one line: its title, then the start of its
/// prompt as typed.
fn task_summary(id: (&'static str, usize), ix: usize, task: &PromptTask, cx: &App) -> AnyElement {
    let summary = h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .gap_3()
        .child(div().flex_none().child(task_title(ix, task, false, cx)))
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
        OutputRow::Notice(notice) => muted(notice.summary.clone().into()),
        OutputRow::Sent(text) => task_table::sent_message(("ask-row-sent", id), text, cx),
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
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

    use super::{OutputRow, PromptMode, PromptTask, Reply, Session, TaskStatus};
    use crate::harness::HarnessEvent;
    use crate::hidden_anchor::{self, HiddenAnchor};
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::prompt_history::{self, RunRecord, SavedPrompt};
    use crate::prompt_queue;
    use crate::task_table::{RAW_LINE_CHARS, ReplyPart, ToolState};

    fn tool(id: &str, name: &str) -> HarnessEvent {
        HarnessEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
        }
    }

    /// The Send to Spec or Send to Code button `id` in a frame drawn now:
    /// none when there is none, else what its tooltip says, which is "Send
    /// to Spec" or "Send to Code" only while it is enabled.
    fn other_mode_button_at(
        handle: AnyWindowHandle,
        id: (&'static str, usize),
        cx: &mut TestAppContext,
    ) -> Option<String> {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let button = window.try_find(id)?;
            Some(button.label().expect("the button has a label").to_string())
        })
        .unwrap()
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

    /// While a task from the Code, Chain, or Spec tab runs, the referenced
    /// spec sidebar slides out from the task view's right edge, over the task
    /// view, which keeps its width meanwhile, lists the spec files the task
    /// references, opens one clicked, and slides back once the run is over.
    /// A question never shows it.
    /// The sidebar's understanding shows the running task's understanding
    /// file as the harness writes it, a row per list item, beneath the
    /// referenced files, which take only the height their placeholder needs,
    /// and filling the rest; a row whose link points to a missing file does
    /// nothing, and one whose file exists opens it.
    #[gpui_kit::test]
    async fn referenced_spec_shows_the_understanding(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-understanding-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n",
        )
        .unwrap();
        std::fs::write(dir.join("spec/a.pi"), "a: 1\n").unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let dir = prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap());
        let file = crate::understanding::path(&dir.join(".suspense/history/1-Prompt_a.pi"));
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Change a".into(), cx);
            this.tasks[ix].mode = Some(crate::chat_input::SendMode::Code);
            this.tasks[ix].status = TaskStatus::Running;
            this.tasks[ix]._understanding_watch =
                PromptMode::watch_understanding(ix, file.clone(), dir.clone(), cx);
            this.set_working(true);
            cx.notify();
        });
        let settle = |cx: &mut TestAppContext| {
            cx.executor().advance_clock(Duration::from_millis(300));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(400));
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };
        settle(cx);
        // With nothing to show yet, it stays in.
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("referenced-files").is_none());
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| assert!(!this.refs_shown));

        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "Notes that aren't a list item.\n\
             - [A is one](spec/a.pi)\n\
             - [B is two](spec/missing.pi)\n",
        )
        .unwrap();
        settle(cx);
        // Beside the sidebar, the chat input's buttons end 8 pixels short of
        // its left edge, as they do the window's.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let sidebar = window.find("referenced-files").bounds();
            let body = window.find("body-tint").bounds();
            let rightmost = ["send", "send-options", "queue", "preview"]
                .map(|id| window.find(id).bounds().right())
                .into_iter()
                .fold(gpui_kit::px(0.), gpui_kit::Pixels::max);
            assert_eq!(
                body.right(),
                sidebar.left(),
                "the chat input doesn't meet the sidebar"
            );
            assert_eq!(
                sidebar.left() - rightmost,
                gpui_kit::px(8.),
                "the chat input's buttons aren't 8 pixels short of the sidebar"
            );
        })
        .unwrap();
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find(("understanding-row", 1usize)).is_some());
            assert!(window.try_find(("understanding-row", 2usize)).is_none());
            let sidebar = window.find("referenced-files").bounds();
            let files = window.find("referenced-files-panel").bounds();
            let understanding = window.find("understanding-panel").bounds();
            assert_eq!(
                files.size.height,
                crate::referenced_spec::files_height(0),
                "the referenced files, with none yet, don't fit their placeholder"
            );
            assert_eq!(
                (understanding.top(), understanding.bottom()),
                (files.bottom(), sidebar.bottom()),
                "the understanding {understanding:?} doesn't fill the sidebar {sidebar:?} \
                 beneath the referenced files {files:?}"
            );
            assert!(
                window.try_find("referenced-spec-split").is_none(),
                "the panels are split some other way than by their edge"
            );
        })
        .unwrap();

        // Dragging the edge between the panels resizes them: the referenced
        // files grow as the understanding shrinks, still filling the sidebar.
        let bounds = |cx: &mut TestAppContext, id: &'static str| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.find(id).bounds()
            })
            .unwrap()
        };
        let start = bounds(cx, "referenced-files-resize").center();
        let end = start + gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(100.));
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_mouse_move(start, None, Default::default());
        visual.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
        let halfway = start + gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(50.));
        visual.simulate_mouse_move(halfway, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(end, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        let (sidebar, files, understanding) = (
            bounds(cx, "referenced-files"),
            bounds(cx, "referenced-files-panel"),
            bounds(cx, "understanding-panel"),
        );
        assert!(
            (files.bottom() - end.y).abs() <= gpui_kit::px(1.),
            "the referenced files {files:?} weren't dragged to {end:?}"
        );
        assert_eq!(
            (understanding.top(), understanding.bottom()),
            (files.bottom(), sidebar.bottom())
        );
        // Kept while the application runs.
        assert!(prompt_mode.read_with(cx, |this, _| this.refs_panels.files().is_some()));
        // Dragged far down, the understanding keeps its header and a row.
        let start = bounds(cx, "referenced-files-resize").center();
        let far = gpui_kit::point(start.x, sidebar.bottom() + gpui_kit::px(200.));
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_mouse_move(start, None, Default::default());
        visual.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(far, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(far, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        let understanding = bounds(cx, "understanding-panel");
        assert!(
            understanding.size.height
                >= crate::referenced_spec::PANEL_MIN_HEIGHT - gpui_kit::px(1.),
            "the understanding {understanding:?} was pushed out"
        );
        // Double-clicked, the edge puts both back.
        let at = bounds(cx, "referenced-files-resize").center();
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_click(at, Default::default());
        visual.simulate_event(gpui_kit::MouseDownEvent {
            position: at,
            button: gpui_kit::MouseButton::Left,
            click_count: 2,
            ..Default::default()
        });
        visual.simulate_event(gpui_kit::MouseUpEvent {
            position: at,
            button: gpui_kit::MouseButton::Left,
            click_count: 2,
            ..Default::default()
        });
        cx.run_until_parked();
        assert_eq!(
            bounds(cx, "referenced-files-panel").size.height,
            crate::referenced_spec::files_height(0),
            "double-clicking the edge didn't put the panels back"
        );
        assert!(prompt_mode.read_with(cx, |this, _| this.refs_panels.files().is_none()));

        prompt_mode.read_with(cx, |this, _| {
            let rows = &this.tasks.last().unwrap().understanding.rows;
            assert_eq!(rows[0].text, "A is one");
            assert!(rows[0].target.is_some() && rows[1].linked && rows[1].target.is_none());
        });

        cx.update_window(handle, |_, window, cx| {
            window.click(("understanding-row", 1usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.open_file_view().is_none(), "a missing file opened")
        });
        cx.update_window(handle, |_, window, cx| {
            window.click(("understanding-row", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, _| {
            assert!(
                this.open_file_view().is_some(),
                "the linked file didn't open"
            )
        });
    }

    /// As the mockup has it, in dark and light mode alike: each panel's
    /// header 32 pixels tall on the darkest surface, the body on the ribbon's
    /// command area colour, no line anywhere, not even along the sidebar's
    /// left edge; the referenced files 24 pixels tall inside 8 pixels of
    /// padding, with no icon but the pencil of an edited file; each
    /// understanding row a softly rounded box on the ribbon's tab row colour,
    /// 32 pixels tall, 8 pixels in and 4 apart. The referenced files take only
    /// the height their rows need, up to half the sidebar's, then scroll.
    #[gpui_kit::test]
    async fn referenced_spec_takes_the_mockups_look(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::{point, px};
        let dir = std::env::temp_dir().join(format!("suspense-refs-look-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n",
        )
        .unwrap();
        for ix in 0..40 {
            std::fs::write(dir.join(format!("spec/f{ix}.pi")), "a: 1\n").unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let dir = prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap());
        let file = crate::understanding::path(&dir.join(".suspense/history/1-Prompt_a.pi"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "- The button should be blue\n- Other things\n").unwrap();
        let read = |this: &mut PromptMode,
                    ix: usize,
                    file: usize,
                    tool: &str,
                    cx: &mut gpui_kit::Context<PromptMode>| {
            let path = dir.join(format!("spec/f{file}.pi"));
            this.apply_event(
                ix,
                HarnessEvent::ToolCalled {
                    id: format!("t{file}"),
                    name: tool.into(),
                    input: serde_json::json!({ "file_path": path }),
                    subagent: false,
                },
                cx,
            );
        };
        let ix = prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Change a".into(), cx);
            this.tasks[ix].mode = Some(crate::chat_input::SendMode::Code);
            this.tasks[ix].status = TaskStatus::Running;
            this.tasks[ix].spec_dir = Some(dir.join("spec"));
            this.tasks[ix]._understanding_watch =
                PromptMode::watch_understanding(ix, file.clone(), dir.clone(), cx);
            read(this, ix, 0, "Read", cx);
            read(this, ix, 1, "Edit", cx);
            this.set_working(true);
            cx.notify();
            ix
        });
        let settle = |cx: &mut TestAppContext| {
            cx.executor().advance_clock(Duration::from_millis(300));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(400));
            // The rows' highlight, just added, is over.
            prompt_mode.update(cx, |this, _| {
                for row in &mut this.tasks[ix].understanding.rows {
                    row.changed = None;
                }
            });
            for _ in 0..3 {
                cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                    .unwrap();
            }
        };
        settle(cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[ix].understanding.rows.len(), 2)
        });

        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            settle(cx);
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let palette = crate::theme::palette(cx);
                let frame = crate::frame_image::Frame::of(window);
                let bounds = |id: &'static str| window.find(id).bounds();
                let sidebar = bounds("referenced-files");
                let (files, understanding) = (
                    bounds("referenced-files-panel"),
                    bounds("understanding-panel"),
                );
                let headers = [
                    bounds("referenced-files-header"),
                    bounds("understanding-header"),
                ];
                let rows = [0usize, 1].map(|ix| window.find(("referenced-file", ix)).bounds());
                let this = prompt_mode.read(cx);
                assert_eq!(
                    (
                        this.refs_scroll.max_offset().y,
                        this.understanding_scroll.max_offset().y
                    ),
                    (px(0.), px(0.)),
                    "{mode:?}: a panel whose rows fit scrolls"
                );
                let boxes = [0usize, 1].map(|ix| window.find(("understanding-row", ix)).bounds());
                assert!(
                    window.try_find(("referenced-file", 2usize)).is_none(),
                    "{mode:?}"
                );

                // The headers, 32 tall on the darkest surface, above the
                // bodies on the command area colour.
                assert_eq!(headers[0].top(), sidebar.top(), "{mode:?}");
                assert_eq!(headers[1].top(), files.bottom(), "{mode:?}");
                for header in headers {
                    assert_eq!(header.size.height, px(32.), "{mode:?}: header");
                    assert_eq!(header.size.width, sidebar.size.width, "{mode:?}");
                    assert_eq!(
                        frame.at(point(header.left() + px(2.), header.top() + px(2.))),
                        palette.darkest,
                        "{mode:?}: header"
                    );
                    // Its last pixel row is its panel body's colour.
                    assert_eq!(
                        frame.at(point(header.left() + px(40.), header.bottom() - px(0.5))),
                        palette.ribbon,
                        "{mode:?}: the header's bottom border isn't the body's colour"
                    );
                    assert_eq!(
                        frame.at(point(header.left() + px(40.), header.bottom() - px(1.5))),
                        palette.darkest,
                        "{mode:?}: the header's border is more than a pixel"
                    );
                }
                assert_eq!(
                    frame.at(point(
                        sidebar.right() - px(3.),
                        headers[0].bottom() + px(3.)
                    )),
                    palette.ribbon,
                    "{mode:?}: the referenced files' body"
                );
                assert_eq!(
                    frame.at(point(sidebar.left() + px(40.), sidebar.bottom() - px(20.))),
                    palette.ribbon,
                    "{mode:?}: the understanding's body"
                );

                // Two rows of 24, 8 in, fitting the panel.
                assert_eq!(rows[0].top(), headers[0].bottom() + px(8.), "{mode:?}");
                assert_eq!(rows[1].top(), rows[0].bottom(), "{mode:?}");
                for row in rows {
                    assert_eq!(row.size.height, px(24.), "{mode:?}: row");
                    assert_eq!(
                        (row.left() - sidebar.left(), sidebar.right() - row.right()),
                        (px(8.), px(8.)),
                        "{mode:?}: row"
                    );
                }
                assert_eq!(files.bottom(), rows[1].bottom() + px(8.), "{mode:?}");
                assert_eq!(
                    files.size.height,
                    crate::referenced_spec::files_height(2),
                    "{mode:?}"
                );
                assert_eq!(understanding.bottom(), sidebar.bottom(), "{mode:?}");

                // The boxes, 32 tall, 8 in and 4 apart, on the tab row
                // colour, rounded.
                assert_eq!(boxes[0].top(), headers[1].bottom() + px(8.), "{mode:?}");
                assert_eq!(boxes[1].top(), boxes[0].bottom() + px(4.), "{mode:?}");
                for bx in boxes {
                    assert_eq!(bx.size.height, px(32.), "{mode:?}: box");
                    assert_eq!(
                        (bx.left() - sidebar.left(), sidebar.right() - bx.right()),
                        (px(8.), px(8.)),
                        "{mode:?}: box"
                    );
                    assert_eq!(
                        frame.at(point(bx.left() + px(4.), bx.top() + px(16.))),
                        palette.ribbon_tabs,
                        "{mode:?}: box"
                    );
                }
                let scale = window.scale_factor();
                assert!(
                    window.painted_quads().iter().any(|quad| {
                        let (x, y) = (
                            quad.bounds.origin.x.0 / scale,
                            quad.bounds.origin.y.0 / scale,
                        );
                        (x - boxes[0].left().as_f32()).abs() < 0.5
                            && (y - boxes[0].top().as_f32()).abs() < 0.5
                            && (quad.corner_radii.top_left.0 / scale - 4.).abs() < 0.5
                    }),
                    "{mode:?}: the box isn't rounded"
                );

                // No line anywhere: down it just inside the edge it is
                // dragged by (the one line, where it meets the task view,
                // which the split draws), down it clear of the text, and
                // across its headers, rows and boxes, only the surfaces it is
                // drawn on; and nothing in it has a border.
                let surfaces = [palette.darkest, palette.ribbon, palette.ribbon_tabs];
                let surface = |x: gpui_kit::Pixels, y: gpui_kit::Pixels| {
                    let at = frame.at(point(x, y));
                    assert!(
                        surfaces.contains(&at),
                        "{mode:?}: {at:06x} at {x:?}, {y:?} isn't a surface of the sidebar"
                    );
                };
                for x in [
                    sidebar.left() + px(1.5),
                    sidebar.left() + px(4.5),
                    sidebar.right() - px(1.5),
                ] {
                    let mut y = sidebar.top() + px(0.5);
                    while y < sidebar.bottom() {
                        surface(x, y);
                        y += px(1.);
                    }
                }
                for y in [
                    headers[0].top() + px(2.5),
                    rows[0].top() + px(2.5),
                    headers[1].bottom() - px(0.5),
                    boxes[0].top() + px(2.5),
                    boxes[0].bottom() + px(2.5),
                ] {
                    let mut x = sidebar.left() + px(1.5);
                    while x < sidebar.right() {
                        surface(x, y);
                        x += px(1.);
                    }
                }
                for quad in window.painted_quads() {
                    let (x, y) = (
                        quad.bounds.origin.x.0 / scale,
                        quad.bounds.origin.y.0 / scale,
                    );
                    let inside = x >= sidebar.left().as_f32() - 0.5
                        && x < sidebar.right().as_f32()
                        && y >= sidebar.top().as_f32()
                        && y < sidebar.bottom().as_f32();
                    let widths = &quad.border_widths;
                    // A header's bottom border, in its body's colour, is no
                    // line.
                    let body_border = widths.top.0 == 0.
                        && widths.left.0 == 0.
                        && widths.right.0 == 0.
                        && quad.border_color == crate::theme::color(palette.ribbon);
                    assert!(
                        !inside
                            || body_border
                            || [widths.top, widths.right, widths.bottom, widths.left]
                                .iter()
                                .all(|width| width.0 == 0.)
                            || quad.border_color.a == 0.,
                        "{mode:?}: a quad at {x}, {y} has a border"
                    );
                }
            })
            .unwrap();
        }

        // Many more files than half the sidebar holds: the referenced files
        // take half its height and scroll, the understanding the rest. In
        // dark mode, on the body's #222222.
        cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
        prompt_mode.update(cx, |this, cx| {
            for file in 2..40 {
                read(this, ix, file, "Read", cx);
            }
            cx.notify();
        });
        settle(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let bounds = |id: &'static str| window.find(id).bounds();
            let sidebar = bounds("referenced-files");
            let (files, understanding) = (
                bounds("referenced-files-panel"),
                bounds("understanding-panel"),
            );
            assert!(
                crate::referenced_spec::files_height(40) > sidebar.size.height / 2.,
                "the window is too tall for the test: {sidebar:?}"
            );
            assert!(
                (files.size.height - sidebar.size.height / 2.).abs() <= px(0.5),
                "the referenced files {files:?} aren't half the sidebar {sidebar:?}"
            );
            assert_eq!(
                (understanding.top(), understanding.bottom()),
                (files.bottom(), sidebar.bottom())
            );
            let list = bounds("referenced-files-list");
            assert!(
                list.bottom() <= files.bottom() + px(0.5),
                "the list {list:?} runs out of its panel {files:?}"
            );
            prompt_mode.read_with(cx, |this, _| {
                assert!(
                    this.refs_scroll.max_offset().y > px(0.),
                    "the referenced files don't scroll"
                )
            });
            // Now that they overflow, the same scrollbar as every other runs
            // down their right, laid over their body's surface: its line, white
            // laid over the body, down both its sides, and the track, the
            // body's own colour, inset between. Scrolled to the top, the thumb
            // is at the track's top.
            let frame = crate::frame_image::Frame::of(window);
            let body = crate::theme::palette(cx).ribbon;
            let colors = crate::scrollbar::scroll_colors(true);
            let column = bounds("referenced-files-scroll-column");
            let track = bounds("referenced-files-scroll-track");
            assert_eq!(column.right(), sidebar.right());
            let at = |x: f32, y: gpui_kit::Pixels| frame.at(point(column.left() + px(x + 0.5), y));
            let low = track.bottom() - px(2.5);
            let c: gpui_kit::Rgba = colors.raised.into();
            let channel = ((body >> 16) & 0xff) as f32;
            let raised = ((channel * (1. - c.a) + 255. * c.r * c.a).round() as u32) * 0x010101;
            let near = |a: u32, b: u32| (a as i32 - b as i32).abs() <= 0x010101;
            for y in [
                column.top() + px(2.5),
                track.top() + track.size.height / 2.,
                low,
            ] {
                let (left, right) = (at(0., y), at(17., y));
                assert!(
                    near(left, raised),
                    "the column's left side at {y:?} is {left:06x}"
                );
                assert!(
                    near(right, raised),
                    "the column's right side at {y:?} is {right:06x}"
                );
            }
            assert_eq!(at(8., low), body, "the track isn't the body's colour");
        })
        .unwrap();

        // An understanding with no list items has nothing to scroll, nor a
        // scrollbar.
        std::fs::write(&file, "Notes that aren't a list item.\n").unwrap();
        settle(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("understanding-row", 0usize)).is_none());
            assert!(window.try_find("understanding-scroll-column").is_none());
            prompt_mode.read_with(cx, |this, _| {
                assert_eq!(
                    this.understanding_scroll.max_offset().y,
                    px(0.),
                    "the empty understanding scrolls"
                )
            });
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[gpui_kit::test]
    async fn referenced_spec_slides_out_while_a_task_runs(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n",
        )
        .unwrap();
        std::fs::write(dir.join("spec/a.pi"), "a: 1\n").unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let dir = prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap());
        let frame = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };
        frame(cx);

        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Change a".into(), cx);
            this.tasks[ix].mode = Some(crate::chat_input::SendMode::Code);
            this.tasks[ix].spec_dir = Some(dir.join("spec"));
            for event in [
                tool("t1", "Read"),
                HarnessEvent::ToolCalled {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({ "file_path": dir.join("spec/a.pi") }),
                    subagent: false,
                },
                tool("t2", "Bash"),
                HarnessEvent::ToolCalled {
                    id: "t2".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({ "command": "cat src/main.rs spec/a.pi" }),
                    subagent: false,
                },
            ] {
                this.apply_event(ix, event, cx);
            }
            this.set_working(true);
            cx.notify();
        });

        // It grows from the task view's right edge, through widths in between,
        // pushing the task view narrower as it does, frame by frame.
        let mut widths = Vec::new();
        let mut history_widths = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(350) {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find(("refs-slide", 1usize)).map(|pane| {
                        let history = window.find("history").bounds();
                        history_widths.push(history.size.width);
                        (pane.bounds(), history)
                    })
                })
                .unwrap();
            if let Some((pane, history)) = found {
                assert!(
                    (pane.left() - history.right()).abs() <= gpui_kit::px(2.),
                    "the sidebar {pane:?} doesn't sit against the task view's right edge {history:?}"
                );
                widths.push(pane.size.width);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        let widest = widths
            .iter()
            .copied()
            .fold(gpui_kit::px(0.), gpui_kit::Pixels::max);
        assert!(
            widths
                .iter()
                .any(|width| *width > gpui_kit::px(1.) && *width < widest - gpui_kit::px(1.)),
            "the sidebar {widths:?} appeared without sliding out"
        );
        assert!(
            history_widths
                .windows(2)
                .all(|pair| pair[1] <= pair[0] + gpui_kit::px(0.5)),
            "the task view {history_widths:?} widened as the sidebar slid out"
        );
        let (widest_view, narrowest_view) = (
            history_widths
                .iter()
                .copied()
                .fold(gpui_kit::px(0.), gpui_kit::Pixels::max),
            history_widths
                .iter()
                .copied()
                .fold(gpui_kit::px(f32::MAX), gpui_kit::Pixels::min),
        );
        assert!(
            history_widths.iter().any(|width| {
                *width < widest_view - gpui_kit::px(1.)
                    && *width > narrowest_view + gpui_kit::px(1.)
            }),
            "the task view {history_widths:?} didn't narrow through widths in between"
        );

        // Settled, it is 260 pixels wide, beside the task view, listing the
        // spec file read and not the code.
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..3 {
            cx.run_until_parked();
            frame(cx);
        }
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let refs = window.find("referenced-files").bounds();
            let history = window.find("history").bounds();
            assert!(
                (refs.size.width - gpui_kit::px(260.)).abs() <= gpui_kit::px(2.),
                "the sidebar is {refs:?}"
            );
            assert!(
                refs.left() >= history.right() - gpui_kit::px(1.),
                "{refs:?} isn't right of {history:?}"
            );
            // The chat input, beneath the space above it, stays left of the
            // sidebar, which runs down beside it rather than above it.
            let body = window.find("prompt-body").bounds();
            assert!(
                refs.left() >= body.right() - gpui_kit::px(1.),
                "{refs:?} isn't right of the body and chat input {body:?}"
            );
            assert!(
                refs.bottom() > body.bottom() + gpui_kit::px(20.),
                "the sidebar {refs:?} stops above the chat input, beneath {body:?}"
            );
            assert!(window.try_find(("referenced-file", 0usize)).is_some());
            assert!(
                window.try_find(("referenced-file", 1usize)).is_none(),
                "a code file is listed"
            );
        })
        .unwrap();

        // Clicking the file opens it.
        cx.update_window(handle, |_, window, cx| {
            window.click(("referenced-file", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.open_file_view().is_some(), "the file didn't open")
        });

        // Once the run is over, it slides back and is gone.
        prompt_mode.update(cx, |this, cx| {
            this.set_working(false);
            cx.notify();
        });
        let mut widths = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(600) {
            let found = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window
                        .try_find(("refs-slide-out", 1usize))
                        .map(|pane| pane.bounds().size.width)
                })
                .unwrap();
            widths.extend(found);
            std::thread::sleep(Duration::from_millis(8));
        }
        assert!(
            widths
                .iter()
                .any(|width| *width > gpui_kit::px(1.) && *width < widths[0] - gpui_kit::px(1.)),
            "the sidebar {widths:?} closed without sliding back"
        );
        frame(cx);
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("referenced-files").is_none());
        })
        .unwrap();

        // A question running shows no sidebar.
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Why?".into(), cx);
            this.tasks[ix].mode = Some(crate::chat_input::SendMode::Ask);
            this.set_working(true);
            cx.notify();
        });
        frame(cx);
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find(("refs-slide", 2usize)).is_none());
            assert!(window.try_find("referenced-files").is_none());
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
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

    /// Each project keeps the chat input's tab it was on: one opened for the
    /// first time starts on Chain, switching back selects the tab it was
    /// left on, Ask's conversation with it, and a queued prompt being edited
    /// keeps its tab until the edit is over.
    #[gpui_kit::test]
    async fn each_project_keeps_its_mode_tab(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let (a, b) = two_projects("mode-tab");
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.run_until_parked();
        let chat = prompt_mode.read_with(cx, |this, _| this.chat_input_view());
        let mode = |cx: &mut TestAppContext| chat.read_with(cx, |input, _| input.mode());
        assert_eq!(mode(cx), SendMode::Both);

        cx.update_window(handle, |_, window, cx| {
            chat.update(cx, |input, cx| input.select_mode(SendMode::Ask, window, cx))
        })
        .unwrap();
        cx.run_until_parked();
        assert!(prompt_mode.read_with(cx, |this, _| this.on_ask_tab));

        // Beta, opened for the first time, starts on Chain.
        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        assert_eq!(mode(cx), SendMode::Both);
        assert!(!prompt_mode.read_with(cx, |this, _| this.on_ask_tab));
        cx.update_window(handle, |_, window, cx| {
            chat.update(cx, |input, cx| {
                input.select_mode(SendMode::Code, window, cx)
            })
        })
        .unwrap();

        // Alpha comes back on Ask, its conversation with it; beta on Code.
        cx.update(|cx| ProjectDirectory::set(a.clone(), cx));
        cx.run_until_parked();
        assert_eq!(mode(cx), SendMode::Ask);
        assert!(prompt_mode.read_with(cx, |this, _| this.on_ask_tab));
        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        assert_eq!(mode(cx), SendMode::Code);
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
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
                    this.working = crate::chat_input::Lanes::ALL;
                    this.apply_event(ix, HarnessEvent::TextStarted, cx);
                    let question = this.start_test_question("Ask in alpha", cx);
                    (ix, question)
                })
            })
            .unwrap();

        cx.update(|cx| ProjectDirectory::set(b.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, cx| {
            assert!(this.tasks.is_empty() && this.asks.is_empty() && !this.working.any());
            assert!(this.is_working(), "alpha's work stopped counting");
            let busy = this.busy_projects();
            assert_eq!(busy.len(), 1);
            assert_eq!(busy[0].project_dir, a);
            assert_eq!(busy[0].tasks, [(task, "Work in alpha".into())]);
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
            assert!(this.working.any());
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
                OutputRow::Notice(notice) => format!("notice {}", notice.summary),
                OutputRow::Sent(text) => format!("sent {text}"),
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

    /// The stretch gives nearly all of a scroll at first, less the further
    /// it goes, and reaches its most only as the build-up nears the barrier.
    #[test]
    fn the_stretch_stiffens_up_to_the_barrier() {
        use super::{SCROLL_BARRIER, STRETCH_PULL, ScrollPush};
        let now = std::time::Instant::now();
        let pull = |amount: f32| {
            f32::from(
                ScrollPush {
                    up: true,
                    amount,
                    at: now,
                }
                .pull(now),
            )
        };
        assert!(pull(5.) > 4.5, "it resists from the start");
        // Still giving, however far it has gone, up to the barrier.
        assert!(pull(SCROLL_BARRIER * 0.5) + 1. < pull(SCROLL_BARRIER * 0.75));
        assert!(pull(SCROLL_BARRIER * 0.75) < STRETCH_PULL - 0.5);
        assert!(pull(SCROLL_BARRIER) >= STRETCH_PULL - 0.5);
        assert!(pull(SCROLL_BARRIER * 2.) <= STRETCH_PULL);
    }

    /// A task from the history replays its recorded output into the task it
    /// was; one with no record is shown as not recorded, with nothing pending.
    /// A wheel scroll up from the top of the latest task's output pulls it
    /// into the stretch there, with no pause, and past the barrier expands
    /// the previous tasks, and a scroll down from their bottom brings the
    /// latest task back; a fling's momentum, a scroll carried on from a
    /// slide, or a sideways one never crosses over.
    #[gpui_kit::test]
    async fn scrolling_past_an_end_crosses_over(cx: &mut TestAppContext) {
        use super::SCROLL_REST;
        use gpui_kit::{ScrollDelta, ScrollWheelEvent, point, px};
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            for text in ["first", "second"] {
                let ix = this.push_task(text.into(), cx);
                this.tasks[ix].status = TaskStatus::Done;
            }
            cx.notify();
        });
        let wheel_in = |cx: &mut TestAppContext, x: f32, y: f32, touch_phase| {
            let at = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.viewport_size()
                })
                .unwrap();
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_event(ScrollWheelEvent {
                position: point(at.width / 2., at.height / 3.),
                delta: ScrollDelta::Pixels(point(px(x), px(y))),
                touch_phase,
                ..Default::default()
            });
            cx.run_until_parked();
        };
        let wheel = |cx: &mut TestAppContext, x: f32, y: f32| {
            wheel_in(cx, x, y, gpui_kit::TouchPhase::Moved)
        };
        let header_top = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.find("task-header").bounds().top()
            })
            .unwrap()
        };
        // Long enough for a slide to settle and the wheel to rest after it.
        let rest =
            || std::thread::sleep(super::PANE_SLIDE_TIME + SCROLL_REST + Duration::from_millis(50));
        let sliding = |cx: &mut TestAppContext, n: usize| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(("scroll-slide-view", n)).is_some()
            })
            .unwrap()
        };
        let click_row = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("history-toggle", cx)
            })
            .unwrap();
            cx.run_until_parked();
        };
        let expanded = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.task_history.expanded)
        };

        let glowing = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("scroll-glow").is_some()
            })
            .unwrap()
        };

        let resting = header_top(cx);
        // Sideways, it stays.
        wheel(cx, 40., 5.);
        assert!(!expanded(cx));
        assert!(!glowing(cx));
        // Carried straight on, with no pause, short of the barrier, it
        // builds up and glows, pulling the view down by less than it
        // scrolled.
        wheel(cx, 0., 120.);
        assert!(!expanded(cx), "it crossed over before the barrier");
        assert!(glowing(cx), "no glow as it built up");
        let pulled = header_top(cx) - resting;
        assert!(
            pulled > px(0.) && pulled < px(super::STRETCH_PULL),
            "the stretch pulled the view {pulled:?}"
        );
        // Left there, it drains away and springs back, and a new scroll
        // starts again.
        std::thread::sleep(super::SCROLL_DRAIN + Duration::from_millis(50));
        assert!(!glowing(cx), "the glow didn't drain away");
        assert_eq!(header_top(cx), resting, "the view didn't spring back");
        // A fling's momentum, once the fingers lift, stops at the end.
        wheel_in(cx, 0., 0., gpui_kit::TouchPhase::Ended);
        wheel(cx, 0., 240.);
        assert!(!expanded(cx), "momentum crossed over");
        assert!(!glowing(cx), "momentum pulled into the stretch");
        std::thread::sleep(SCROLL_REST + Duration::from_millis(20));
        wheel(cx, 0., 120.);
        assert!(!expanded(cx), "what drained away still counted");
        // Scrolled back, it goes down; scrolled on, past the barrier, it
        // crosses over.
        wheel(cx, 0., -60.);
        wheel(cx, 0., 680.);
        assert!(!expanded(cx), "scrolling back didn't take it down");
        wheel(cx, 0., 60.);
        assert!(
            expanded(cx),
            "a new scroll up past the barrier didn't open the previous tasks"
        );
        assert!(sliding(cx, 0), "crossing over didn't slide");
        // While it slides, the wheel doesn't cross back.
        wheel(cx, 0., -40.);
        assert!(expanded(cx), "a scroll while sliding crossed back");
        rest();
        assert!(!sliding(cx, 0), "the slide didn't settle");
        assert!(!glowing(cx), "the glow didn't fade");
        for _ in 0..6 {
            wheel(cx, 0., -120.);
        }
        assert!(expanded(cx), "it crossed back short of the barrier");
        wheel(cx, 0., -120.);
        assert!(
            !expanded(cx),
            "a new scroll down past the barrier didn't bring back the latest task"
        );
        assert!(sliding(cx, 1), "crossing back didn't slide");
        rest();
        assert!(!sliding(cx, 1));

        // A steady scroll, a wheel's notches a little apart, never drains,
        // and crosses over without stopping, each way.
        let steady = |cx: &mut TestAppContext, y: f32| {
            for _ in 0..13 {
                wheel(cx, 0., y);
                std::thread::sleep(Duration::from_millis(150));
            }
        };
        steady(cx, 60.);
        assert!(expanded(cx), "a steady scroll up didn't cross over");
        rest();
        steady(cx, -60.);
        assert!(!expanded(cx), "a steady scroll down didn't cross back");
        rest();

        // Clicking the row isn't animated.
        click_row(cx);
        assert!(expanded(cx));
        assert!(!sliding(cx, 4), "clicking the row slid");
        click_row(cx);
        assert!(!expanded(cx));

        // With no previous tasks, nothing opens.
        prompt_mode.update(cx, |this, cx| {
            this.tasks.truncate(1);
            cx.notify();
        });
        rest();
        let resting = header_top(cx);
        wheel(cx, 0., 240.);
        assert!(!expanded(cx));
        assert!(!glowing(cx), "it glows with nowhere to cross to");
        assert_eq!(
            header_top(cx),
            resting,
            "it stretches with nowhere to cross to"
        );
    }

    /// The scroll that crosses over stops there: neither the step that
    /// crosses nor any more of the same scroll moves the view it switched
    /// to, during the slide or after; only a new scroll, after the wheel has
    /// been still a moment, moves it. So each way.
    #[gpui_kit::test]
    async fn a_scroll_that_crosses_over_stops_there(cx: &mut TestAppContext) {
        use gpui_kit::{ScrollDelta, ScrollWheelEvent, point, px};
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..200).map(|n| format!("Line {n}\n\n")).collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for n in 0..60 {
                    let ix = this.push_task(format!("Task {n}").into(), cx);
                    this.tasks[ix].status = TaskStatus::Done;
                }
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
        let wheel = |cx: &mut TestAppContext, y: f32| {
            let at = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.viewport_size()
                })
                .unwrap();
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_event(ScrollWheelEvent {
                position: point(at.width / 2., at.height / 2.),
                delta: ScrollDelta::Pixels(point(px(0.), px(y))),
                ..Default::default()
            });
            cx.run_until_parked();
        };
        let expanded = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.task_history.expanded)
        };
        // How far the view shown is from the end it arrived at.
        let from_end = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            prompt_mode.read_with(cx, |this, _| {
                if this.task_history.expanded {
                    f32::from(this.task_history.rows.to_bottom())
                } else {
                    f32::from(this.output_table.list().to_top())
                }
            })
        };
        // Scrolling on, a step every 40 milliseconds, until the slide has
        // settled and a little after.
        let carry_on = |cx: &mut TestAppContext, y: f32| {
            let until =
                std::time::Instant::now() + super::PANE_SLIDE_TIME + Duration::from_millis(150);
            while std::time::Instant::now() < until {
                wheel(cx, y);
                std::thread::sleep(Duration::from_millis(40));
            }
        };
        let pause = || std::thread::sleep(super::SCROLL_DRAIN + Duration::from_millis(50));

        for up in [true, false] {
            let y = if up { 200. } else { -200. };
            if up {
                prompt_mode.update(cx, |this, _| this.output_table.scroll_to_top());
            }
            from_end(cx);
            pause();
            for _ in 0..4 {
                wheel(cx, y);
            }
            assert_eq!(expanded(cx), up, "it didn't cross over");
            carry_on(cx, y);
            assert_eq!(expanded(cx), up, "the rest of the scroll crossed back");
            assert!(
                from_end(cx) <= 1.,
                "the rest of the scroll moved the view it switched to, {} from its end",
                from_end(cx)
            );
            // A new scroll the same way moves it on, away from the end it
            // arrived at.
            std::thread::sleep(super::SCROLL_REST + Duration::from_millis(20));
            wheel(cx, y);
            assert!(from_end(cx) > 100., "a new scroll didn't move the view");
            // Back at its end, for the crossing back.
            wheel(cx, -y);
            pause();
        }
    }

    /// A scroll begun with the view already resting at its end pulls into
    /// the stretch from its first step, however the view got there: the
    /// previous tasks opened on their bottom by a click, however many there
    /// are and however few of their rows have been measured, and the latest
    /// task's long output scrolled to its top.
    #[gpui_kit::test]
    async fn a_scroll_begun_at_an_end_stretches_at_once(cx: &mut TestAppContext) {
        use gpui_kit::{ScrollDelta, ScrollWheelEvent, point, px};
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..200).map(|n| format!("Line {n}\n\n")).collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                for n in 0..60 {
                    let text = if n % 3 == 0 {
                        format!("Task {n}\n\nwith\n\nseveral\n\nlines")
                    } else {
                        format!("Task {n}")
                    };
                    let ix = this.push_task(text.into(), cx);
                    this.tasks[ix].status = TaskStatus::Done;
                }
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
        let frame = |cx: &mut TestAppContext| {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };
        let wheel = |cx: &mut TestAppContext, y: f32| {
            let at = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.viewport_size()
                })
                .unwrap();
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_event(ScrollWheelEvent {
                position: point(at.width / 2., at.height / 2.),
                delta: ScrollDelta::Pixels(point(px(0.), px(y))),
                ..Default::default()
            });
            cx.run_until_parked();
        };
        let built_up = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.scroll_push.get().amount)
        };
        // A new scroll, the wheel having been still a while.
        let still = || std::thread::sleep(super::SCROLL_DRAIN + Duration::from_millis(50));

        // The latest task's output, resting at its top.
        prompt_mode.update(cx, |this, _| this.output_table.scroll_to_top());
        frame(cx);
        frame(cx);
        still();
        wheel(cx, 40.);
        assert_eq!(
            built_up(cx),
            40.,
            "a scroll from the output's top didn't stretch"
        );
        still();

        // The previous tasks, opened on their bottom by clicking their row.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("history-toggle", cx)
        })
        .unwrap();
        frame(cx);
        frame(cx);
        assert!(prompt_mode.read_with(cx, |this, _| this.task_history.expanded));
        still();
        wheel(cx, -40.);
        assert_eq!(
            built_up(cx),
            40.,
            "a scroll from the previous tasks' bottom didn't stretch"
        );
    }

    /// A scroll step that brings the output to its top counts whatever of it
    /// goes past the top toward the barrier.
    #[gpui_kit::test]
    async fn a_step_reaching_the_top_builds_up_what_goes_past(cx: &mut TestAppContext) {
        use gpui_kit::{ScrollDelta, ScrollWheelEvent, point, px};
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..200).map(|n| format!("Line {n}\n\n")).collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let first = this.push_task("First".into(), cx);
                this.tasks[first].status = TaskStatus::Done;
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
        let wheel = |cx: &mut TestAppContext, y: f32| {
            let at = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.viewport_size()
                })
                .unwrap();
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_event(ScrollWheelEvent {
                position: point(at.width / 2., at.height / 3.),
                delta: ScrollDelta::Pixels(point(px(0.), px(y))),
                ..Default::default()
            });
            cx.run_until_parked();
        };
        let expanded = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.task_history.expanded)
        };
        prompt_mode.update(cx, |this, _| {
            this.output_table
                .scroll()
                .set_offset(point(px(0.), px(-100.)))
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        // 100 pixels to the top, then 150 past it.
        wheel(cx, 250.);
        assert!(!expanded(cx), "it crossed over before the barrier");
        let top = prompt_mode.read_with(cx, |this, _| this.output_table.scroll().offset().y);
        assert!(top >= px(-1.), "the step didn't take the output to its top");
        wheel(cx, 500.);
        assert!(!expanded(cx), "it crossed over before the barrier");
        wheel(cx, 150.);
        assert!(expanded(cx), "what went past the top didn't build up");
    }

    /// Closed, by a click or a scroll, the previous tasks give back the
    /// latest task's output as it was left: where it was scrolled, or locked
    /// to its bottom and following what arrived meanwhile; with another task
    /// the latest since, at its top, unlocked.
    #[gpui_kit::test]
    async fn the_latest_task_comes_back_as_it_was_left(cx: &mut TestAppContext) {
        use gpui_kit::{point, px};
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..200).map(|n| format!("Line {n}\n\n")).collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let first = this.push_task("First".into(), cx);
                this.tasks[first].status = TaskStatus::Done;
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
        let frames = |cx: &mut TestAppContext| {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
        };
        let click_row = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click("history-toggle", cx)
            })
            .unwrap();
            frames(cx);
        };
        let more = |cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.apply_event(1, HarnessEvent::TextDelta(long.clone()), cx)
            });
        };
        let scrolled = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                let scroll = this.output_table.scroll();
                (scroll.offset().y, scroll.max_offset().y, this.output_locked)
            })
        };
        frames(cx);

        // Unlocked, partway down, it comes back just there.
        prompt_mode.update(cx, |this, _| {
            this.output_table
                .scroll()
                .set_offset(point(px(0.), px(-300.)))
        });
        frames(cx);
        let (left, _, _) = scrolled(cx);
        assert!(left < px(-1.), "the output didn't scroll");
        click_row(cx);
        more(cx);
        click_row(cx);
        assert_eq!(
            scrolled(cx).0,
            left,
            "the output didn't come back where it was"
        );
        assert!(!scrolled(cx).2);

        // Locked, it comes back locked at its bottom, with what arrived.
        prompt_mode.update(cx, |this, cx| this.set_output_lock(true, cx));
        frames(cx);
        click_row(cx);
        more(cx);
        click_row(cx);
        let (offset, max, locked) = scrolled(cx);
        assert!(locked, "the lock didn't come back");
        assert!(
            (offset + max).abs() <= px(1.),
            "locked, it isn't at its bottom: {offset:?} of {max:?}"
        );

        // With another task the latest since, it starts at its top.
        click_row(cx);
        prompt_mode.update(cx, |this, cx| {
            this.push_task("Another".into(), cx);
        });
        click_row(cx);
        let (offset, _, locked) = scrolled(cx);
        assert_eq!(offset, px(0.));
        assert!(!locked);
    }

    /// Each previous task's heading is faintly tinted with its mode's colour;
    /// one whose mode isn't known isn't.
    #[gpui_kit::test]
    async fn previous_tasks_hint_at_their_mode(cx: &mut TestAppContext) {
        use super::HISTORY_MODE_HINT;
        use crate::chat_input::SendMode;
        use gpui_kit::Hsla;
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            for mode in [Some(SendMode::Code), None, Some(SendMode::Spec), None] {
                let ix = this.push_task("Do it".into(), cx);
                this.tasks[ix].mode = mode;
                this.tasks[ix].status = TaskStatus::Done;
            }
            this.task_history.expanded = true;
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let hint = |mode| Hsla {
                a: HISTORY_MODE_HINT,
                ..crate::chat_input::mode_color(mode, cx)
            };
            let painted = |color: Hsla| {
                window
                    .painted_quads()
                    .into_iter()
                    .filter(|quad| quad.background.as_solid() == Some(color))
                    .count()
            };
            assert_eq!(
                painted(hint(SendMode::Code)),
                1,
                "Code's heading isn't tinted"
            );
            assert_eq!(
                painted(hint(SendMode::Spec)),
                1,
                "Spec's heading isn't tinted"
            );
            assert_eq!(painted(hint(SendMode::Both)), 0);
        })
        .unwrap();
    }

    /// A git repository with `kept.txt` and `other.txt` snapshotted before
    /// and after both are changed, for the changed files' tests.
    use super::{ChangedFiles, OpenSnapshotDiff};
    use crate::task_snapshot;
    use gpui_kit::ElementId;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    fn changed_repo(name: &str) -> (PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("kept.txt"), "one\n").unwrap();
        std::fs::write(dir.join("other.txt"), "a\n").unwrap();
        let before = task_snapshot::take(&dir, "Prompt_t", "before").unwrap();
        std::fs::write(dir.join("kept.txt"), "two\n").unwrap();
        std::fs::write(dir.join("other.txt"), "b\n").unwrap();
        let after = task_snapshot::take(&dir, "Prompt_t", "after").unwrap();
        (dir, before, after)
    }

    /// A task lists the files changed while it ran, collapsed to a heading
    /// that counts them; the agent's are plain, the rest marked as changed
    /// otherwise, and clicking one asks for its diff between the snapshots.
    #[gpui_kit::test]
    async fn a_task_lists_the_files_changed_while_it_ran(cx: &mut TestAppContext) {
        let (dir, before, after) = changed_repo("changed-files");
        let changed = ChangedFiles::read(
            dir.clone(),
            before.clone(),
            after.clone(),
            &[dir.join("kept.txt")],
        )
        .unwrap();
        assert_eq!(
            changed
                .files
                .iter()
                .map(|(change, agent)| (change.path.display().to_string(), *agent))
                .collect::<Vec<_>>(),
            [
                ("kept.txt".to_string(), true),
                ("other.txt".to_string(), false)
            ]
        );
        let (prompt_mode, handle) = open(cx);
        let opened = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let opened = opened.clone();
            cx.subscribe(&prompt_mode, move |_, open: &OpenSnapshotDiff, _| {
                opened.borrow_mut().push((
                    open.path.clone(),
                    open.before.clone(),
                    open.after.clone(),
                ))
            })
            .detach();
        });
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Change it".into(), cx);
            this.tasks[ix].status = TaskStatus::Done;
            this.tasks[ix].changed = Some(changed);
            cx.notify();
        });
        let key = super::id_of_latest(0);
        let find = |cx: &mut TestAppContext, id: ElementId| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        assert!(find(cx, ("changed-files-toggle", key).into()));
        assert!(
            !find(cx, ("changed-file", 0usize).into()),
            "listed before it was opened"
        );
        cx.update_window(handle, |_, window, cx| {
            window.click(("changed-files-toggle", key), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(find(cx, ("changed-file", 1usize).into()));
        assert!(find(cx, ("changed-file-elsewhere", 1usize).into()));
        cx.update_window(handle, |_, window, cx| {
            window.click(("changed-file", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            *opened.borrow(),
            [(PathBuf::from("kept.txt"), before, after)]
        );

        // A run that changed nothing says so, with nothing to expand.
        prompt_mode.update(cx, |this, cx| {
            if let Some(changed) = this.tasks[0].changed.as_mut() {
                changed.files.clear();
            }
            cx.notify();
        });
        assert!(find(cx, ("changed-files", key).into()));
        assert!(!find(cx, ("changed-files-toggle", key).into()));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What a Code task changed in the spec is put back, and said so in a
    /// passive notice, not an error: the task is done, and the notice opens
    /// onto the files put back and closes again.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn changes_put_back_are_a_notice(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("put-back-notice", &prompt_mode, cx);
        // A harness that writes into the spec, then is done.
        let script = dir.join("spec-writing-harness.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             IFS= read -r line\n\
             echo stray > spec/stray.pi\n\
             sleep 0.7\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send(
                    "Change the code".into(),
                    SendMode::Code,
                    Vec::new(),
                    window,
                    cx,
                )
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the task and its notice", |this| {
            !this.working.any()
                && this.tasks.first().is_some_and(|task| {
                    task.reply
                        .rows()
                        .iter()
                        .any(|row| matches!(row, OutputRow::Notice(_)))
                })
        });
        crate::harness::use_program_for_test(None);
        assert!(!dir.join("spec/stray.pi").exists(), "the spec file stayed");
        let notice_row = prompt_mode.read_with(cx, |this, _| {
            let task = &this.tasks[0];
            assert_eq!(task.status, TaskStatus::Done, "the notice failed the task");
            assert!(task.reply.errors().is_empty(), "{:?}", task.reply.errors());
            task.reply
                .rows()
                .iter()
                .position(|row| {
                    matches!(row, OutputRow::Notice(notice)
                    if notice.summary == "Put back 1 spec file this Code task changed")
                })
                .expect("no notice of the file put back")
        });
        let find = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        settle_sidebar(cx, handle);
        assert!(find(("notice-summary", notice_row), cx), "no notice shown");
        assert!(!find(("notice-details", notice_row), cx), "it starts open");
        cx.update_window(handle, |_, window, cx| {
            window.click(("notice-summary", notice_row), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(find(("notice-details", notice_row), cx), "it didn't open");
        cx.update_window(handle, |_, window, cx| {
            window.click(("notice-summary", notice_row), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!find(("notice-details", notice_row), cx), "it didn't close");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task is named after the title the harness gives it, saved in the
    /// history under that name, and a resend is named after it, numbered,
    /// without asking again.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn tasks_are_named_after_their_titles(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        fn titled(_: &std::path::Path, prompt: &str) -> anyhow::Result<String> {
            assert_eq!(prompt, "Tidy the queue up");
            Ok("Tidy the queue.".into())
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("titled", &prompt_mode, cx);
        prompt_mode.update(cx, |this, _| this.titler = titled);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send(
                    "Tidy the queue up".into(),
                    SendMode::Freeform,
                    Vec::new(),
                    window,
                    cx,
                )
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the naming", |this| {
            this.tasks
                .first()
                .is_some_and(|task| task.name.as_ref() == "TidyTheQueue")
        });
        prompt_mode.update(cx, |this, cx| this.cancel_task(0, cx));
        run_until(cx, &prompt_mode, "the cancel", |this| !this.working.any());
        let saved = || {
            let mut names: Vec<String> = std::fs::read_dir(dir.join(".suspense/history"))
                .unwrap()
                .filter_map(|entry| {
                    let file = entry.unwrap().file_name().into_string().unwrap();
                    let name = file.split_once('-')?.1.strip_suffix(".pi")?.to_string();
                    Some(name)
                })
                .collect();
            names.sort();
            names
        };
        assert_eq!(saved(), ["TidyTheQueue"]);

        // Resent, it isn't titled again.
        fn untitled(_: &std::path::Path, _: &str) -> anyhow::Result<String> {
            panic!("titled again")
        }
        prompt_mode.update(cx, |this, _| this.titler = untitled);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.resend(|this| &this.tasks, 0, window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the resend's naming", |this| {
            this.tasks
                .get(1)
                .is_some_and(|task| task.name.as_ref() == "TidyTheQueue2")
        });
        prompt_mode.update(cx, |this, cx| this.cancel_task(1, cx));
        run_until(cx, &prompt_mode, "the resend's cancel", |this| {
            !this.working.any()
        });
        assert_eq!(saved(), ["TidyTheQueue", "TidyTheQueue2"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// In a git repository, a task's run is snapshotted as the harness
    /// starts it and as it ends: the file the harness made is listed as
    /// changed during the task, the two trees are pinned under the private
    /// refs and kept in its record, and the repository's own index is left
    /// as it was.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn a_run_is_snapshotted_in_a_git_repository(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("snapshot-run", &prompt_mode, cx);
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "/.suspense/\n").unwrap();
        // A harness that makes a file of its own accord, reporting no tool.
        let script = dir.join("making-harness.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             IFS= read -r line\n\
             echo made > made.txt\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Make it".into(), SendMode::Freeform, Vec::new(), window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the task", |this| {
            !this.working.any()
                && this
                    .tasks
                    .first()
                    .is_some_and(|task| task.changed.is_some())
        });
        let (name, files) = prompt_mode.read_with(cx, |this, _| {
            let task = &this.tasks[0];
            let files: Vec<_> = task
                .changed
                .as_ref()
                .unwrap()
                .files
                .iter()
                .map(|(change, agent)| (change.path.display().to_string(), *agent))
                .collect();
            (task.name.to_string(), files)
        });
        assert!(
            files.contains(&("made.txt".to_string(), false)),
            "made.txt isn't listed as changed during the task: {files:?}"
        );
        let refs = git(&["for-each-ref", "--format=%(refname)", "refs/suspense/"]);
        assert!(
            refs.contains(&format!("refs/suspense/{name}/before")),
            "{refs}"
        );
        assert!(
            refs.contains(&format!("refs/suspense/{name}/after")),
            "{refs}"
        );
        let record = prompt_history::load(&dir)
            .into_iter()
            .find_map(|saved| saved.record)
            .expect("no record was saved");
        assert!(record.snapshot_before.is_some() && record.snapshot_after.is_some());
        // Nothing was staged in the repository's own index.
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "");
        crate::harness::use_program_for_test(None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Restored from the history, a task with both snapshots in its record
    /// lists its changed files again; outside a git repository, none.
    #[test]
    fn restored_tasks_list_their_changed_files() {
        let (dir, before, after) = changed_repo("changed-restore");
        let saved = || SavedPrompt {
            sent_at: 0,
            anchor: HiddenAnchor::random(),
            text: "Do it".into(),
            record: Some(RunRecord {
                user_prompt: Some("Do it".into()),
                snapshot_before: Some(before.clone()),
                snapshot_after: Some(after.clone()),
                ..RunRecord::default()
            }),
        };
        let task = PromptTask::restore_in(saved(), Some(&dir));
        let files = &task.changed.as_ref().unwrap().files;
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|(_, agent)| !agent));
        let elsewhere = std::env::temp_dir();
        assert!(task_snapshot::repo_top(&elsewhere).is_none());
        assert!(
            PromptTask::restore_in(saved(), Some(&elsewhere))
                .changed
                .is_none()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Chain task and the steps it sent straight after, each sent from the
    /// one before, make one chain; a task sent by hand from a step later, or
    /// a code step whose chain task is missing, stands alone. In the
    /// previous tasks, the chain is headed by one parent row, its status the
    /// chain's, with each step still an item beneath it, and a session
    /// divider never falls inside it.
    #[gpui_kit::test]
    async fn chained_tasks_are_grouped_under_one_parent(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use crate::hidden_anchor::CodeTask;
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            let step = |this: &mut PromptMode,
                        mode: SendMode,
                        from: Option<usize>,
                        post_build: bool,
                        status: TaskStatus,
                        session: &str,
                        cx: &mut gpui_kit::Context<PromptMode>| {
                let ix = this.push_task("Build the thing\nin detail".into(), cx);
                let from = from.map(|from| this.tasks[from].name.to_string());
                let task = &mut this.tasks[ix];
                task.sent.mode = Some(mode);
                task.mode = Some(mode);
                task.sent.code_task = from.as_ref().map(|_| CodeTask::default());
                task.sent.sent_from = from;
                task.sent.post_build_update = post_build;
                task.status = status;
                task.session = Some(session.to_string().into());
            };
            step(this, SendMode::Code, None, false, TaskStatus::Done, "a", cx);
            step(this, SendMode::Both, None, true, TaskStatus::Done, "a", cx);
            step(
                this,
                SendMode::Code,
                Some(1),
                true,
                TaskStatus::Failed,
                "b",
                cx,
            );
            step(
                this,
                SendMode::Spec,
                Some(2),
                false,
                TaskStatus::Done,
                "b",
                cx,
            );
            // Sent to Spec by hand from the code step, afterwards.
            step(
                this,
                SendMode::Spec,
                Some(2),
                false,
                TaskStatus::Done,
                "b",
                cx,
            );
            // A code step with no chain before it.
            step(
                this,
                SendMode::Code,
                Some(0),
                false,
                TaskStatus::Done,
                "b",
                cx,
            );
            this.task_history.expanded = true;
            cx.notify();
        });
        prompt_mode.read_with(cx, |this, _| {
            let layout = super::chain_layout(&this.tasks);
            let chains: Vec<_> = (0..this.tasks.len()).map(|ix| layout.step_of(ix)).collect();
            let step = |start, pos, len| Some(super::ChainStep { start, pos, len });
            assert_eq!(
                chains,
                [
                    None,
                    step(1, 0, 3),
                    step(1, 1, 3),
                    step(1, 2, 3),
                    None,
                    None
                ]
            );
            let steps: Vec<_> = this.tasks[1..4].iter().collect();
            assert_eq!(super::chain_status(&steps), TaskStatus::Failed);
            // The session changed at the code step, but the divider goes
            // above the chain.
            assert_eq!(
                super::HistoryList::session_groups(&this.tasks, &layout),
                [0, 1]
            );
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find(("history-chain", 1usize)).is_some(),
                "no parent row"
            );
            assert!(window.try_find(("history-chain", 2usize)).is_none());
            assert!(window.try_find(("history-chain", 4usize)).is_none());
            // Each step is still an item of its own, set in beneath it.
            let parent = window.find(("history-chain", 1usize)).bounds();
            let steps: Vec<_> = (1..4usize)
                .map(|ix| window.find(("history-task", ix)).bounds())
                .collect();
            let alone = window.find(("history-task", 4usize)).bounds();
            assert!(
                steps
                    .iter()
                    .all(|step| step.left() > parent.left() + gpui_kit::px(1.))
            );
            assert!(steps[0].top() >= parent.bottom() - gpui_kit::px(1.));
            assert!(alone.left() < steps[0].left(), "a hand-sent task is set in");
        })
        .unwrap();
    }

    /// The previous tasks' filter bar hides the tasks matching no chip on:
    /// chips in a group combine as either, the groups as both. The row says
    /// how many show, an open task filtered out closes, and with none left
    /// the list says so; Clear shows them all again.
    #[gpui_kit::test]
    async fn previous_tasks_can_be_filtered(cx: &mut TestAppContext) {
        use super::TaskFilter;
        use crate::chat_input::SendMode;
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            for (mode, marked) in [
                (SendMode::Code, false),
                (SendMode::Spec, true),
                (SendMode::Both, false),
                (SendMode::Code, true),
            ] {
                let ix = this.push_task("Do it".into(), cx);
                let task = &mut this.tasks[ix];
                task.mode = Some(mode);
                task.sent.mode = Some(mode);
                task.status = TaskStatus::Done;
                task.marked_done = marked;
            }
            this.task_history.expanded = true;
            this.task_history.open = Some(1);
            cx.notify();
        });
        let render = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
        };
        let visible =
            |cx: &mut TestAppContext| prompt_mode.read_with(cx, |this, _| this.task_visibility());
        render(cx);
        assert!(visible(cx).is_none());
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("task-filters").is_some(), "no filter bar")
        })
        .unwrap();

        // Code, clicked: only Code tasks show, the open Spec task closes.
        cx.update_window(handle, |_, window, cx| {
            window.click(("task-filter", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(visible(cx), Some(vec![true, false, false, true]));
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.task_history.open, None);
        });
        // With Spec as well, either; with Marked done, both.
        prompt_mode.update(cx, |this, cx| {
            this.toggle_task_filter(TaskFilter::Mode(SendMode::Spec), cx);
        });
        assert_eq!(visible(cx), Some(vec![true, true, false, true]));
        prompt_mode.update(cx, |this, cx| {
            this.toggle_task_filter(TaskFilter::MarkedDone, cx)
        });
        assert_eq!(visible(cx), Some(vec![false, true, false, true]));
        render(cx);
        // Of those, the latest task, heading the view, isn't counted.
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.task_history.shown.get(), Some(1))
        });
        // Each chip counts the previous tasks it matches alone: of the two
        // Code tasks, the latest isn't one of the previous.
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.filter_count(TaskFilter::Mode(SendMode::Code)), 1);
            assert_eq!(this.filter_count(TaskFilter::MarkedDone), 1);
        });

        // Nothing matching, the list says so.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.filters = vec![TaskFilter::Mode(SendMode::Freeform)];
            cx.notify();
        });
        render(cx);
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find("no-filtered-tasks").is_some())
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            window.click("clear-task-filters", cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert!(visible(cx).is_none());
    }

    /// Subagents are shown by the mode that started them: a task of its own
    /// has one unlabelled group in its mode's colour; while a chain runs, a
    /// group per step so far, each labelled and coloured by that step's own
    /// mode, never Chain's.
    #[gpui_kit::test]
    async fn subagents_are_shown_by_the_mode_that_started_them(cx: &mut TestAppContext) {
        use crate::chat_input::{SendMode, mode_color};
        use crate::hidden_anchor::CodeTask;
        use crate::subagents::{State, Subagent};
        let (prompt_mode, handle) = open(cx);
        let agent = |id: &str| Subagent {
            id: id.into(),
            description: format!("Look into {id}").into(),
            kind: None,
            activity: None,
            state: State::Running,
        };
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Alone".into(), cx);
            this.tasks[ix].mode = Some(SendMode::Code);
            this.tasks[ix].sent.mode = Some(SendMode::Code);
            this.tasks[ix].subagents.list.push(agent("a"));
        });
        let groups = cx.update(|cx| prompt_mode.read(cx).running_subagents(cx));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, None);
        assert_eq!(groups[0].started_by.as_ref(), "the Code task");
        let code = cx.update(|cx| mode_color(SendMode::Code, cx));
        let spec = cx.update(|cx| mode_color(SendMode::Spec, cx));
        assert_eq!(groups[0].color, Some(code));

        prompt_mode.update(cx, |this, cx| {
            let chain = this.push_task("Both".into(), cx);
            this.tasks[chain].mode = Some(SendMode::Both);
            this.tasks[chain].sent.mode = Some(SendMode::Both);
            this.tasks[chain].status = TaskStatus::Done;
            this.tasks[chain].subagents.list.push(agent("b"));
            let name = this.tasks[chain].name.to_string();
            let step = this.push_task("Both".into(), cx);
            let task = &mut this.tasks[step];
            task.mode = Some(SendMode::Code);
            task.sent.mode = Some(SendMode::Code);
            task.sent.sent_from = Some(name);
            task.sent.code_task = Some(CodeTask::default());
            task.status = TaskStatus::Running;
            task.subagents.list.push(agent("c"));
            this.set_working(true);
            cx.notify();
        });
        let groups = cx.update(|cx| prompt_mode.read(cx).running_subagents(cx));
        let shown: Vec<_> = groups
            .iter()
            .map(|group| {
                (
                    group.label.clone(),
                    group.color,
                    group.started_by.to_string(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (Some("Spec".into()), Some(spec), "the Spec step".to_string()),
                (Some("Code".into()), Some(code), "the Code step".to_string()),
            ]
        );
        cx.executor().advance_clock(Duration::from_millis(600));
        cx.update_window(handle, |_, window, cx| {
            for _ in 0..3 {
                window.render_frame(cx);
            }
            assert!(
                window.try_find("subagent-group-Spec").is_some(),
                "no Spec group"
            );
            assert!(
                window.try_find("subagent-group-Code").is_some(),
                "no Code group"
            );
            assert!(window.try_find(("subagent", 1usize)).is_some());
            // The subagents sit along the bottom, beneath the referenced
            // files and the understanding.
            let sidebar = window.find("referenced-files").bounds();
            let files = window.find("referenced-files-panel").bounds();
            let understanding = window.find("understanding-panel").bounds();
            let agents = window.find("subagents-panel").bounds();
            assert!(files.bottom() <= understanding.top() + gpui_kit::px(1.));
            assert!(understanding.bottom() <= agents.top() + gpui_kit::px(1.));
            assert!((agents.bottom() - sidebar.bottom()).abs() <= gpui_kit::px(1.));
        })
        .unwrap();
    }

    /// Queued prompts sharing a context are joined by a line between their
    /// switches, from the bottom of one to the top of the next; one starting
    /// a new conversation isn't joined to the one above it.
    #[gpui_kit::test]
    async fn queued_context_lines_run_between_the_switches(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-queue-lines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        // The second wraps, so its row is taller than the others.
        for text in [
            "first",
            "second word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word word",
            "third",
        ] {
            prompt_queue::add(HiddenAnchor::random(), text.into(), &dir).unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, cx| {
            this.queue_expanded = true;
            this.queue[2].new_conversation = true;
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let border = gpui_kit::component::ActiveTheme::theme(cx).border;
            let switches: Vec<_> = (0..3usize)
                .map(|ix| window.find(("new-conversation-queued", ix)).bounds())
                .collect();
            // Every 1px line in the line colour, as (top, bottom, centre x).
            let lines: Vec<_> = window
                .painted_quads()
                .into_iter()
                .filter(|quad| {
                    quad.background.as_solid() == Some(border)
                        && (quad.bounds.size.width.0 - window.scale_factor()).abs() < 0.5
                })
                .map(|quad| {
                    let scale = window.scale_factor();
                    let b = quad.bounds;
                    (
                        b.origin.y.0 / scale,
                        (b.origin.y.0 + b.size.height.0) / scale,
                        (b.origin.x.0 + b.size.width.0 / 2.) / scale,
                    )
                })
                .collect();
            let joined = |a: usize, b: usize| {
                let (from, to) = (switches[a].bottom().as_f32(), switches[b].top().as_f32());
                let x = switches[a].center().x.as_f32();
                let covered: f32 = lines
                    .iter()
                    .filter(|(_, _, cx)| (cx - x).abs() < 1.)
                    .map(|(top, bottom, _)| (bottom.min(to) - top.max(from)).max(0.))
                    .sum();
                covered >= to - from - 1.
            };
            // Each switch is centred in its row, however tall.
            for ix in 0..3usize {
                let row = window.find(("queued-prompt", ix)).bounds();
                assert!(
                    (switches[ix].center().y - row.center().y).abs() <= gpui_kit::px(1.),
                    "switch {ix} {:?} isn't centred in its row {row:?}",
                    switches[ix]
                );
            }
            let rows: Vec<_> = (0..2usize)
                .map(|ix| window.find(("queued-prompt", ix)).bounds().size.height)
                .collect();
            assert!(
                rows[1] > rows[0] + gpui_kit::px(4.),
                "the rows aren't of different heights: {rows:?}"
            );
            assert!(joined(0, 1), "the first two switches aren't joined");
            assert!(
                !joined(1, 2),
                "a new conversation is joined to the one above"
            );
            // Nothing is drawn under a switch.
            for switch in &switches {
                let x = switch.center().x.as_f32();
                assert!(!lines.iter().any(|(top, bottom, cx)| (cx - x).abs() < 1.
                    && *top < switch.bottom().as_f32() - 1.
                    && *bottom > switch.top().as_f32() + 1.));
            }
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The gap a drag lands in: before an item over its first half, after it
    /// over its second; the gaps either side of the dragged item move nothing.
    #[test]
    fn drag_reorder_gaps() {
        use super::{drag_gap, gap_target};
        use gpui_kit::{Bounds, point, px, size};
        let row = Bounds::new(point(px(0.), px(100.)), size(px(200.), px(20.)));
        assert_eq!(drag_gap(3, row, point(px(10.), px(104.)), true), Some(3));
        assert_eq!(drag_gap(3, row, point(px(10.), px(116.)), true), Some(4));
        assert_eq!(drag_gap(3, row, point(px(10.), px(130.)), true), None);
        assert_eq!(drag_gap(3, row, point(px(190.), px(104.)), false), Some(4));
        assert_eq!(gap_target(1, 1), None);
        assert_eq!(gap_target(1, 2), None);
        assert_eq!(gap_target(1, 0), Some(0));
        assert_eq!(gap_target(1, 4), Some(3));
    }

    /// Mid-drag, a queued prompt shows where it will land as an accent line
    /// in the gap, with the prompt dragged dimmed in place, and no row lit
    /// as a target.
    #[gpui_kit::test]
    async fn dragging_a_queued_prompt_shows_the_gap(cx: &mut TestAppContext) {
        use gpui_kit::component::ActiveTheme as _;
        let dir = std::env::temp_dir().join(format!("suspense-drag-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        for text in ["first", "second", "third"] {
            prompt_queue::add(HiddenAnchor::random(), text.into(), &dir).unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| this.queue_expanded = true);
        let row = |cx: &mut TestAppContext, ix: usize| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.find(("queued-prompt", ix)).bounds()
            })
            .unwrap()
        };
        let (first, third) = (row(cx, 0), row(cx, 2));
        let end = gpui_kit::point(third.center().x, third.top() + third.size.height * 0.8);
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_mouse_move(first.center(), None, Default::default());
        visual.simulate_mouse_down(
            first.center(),
            gpui_kit::MouseButton::Left,
            Default::default(),
        );
        visual.simulate_mouse_move(end, gpui_kit::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(end, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, _| {
            let id = this.queue[0].id;
            assert_eq!(
                this.queue_gap.get(),
                Some((id, Some(3))),
                "no gap past the last"
            );
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let (accent, target) = (cx.theme().accent, cx.theme().drop_target);
            let scale = window.scale_factor();
            let quads = window.painted_quads();
            let line = quads.iter().any(|quad| {
                quad.background.as_solid() == Some(accent)
                    && (quad.bounds.size.height.0 / scale - 2.).abs() < 0.5
                    && quad.bounds.size.width.0 / scale > 20.
            });
            assert!(line, "no insertion line in the gap");
            assert!(
                !quads
                    .iter()
                    .any(|quad| quad.background.as_solid() == Some(target)),
                "a row is lit as a target"
            );
        })
        .unwrap();
        visual.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
        cx.run_until_parked();
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.queued_texts()),
            ["second", "third", "first"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A run's outcome tags follow the files its tool calls wrote or edited:
    /// spec, code, or elsewhere outside the project's data; with none,
    /// Answered when it gave a summary, else No changes. Reading never counts.
    #[test]
    fn outcome_tags_follow_what_a_run_wrote() {
        use crate::task_table::{OutcomeTag::*, outcome_tags};
        use std::path::{Path, PathBuf};
        let dir = Path::new("/p");
        let (spec, code) = (Some(Path::new("/p/spec")), Some(Path::new("/p/src")));
        let tags = |files: &[&str], summary| {
            let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            outcome_tags(&files, dir, spec, code, summary)
        };
        assert_eq!(
            tags(&["/p/spec/a.pi", "/p/src/main.rs"], true),
            [WroteSpec, WroteCode]
        );
        assert_eq!(tags(&["/p/.claude/reference/a.md"], true), [WroteSpec]);
        assert_eq!(tags(&["/p/README.md"], false), [WroteFiles]);
        assert_eq!(
            tags(&["/p/.suspense/history/x.understanding.md"], true),
            [Answered]
        );
        assert_eq!(tags(&[], true), [Answered]);
        assert_eq!(tags(&[], false), [NoChanges]);
    }

    /// Once a task's run is over, its final summary is headed by what it
    /// did, and not before; a file it only read counts for nothing.
    #[gpui_kit::test]
    async fn a_finished_run_is_tagged_with_what_it_did(cx: &mut TestAppContext) {
        use crate::task_table::OutcomeTag;
        let dir = std::env::temp_dir().join(format!("suspense-outcome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "export piton-config Project:\n    root: ./spec\n\nbelay-config B:\n    codeRoot: ./src\n",
        )
        .unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let tool = |id: &str, name: &str, file: &str| HarnessEvent::ToolCalled {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({ "file_path": file }),
            subagent: false,
        };
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Change it".into(), cx);
            this.show_compiled(ix, "Prompt_0".into(), "Change it".into(), cx);
            for event in [
                HarnessEvent::ToolStarted {
                    id: "t1".into(),
                    name: "Edit".into(),
                },
                tool("t1", "Edit", "spec/a.pi"),
                HarnessEvent::ToolStarted {
                    id: "t2".into(),
                    name: "Read".into(),
                },
                tool("t2", "Read", "src/main.rs"),
                HarnessEvent::TextStarted,
                HarnessEvent::TextDelta("Changed the spec.".into()),
            ] {
                this.apply_event(ix, event, cx);
            }
            assert!(
                this.tasks[ix].reply.outcome().is_empty(),
                "tagged while running"
            );
            this.apply_event(
                ix,
                HarnessEvent::Finished {
                    is_error: false,
                    result: "Changed the spec.".into(),
                },
                cx,
            );
            assert_eq!(this.tasks[ix].reply.outcome(), [OutcomeTag::WroteSpec]);
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            assert!(
                window.try_find("outcome-Wrote spec").is_some(),
                "no tag shown"
            );
            assert!(window.try_find("outcome-Wrote code").is_none());
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task whose mode isn't known shows its subagents with no mode colour,
    /// started by "the task"; a Chain task on its own, before its code step,
    /// is the chain's spec step, its subagents Spec's.
    #[gpui_kit::test]
    async fn subagents_of_an_unknown_mode_or_a_lone_chain(cx: &mut TestAppContext) {
        use crate::chat_input::{SendMode, mode_color};
        let (prompt_mode, _) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            this.push_task("Old".into(), cx);
        });
        let groups = cx.update(|cx| prompt_mode.read(cx).running_subagents(cx));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].color, None);
        assert_eq!(groups[0].label, None);
        assert_eq!(groups[0].started_by.as_ref(), "the task");

        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Both".into(), cx);
            this.tasks[ix].mode = Some(SendMode::Both);
            this.tasks[ix].sent.mode = Some(SendMode::Both);
        });
        let spec = cx.update(|cx| mode_color(SendMode::Spec, cx));
        let groups = cx.update(|cx| prompt_mode.read(cx).running_subagents(cx));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label.as_ref().map(|l| l.as_ref()), Some("Spec"));
        assert_eq!(groups[0].color, Some(spec));
        assert_eq!(groups[0].started_by.as_ref(), "the Spec step");
    }

    /// A chain's steps are found by which step each was sent from, not by
    /// being next to each other: a task of the other lane run between them
    /// is listed after the chain, and the chain where its first step was
    /// sent, its steps together.
    #[test]
    fn chain_steps_are_listed_together_whatever_ran_between() {
        use crate::chat_input::SendMode;
        use crate::hidden_anchor::CodeTask;
        let mut tasks: Vec<PromptTask> = Vec::new();
        let mut step = |mode: SendMode, from: Option<usize>, post_build: bool| {
            let mut task = PromptTask::new("Build it".into());
            task.name = format!("Task{}", tasks.len()).into();
            task.sent.mode = Some(mode);
            task.sent.sent_from = from.map(|from| tasks[from].name.to_string());
            task.sent.code_task = from.map(|_| CodeTask::default());
            task.sent.post_build_update = post_build;
            tasks.push(task);
        };
        step(SendMode::Both, None, true); // 0: the chain
        step(SendMode::Code, None, false); // 1: a Code task beside its spec step
        step(SendMode::Spec, None, false); // 2: a Spec task beside its code step
        step(SendMode::Code, Some(0), true); // 3: its code step
        step(SendMode::Spec, None, false); // 4: another Spec task
        step(SendMode::Spec, Some(3), false); // 5: its follow-up
        let layout = super::chain_layout(&tasks);
        assert_eq!(layout.order, [0, 3, 5, 1, 2, 4]);
        let chain = |pos| {
            Some(super::ChainStep {
                start: 0,
                pos,
                len: 3,
            })
        };
        assert_eq!(
            layout.steps,
            [chain(0), chain(1), chain(2), None, None, None]
        );
        assert_eq!(layout.step_of(5), chain(2));
        assert_eq!(layout.place[1], 3);
        assert_eq!(layout.members(chain(0).unwrap()), [0, 3, 5]);
    }

    /// Each lane sends its own queued prompts, first to last, whatever the
    /// other lane is doing: a Spec prompt is sent while a Code task runs,
    /// and a Code prompt while a Spec task does. A prompt that must wait
    /// holds back those of its lane after it, and a Freeform prompt, needing
    /// both lanes, holds back both.
    #[gpui_kit::test]
    async fn each_lane_sends_its_own_queued_prompts(cx: &mut TestAppContext) {
        use crate::chat_input::{Lanes, SendMode};
        let (prompt_mode, _handle) = open(cx);
        prompt_mode.update(cx, |this, _| {
            this.project_dir = Some(std::env::temp_dir());
            let item = |id: usize, mode: SendMode, saved: bool| super::QueueItem {
                id,
                text: "queued".into(),
                saved: saved.then(|| prompt_queue::QueuedPrompt {
                    file: std::env::temp_dir().join("never-written.pi"),
                    anchor: HiddenAnchor::random(),
                    text: "queued".into(),
                }),
                wait: false,
                sent_from: None,
                images: Vec::new(),
                new_conversation: false,
                mode: Some(mode),
                queued_at: id as u128,
            };
            let code_lane = Lanes {
                code: true,
                spec: false,
            };
            let spec_lane = Lanes {
                code: false,
                spec: true,
            };
            let cases = [
                (
                    code_lane,
                    vec![(SendMode::Code, true), (SendMode::Spec, true)],
                    Some(1),
                ),
                (
                    spec_lane,
                    vec![(SendMode::Spec, true), (SendMode::Code, true)],
                    Some(1),
                ),
                (
                    spec_lane,
                    vec![(SendMode::Both, true), (SendMode::Code, true)],
                    Some(1),
                ),
                (
                    code_lane,
                    vec![(SendMode::Freeform, true), (SendMode::Spec, true)],
                    None,
                ),
                (
                    spec_lane,
                    vec![(SendMode::Freeform, true), (SendMode::Code, true)],
                    None,
                ),
                (
                    Lanes::NONE,
                    vec![
                        (SendMode::Code, false),
                        (SendMode::Code, true),
                        (SendMode::Spec, true),
                    ],
                    Some(2),
                ),
                (
                    Lanes::ALL,
                    vec![(SendMode::Code, true), (SendMode::Spec, true)],
                    None,
                ),
                (Lanes::NONE, vec![(SendMode::Freeform, true)], Some(0)),
            ];
            for (working, queue, next) in cases {
                this.working = working;
                this.queue = queue
                    .iter()
                    .enumerate()
                    .map(|(id, &(mode, saved))| item(id, mode, saved))
                    .collect();
                assert_eq!(this.next_sendable(), next, "{working:?} {queue:?}");
            }
            this.queue.clear();
            this.working = Lanes::NONE;
            this.project_dir = None;
        });
    }

    /// Previous tasks are grouped by the conversation they ran in: a group
    /// starts wherever the conversation changes, and a task whose
    /// conversation isn't known stays with the group before it.
    #[test]
    fn previous_tasks_group_by_session() {
        let task = |session: Option<&str>| {
            let mut task = PromptTask::new("Do it".into());
            if let Some(session) = session {
                task.apply(HarnessEvent::Session(session.into()));
            }
            task
        };
        let tasks = [
            task(None),
            task(Some("a")),
            task(Some("a")),
            task(None),
            task(Some("b")),
            task(Some("a")),
        ];
        let groups = |tasks: &[PromptTask]| {
            super::HistoryList::session_groups(tasks, &super::chain_layout(tasks))
        };
        assert_eq!(groups(&tasks), [0, 1, 4, 5]);
        assert_eq!(groups(&tasks[1..]), [0, 3, 4]);
        assert!(groups(&[]).is_empty());
    }

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
            sent_at: 0,
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
            sent_at: 0,
            anchor: HiddenAnchor::random(),
            text: "Old".into(),
            record: None,
        });
        assert_eq!(unrecorded.status, TaskStatus::Unrecorded);
        assert!(unrecorded.reply.done && unrecorded.compiled.is_none());
    }

    /// A task sent more while it ran: a result that leaves a message sent
    /// unanswered doesn't end it, and each message is a Sent row where it was
    /// sent. Its record keeps each message, as typed and as compiled, among
    /// the harness's lines, so the history brings the task back as the
    /// exchange it was, in the state of its last result.
    #[test]
    fn a_task_sent_more_replays_as_the_exchange_it_was() {
        let project_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/fed-task-test");
        std::fs::remove_dir_all(&project_dir).ok();
        let message = "Also check b.\nThoroughly.";
        let taken = |text: &str| {
            HarnessEvent::Output(
                serde_json::json!({ "type": "user", "isReplay": true, "parent_tool_use_id": null,
                    "message": { "role": "user", "content": [{ "type": "text", "text": text }] } })
                .to_string(),
            )
        };
        let line = |line: &str| HarnessEvent::Output(line.into());
        let run = [
            taken("Check a."),
            line(
                r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"t1","name":"Read"}}}"#,
            ),
            HarnessEvent::Sent {
                text: message.into(),
                compiled: "Also check b, compiled.".into(),
            },
            line(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#,
            ),
            line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"a is fine."}}}"#,
            ),
            line(r#"{"type":"result","is_error":false,"result":"a is fine."}"#),
            taken("Also check b, compiled."),
            line(
                r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text","text":""}}}"#,
            ),
            line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"b is fine too."}}}"#,
            ),
            line(r#"{"type":"result","is_error":false,"result":"b is fine too."}"#),
        ];
        let mut record = RunRecord {
            user_prompt: Some("Check a, compiled.".into()),
            ..RunRecord::default()
        };
        for event in &run {
            record.note(event);
        }
        let anchor = HiddenAnchor::random();
        let file = hidden_anchor::save(&anchor, "Check a.", &project_dir).unwrap();
        prompt_history::save_record(&file, &record).unwrap();

        let saved = prompt_history::load(&project_dir).pop().unwrap();
        assert_eq!(saved.record.as_ref(), Some(&record));
        assert_eq!(
            record.output[2],
            serde_json::json!({ "sent": { "text": message, "compiled": "Also check b, compiled." } })
        );

        // Until the last result, the task runs on: the first is an answer.
        let mut events = saved.record.as_ref().unwrap().events();
        let answer = events
            .iter()
            .position(|event| matches!(event, HarnessEvent::Answered { .. }))
            .unwrap();
        events.truncate(answer + 1);
        let mut live = PromptTask::new("Check a.".into());
        live.set_compiled(super::Compiled::new("Prompt_0".into(), "Check a.".into()));
        for event in events {
            live.apply(event);
        }
        assert_eq!(live.status, TaskStatus::Running);
        assert!(!live.reply.is_done());
        assert_eq!(live.reply.rows().last(), Some(&OutputRow::Pending));

        let task = PromptTask::restore(saved);
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(
            task.compiled.as_ref().unwrap().markdown,
            "Check a, compiled.",
            "the header shows the prompt the task was sent with"
        );
        assert!(matches!(
            task.reply.rows().as_slice(),
            [
                OutputRow::Tool(call),
                OutputRow::Sent(sent),
                OutputRow::Text("a is fine."),
                OutputRow::Text("b is fine too."),
            ] if call.state == ToolState::Done && *sent == message
        ));

        // A task ends in the state of its last result.
        let mut failed = RunRecord::default();
        for event in [
            HarnessEvent::Sent {
                text: "More.".into(),
                compiled: "More.".into(),
            },
            line(r#"{"type":"result","is_error":false,"result":"Done."}"#),
            taken("More."),
            line(r#"{"type":"result","is_error":true,"result":"overloaded"}"#),
        ] {
            failed.note(&event);
        }
        let failed = PromptTask::restore(SavedPrompt {
            sent_at: 0,
            anchor: HiddenAnchor::random(),
            text: "Do it".into(),
            record: Some(failed),
        });
        assert_eq!(failed.status, TaskStatus::Failed);
        std::fs::remove_dir_all(&project_dir).ok();
    }

    /// The chat input offers to send more to the latest task only while it
    /// runs with a harness that can be fed, its output shown rather than the
    /// previous tasks. What is sent goes to the run, in its place among what
    /// the harness does; once the run takes no more, a message is queued as
    /// a task instead.
    #[gpui_kit::test]
    async fn a_running_task_can_be_sent_more(cx: &mut TestAppContext) {
        use futures::StreamExt as _;
        let dir =
            std::env::temp_dir().join(format!("suspense-send-to-task-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let input = prompt_mode.read_with(cx, |this, _| this.chat_input.clone());
        let offered = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            input.read_with(cx, |input, _| input.can_send_to_task())
        };
        assert!(!offered(cx), "offered with no task running");

        let (feed, mut events) = crate::harness::Feed::for_test();
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Check a".into(), cx);
            this.tasks[ix].status = TaskStatus::Running;
            this.working = crate::chat_input::Lanes::ALL;
        });
        assert!(!offered(cx), "offered for a harness that can't be fed");
        prompt_mode.update(cx, |this, _| {
            this.tasks.last_mut().unwrap().feed = Some(feed.clone())
        });
        assert!(offered(cx));

        // Not while the previous tasks are expanded over its output.
        prompt_mode.update(cx, |this, _| this.task_history.expanded = true);
        assert!(!offered(cx));
        prompt_mode.update(cx, |this, _| this.task_history.expanded = false);
        assert!(offered(cx));

        feed.send("Also b.".into(), "Also b, compiled.".into(), &[])
            .unwrap();
        assert_eq!(
            events.next().await,
            Some(HarnessEvent::Sent {
                text: "Also b.".into(),
                compiled: "Also b, compiled.".into()
            })
        );

        // The run takes no more: a message on its way is queued as a task.
        feed.close();
        assert!(feed.send("Late.".into(), "Late.".into(), &[]).is_err());
        assert!(!offered(cx));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_task(
                    "Also c.".into(),
                    crate::chat_input::SendMode::Code,
                    Default::default(),
                    window,
                    cx,
                )
            })
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.queue.len(), 1);
            assert_eq!(this.queue[0].text.as_ref(), "Also c.");
        });
        std::fs::remove_dir_all(&dir).ok();
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
                sent_at: 0,
                anchor: HiddenAnchor::random(),
                text: "Do it".into(),
                record: Some(record),
            }
        };
        let history = [
            saved(&[r#"{"type":"system","subtype":"init","session_id":"old"}"#]),
            saved(&[
                r#"{"type":"system","subtype":"init","session_id":"latest"}"#,
                r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":2000,"output_tokens":90}}}"#,
                r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":5,"cache_read_input_tokens":3000,"output_tokens":20}}}"#,
            ]),
            saved(&["not json"]),
            SavedPrompt {
                sent_at: 0,
                anchor: HiddenAnchor::random(),
                text: "Old".into(),
                record: None,
            },
        ];
        let project = std::path::Path::new("/project");
        let mut session = Session::latest(&history, project, None);
        assert_eq!(
            Session::resume(&session, project).as_deref(),
            Some("latest")
        );
        assert_eq!(
            Session::resume(&session, std::path::Path::new("/other")),
            None
        );
        // Its context as the run left it: the latest reply's.
        assert_eq!(
            session.as_ref().and_then(|session| session.context),
            Some(3025)
        );

        Session::forget(&mut session, "old");
        assert!(session.is_some(), "a different session was forgotten");
        Session::forget(&mut session, "latest");
        assert!(session.is_none());
        assert!(Session::latest(&history[2..], project, None).is_none());

        // Left for a new one, the latest conversation isn't carried on; an
        // older one left changes nothing.
        assert!(Session::latest(&history, project, Some("latest")).is_none());
        assert_eq!(
            Session::latest(&history, project, Some("old")).map(|session| session.id),
            Some("latest".into())
        );
    }

    /// The chat input's usage follows what the project's runs report: the
    /// selected tab's conversation, the whole project and its runs, and the
    /// plan limits of the harness picked. New conversation leaves the
    /// conversation's figures behind, but not the project's.
    #[gpui_kit::test]
    async fn usage_follows_the_runs_by_conversation_and_project(cx: &mut TestAppContext) {
        use crate::agent::Agent;
        use crate::usage::{Conversation, PlanLimit, Spend};

        let dir = std::env::temp_dir().join(format!("suspense-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let input = prompt_mode.read_with(cx, |this, _| this.chat_input.clone());
        let shown = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            input.read_with(cx, |input, _| input.usage().clone())
        };
        let spend = |input: u64, output: u64, cost: f64| HarnessEvent::Spent {
            spend: Spend {
                input: Some(input),
                output: Some(output),
                cost: Some(cost),
                ..Spend::default()
            },
            tally: crate::usage::Tally::Run,
        };
        assert!(shown(cx).is_empty());
        assert_eq!(shown(cx).summary().0, "Usage");

        prompt_mode.update(cx, |this, _| {
            let task = Some(
                this.usage
                    .start_run(Conversation::Tasks, this.session_epoch),
            );
            for event in [
                HarnessEvent::Session("s1".into()),
                HarnessEvent::Model("claude-opus".into()),
                spend(10, 100, 0.5),
            ] {
                this.follow_usage(task, Agent::Claude, &event);
            }
            let question = Some(
                this.usage
                    .start_run(Conversation::Questions, this.ask_session_epoch),
            );
            this.follow_usage(question, Agent::Claude, &spend(5, 50, 0.25));
        });
        let usage = shown(cx);
        assert_eq!(usage.conversation, Some(Conversation::Tasks));
        assert_eq!(usage.conversation_spend.cost, Some(0.5));
        assert_eq!(usage.project_spend.cost, Some(0.75));
        assert_eq!(usage.project_spend.output, Some(150));
        assert_eq!(usage.project_spend.cache_read, None);
        assert_eq!(usage.runs, 2);
        assert_eq!(usage.harness, Some(Agent::Claude));
        assert_eq!(usage.model.as_deref(), Some("claude-opus"));
        assert!(usage.limits.is_empty());
        assert_eq!(usage.summary().0, "Usage $0.75");

        // On the Ask tab, the questions' conversation.
        prompt_mode.update(cx, |this, _| this.on_ask_tab = true);
        let usage = shown(cx);
        assert_eq!(usage.conversation, Some(Conversation::Questions));
        assert_eq!(usage.conversation_spend.cost, Some(0.25));
        prompt_mode.update(cx, |this, _| this.on_ask_tab = false);

        // Plan limits, once reported, lead the summary.
        prompt_mode.update(cx, |this, _| {
            this.follow_usage(
                None,
                Agent::Claude,
                &HarnessEvent::Limits(vec![PlanLimit {
                    name: "seven_day".into(),
                    used: 0.42,
                    resets_at: None,
                }]),
            )
        });
        assert_eq!(shown(cx).summary().0, "Usage 42%");

        // A new conversation has spent nothing; the project still has.
        prompt_mode.update(cx, |this, cx| {
            this.session = Some(Session {
                project_dir: this.project_dir.clone().unwrap(),
                id: "s1".into(),
                context: None,
                system_prompt: None,
            });
            this.new_conversation(cx)
        });
        cx.run_until_parked();
        let usage = shown(cx);
        assert!(usage.conversation_spend.is_empty());
        assert_eq!(usage.project_spend.cost, Some(0.75));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The chat input shows how much context the selected tab's conversation
    /// holds. New conversation leaves it, even while a task of it runs: the
    /// next prompt starts a new conversation, a run of the old one that is
    /// still going can't bring it back, and neither can reopening the
    /// project, even when that run hadn't said which conversation it was yet.
    /// The Ask tab's conversation is its own.
    #[gpui_kit::test]
    async fn new_conversation_leaves_the_conversation_and_its_context(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-new-conversation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let dir = prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap());
        let input = prompt_mode.read_with(cx, |this, _| this.chat_input.clone());
        let shown = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            input.read_with(cx, |input, _| input.context())
        };
        // A run of the tasks' conversation, as the harness reports it.
        let run = |this: &mut PromptMode,
                   epoch: u64,
                   events: &[HarnessEvent],
                   cx: &mut gpui_kit::Context<PromptMode>| {
            let mut run = None;
            for event in events {
                if let Some(left) = Session::follow(
                    &mut this.session,
                    this.session_epoch == epoch,
                    &mut run,
                    event,
                    &dir,
                    None,
                ) {
                    PromptMode::keep_left(dir.clone(), crate::conversations::Kind::Tasks, left, cx);
                }
            }
        };
        let left = |dir: &std::path::Path| {
            crate::conversations::left(dir, crate::conversations::Kind::Tasks)
        };
        assert_eq!(shown(cx), None, "a new project already holds context");

        prompt_mode.update(cx, |this, cx| {
            run(
                this,
                0,
                &[
                    HarnessEvent::Session("s1".into()),
                    HarnessEvent::Usage { context: 42_100 },
                ],
                cx,
            )
        });
        assert_eq!(shown(cx), Some(42_100));
        cx.update_window(handle, |_, window, cx| {
            assert!(window.find("chat-context").visible());
        })
        .unwrap();

        // On the Ask tab, the questions' conversation, which has none yet.
        prompt_mode.update(cx, |this, _| this.on_ask_tab = true);
        assert_eq!(shown(cx), None);
        prompt_mode.update(cx, |this, _| this.on_ask_tab = false);
        assert_eq!(shown(cx), Some(42_100));

        // New conversation while a task of it runs: the next task starts
        // afresh, with nothing to resume, and the conversation left is kept
        // with the project.
        prompt_mode.update(cx, |this, _| this.working = crate::chat_input::Lanes::ALL);
        shown(cx);
        assert!(input.read_with(cx, |input, _| input.conversation_running()));
        cx.update_window(handle, |_, window, cx| window.click("new-conversation", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(shown(cx), None, "New conversation left context behind");
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(Session::resume(&this.session, &dir), None);
        });
        assert_eq!(left(&dir).as_deref(), Some("s1"));
        assert_eq!(
            crate::conversations::left(&dir, crate::conversations::Kind::Questions),
            None
        );

        // The old run, still going, reports on: it doesn't come back.
        prompt_mode.update(cx, |this, cx| {
            run(
                this,
                0,
                &[
                    HarnessEvent::Session("s1".into()),
                    HarnessEvent::Usage { context: 50_000 },
                ],
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(
            shown(cx),
            None,
            "a run of the old conversation brought it back"
        );
        assert_eq!(left(&dir).as_deref(), Some("s1"));
        prompt_mode.update(cx, |this, _| this.working = crate::chat_input::Lanes::NONE);

        // A run started since is the new conversation.
        prompt_mode.update(cx, |this, cx| {
            let epoch = this.session_epoch;
            run(
                this,
                epoch,
                &[
                    HarnessEvent::Session("s2".into()),
                    HarnessEvent::Usage { context: 900 },
                ],
                cx,
            )
        });
        assert_eq!(shown(cx), Some(900));

        // Left with no run under way, there is nothing more to leave until
        // one is.
        prompt_mode.update(cx, |this, cx| this.new_conversation(cx));
        cx.run_until_parked();
        assert_eq!(shown(cx), None);
        assert_eq!(left(&dir).as_deref(), Some("s2"));
        let epoch = prompt_mode.read_with(cx, |this, _| this.session_epoch);
        prompt_mode.update(cx, |this, cx| this.new_conversation(cx));
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.session_epoch),
            epoch,
            "left a conversation with nothing to carry on and no run"
        );

        // The first task of a conversation runs, and New conversation is
        // pressed before the harness says which conversation it is: once it
        // does, that one is kept as left too, and isn't carried on.
        prompt_mode.update(cx, |this, _| this.working = crate::chat_input::Lanes::ALL);
        shown(cx);
        cx.update_window(handle, |_, window, cx| window.click("new-conversation", cx))
            .unwrap();
        cx.run_until_parked();
        assert_ne!(
            prompt_mode.read_with(cx, |this, _| this.session_epoch),
            epoch,
            "couldn't leave the conversation of a task running"
        );
        prompt_mode.update(cx, |this, cx| {
            run(
                this,
                epoch,
                &[
                    HarnessEvent::Session("s3".into()),
                    HarnessEvent::Usage { context: 7_000 },
                ],
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(shown(cx), None, "the run's new conversation was carried on");
        assert_eq!(left(&dir).as_deref(), Some("s3"));
        prompt_mode.update(cx, |this, _| this.working = crate::chat_input::Lanes::NONE);

        // The next task starts a conversation of its own, which is carried on.
        prompt_mode.update(cx, |this, cx| {
            let epoch = this.session_epoch;
            run(
                this,
                epoch,
                &[
                    HarnessEvent::Session("s4".into()),
                    HarnessEvent::Usage { context: 1_200 },
                ],
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(shown(cx), Some(1_200));
        assert_eq!(left(&dir).as_deref(), Some("s3"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Ctrl+Shift+Up in the chat input moves focus to the editor of the file
    /// whose tab is selected, and leaves it where it is while Chat is.
    #[gpui_kit::test]
    async fn ctrl_shift_up_focuses_the_selected_file(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-ctrl-shift-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "# Notes\n").unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| {
            crate::chat_input::bind_keys(cx);
            ProjectDirectory::set(dir.clone(), cx)
        });
        cx.run_until_parked();
        let chat = prompt_mode.read_with(cx, |this, _| this.chat_input_view());
        let press = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                chat.update(cx, |input, cx| input.focus_editor_for_test(window, cx));
                window.press("ctrl-shift-up", cx);
            })
            .unwrap();
            cx.run_until_parked();
        };

        // With Chat selected, the chat input keeps focus.
        press(cx);
        cx.update_window(handle, |_, window, cx| {
            assert!(chat.read(cx).is_focused(window, cx));
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| this.open_file(file.clone(), window, cx))
        })
        .unwrap();
        cx.run_until_parked();
        press(cx);
        let view = prompt_mode.read_with(cx, |this, _| this.open_file_view().unwrap());
        cx.update_window(handle, |_, window, cx| {
            assert!(
                view.read(cx).editor_focused(window, cx),
                "the editor isn't focused"
            );
            assert!(!chat.read(cx).is_focused(window, cx));
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Files edited on disk while they have unsaved changes are asked
    /// about one after another, in a dialog named for the file: Merge
    /// selects the file's tab and shows the merge in place of its editor,
    /// and closing the dialog keeps the changes.
    #[gpui_kit::test]
    async fn files_changed_on_disk_are_asked_about_in_turn(cx: &mut TestAppContext) {
        use super::ConflictChoice;
        use gpui_kit::component::WindowExt as _;
        let dir = std::env::temp_dir().join(format!("suspense-conflicts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let (first, second) = (dir.join("first.md"), dir.join("second.md"));
        std::fs::write(&first, "first\n").unwrap();
        std::fs::write(&second, "second\n").unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        for file in [&first, &second] {
            cx.update_window(handle, |_, window, cx| {
                prompt_mode.update(cx, |this, cx| this.open_file(file.clone(), window, cx))
            })
            .unwrap();
        }
        cx.run_until_parked();
        let views = prompt_mode.read_with(cx, |this, _| this.open_file_views());
        // Chat selected; both files have unsaved changes.
        prompt_mode.update(cx, |this, cx| this.select_tab(None, cx));
        for view in &views {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| view.type_text("mine ", window, cx))
            })
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&first, "first, edited\n").unwrap();
        std::fs::write(&second, "second, edited\n").unwrap();

        let asked = |cx: &mut TestAppContext| {
            for _ in 0..300 {
                cx.run_until_parked();
                if prompt_mode.read_with(cx, |this, _| this.conflict_asked.is_some())
                    && prompt_mode.read_with(cx, |this, _| this.disk_conflicts.len() == 1)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
                cx.executor().advance_clock(Duration::from_millis(50));
            }
            panic!("not asked about both files");
        };
        asked(cx);
        let first_asked = prompt_mode.read_with(cx, |this, _| this.conflict_asked.unwrap());
        let (asked_view, other) = if first_asked == views[0].entity_id() {
            (views[0].clone(), views[1].clone())
        } else {
            (views[1].clone(), views[0].clone())
        };
        cx.update_window(handle, |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            prompt_mode.update(cx, |this, cx| {
                this.answer_conflict(first_asked, ConflictChoice::Merge, window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, cx| {
            assert_eq!(
                this.open_file_view().map(|view| view.entity_id()),
                Some(asked_view.entity_id()),
                "Merge didn't select the file's tab"
            );
            assert!(asked_view.read(cx).merge_view().is_some());
            // The other file is asked about next.
            assert_eq!(this.conflict_asked, Some(other.entity_id()));
        });

        // Closed, as <Escape> closes it, the changes are kept.
        cx.update_window(handle, |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            let id = other.entity_id();
            prompt_mode.update(cx, |this, cx| {
                this.answer_conflict(id, ConflictChoice::KeepMine, window, cx)
            });
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, cx| {
            assert_eq!(this.conflict_asked, None);
            assert!(other.read(cx).is_dirty() && !other.read(cx).has_conflict());
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Ctrl+N in the chat input starts a new conversation, as New
    /// conversation does, and whether a task starts one is part of it: the
    /// first queued prompt's hidden anchor records it, passing it on to the
    /// next if that one is cancelled, and with nothing queued, the next task
    /// queued records it.
    #[gpui_kit::test]
    async fn ctrl_n_starts_a_new_conversation_with_the_next_prompt(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-ctrl-n-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
        // A task of the conversation runs.
        prompt_mode.update(cx, |this, _| {
            this.session = Some(Session {
                project_dir: dir.clone(),
                id: "s1".into(),
                context: Some(1_000),
                system_prompt: None,
            });
            this.working = crate::chat_input::Lanes::ALL;
        });
        let press = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                chat.update(cx, |input, cx| input.focus_editor_for_test(window, cx));
                window.press("ctrl-n", cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        let marks = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                this.queue
                    .iter()
                    .map(|item| {
                        let saved = item.saved.as_ref().unwrap();
                        let (anchor, _) =
                            HiddenAnchor::parse(&std::fs::read_to_string(&saved.file).unwrap())
                                .unwrap();
                        (item.new_conversation, anchor.new_conversation)
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(marks(cx), [(false, None), (false, None)]);

        press(cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(
                this.session_epoch, 1,
                "Ctrl+N didn't leave the conversation"
            );
            assert!(this.session.is_none());
            assert!(!this.new_conversation_pending);
        });
        assert_eq!(marks(cx), [(true, Some(true)), (false, None)]);

        // Cancelled, the first passes it on to the next.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let first = this.queue[0].id;
                this.cancel(first, window, cx);
            })
        })
        .unwrap();
        assert_eq!(marks(cx), [(true, Some(true))]);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let second = this.queue[0].id;
                this.cancel(second, window, cx);
            })
        })
        .unwrap();

        // With nothing queued, the next task queued starts it.
        prompt_mode.read_with(cx, |this, _| assert!(this.new_conversation_pending));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                for text in ["third", "fourth"] {
                    this.enqueue(
                        text.into(),
                        true,
                        crate::chat_input::SendMode::Code,
                        crate::hidden_anchor::Attached::default(),
                        false,
                        None,
                        None,
                        false,
                        None,
                        window,
                        cx,
                    );
                }
            })
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert!(!this.new_conversation_pending);
            let marked: Vec<bool> = this
                .queue
                .iter()
                .map(|item| item.new_conversation)
                .collect();
            assert_eq!(marked, [true, false]);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Queued prompts can be dragged up and down the queue, and each toggled to
    /// start a new conversation or carry on the tasks', all kept with the
    /// project.
    #[gpui_kit::test]
    async fn queued_prompts_can_be_reordered_and_start_new_conversations(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-reorder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        for text in ["first", "second", "third"] {
            prompt_queue::add(HiddenAnchor::random(), text.into(), &dir).unwrap();
        }
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| this.queue_expanded = true);
        let click = |cx: &mut TestAppContext, id: &'static str, ix: usize| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click((id, ix), cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        // Drags row `from` by the pointer into the gap past row `to`, on its
        // far half, so it takes that row's place.
        let drag = |cx: &mut TestAppContext, from: usize, to: usize| {
            let at = |cx: &mut TestAppContext, ix: usize, share: f32| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let b = window.find(("queued-prompt", ix)).bounds();
                    gpui_kit::point(b.center().x, b.top() + b.size.height * share)
                })
                .unwrap()
            };
            let share = if to > from { 0.8 } else { 0.2 };
            let (start, end) = (at(cx, from, 0.5), at(cx, to, share));
            let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
            visual.simulate_mouse_move(start, None, Default::default());
            visual.simulate_mouse_down(start, gpui_kit::MouseButton::Left, Default::default());
            let halfway = start + (end - start) / 2.;
            visual.simulate_mouse_move(halfway, gpui_kit::MouseButton::Left, Default::default());
            visual.simulate_mouse_move(end, gpui_kit::MouseButton::Left, Default::default());
            visual.simulate_mouse_up(end, gpui_kit::MouseButton::Left, Default::default());
            cx.run_until_parked();
        };
        let on_disk = |dir: &std::path::Path| {
            prompt_queue::load(dir)
                .into_iter()
                .map(|queued| (queued.text, queued.anchor.new_conversation))
                .collect::<Vec<_>>()
        };

        // Dropped on itself, it stays put.
        drag(cx, 1, 1);
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.queued_texts()),
            ["first", "second", "third"]
        );

        drag(cx, 0, 1);
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.queued_texts()),
            ["second", "first", "third"]
        );
        // Past more than one, it takes the place it is dropped on.
        drag(cx, 1, 2);
        drag(cx, 0, 2);
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.queued_texts()),
            ["third", "first", "second"]
        );
        drag(cx, 2, 0);
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.queued_texts()),
            ["second", "third", "first"]
        );
        assert_eq!(
            on_disk(&dir)
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            ["second", "third", "first"]
        );
        click(cx, "new-conversation-queued", 1);
        assert_eq!(
            on_disk(&dir),
            [
                ("second".to_string(), None),
                ("third".to_string(), Some(true)),
                ("first".to_string(), None)
            ]
        );
        // Moved, it keeps its mark; toggled again, it carries on.
        drag(cx, 1, 0);
        assert_eq!(on_disk(&dir)[0], ("third".to_string(), Some(true)));
        click(cx, "new-conversation-queued", 0);
        assert_eq!(on_disk(&dir)[0], ("third".to_string(), Some(false)));
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.queue.iter().all(|item| !item.new_conversation));
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A sliced prompt's header shows what was typed first, with its spec
    /// slices collapsed into a row beneath it that shows and hides them.
    #[gpui_kit::test]
    async fn a_sliced_prompt_collapses_its_slices(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let compiled = "Fix the ribbon.\n\nSpec slices:\n\n# RibbonScope\n\nSLICE BODY\n\n# ThemeScope\n\nMORE";
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Fix the ribbon.".into(), cx);
                this.show_compiled(ix, "Prompt_0".into(), compiled.into(), cx);
            });
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("prompt-slices", 0usize)).is_some()
        })
        .await;
        let rows = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            prompt_mode.read_with(cx, |this, _| this.header_prompt.rows.count())
        };
        prompt_mode.read_with(cx, |this, _| {
            let compiled = this.tasks[0].compiled.as_ref().unwrap();
            assert_eq!(compiled.shown().as_ref(), "Fix the ribbon.");
            assert_eq!(compiled.slices().unwrap().count, 2);
        });
        // The prompt, then the row that shows its slices.
        assert_eq!(rows(cx), 2);
        cx.update_window(handle, |_, window, cx| {
            window.click(("prompt-slices", 0usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, _| assert!(this.tasks[0].slices_open));
        // And now each block of the slices beneath it.
        assert_eq!(rows(cx), 6);
    }

    /// A long compiled prompt, as one sent with its spec slices, is drawn a
    /// block at a time: the header stops growing where it scrolls, and only
    /// the blocks in view are laid out.
    #[gpui_kit::test]
    async fn a_long_prompt_header_lays_out_only_what_is_in_view(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let long: String = (0..400)
            .map(|n| format!("## Section {n}\n\nSome text about section {n}.\n\n"))
            .collect();
        cx.update_window(handle, |_, _, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Do it".into(), cx);
                this.show_compiled(ix, "Prompt_0".into(), long.clone(), cx);
            });
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find(("compiled-prompt", 0usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let prompt = window.find(("compiled-prompt", 0usize)).bounds();
            assert!(
                prompt.size.height <= super::MAX_PROMPT_HEIGHT + gpui_kit::px(0.5),
                "{prompt:?}"
            );
            assert!(prompt.size.height > gpui_kit::px(40.), "{prompt:?}");
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            let rows = &this.header_prompt.rows;
            assert_eq!(rows.count(), 800);
            let drawn = (0..rows.count())
                .filter(|ix| rows.state().bounds_for_item(*ix).is_some())
                .count();
            assert!(drawn < 50, "{drawn} blocks were laid out");
        });
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

    /// Clicking the file a file tool read opens it in a tab.
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
        assert!(prompt_mode.read_with(cx, |this, _| this.open_file_view().is_some()));

        // Text the file sends to the prompt is attached to it.
        let file_view = prompt_mode.read_with(cx, |this, _| this.open_file_view().unwrap());
        file_view.update(cx, |_, cx| {
            cx.emit(crate::file_view::SendToPrompt("selected text".into()))
        });
        cx.run_until_parked();
        let attached = prompt_mode.read_with(cx, |this, cx| {
            this.chat_input
                .read(cx)
                .attachments()
                .iter()
                .map(|attachment| attachment.text().unwrap_or_default().to_string())
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
    /// previous tasks and the queue, each time it is opened, however it was
    /// scrolled before.
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
                this.working = crate::chat_input::Lanes::ALL;
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

        {
            let (toggle, list, item) = ("history-toggle", "task-list-scroll", "history-task");
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
                    this.task_history
                        .rows
                        .state()
                        .scroll_to(gpui_kit::ListOffset {
                            item_ix: 0,
                            offset_in_item: gpui_kit::px(0.),
                        });
                });
                settle(cx);
                assert!(in_view(cx, list, (item, 0usize).into()));
                cx.update_window(handle, |_, window, cx| window.click(toggle, cx))
                    .unwrap();
            }
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

    /// Finishes the question `id` with `answer` as its reply.
    fn answer(
        this: &mut PromptMode,
        id: usize,
        answer: &str,
        cx: &mut gpui_kit::Context<PromptMode>,
    ) {
        this.update_ask(
            id,
            |ask| {
                ask.apply(HarnessEvent::TextStarted);
                ask.apply(HarnessEvent::TextDelta(answer.into()));
                ask.apply(HarnessEvent::Finished {
                    is_error: false,
                    result: String::new(),
                });
                ask.end();
            },
            cx,
        );
    }

    /// Draws a few frames, letting whatever measures or slides settle.
    fn frames(handle: AnyWindowHandle, cx: &mut TestAppContext) {
        for _ in 0..6 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn bounds_of(
        handle: AnyWindowHandle,
        id: impl Into<gpui_kit::ElementId>,
        cx: &mut TestAppContext,
    ) -> Option<Bounds> {
        let id = id.into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).map(|found| found.bounds())
        })
        .unwrap()
    }

    /// On the Ask tab, the body splits side by side above the chat input: the
    /// task view on the left, and the Ask conversation on the right, half the
    /// width to start with, headed as tall as the tab bar, and saying so
    /// while no question has been asked. Its edge drags from a quarter to
    /// three quarters of the width. Off the Ask tab, it goes.
    #[gpui_kit::test]
    async fn the_ask_tab_splits_the_body_with_the_ask_conversation(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        frames(handle, cx);
        assert!(bounds_of(handle, "ask-pane", cx).is_none());
        let whole = bounds_of(handle, "history", cx).unwrap();

        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            cx.notify();
        });
        frames(handle, cx);
        let pane = bounds_of(handle, "ask-pane", cx).expect("no Ask conversation");
        let split = bounds_of(handle, "ask-split", cx).unwrap();
        let history = bounds_of(handle, "history", cx).unwrap();
        let tabs = bounds_of(handle, "chat-tabs", cx).unwrap();
        let px = gpui_kit::px;
        assert!(
            (pane.size.width - split.size.width * 0.5).abs() <= px(1.),
            "the pane {pane:?} isn't half of {split:?}"
        );
        assert!((pane.right() - split.right()).abs() <= px(1.));
        assert!(
            history.right() <= pane.left() + px(1.),
            "{history:?} runs beneath {pane:?}"
        );
        assert!(history.size.width < whole.size.width);
        assert!(
            pane.bottom() <= tabs.top() + px(1.),
            "the pane runs over the chat input"
        );
        let header = bounds_of(handle, "ask-pane-header", cx).unwrap();
        assert!((header.size.height - px(32.)).abs() <= px(1.), "{header:?}");
        assert!(bounds_of(handle, "ask-pane-empty", cx).is_some());

        prompt_mode.update(cx, |this, cx| {
            this.drag_ask_split(split.left(), split, cx);
            assert_eq!(this.ask_split_share, 0.75);
            this.drag_ask_split(split.right(), split, cx);
            assert_eq!(this.ask_split_share, 0.25);
            this.drag_ask_split(split.left() + split.size.width * 0.6, split, cx);
            assert!((this.ask_split_share - 0.4).abs() < 0.01);
        });

        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = false;
            cx.notify();
        });
        frames(handle, cx);
        assert!(bounds_of(handle, "ask-pane", cx).is_none());
        assert_eq!(bounds_of(handle, "history", cx).unwrap(), whole);
        // The width it was dragged to comes back with it.
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            cx.notify();
        });
        frames(handle, cx);
        let pane = bounds_of(handle, "ask-pane", cx).unwrap();
        let split = bounds_of(handle, "ask-split", cx).unwrap();
        assert!((pane.size.width - split.size.width * 0.4).abs() <= px(1.));
    }

    /// The Ask conversation reads as a chat, oldest first: each question in a
    /// box against the right, its answer beneath it, and a divider above the
    /// first question of each conversation. New conversation puts one in at
    /// the bottom at once, only one however often it is pressed, and the next
    /// question asked begins that conversation.
    #[gpui_kit::test]
    async fn the_ask_conversation_reads_as_a_chat_marking_each_new_conversation(
        cx: &mut TestAppContext,
    ) {
        let dir = std::env::temp_dir().join(format!("suspense-ask-chat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        frames(handle, cx);
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            for (text, new) in [("First?", true), ("Second?", false), ("Third?", true)] {
                let id = this.push_ask(text.into(), cx);
                this.asks.last_mut().unwrap().task.new_conversation = new;
                answer(this, id, &format!("The answer to {text}"), cx);
            }
        });
        frames(handle, cx);
        let pane = bounds_of(handle, "ask-pane", cx).unwrap();
        let mut last_bottom = pane.top();
        for id in 1..=3usize {
            let question = bounds_of(handle, ("question", super::ASK_IX - id), cx)
                .unwrap_or_else(|| panic!("question {id} isn't shown"));
            assert!(
                question.left() > pane.left() + pane.size.width * 0.3,
                "question {id} {question:?} isn't against the right of {pane:?}"
            );
            assert!(
                question.top() >= last_bottom,
                "the questions are out of order"
            );
            last_bottom = question.bottom();
        }
        let divider = |id: usize, cx: &mut TestAppContext| {
            bounds_of(handle, ("ask-conversation", id), cx).is_some()
        };
        assert!(
            divider(super::ASK_IX - 1, cx),
            "the first conversation isn't marked"
        );
        assert!(
            !divider(super::ASK_IX - 2, cx),
            "a question carrying on is marked"
        );
        assert!(
            divider(super::ASK_IX - 3, cx),
            "the new conversation isn't marked"
        );
        assert!(!divider(usize::MAX, cx));
        cx.update_window(handle, |_, window, _| {
            let mut answers = window.within("ask-pane");
            assert!(
                answers
                    .within(("answer-of", super::ASK_IX - 1))
                    .try_find(("answer-row", 0usize))
                    .is_some(),
                "no answer shows"
            );
            assert!(
                answers.try_find(("output-type", 0usize)).is_none(),
                "the answer is drawn as a table"
            );
        })
        .unwrap();

        prompt_mode.update(cx, |this, cx| {
            this.ask_session = Some(Session {
                project_dir: dir.clone(),
                id: "questions".into(),
                context: Some(1200),
                system_prompt: None,
            });
            this.new_conversation(cx);
            assert!(this.ask_new_pending);
            this.new_conversation(cx);
        });
        frames(handle, cx);
        assert!(
            divider(usize::MAX, cx),
            "New conversation isn't marked at the bottom"
        );

        prompt_mode.update(cx, |this, cx| {
            this.ask(
                "Fourth?".into(),
                super::Attached::default(),
                false,
                None,
                cx,
            );
            assert!(!this.ask_new_pending);
            assert!(this.asks.last().unwrap().task.new_conversation);
            assert!(this.asks.last().unwrap().task.asked_at.is_some());
        });
        frames(handle, cx);
        assert!(!divider(usize::MAX, cx));
        assert!(
            divider(super::ASK_IX - 4, cx),
            "the question asked isn't marked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A finished question reads as a chat reply: only its answer as prose,
    /// with the steps before it summed up in one line that a click expands
    /// into compact lines and collapses again; a task shows its whole chain
    /// as a table.
    #[gpui_kit::test]
    async fn an_answer_sums_up_its_steps_but_a_task_does_not(cx: &mut TestAppContext) {
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
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("What does the chain do?".into(), cx);
            for event in events() {
                this.apply_event(ix, event, cx);
            }
            this.on_ask_tab = true;
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
        });
        let steps = ("answer-steps", super::ASK_IX - 1);
        let rows = |cx: &mut TestAppContext| {
            frames(handle, cx);
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let count =
                    |window: &mut gpui_kit::Window, within: &'static str, id: &'static str| {
                        let scoped = window.within(within);
                        (0..3usize)
                            .filter(|row| scoped.try_find((id, *row)).is_some())
                            .count()
                    };
                (
                    window.try_find(steps).is_some(),
                    count(window, "ask-pane", "answer-step"),
                    count(window, "ask-pane", "answer-row"),
                    count(window, "ask-pane", "output-row"),
                    count(window, "task-output", "output-row"),
                )
            })
            .unwrap()
        };
        let (toggle, step_rows, answer_rows, table_rows, task_rows) = rows(cx);
        assert!(toggle, "the answer has no line for its steps");
        assert_eq!((step_rows, answer_rows), (0, 1), "only the answer shows");
        assert_eq!(table_rows, 0, "the answer is drawn as a table");
        assert_eq!(task_rows, 3, "the task collapsed its chain");
        prompt_mode.read_with(cx, |this, _| {
            let reply = &this.asks[0].task.reply;
            let chat = reply.chat_rows();
            assert_eq!(chat.answer, [2]);
            assert_eq!(reply.steps_summary(&chat.steps), "Read 1 file");
        });
        cx.update_window(handle, |_, window, cx| window.click(steps, cx))
            .unwrap();
        let (toggle, step_rows, answer_rows, ..) = rows(cx);
        assert!(toggle, "expanding the steps took away their line");
        assert_eq!((step_rows, answer_rows), (2, 1), "the steps did not expand");
        cx.update_window(handle, |_, window, cx| window.click(steps, cx))
            .unwrap();
        assert_eq!(rows(cx).1, 0, "the steps did not collapse again");
    }

    /// A question that failed ends with its error, one stopped with
    /// "Stopped", and one finished with nothing to say with "No answer.";
    /// the steps are summed up by kind where they can be.
    #[gpui_kit::test]
    async fn an_answer_ends_as_its_question_did(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            let failed = this.push_ask("Will it fail?".into(), cx);
            this.update_ask(
                failed,
                |ask| {
                    ask.apply(HarnessEvent::Failed("no harness".into()));
                    ask.end();
                },
                cx,
            );
            let empty = this.push_ask("Anything?".into(), cx);
            this.update_ask(
                empty,
                |ask| {
                    ask.set_compiled(super::Compiled::new("Prompt_0".into(), "Anything?".into()));
                    ask.apply(HarnessEvent::Finished {
                        is_error: false,
                        result: String::new(),
                    });
                    ask.end();
                },
                cx,
            );
            let stopped = this.push_ask("Still there?".into(), cx);
            this.update_ask(stopped, |ask| ask.apply(HarnessEvent::TextStarted), cx);
            this.stop_ask(stopped, cx);
        });
        frames(handle, cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.asks[0].task.status, TaskStatus::Failed);
            assert_eq!(this.asks[0].task.reply.errors(), ["no harness"]);
            assert_eq!(this.asks[1].task.status, TaskStatus::Done);
            assert!(this.asks[1].task.reply.chat_rows().answer.is_empty());
            assert_eq!(this.asks[2].task.status, TaskStatus::Cancelled);
        });
        for id in 1..=3 {
            assert!(
                bounds_of(handle, ("question-end", super::ASK_IX - id), cx).is_some(),
                "question {id} has no end"
            );
        }
    }

    /// While the latest task was sent in Freeform, the task view shows its
    /// Freeform conversation as a chat: each prompt a message, each reply
    /// prose, a message sent to the run a message of its own, and no header
    /// or table. A task of another mode shows as a task again, and a
    /// Freeform task opened among the previous tasks is laid out as a chat.
    #[gpui_kit::test]
    async fn freeform_tasks_read_as_a_chat(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let (prompt_mode, handle) = open(cx);
        let push = |this: &mut PromptMode,
                    mode: SendMode,
                    text: &str,
                    cx: &mut gpui_kit::Context<PromptMode>| {
            let ix = this.push_task(text.to_string().into(), cx);
            this.tasks[ix].mode = Some(mode);
            this.tasks[ix].sent.mode = Some(mode);
            ix
        };
        let sent_row = prompt_mode.update(cx, |this, cx| {
            let code = push(this, SendMode::Code, "Change the code", cx);
            this.tasks[code].status = TaskStatus::Done;
            for text in ["Hello?", "And then?"] {
                let ix = push(this, SendMode::Freeform, text, cx);
                for event in [
                    HarnessEvent::TextStarted,
                    HarnessEvent::TextDelta(format!("Answer to {text}")),
                ] {
                    this.apply_event(ix, event, cx);
                }
            }
            // A message sent to the latest while it ran, and its answer.
            let latest = this.tasks.len() - 1;
            this.apply_event(
                latest,
                HarnessEvent::Sent {
                    text: "Also this".into(),
                    compiled: "Also this".into(),
                },
                cx,
            );
            this.apply_event(latest, HarnessEvent::TextStarted, cx);
            this.apply_event(latest, HarnessEvent::TextDelta("Did that too.".into()), cx);
            for ix in 1..3 {
                this.tasks[ix].reply.stop();
                this.tasks[ix].status = TaskStatus::Done;
            }
            assert_eq!(
                this.freeform_keys(),
                [
                    super::ask_pane::QuestionKey::Task(1),
                    super::ask_pane::QuestionKey::Task(2)
                ]
            );
            let segments = this.tasks[2].reply.chat_segments();
            assert_eq!(segments.len(), 2, "{segments:?}");
            segments[1].sent.unwrap()
        });
        frames(handle, cx);
        cx.update_window(handle, |_, window, _| {
            for ix in [1usize, 2] {
                assert!(
                    window.try_find(("question", ix)).is_some(),
                    "no message {ix}"
                );
            }
            assert!(
                window.try_find(("question", 0usize)).is_none(),
                "the Code task is in the chat"
            );
            assert!(
                window.try_find(("sent-message", sent_row)).is_some(),
                "no message sent"
            );
            assert!(
                window.try_find("task-header").is_none(),
                "a Freeform task has a header"
            );
            assert!(
                window.try_find("task-output").is_none(),
                "a Freeform task has a table"
            );
        })
        .unwrap();

        // Opened among the previous tasks, a Freeform task is a chat too.
        prompt_mode.update(cx, |this, cx| {
            let ix = push(this, SendMode::Code, "Back to code", cx);
            this.tasks[ix].status = TaskStatus::Done;
            this.task_history.toggle();
            this.task_history.open = Some(1);
            cx.notify();
        });
        frames(handle, cx);
        cx.update_window(handle, |_, window, _| {
            assert!(
                window.try_find("freeform-chat").is_none(),
                "the chat outlived Freeform"
            );
            assert!(
                window.try_find(("history-panel", 1usize)).is_some(),
                "not opened"
            );
            assert!(
                window.try_find(("freeform-reply", 1usize)).is_some(),
                "no chat reply opened"
            );
            assert!(
                window.try_find(("question", 1usize)).is_some(),
                "no message opened"
            );
        })
        .unwrap();
    }

    /// Selecting some of an answer's text in the Ask conversation offers a
    /// popover to copy it or attach it to the prompt; either closes the
    /// popover, and attaching lists the text above the chat input.
    #[gpui_kit::test]
    async fn selected_answer_text_can_be_copied_or_attached(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            let id = this.push_ask("What does the chain do?".into(), cx);
            answer(this, id, "It joins Code and Spec into one prompt.", cx);
        });
        frames(handle, cx);
        let select_answer = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let row = window
                    .within("ask-pane")
                    .find(("answer-row", 0usize))
                    .bounds();
                // Along the answer's text.
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
                .map(|attachment| attachment.text().unwrap_or_default().to_string())
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

    /// Off the Ask tab, the questions still running are rows stacked right
    /// above the chat input's tabs, pushing the task view up; on the Ask tab
    /// there is no stack, the questions being in the Ask conversation, where
    /// a running one can be stopped, leaving the others running. A question
    /// leaves the stack once it is over, and clicking a row shows it on the
    /// Ask tab.
    #[gpui_kit::test]
    async fn running_questions_stack_above_the_tabs_off_the_ask_tab(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        let whole = {
            frames(handle, cx);
            bounds_of(handle, "history", cx).unwrap()
        };
        prompt_mode.update(cx, |this, cx| {
            for text in ["Still thinking?", "Also thinking?"] {
                let id = this.push_ask(text.into(), cx);
                this.update_ask(id, |ask| ask.apply(HarnessEvent::TextStarted), cx);
            }
        });
        let (row, tabs) = settle(
            handle,
            ("ask-row", 2usize),
            |row, tabs| {
                row.size.height > gpui_kit::px(0.)
                    && (row.bottom() - tabs.top()).abs() <= gpui_kit::px(2.)
            },
            cx,
        );
        assert!(row.bottom() <= tabs.top() + gpui_kit::px(2.));
        let history = bounds_of(handle, "history", cx).unwrap();
        assert!(
            history.size.height < whole.size.height,
            "the stack covers the task view"
        );

        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            cx.notify();
        });
        frames(handle, cx);
        assert!(
            bounds_of(handle, "ask", cx).is_none(),
            "the stack shows on the Ask tab"
        );
        assert!(bounds_of(handle, ("question", super::ASK_IX - 1), cx).is_some());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("stop-ask", 1usize), cx)
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.asks[0].task.status, TaskStatus::Cancelled);
            assert!(
                this.asks[1].task.status.is_active(),
                "the other question stopped"
            );
        });

        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = false;
            cx.notify();
        });
        // Once it has slid up whole, as tall as its row.
        let (row, _) = settle(
            handle,
            ("ask-row", 2usize),
            |row, _| row.size.height > gpui_kit::px(0.),
            cx,
        );
        let _ = settle(
            handle,
            ("ask-card", 2usize),
            |card, _| card.size.height >= row.size.height - gpui_kit::px(0.5),
            cx,
        );
        assert!(
            bounds_of(handle, ("ask-row", 1usize), cx).is_none(),
            "a stopped question stayed in the stack"
        );
        cx.update_window(handle, |_, window, cx| {
            window.click(("ask-row", 2usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        prompt_mode.read_with(cx, |this, cx| {
            assert!(this.on_ask_tab, "the row didn't show its question");
            assert_eq!(this.chat_input.read(cx).mode(), super::SendMode::Ask);
            assert!(!this.ask_pane.locked);
        });
    }

    /// However many questions the Ask conversation holds, it lays out only
    /// what is in view, opening on the newest.
    #[gpui_kit::test]
    async fn a_long_ask_conversation_lays_out_only_what_is_in_view(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        prompt_mode.update(cx, |this, cx| {
            this.on_ask_tab = true;
            for n in 0..200 {
                let id = this.push_ask(format!("Question {n}").into(), cx);
                answer(this, id, &format!("Answer {n}"), cx);
            }
        });
        frames(handle, cx);
        assert!(bounds_of(handle, ("question", super::ASK_IX - 200), cx).is_some());
        assert!(bounds_of(handle, ("question", super::ASK_IX - 1), cx).is_none());
        prompt_mode.read_with(cx, |this, _| {
            let rows = this.ask_pane.rows();
            assert!(rows.count() > 200 * 4, "{} rows", rows.count());
            let drawn = (0..rows.count())
                .filter(|ix| rows.state().bounds_for_item(*ix).is_some())
                .count();
            assert!(drawn < 200, "{drawn} rows were laid out");
        });
    }

    /// Questions saved with the project load back into the Ask conversation,
    /// their output replayed from their records, with when they were asked.
    #[gpui_kit::test]
    async fn saved_questions_load_into_the_ask_conversation(cx: &mut TestAppContext) {
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
            assert!(this.answers[0].asked_at.is_some());
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
                this.working = crate::chat_input::Lanes::ALL;
            });
        })
        .unwrap();

        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("task-header").is_some() && window.try_find("task-output").is_some()
        })
        .await;
        // The referenced spec sidebar slides out over the queue's right end
        // as the task runs; let it settle beside it.
        std::thread::sleep(super::PANE_SLIDE_TIME);
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
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
                this.working = crate::chat_input::Lanes::NONE;
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
                this.working = crate::chat_input::Lanes::ALL;
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
            assert!(this.working.any(), "the task stopped working");
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
                        // Not the scroll column's buttons, hovered.
                        b.origin.x.0 >= output.left().as_f32() * scale
                            && b.origin.x.0 < column.left().as_f32() * scale
                            && b.origin.y.0 >= output.top().as_f32() * scale
                            && b.origin.y.0 < (output.top().as_f32() + 80.) * scale
                    })
                    .count()
            };
            assert_eq!(edges(light), 2, "the header's lit edges");
            assert_eq!(edges(shade), 2, "the header's shaded edges");
            // The track, and every button, spans the column's full width.
            let track = window.find("task-output-scroll-track").bounds();
            assert!(track.left() == column.left() && track.right() == column.right());
            for id in [
                "task-output-scroll-up",
                "task-output-scroll-down",
                "task-output-scroll-lock",
            ] {
                let part = window.find(id).bounds();
                assert!(
                    part.left() == column.left() && part.right() == column.right(),
                    "{id} {part:?} isn't the width of {column:?}"
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

    /// Resending a task sends its prompt again as it was sent, queuing while
    /// the harness works, and resending a previous answer asks it again,
    /// with its attachments, leaving both where they were.
    #[gpui_kit::test]
    async fn previous_prompts_can_be_resent(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-resend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Fix it".into(), cx);
                this.tasks[ix].sent = super::SentAs {
                    mode: Some(SendMode::Spec),
                    attached_text: vec!["note".into()],
                    attached_images: Vec::new(),
                    sliced: false,
                    code_task: None,
                    sent_from: None,
                    post_build_update: false,
                };
                let mut answer = PromptTask::new("Why?".into());
                answer.sent = super::SentAs {
                    mode: Some(SendMode::Ask),
                    attached_text: vec!["context".into()],
                    attached_images: Vec::new(),
                    sliced: true,
                    code_task: None,
                    sent_from: None,
                    post_build_update: false,
                };
                this.answers.push(answer);
                this.working = crate::chat_input::Lanes::ALL;
                this.resend(|this| &this.tasks, ix, window, cx);
                assert_eq!(this.tasks.len(), 1, "the task resent moved");
                assert_eq!(this.queue.len(), 1);
                assert_eq!(this.queue[0].text.as_ref(), "Fix it");
                this.resend(|this| &this.answers, 0, window, cx);
                assert_eq!(this.answers.len(), 1);
                assert_eq!(this.asks.len(), 1);
                assert_eq!(this.asks[0].task.text.as_ref(), "Why?");
                assert_eq!(this.asks[0].task.sent.attached_text, ["context"]);
                assert!(this.asks[0].task.sent.sliced);
                this.working = crate::chat_input::Lanes::NONE;
            });
        })
        .unwrap();
        cx.run_until_parked();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only a task sent in Code or Spec offers to be sent to the other mode,
    /// at the right of the latest task's header and of each previous task's
    /// heading; not one sent in Chain, a question, or one of unknown mode.
    /// Clicking it sends the task again as resending does, but in the other
    /// mode, leaving the heading closed and the task where it was. The task
    /// sent is complete, and its button, clicked, sends nothing back.
    #[gpui_kit::test]
    async fn code_and_spec_tasks_can_be_sent_to_the_other_mode(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-other-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let push = |mode: Option<SendMode>, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                this.tasks[ix].sent = super::SentAs {
                    mode,
                    attached_text: vec!["note".into()],
                    attached_images: Vec::new(),
                    sliced: true,
                    code_task: None,
                    sent_from: None,
                    post_build_update: false,
                };
                this.tasks[ix].mode = mode;
                // Over, so the next heads the view in its place.
                this.tasks[ix].status = TaskStatus::Done;
            })
        };
        let offered = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        let settle = |cx: &mut TestAppContext| {
            let start = std::time::Instant::now();
            while prompt_mode.read_with(cx, |this, _| this.working.any()) {
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "the task never ended"
                );
                cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        let modes = [
            Some(SendMode::Code),
            Some(SendMode::Both),
            Some(SendMode::Spec),
            None,
        ];
        for mode in modes {
            push(mode, cx);
        }
        // The latest, of unknown mode, doesn't offer it.
        assert!(!offered(("send-to-other-latest", 3), cx));
        push(Some(SendMode::Spec), cx);
        assert!(offered(("send-to-other-latest", 4), cx));

        // Nor does a question.
        prompt_mode.update(cx, |this, _| {
            let mut answer = PromptTask::new("Why?".into());
            answer.sent.mode = Some(SendMode::Ask);
            assert_eq!(super::other_mode(&answer), None);
            this.answers.push(answer);
        });
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.answers, 0, window, cx);
                assert!(this.asks.is_empty(), "a question was sent to another mode");
            })
        })
        .unwrap();

        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = true;
            cx.notify();
        });
        for (item, mode) in modes.into_iter().enumerate() {
            assert_eq!(
                offered(("send-to-other-previous", item), cx),
                matches!(mode, Some(SendMode::Code | SendMode::Spec)),
                "previous task {item}, sent in {mode:?}"
            );
        }

        // A Code task goes to Spec, as it was otherwise sent.
        cx.update_window(handle, |_, window, cx| {
            window.click(("send-to-other-previous", 0usize), cx)
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.task_history.open, None, "the click opened the heading");
            assert_eq!(this.tasks.len(), 6);
            assert_eq!(this.tasks[0].sent.mode, Some(SendMode::Code));
            let sent = &this.tasks[5];
            assert_eq!(sent.text.as_ref(), "Task 0");
            assert_eq!(
                sent.sent,
                super::SentAs {
                    mode: Some(SendMode::Spec),
                    attached_text: vec!["note".into()],
                    attached_images: Vec::new(),
                    sliced: true,
                    // Told of the code task, which never finished.
                    code_task: Some(crate::hidden_anchor::CodeTask {
                        prompt: "Task 0".into(),
                        result: None,
                    }),
                    // Knowing the task it was sent from.
                    sent_from: Some(this.tasks[0].name.to_string()),
                    post_build_update: false,
                }
            );
        });
        settle(cx);

        // The latest, now that Spec task, sent from Code, is complete: it
        // isn't offered, and clicked, it sends nothing back.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = false;
            cx.notify();
        });
        // The referenced spec sidebar, over the header's right while the
        // task ran, slides back first.
        cx.wait_for(handle, Duration::from_secs(2), |window, _| {
            window.try_find("referenced-files").is_none()
        })
        .await;
        assert_eq!(
            other_mode_button_at(handle, ("send-to-other-latest", 5), cx).as_deref(),
            Some("Complete, sent here from Code")
        );
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("send-to-other-latest", 5usize), cx)
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| assert_eq!(this.tasks.len(), 6));

        // A Spec task goes to Code.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = true;
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("send-to-other-previous", 4usize), cx)
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks.len(), 7);
            assert_eq!(this.tasks[6].text.as_ref(), "Task 4");
            assert_eq!(this.tasks[6].sent.mode, Some(SendMode::Code));
        });

        // While the harness works, it queues.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, 2, window, cx);
                assert_eq!(this.tasks.len(), 7);
                assert_eq!(this.queue.len(), 1);
                assert_eq!(this.queue[0].text.as_ref(), "Task 2");
                this.queue.clear();
            })
        })
        .unwrap();
        settle(cx);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task sent to the other mode, from Code or from Spec, keeps its Send
    /// to Spec or Send to Code button, but greyed out and disabled while the
    /// task sent from it waits in the queue, builds, compiles, or runs, and
    /// once it is done, in the latest task's header or among the previous
    /// tasks, its tooltip saying it is being sent or was sent; if that task
    /// fails, is cancelled, or wasn't recorded, it is enabled again. Clicked
    /// meanwhile, it sends nothing. Batch actions neither count nor send it, though it
    /// stays selected. Resend is offered throughout, and a task sent to the
    /// other mode, resent, is still sent from the task it was.
    #[gpui_kit::test]
    async fn tasks_sent_to_the_other_mode_no_longer_offer_it(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-sent-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let push =
            |mode: SendMode, status: TaskStatus, from: Option<usize>, cx: &mut TestAppContext| {
                prompt_mode.update(cx, |this, cx| {
                    let sent_from = from.map(|from| this.tasks[from].name.to_string());
                    let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                    this.tasks[ix].sent = super::SentAs {
                        mode: Some(mode),
                        attached_text: Vec::new(),
                        attached_images: Vec::new(),
                        sliced: false,
                        code_task: None,
                        sent_from,
                        post_build_update: false,
                    };
                    this.tasks[ix].mode = Some(mode);
                    this.tasks[ix].status = status;
                    ix
                })
            };
        let shown = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        // The Send to Spec or Send to Code button `id` as drawn, by its
        // tooltip, and whether it is enabled, as its task stands.
        let button = |id: (&'static str, usize), cx: &mut TestAppContext| {
            let tooltip = other_mode_button_at(handle, id, cx).expect("no button drawn");
            let enabled = prompt_mode.read_with(cx, |this, _| {
                let (_, state) = this.other_mode_state(&this.tasks[id.1]).unwrap();
                state == super::OtherModeState::Offered
            });
            (enabled, tooltip)
        };
        let expand = |expanded: bool, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.task_history.expanded = expanded;
                cx.notify();
            })
        };
        let set_status = |ix: usize, status: TaskStatus, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.tasks[ix].status = status;
                cx.notify();
            })
        };
        let queue_from = |from: usize, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                let sent_from = Some(this.tasks[from].name.to_string());
                this.queue.push(super::QueueItem {
                    id: usize::MAX - from,
                    text: "queued".into(),
                    saved: None,
                    wait: true,
                    sent_from,
                    images: Vec::new(),
                    new_conversation: false,
                    mode: Some(crate::chat_input::SendMode::Spec),
                    queued_at: 0,
                });
                cx.notify();
            })
        };
        let clear_queue = |cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.queue.clear();
                cx.notify();
            })
        };

        let code = push(SendMode::Code, TaskStatus::Done, None, cx);
        let spec = push(SendMode::Spec, TaskStatus::Done, None, cx);

        // The latest, a Spec task, offers Send to Code until a prompt sent
        // from it waits in the queue, when it is disabled; Resend stays.
        assert_eq!(
            button(("send-to-other-latest", spec), cx),
            (true, "Send to Code".into())
        );
        queue_from(spec, cx);
        assert_eq!(
            button(("send-to-other-latest", spec), cx),
            (false, "Being sent to Code".into())
        );
        assert!(shown(("resend-latest", spec), cx));
        clear_queue(cx);
        assert_eq!(
            button(("send-to-other-latest", spec), cx),
            (true, "Send to Code".into())
        );

        // The Code task sent to Spec: disabled while that task is under way
        // or done, enabled again once it failed, was cancelled, or wasn't
        // recorded.
        let to_spec = push(SendMode::Spec, TaskStatus::Building, Some(code), cx);
        expand(true, cx);
        for status in [
            TaskStatus::Building,
            TaskStatus::Compiling,
            TaskStatus::Running,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Unrecorded,
        ] {
            set_status(to_spec, status, cx);
            let sent = status.is_active() || status == TaskStatus::Done;
            let tooltip = if status == TaskStatus::Done {
                "Sent to Spec"
            } else if sent {
                "Being sent to Spec"
            } else {
                "Send to Spec"
            };
            assert_eq!(
                button(("send-to-other-previous", code), cx),
                (!sent, tooltip.into()),
                "Send to Spec, with the task sent from it {}",
                status.label()
            );
            assert!(shown(("resend-previous", code), cx));
            // The task sent, itself sent from Code, is complete, however it
            // stands.
            assert_eq!(
                button(("send-to-other-previous", to_spec), cx),
                (false, "Complete, sent here from Code".into())
            );
        }

        // The other way, the Spec task sent to Code.
        let to_code = push(SendMode::Code, TaskStatus::Running, Some(spec), cx);
        assert_eq!(
            button(("send-to-other-previous", spec), cx),
            (false, "Being sent to Code".into())
        );
        set_status(to_code, TaskStatus::Done, cx);
        assert_eq!(
            button(("send-to-other-previous", spec), cx),
            (false, "Sent to Code".into())
        );
        assert!(shown(("resend-previous", spec), cx));
        set_status(to_code, TaskStatus::Failed, cx);
        assert_eq!(
            button(("send-to-other-previous", spec), cx),
            (true, "Send to Code".into())
        );
        // Waiting in the queue, it is disabled too.
        queue_from(spec, cx);
        assert_eq!(
            button(("send-to-other-previous", spec), cx),
            (false, "Being sent to Code".into())
        );

        // Batch actions neither count nor send them, but they stay selected.
        set_status(to_spec, TaskStatus::Done, cx);
        prompt_mode.update(cx, |this, cx| {
            this.task_history.click_checkbox(code, false);
            this.task_history.click_checkbox(spec, false);
            cx.notify();
        });
        // The actions shown: the count, to Spec, and to Code.
        let actions = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.try_find("history-selected-count").is_some(),
                    window.try_find("history-send-selected-to-spec").is_some(),
                    window.try_find("history-send-selected-to-code").is_some(),
                )
            })
            .unwrap()
        };
        assert_eq!(actions(cx), (true, false, false));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                assert!(this.selected_for(SendMode::Spec).is_empty());
                assert!(this.selected_for(SendMode::Code).is_empty());
                this.send_selected_to(SendMode::Spec, window, cx);
                this.send_selected_to(SendMode::Code, window, cx);
                // Nor are they sent from their buttons, clicked meanwhile.
                this.send_to_other_mode(|this| &this.tasks, code, window, cx);
                this.send_to_other_mode(|this| &this.tasks, spec, window, cx);
                assert_eq!(this.tasks.len(), 4, "a task sent was sent again");
                assert_eq!(this.queue.len(), 1, "a task sent was queued again");
                assert_eq!(
                    this.task_history
                        .selected
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    [code, spec],
                    "deselected though not sent"
                );
            })
        })
        .unwrap();
        // Once what was sent from them came to nothing, they count again.
        set_status(to_spec, TaskStatus::Cancelled, cx);
        clear_queue(cx);
        assert_eq!(actions(cx), (true, true, true));
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.selected_for(SendMode::Spec), [code]);
            assert_eq!(this.selected_for(SendMode::Code), [spec]);
        });

        // Resent, a task sent to the other mode is still sent from the task
        // it was; being complete, it isn't sent back.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.working = crate::chat_input::Lanes::ALL;
                this.resend(|this| &this.tasks, to_spec, window, cx);
                this.send_to_other_mode(|this| &this.tasks, to_spec, window, cx);
                let from: Vec<Option<String>> = this
                    .queue
                    .iter()
                    .map(|item| item.sent_from.clone())
                    .collect();
                assert_eq!(from, [Some(this.tasks[code].name.to_string())]);
                // Queued again, the Code task isn't offered Send to Spec.
                assert_eq!(this.offers_other_mode(&this.tasks[code]), None);
                this.queue.clear();
                this.working = crate::chat_input::Lanes::NONE;
            })
        })
        .unwrap();
        cx.run_until_parked();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task sent to the other mode is saved in the history with the task
    /// it was sent from, from Code and from Spec, so once what was sent from
    /// it is done, its button is disabled, saying it was sent there, and the
    /// task sent is complete, saying it was sent here, as it runs and once
    /// the history is loaded again. A prompt queued from it is
    /// saved with it, and disables it again once the queue is loaded back.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn tasks_sent_to_the_other_mode_are_remembered_in_the_history(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        // The stand-in harness is a real process, whose events arrive from
        // its own thread.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("sent-once-history", &prompt_mode, cx);
        // A harness that is done at once.
        let script = dir.join("done-harness.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             IFS= read -r line\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        let offered = |ix: usize, cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                this.offers_other_mode(&this.tasks[ix]).is_some()
            })
        };
        let shown = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };

        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Change it".into(), SendMode::Code, Vec::new(), window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the Code task", |this| {
            !this.working.any() && this.tasks.len() == 1
        });
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.tasks[0].status),
            TaskStatus::Done
        );
        assert!(offered(0, cx));

        // Sent to Spec, it no longer offers it, while that runs and once it
        // is done.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, 0, window, cx);
                assert!(this.tasks[1].status.is_active());
                assert_eq!(
                    this.tasks[1].sent.sent_from.as_deref(),
                    Some(this.tasks[0].name.as_ref())
                );
            })
        })
        .unwrap();
        assert!(!offered(0, cx));
        run_until(cx, &prompt_mode, "the task sent to Spec", |this| {
            !this.working.any() && this.tasks.len() == 2
        });
        assert_eq!(
            prompt_mode.read_with(cx, |this, _| this.tasks[1].status),
            TaskStatus::Done
        );
        assert!(!offered(0, cx));
        // That Spec task, sent from Code, is complete.
        assert!(!offered(1, cx));

        // A Spec task, sent to Code.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Describe it".into(), SendMode::Spec, Vec::new(), window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the Spec task", |this| {
            !this.working.any() && this.tasks.len() == 3
        });
        assert!(offered(2, cx));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, 2, window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the task sent to Code", |this| {
            !this.working.any() && this.tasks.len() == 4
        });
        let names: Vec<String> = prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[3].status, TaskStatus::Done);
            assert_eq!(this.tasks[3].sent.mode, Some(SendMode::Code));
            this.tasks
                .iter()
                .map(|task| task.name.to_string())
                .collect()
        });
        assert!(!offered(2, cx));
        // The Code task sent from it is complete too.
        assert!(!offered(3, cx));

        // Loaded again from the history, each still knows where it was sent
        // from, and so still doesn't offer it: those sent from are sent, and
        // those sent are complete.
        prompt_mode.update(cx, |this, cx| {
            this.tasks.clear();
            this.load_history(cx);
        });
        run_until(cx, &prompt_mode, "the history loading", |this| {
            this.tasks.len() == 4
        });
        // Tasks saved in the same second load back in no set order, so each
        // is found by its name.
        let at = |name: &String, cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                this.tasks
                    .iter()
                    .position(|task| task.name.as_ref() == name)
                    .unwrap()
            })
        };
        let from: Vec<(Option<String>, TaskStatus)> = names
            .iter()
            .map(|name| {
                let ix = at(name, cx);
                prompt_mode.read_with(cx, |this, _| {
                    (this.tasks[ix].sent.sent_from.clone(), this.tasks[ix].status)
                })
            })
            .collect();
        assert_eq!(
            from,
            [
                (None, TaskStatus::Done),
                (Some(names[0].clone()), TaskStatus::Done),
                (None, TaskStatus::Done),
                (Some(names[2].clone()), TaskStatus::Done),
            ]
        );
        // Its button, in the latest task's header or its previous task's
        // heading, whichever it is in.
        let button = |ix: usize, cx: &mut TestAppContext| {
            let latest = prompt_mode.update(cx, |this, cx| {
                let latest = ix + 1 == this.tasks.len();
                this.task_history.expanded = !latest;
                cx.notify();
                latest
            });
            let (send, resend) = if latest {
                ("send-to-other-latest", "resend-latest")
            } else {
                ("send-to-other-previous", "resend-previous")
            };
            assert!(shown((resend, ix), cx), "no Resend for task {ix}");
            other_mode_button_at(handle, (send, ix), cx)
        };
        for (n, (name, tooltip)) in names
            .iter()
            .zip([
                "Sent to Spec",
                "Complete, sent here from Code",
                "Sent to Code",
                "Complete, sent here from Spec",
            ])
            .enumerate()
        {
            let ix = at(name, cx);
            assert_eq!(
                button(ix, cx).as_deref(),
                Some(tooltip),
                "task {n}, at {ix}"
            );
            assert_eq!(
                prompt_mode.read_with(cx, |this, _| this.offers_other_mode(&this.tasks[ix])),
                None,
                "task {n} sent, at {ix}"
            );
        }

        // A new Code task, queued from, is saved with where it was sent
        // from, and loaded back so.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Change more".into(), SendMode::Code, Vec::new(), window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the new Code task", |this| {
            !this.working.any() && this.tasks.len() == 5
        });
        let last = 4;
        let last_name = prompt_mode.read_with(cx, |this, _| this.tasks[last].name.to_string());
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.working = crate::chat_input::Lanes::ALL;
                this.send_to_other_mode(|this| &this.tasks, last, window, cx);
            })
        })
        .unwrap();
        assert_eq!(button(last, cx).as_deref(), Some("Being sent to Spec"));
        run_until(cx, &prompt_mode, "the prompt queued saving", |this| {
            this.queue.first().is_some_and(|item| item.saved.is_some())
        });
        prompt_mode.update(cx, |this, cx| {
            this.queue.clear();
            assert!(this.offers_other_mode(&this.tasks[last]).is_some());
            this.load_queue(cx);
            assert_eq!(this.queue.len(), 1);
            assert_eq!(this.queue[0].sent_from.as_deref(), Some(last_name.as_str()));
            assert_eq!(this.offers_other_mode(&this.tasks[last]), None);
            for item in this.queue.drain(..) {
                prompt_queue::remove(&item.saved.unwrap().file).unwrap();
            }
            this.queue_held = false;
            this.working = crate::chat_input::Lanes::NONE;
        });
        crate::harness::use_program_for_test(None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task itself sent from the other mode, from Code to Spec or from
    /// Spec to Code, is complete: its Send to Code or Send to Spec button is
    /// greyed out and disabled for good, in the latest task's header and in
    /// its previous task's heading, its tooltip saying it was sent here from
    /// Code, or from Spec, whatever becomes of a task sent from it and though
    /// it is marked done. Clicked, it sends nothing; right-clicked, it opens
    /// no menu, and neither opens nor closes the item. Batch actions neither
    /// count nor send it, though it stays selected. Resent, it is sent from
    /// the task it first was, and so is complete in the same way.
    #[gpui_kit::test]
    async fn tasks_sent_from_the_other_mode_are_complete(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let push =
            |mode: SendMode, status: TaskStatus, from: Option<usize>, cx: &mut TestAppContext| {
                prompt_mode.update(cx, |this, cx| {
                    let sent_from = from.map(|from| this.tasks[from].name.to_string());
                    let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                    this.tasks[ix].sent = super::SentAs {
                        mode: Some(mode),
                        attached_text: Vec::new(),
                        attached_images: Vec::new(),
                        sliced: false,
                        code_task: None,
                        sent_from,
                        post_build_update: false,
                    };
                    this.tasks[ix].mode = Some(mode);
                    this.tasks[ix].status = status;
                    ix
                })
            };
        let tooltip = |id: (&'static str, usize), cx: &mut TestAppContext| {
            other_mode_button_at(handle, id, cx).expect("no button drawn")
        };
        let offered = |ix: usize, cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.offers_other_mode(&this.tasks[ix]))
        };
        // How many tasks there are, and prompts queued.
        let sent = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| (this.tasks.len(), this.queue.len()))
        };
        let click = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click(id, cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        // Right-clicks `id`, and says whether a menu opened.
        let menu_opens = |id: (&'static str, usize), cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.right_click(id, cx);
            })
            .unwrap();
            cx.run_until_parked();
            let opened = cx
                .update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    window.try_find("popup-menu").is_some()
                })
                .unwrap();
            assert_eq!(
                opened,
                prompt_mode.read_with(cx, |this, _| this.mark_menu.is_some())
            );
            opened
        };
        let open_item =
            |cx: &mut TestAppContext| prompt_mode.read_with(cx, |this, _| this.task_history.open);

        let code = push(SendMode::Code, TaskStatus::Done, None, cx);
        let spec = push(SendMode::Spec, TaskStatus::Done, None, cx);
        let to_spec = push(SendMode::Spec, TaskStatus::Done, Some(code), cx);
        let to_code = push(SendMode::Code, TaskStatus::Done, Some(spec), cx);

        // The latest, a Code task sent from Spec: disabled, saying so, and
        // sending nothing, however it is asked to.
        let latest = ("send-to-other-latest", to_code);
        assert_eq!(tooltip(latest, cx), "Complete, sent here from Spec");
        assert_eq!(offered(to_code, cx), None);
        click(latest, cx);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, to_code, window, cx)
            })
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(sent(cx), (4, 0), "a complete task was sent back to Spec");
        assert!(!menu_opens(latest, cx), "a complete task opened its menu");

        // A previous task, a Spec task sent from Code, the same; and
        // right-clicked, it neither opens nor closes the item.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = true;
            cx.notify();
        });
        let previous = ("send-to-other-previous", to_spec);
        assert_eq!(tooltip(previous, cx), "Complete, sent here from Code");
        assert_eq!(offered(to_spec, cx), None);
        click(previous, cx);
        assert_eq!(sent(cx), (4, 0), "a complete task was sent back to Code");
        assert_eq!(open_item(cx), None, "clicking opened the item");
        assert!(!menu_opens(previous, cx), "a complete task opened its menu");
        assert_eq!(open_item(cx), None, "right-clicking opened the item");
        prompt_mode.update(cx, |this, cx| {
            this.task_history.open = Some(to_spec);
            cx.notify();
        });
        assert!(!menu_opens(previous, cx));
        assert_eq!(
            open_item(cx),
            Some(to_spec),
            "right-clicking closed the item"
        );
        prompt_mode.update(cx, |this, cx| {
            this.task_history.open = None;
            cx.notify();
        });
        // The tasks they were sent from, not complete, still open theirs.
        assert!(menu_opens(("send-to-other-previous", code), cx));
        prompt_mode.update(cx, |this, cx| {
            this.mark_menu = None;
            cx.notify();
        });
        // Nor can one be marked from its menu, even asked to directly.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let name = this.tasks[to_spec].name.clone();
                this.open_mark_menu(name, Default::default(), window, cx);
                assert!(this.mark_menu.is_none());
            })
        })
        .unwrap();

        // Batch actions neither count nor send them, though they stay
        // selected.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.click_checkbox(to_spec, false);
            this.task_history.click_checkbox(to_code, false);
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("history-selected-count").is_some());
            assert!(window.try_find("history-send-selected-to-spec").is_none());
            assert!(window.try_find("history-send-selected-to-code").is_none());
            prompt_mode.update(cx, |this, cx| {
                assert!(this.selected_for(SendMode::Spec).is_empty());
                assert!(this.selected_for(SendMode::Code).is_empty());
                this.send_selected_to(SendMode::Spec, window, cx);
                this.send_selected_to(SendMode::Code, window, cx);
                assert_eq!(
                    this.task_history
                        .selected
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    [to_spec, to_code],
                    "deselected though not sent"
                );
            })
        })
        .unwrap();
        assert_eq!(sent(cx), (4, 0), "a batch sent a complete task");
        prompt_mode.update(cx, |this, cx| {
            this.task_history.clear_selection();
            cx.notify();
        });

        // Resent, each is sent from the task it first was, from the queue as
        // when sent at once, so it is complete once it is a task.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.working = crate::chat_input::Lanes::ALL;
                this.resend(|this| &this.tasks, to_spec, window, cx);
                this.resend(|this| &this.tasks, to_code, window, cx);
                let from: Vec<Option<String>> = this
                    .queue
                    .iter()
                    .map(|item| item.sent_from.clone())
                    .collect();
                assert_eq!(
                    from,
                    [
                        Some(this.tasks[code].name.to_string()),
                        Some(this.tasks[spec].name.to_string()),
                    ]
                );
                this.queue.clear();
                this.working = crate::chat_input::Lanes::NONE;
            })
        })
        .unwrap();
        let resent = push(SendMode::Spec, TaskStatus::Done, Some(code), cx);
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = false;
            cx.notify();
        });
        assert_eq!(
            tooltip(("send-to-other-latest", resent), cx),
            "Complete, sent here from Code"
        );
        assert_eq!(offered(resent, cx), None);

        // Complete comes before anything else: its mark, and a task sent
        // from it (as one could be before tasks sent over were complete).
        prompt_mode.update(cx, |this, cx| {
            this.tasks[to_spec].marked_done = true;
            this.task_history.expanded = true;
            cx.notify();
        });
        assert_eq!(tooltip(previous, cx), "Complete, sent here from Code");
        let onward = push(SendMode::Code, TaskStatus::Running, Some(to_spec), cx);
        assert_eq!(tooltip(previous, cx), "Complete, sent here from Code");
        prompt_mode.update(cx, |this, cx| {
            this.tasks[onward].status = TaskStatus::Done;
            cx.notify();
        });
        assert_eq!(tooltip(previous, cx), "Complete, sent here from Code");
        assert_eq!(sent(cx), (6, 0));
        cx.run_until_parked();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Right-clicking a task's Send to Spec or Send to Code button, enabled
    /// or not, in the latest task's header or a previous task's heading,
    /// opens a menu offering "Mark as done", or "Mark as not done" on a task
    /// marked so, and never opens or closes the item. Marked done, the
    /// button is disabled, its tooltip saying so, and nothing is sent,
    /// whether it is clicked or a batch action is used, which leaves it
    /// selected; marked not done, it is enabled again, unless a task sent
    /// from it is under way or finished as Done.
    #[gpui_kit::test]
    async fn tasks_can_be_marked_done_by_hand(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-mark-done-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let push =
            |mode: SendMode, status: TaskStatus, from: Option<usize>, cx: &mut TestAppContext| {
                prompt_mode.update(cx, |this, cx| {
                    let sent_from = from.map(|from| this.tasks[from].name.to_string());
                    let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                    this.tasks[ix].sent = super::SentAs {
                        mode: Some(mode),
                        attached_text: Vec::new(),
                        attached_images: Vec::new(),
                        sliced: false,
                        code_task: None,
                        sent_from,
                        post_build_update: false,
                    };
                    this.tasks[ix].mode = Some(mode);
                    this.tasks[ix].status = status;
                    ix
                })
            };
        let tooltip = |id: (&'static str, usize), cx: &mut TestAppContext| {
            other_mode_button_at(handle, id, cx).expect("no button drawn")
        };
        // Right-clicks `id`, and says what the menu that opened offers.
        let right_click = |id: (&'static str, usize), cx: &mut TestAppContext| -> String {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.right_click(id, cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let mut menu = window.within("popup-menu");
                assert!(menu.try_find(1usize).is_none(), "more than one item");
                menu.find(0usize)
                    .label()
                    .expect("the item has a label")
                    .to_string()
            })
            .unwrap()
        };
        // Chooses the open menu's item, which closes it.
        let choose = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.within("popup-menu").click(0usize, cx)
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(
                    window.try_find("popup-menu").is_none(),
                    "the menu stayed open"
                );
            })
            .unwrap();
        };
        let marked = |ix: usize, cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| this.tasks[ix].marked_done)
        };
        let set_marked = |ix: usize, marked_done: bool, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                let name = this.tasks[ix].name.to_string();
                this.set_marked_done(&name, marked_done, cx)
            })
        };
        let set_status = |ix: usize, status: TaskStatus, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.tasks[ix].status = status;
                cx.notify();
            })
        };
        // How many tasks there are, and prompts queued.
        let sent = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| (this.tasks.len(), this.queue.len()))
        };

        let code = push(SendMode::Code, TaskStatus::Done, None, cx);
        let spec = push(SendMode::Spec, TaskStatus::Done, None, cx);
        let latest = ("send-to-other-latest", spec);

        // The latest, marked done from its menu: disabled, sending nothing.
        assert_eq!(tooltip(latest, cx), "Send to Code");
        assert_eq!(right_click(latest, cx), "Mark as done");
        choose(cx);
        assert!(marked(spec, cx));
        assert_eq!(tooltip(latest, cx), "Marked as done");
        assert_eq!(sent(cx), (2, 0), "marking it done sent it");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(latest, cx);
        })
        .unwrap();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, spec, window, cx)
            })
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(sent(cx), (2, 0), "a task marked done was sent");
        // Disabled, it still opens its menu, which marks it not done.
        assert_eq!(right_click(latest, cx), "Mark as not done");
        choose(cx);
        assert!(!marked(spec, cx));
        assert_eq!(tooltip(latest, cx), "Send to Code");
        assert_eq!(sent(cx), (2, 0));

        // Among the previous tasks, right-clicking neither opens nor closes
        // the item, enabled or not.
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = true;
            cx.notify();
        });
        let previous = ("send-to-other-previous", code);
        let open_item =
            |cx: &mut TestAppContext| prompt_mode.read_with(cx, |this, _| this.task_history.open);
        assert_eq!(right_click(previous, cx), "Mark as done");
        assert_eq!(open_item(cx), None, "right-clicking opened the item");
        choose(cx);
        assert_eq!(tooltip(previous, cx), "Marked as done");
        assert_eq!(open_item(cx), None);
        prompt_mode.update(cx, |this, cx| {
            this.task_history.open = Some(code);
            cx.notify();
        });
        assert_eq!(right_click(previous, cx), "Mark as not done");
        assert_eq!(open_item(cx), Some(code), "right-clicking closed the item");
        choose(cx);
        assert_eq!(tooltip(previous, cx), "Send to Spec");
        assert_eq!(open_item(cx), Some(code));
        prompt_mode.update(cx, |this, cx| {
            this.task_history.open = None;
            cx.notify();
        });

        // Batch actions treat tasks marked done as sent: neither counted nor
        // sent, though still selected.
        set_marked(code, true, cx);
        set_marked(spec, true, cx);
        prompt_mode.update(cx, |this, cx| {
            this.task_history.click_checkbox(code, false);
            this.task_history.click_checkbox(spec, false);
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("history-selected-count").is_some());
            assert!(window.try_find("history-send-selected-to-spec").is_none());
            assert!(window.try_find("history-send-selected-to-code").is_none());
            prompt_mode.update(cx, |this, cx| {
                assert!(this.selected_for(SendMode::Spec).is_empty());
                assert!(this.selected_for(SendMode::Code).is_empty());
                this.send_selected_to(SendMode::Spec, window, cx);
                this.send_selected_to(SendMode::Code, window, cx);
                assert_eq!(
                    this.task_history
                        .selected
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    [code, spec],
                    "deselected though not sent"
                );
            })
        })
        .unwrap();
        assert_eq!(sent(cx), (2, 0), "a batch sent a task marked done");
        // Unmarked, they count again.
        set_marked(code, false, cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.selected_for(SendMode::Spec), [code]);
            assert!(this.selected_for(SendMode::Code).is_empty());
        });
        prompt_mode.update(cx, |this, cx| {
            this.task_history.clear_selection();
            cx.notify();
        });

        // Marked not done, it stays disabled while a task sent from it is
        // under way or finished as Done, and is enabled once that failed.
        let to_code = push(SendMode::Code, TaskStatus::Running, Some(spec), cx);
        let previous = ("send-to-other-previous", spec);
        assert_eq!(tooltip(previous, cx), "Being sent to Code");
        set_marked(spec, false, cx);
        assert_eq!(tooltip(previous, cx), "Being sent to Code");
        set_status(to_code, TaskStatus::Done, cx);
        assert_eq!(tooltip(previous, cx), "Sent to Code");
        assert_eq!(right_click(previous, cx), "Mark as done");
        choose(cx);
        assert_eq!(tooltip(previous, cx), "Sent to Code");
        set_status(to_code, TaskStatus::Failed, cx);
        assert_eq!(tooltip(previous, cx), "Marked as done");
        set_marked(spec, false, cx);
        assert_eq!(tooltip(previous, cx), "Send to Code");
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(
                this.offers_other_mode(&this.tasks[spec]),
                Some(SendMode::Code)
            );
        });
        cx.run_until_parked();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task marked done is saved so in its history record, with the rest
    /// of the record kept as it was, and loads back marked; one whose run
    /// left no record is given a record of its mark alone, and loads back
    /// marked and still without a record. Marked not done, it loads back so.
    #[gpui_kit::test]
    async fn marks_are_kept_in_the_history(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir =
            std::env::temp_dir().join(format!("suspense-mark-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut recorded = HiddenAnchor::random();
        recorded.mode = Some(SendMode::Code);
        let recorded_file = hidden_anchor::save(&recorded, "Recorded", &dir).unwrap();
        let mut record = RunRecord {
            user_prompt: Some("Recorded, compiled".into()),
            ..RunRecord::default()
        };
        record.note(&HarnessEvent::Output(
            r#"{"type":"result","is_error":false,"result":"Done."}"#.into(),
        ));
        prompt_history::save_record(&recorded_file, &record).unwrap();
        let mut unrecorded = HiddenAnchor::random();
        unrecorded.mode = Some(SendMode::Spec);
        let unrecorded_file = hidden_anchor::save(&unrecorded, "Unrecorded", &dir).unwrap();
        let names = [recorded.name().to_string(), unrecorded.name().to_string()];

        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        let reload = |cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.tasks.clear();
                this.load_history(cx);
            });
            run_until(cx, &prompt_mode, "the history loading", |this| {
                this.tasks.len() == 2
            });
        };
        run_until(cx, &prompt_mode, "the history loading", |this| {
            this.tasks.len() == 2
        });
        // Each task, by its name: whether it is marked, and its status.
        let task = |name: &String, cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                let task = this
                    .tasks
                    .iter()
                    .find(|task| task.name.as_ref() == name)
                    .unwrap();
                (task.marked_done, task.status)
            })
        };
        let set_marked = |name: &String, marked_done: bool, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| this.set_marked_done(name, marked_done, cx));
            cx.run_until_parked();
        };
        let saved = |file: &std::path::Path| -> RunRecord {
            serde_json::from_str(&std::fs::read_to_string(file.with_extension("json")).unwrap())
                .unwrap()
        };
        assert_eq!(task(&names[0], cx), (false, TaskStatus::Done));
        assert_eq!(task(&names[1], cx), (false, TaskStatus::Unrecorded));

        set_marked(&names[0], true, cx);
        set_marked(&names[1], true, cx);
        let marked = saved(&recorded_file);
        assert!(marked.marked_done);
        assert_eq!(
            RunRecord {
                marked_done: false,
                ..marked
            },
            record,
            "the rest of the record changed"
        );
        let mark_alone = saved(&unrecorded_file);
        assert!(mark_alone.marked_done && mark_alone.holds_only_the_mark());

        reload(cx);
        assert_eq!(task(&names[0], cx), (true, TaskStatus::Done));
        assert_eq!(task(&names[1], cx), (true, TaskStatus::Unrecorded));
        prompt_mode.update(cx, |this, cx| {
            this.task_history.expanded = true;
            cx.notify();
        });
        for name in &names {
            let ix = prompt_mode.read_with(cx, |this, _| {
                this.tasks
                    .iter()
                    .position(|task| task.name.as_ref() == name)
                    .unwrap()
            });
            assert_eq!(
                other_mode_button_at(handle, ("send-to-other-previous", ix), cx).as_deref(),
                Some("Marked as done")
            );
        }

        set_marked(&names[0], false, cx);
        set_marked(&names[1], false, cx);
        assert!(!saved(&recorded_file).marked_done);
        reload(cx);
        assert_eq!(task(&names[0], cx), (false, TaskStatus::Done));
        assert_eq!(task(&names[1], cx), (false, TaskStatus::Unrecorded));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task marked done while it runs has no record to save the mark in
    /// yet; its run saves it with the record once it is over.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn a_task_marked_done_while_it_runs_is_saved_so(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        if crate::piton_build::piton_missing() {
            return;
        }
        // The stand-in harness is a real process, whose events arrive from
        // its own thread.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("mark-while-running", &prompt_mode, cx);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Change it".into(), SendMode::Code, Vec::new(), window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the task running", |this| {
            this.tasks
                .first()
                .is_some_and(|task| task.status == TaskStatus::Running)
        });
        let name = prompt_mode.update(cx, |this, cx| {
            let name = this.tasks[0].name.to_string();
            this.set_marked_done(&name, true, cx);
            name
        });
        cx.run_until_parked();
        let file = prompt_history::history_file(&dir, &name).expect("the task wasn't saved");
        assert!(
            !file.with_extension("json").exists(),
            "a record was saved while the task ran"
        );
        prompt_mode.update(cx, |this, cx| this.cancel_task(0, cx));
        run_until(cx, &prompt_mode, "the task ending", |this| {
            !this.working.any()
        });
        cx.run_until_parked();
        let record: RunRecord =
            serde_json::from_str(&std::fs::read_to_string(file.with_extension("json")).unwrap())
                .unwrap();
        assert!(record.marked_done && record.cancelled);

        prompt_mode.update(cx, |this, cx| {
            this.tasks.clear();
            this.load_history(cx);
        });
        run_until(cx, &prompt_mode, "the history loading", |this| {
            this.tasks.len() == 1
        });
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.tasks[0].marked_done);
            assert_eq!(this.tasks[0].status, TaskStatus::Cancelled);
        });
        crate::harness::use_program_for_test(None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every previous task has a checkbox before its status, which selects it
    /// without opening it; a shift-click selects the range from the one
    /// clicked last. While any is selected the row counts them and offers to
    /// send the Code ones to Spec and the Spec ones to Code, and to clear
    /// them. A batch sends oldest first, the first at once and the rest
    /// queued behind it in order, each in the other mode, never a Chain task
    /// or one of unknown mode, then deselects what it sent, leaving the list
    /// as it was. The selection is the project's, and outlasts closing the
    /// list.
    #[gpui_kit::test]
    async fn previous_tasks_can_be_selected_and_sent_to_the_other_mode(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir = std::env::temp_dir().join(format!("suspense-batch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        let modes = [
            Some(SendMode::Code),
            Some(SendMode::Both),
            Some(SendMode::Spec),
            Some(SendMode::Code),
            None,
            Some(SendMode::Spec),
            Some(SendMode::Code),
        ];
        prompt_mode.update(cx, |this, cx| {
            for mode in modes {
                let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                this.tasks[ix].sent = super::SentAs {
                    mode,
                    attached_text: vec!["note".into()],
                    attached_images: Vec::new(),
                    sliced: true,
                    code_task: None,
                    sent_from: None,
                    post_build_update: false,
                };
                this.tasks[ix].mode = mode;
            }
            this.task_history.expanded = true;
            cx.notify();
        });
        let shown = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id).is_some()
            })
            .unwrap()
        };
        // The actions shown: to Spec, to Code, and the count and Clear.
        let actions = |cx: &mut TestAppContext| {
            (
                shown("history-send-selected-to-spec", cx),
                shown("history-send-selected-to-code", cx),
                shown("history-selected-count", cx) && shown("history-clear-selected", cx),
            )
        };
        let click = |id: gpui_kit::ElementId, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| window.click(id, cx))
                .unwrap();
        };
        let shift_click = |item: usize, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                use gpui_kit::{
                    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
                    PlatformInput,
                };
                window.render_frame(cx);
                let at = window.find(("history-select", item)).bounds().center();
                let modifiers = Modifiers {
                    shift: true,
                    ..Modifiers::default()
                };
                for event in [
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: at,
                        pressed_button: None,
                        modifiers,
                    }),
                    PlatformInput::MouseDown(MouseDownEvent {
                        position: at,
                        button: MouseButton::Left,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }),
                    PlatformInput::MouseUp(MouseUpEvent {
                        position: at,
                        button: MouseButton::Left,
                        modifiers,
                        click_count: 1,
                    }),
                ] {
                    window.dispatch_event(event, cx);
                }
            })
            .unwrap();
        };
        let selected = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                this.task_history
                    .selected
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
            })
        };
        let still = |cx: &mut TestAppContext| {
            prompt_mode.read_with(cx, |this, _| {
                assert!(this.task_history.expanded, "the list closed");
                assert_eq!(this.task_history.open, None, "a heading opened");
            })
        };

        // Nothing selected, no actions.
        assert_eq!(actions(cx), (false, false, false));

        // A click selects a task without opening it.
        click(("history-select", 0usize).into(), cx);
        still(cx);
        assert_eq!(selected(cx), [0]);
        assert_eq!(actions(cx), (true, false, true));

        // A shift-click selects the range from the one clicked last.
        shift_click(3, cx);
        still(cx);
        assert_eq!(selected(cx), [0, 1, 2, 3]);
        assert_eq!(actions(cx), (true, true, true));
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.selected_for(SendMode::Spec), [0, 3]);
            assert_eq!(
                this.selected_for(SendMode::Code),
                [2],
                "the Chain task is sent"
            );
        });

        // Clicking a selected one deselects it, and with it goes the only
        // Spec task, and "Send to Code".
        click(("history-select", 2usize).into(), cx);
        assert_eq!(selected(cx), [0, 1, 3]);
        assert_eq!(actions(cx), (true, false, true));

        // Kept while the list is closed and opened again, with the row's
        // toggle, and while another project is on screen.
        click("history-toggle".into(), cx);
        assert!(!prompt_mode.read_with(cx, |this, _| this.task_history.expanded));
        click("history-toggle".into(), cx);
        assert_eq!(selected(cx), [0, 1, 3]);
        cx.update(|cx| {
            let mut other = super::ProjectSession::new(None, cx);
            prompt_mode.update(cx, |this, _| {
                this.swap_session(&mut other);
                assert!(
                    this.task_history.selected.is_empty(),
                    "shared with a project"
                );
                this.swap_session(&mut other);
            });
        });
        assert_eq!(selected(cx), [0, 1, 3]);

        // Clear deselects them all, leaving the list as it was.
        click("history-clear-selected".into(), cx);
        still(cx);
        assert!(selected(cx).is_empty());
        assert_eq!(actions(cx), (false, false, false));

        // With the harness free, the first Code task starts at once in the
        // spec lane and the next queues behind it, each to Spec; the first
        // Spec task sent to Code then starts at once in the code lane, beside
        // it, and the next queues behind it. Chain and unknown are left.
        click(("history-select", 0usize).into(), cx);
        shift_click(5, cx);
        assert_eq!(selected(cx), [0, 1, 2, 3, 4, 5]);
        click("history-send-selected-to-spec".into(), cx);
        still(cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks.len(), 8);
            assert_eq!(this.tasks[7].text.as_ref(), "Task 0");
            assert_eq!(this.tasks[7].sent.mode, Some(SendMode::Spec));
            let queued: Vec<_> = this
                .queue
                .iter()
                .map(|item| item.text.to_string())
                .collect();
            assert_eq!(queued, ["Task 3"]);
        });
        assert_eq!(selected(cx), [1, 2, 4, 5]);
        assert_eq!(actions(cx), (false, true, true));
        click("history-send-selected-to-code".into(), cx);
        still(cx);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks.len(), 9);
            assert_eq!(this.tasks[8].text.as_ref(), "Task 2");
            assert_eq!(this.tasks[8].sent.mode, Some(SendMode::Code));
            let queued: Vec<_> = this
                .queue
                .iter()
                .map(|item| item.text.to_string())
                .collect();
            assert_eq!(queued, ["Task 3", "Task 5"]);
        });
        assert_eq!(selected(cx), [1, 4]);
        assert_eq!(actions(cx), (false, false, true));

        // What was queued here can't compile outside a project; see
        // `batch_sent_tasks_run_in_order_in_the_other_mode` for them running.
        prompt_mode.update(cx, |this, _| this.queue.clear());
        let start = std::time::Instant::now();
        while prompt_mode.read_with(cx, |this, _| this.working.any()) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the task never ended"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A batch sent to the other mode runs each of its tasks in turn, in the
    /// order they were first sent, each in the other mode and saved in the
    /// history as a new task: the first straight away, the rest from the
    /// queue as the harness comes free.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn batch_sent_tasks_run_in_order_in_the_other_mode(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        // The stand-in harness is a real process, a run of each lane at
        // once, whose events arrive from their own threads.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("batch-send", &prompt_mode, cx);
        // A harness that is done at once.
        let script = dir.join("quick-harness.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        let modes = [
            Some(SendMode::Spec),
            Some(SendMode::Code),
            Some(SendMode::Both),
            Some(SendMode::Spec),
            Some(SendMode::Code),
        ];
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                for mode in modes {
                    let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                    this.tasks[ix].sent = super::SentAs {
                        mode,
                        attached_text: Vec::new(),
                        attached_images: Vec::new(),
                        sliced: false,
                        code_task: None,
                        sent_from: None,
                        post_build_update: false,
                    };
                    this.tasks[ix].mode = mode;
                    this.tasks[ix].status = TaskStatus::Done;
                }
                for item in 0..modes.len() {
                    this.task_history.click_checkbox(item, false);
                }
                this.send_selected_to(SendMode::Code, window, cx);
                this.send_selected_to(SendMode::Spec, window, cx);
                // One starts in each lane, the rest queue behind it.
                assert_eq!(this.tasks.len(), 7, "more than one started in a lane");
                assert_eq!(this.queue.len(), 2);
                assert_eq!(
                    this.task_history
                        .selected
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    [2],
                    "the Chain task was sent, or a sent one kept"
                );
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the batch running", |this| {
            !this.working.any() && this.queue.is_empty() && this.tasks.len() == 9
        });
        crate::harness::use_program_for_test(None);
        prompt_mode.read_with(cx, |this, _| {
            // Each lane runs its own in the order they were first sent.
            let sent_in = |mode| {
                this.tasks[5..]
                    .iter()
                    .filter(|task| task.sent.mode == Some(mode))
                    .map(|task| task.text.to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(sent_in(SendMode::Code), ["Task 0", "Task 3"]);
            assert_eq!(sent_in(SendMode::Spec), ["Task 1", "Task 4"]);
            assert_eq!(this.tasks[5].text.as_ref(), "Task 0");
            assert_eq!(this.tasks[6].text.as_ref(), "Task 1");
        });
        // Each is saved in the history as a new task. (Its files are named
        // to the second, so tasks this quick may load back in any order.)
        let mut saved: Vec<_> = prompt_history::load(&dir)
            .into_iter()
            .map(|saved| saved.text)
            .collect();
        saved.sort();
        assert_eq!(saved, ["Task 0", "Task 1", "Task 3", "Task 4"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Code task sent to Spec, from its button or in a batch, is told what
    /// the code task did: its system prompt is Spec's, then, on a paragraph
    /// of its own, the code-to-spec prompt holding the code task's prompt as
    /// typed and its final output, the reply text after its last tool call,
    /// exactly as they were, or a line saying it left none when it didn't
    /// finish. It remembers the code task in its history record, is told it
    /// again when resent, and says beneath its anchor's name that it was sent
    /// from Code. It and its resend are complete, never sent back to Code; a
    /// Spec task sent to Code is given Code's system prompt alone.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn code_tasks_sent_to_spec_are_told_what_the_code_task_did(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use crate::hidden_anchor::CodeTask;
        use crate::system_prompts;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        // The stand-in harness is a real process, whose events arrive from
        // its own thread.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("code-to-spec", &prompt_mode, cx);
        // A harness that is done at once, keeping the system prompt and the
        // message of each run, in turn.
        let (script, runs) = recording_harness(&dir, false);
        crate::harness::use_program_for_test(Some(script));
        // The instructions heading each run's message; every run's system
        // prompt is the project's, the same whatever the mode.
        let project_system = hidden_anchor::project_system_prompt(&dir).unwrap();
        let run = |n: usize| {
            let (message, system) = recorded_run(&runs, n);
            assert_eq!(
                system, project_system,
                "run {n} wasn't sent the project's system prompt"
            );
            instructions_in(&message).to_string()
        };

        let asked = "Make `{it}` do \\ {1 + 2} \\, per @{Nowhere}.\n\\\\\\\n  - ${CODE_RESULT}";
        let answer =
            "  Built `{a: 1}`, ${x} and \\ {y} \\.\n\\\\\\\\\n${UNDERSTANDING_FILE} ${CODE_PROMPT}";
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let push = |this: &mut PromptMode,
                            text: &str,
                            mode: SendMode,
                            status: TaskStatus,
                            events: Vec<HarnessEvent>,
                            cx: &mut gpui_kit::Context<PromptMode>| {
                    let ix = this.push_task(text.to_string().into(), cx);
                    this.tasks[ix].sent = super::SentAs {
                        mode: Some(mode),
                        attached_text: vec!["note".into()],
                        attached_images: Vec::new(),
                        sliced: false,
                        code_task: None,
                        sent_from: None,
                        post_build_update: false,
                    };
                    this.tasks[ix].mode = Some(mode);
                    for event in events {
                        this.tasks[ix].reply.apply(event);
                    }
                    this.tasks[ix].status = status;
                };
                let text = |text: &str| {
                    [
                        HarnessEvent::TextStarted,
                        HarnessEvent::TextDelta(text.into()),
                    ]
                };
                let tool = |id: &str| HarnessEvent::ToolStarted {
                    id: id.into(),
                    name: "Edit".into(),
                };
                push(
                    this,
                    asked,
                    SendMode::Code,
                    TaskStatus::Done,
                    text("Looking first.")
                        .into_iter()
                        .chain([tool("t1")])
                        .chain(text("Checking."))
                        .chain([tool("t2")])
                        .chain(text(answer))
                        .chain([HarnessEvent::Finished {
                            is_error: false,
                            result: String::new(),
                        }])
                        .collect(),
                    cx,
                );
                // Cancelled, it said something after its last call, but
                // didn't finish.
                push(
                    this,
                    "Half of it.",
                    SendMode::Code,
                    TaskStatus::Cancelled,
                    [tool("t1")]
                        .into_iter()
                        .chain(text("Partly done."))
                        .collect(),
                    cx,
                );
                push(
                    this,
                    "Spec it.",
                    SendMode::Spec,
                    TaskStatus::Done,
                    Vec::new(),
                    cx,
                );
                this.send_to_other_mode(|this| &this.tasks, 0, window, cx);
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the code task sent to Spec", |this| {
            !this.working.any() && this.tasks.len() == 4
        });
        let told = CodeTask {
            prompt: asked.to_string(),
            result: Some(answer.trim_end().to_string()),
        };
        let handoff = |result: &str| {
            format!(
                "\n\n{}",
                system_prompts::fill_code_task(
                    &system_prompts::fill_instructions(
                        system_prompts::default_prompt(system_prompts::Prompt::CodeToSpec),
                        "./src",
                        "./spec",
                        None,
                    ),
                    asked,
                    Some(result),
                )
            )
        };
        let told_spec = |sent: &str, result: &str| {
            assert!(
                sent.starts_with("We're working on the spec "),
                "not Spec's instructions: {sent}"
            );
            assert!(sent.ends_with(&handoff(result)), "{sent}");
            assert_eq!(
                sent.matches("This prompt was first sent").count(),
                1,
                "{sent}"
            );
            // Spec's understanding file is filled in, and nothing else.
            assert!(
                sent.contains(".suspense/history/")
                    && sent.contains(".understanding.md, and keep it current"),
                "{sent}"
            );
        };
        prompt_mode.read_with(cx, |this, _| {
            let sent = &this.tasks[3];
            assert_eq!(sent.text.as_ref(), asked);
            assert_eq!(
                sent.sent,
                super::SentAs {
                    mode: Some(SendMode::Spec),
                    attached_text: vec!["note".into()],
                    attached_images: Vec::new(),
                    sliced: false,
                    code_task: Some(told.clone()),
                    sent_from: Some(this.tasks[0].name.to_string()),
                    post_build_update: false,
                }
            );
            assert!(
                sent.reply.parts.iter().all(|part| !matches!(
                    part,
                    ReplyPart::Error(error) if error.contains("compile")
                )),
                "it didn't compile"
            );
        });
        told_spec(&run(0), answer.trim_end());
        // Its header says, beneath its anchor's name, that it came from Code;
        // a task that didn't, doesn't.
        let from_code = |ix: usize, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(("prompt-sent-from", ix)).is_some()
            })
            .unwrap()
        };
        assert!(prompt_mode.read_with(cx, |this, _| this.tasks[3].compiled.is_some()));
        assert!(from_code(3, cx), "the header doesn't say it came from Code");

        // Resent, it is told the same.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.resend(|this| &this.tasks, 3, window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the Spec task resent", |this| {
            !this.working.any() && this.tasks.len() == 5
        });
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[4].sent, this.tasks[3].sent);
        });
        told_spec(&run(1), answer.trim_end());

        // Sent from Code, it is complete, as is its resend, and neither is
        // sent on to Code; a Spec task sent to Code is given Code's system
        // prompt alone.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_to_other_mode(|this| &this.tasks, 3, window, cx);
                this.send_to_other_mode(|this| &this.tasks, 4, window, cx);
                assert_eq!(this.tasks.len(), 5, "a complete task was sent back");
                assert!(!this.working.any());
                this.send_to_other_mode(|this| &this.tasks, 2, window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the Spec task sent to Code", |this| {
            !this.working.any() && this.tasks.len() == 6
        });
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[5].sent.mode, Some(SendMode::Code));
            assert_eq!(this.tasks[5].sent.code_task, None);
        });
        let code = run(2);
        assert!(code.starts_with("We're working on the code "), "{code}");
        assert!(!code.contains("first sent to change the code"), "{code}");
        assert!(!from_code(5, cx));

        // A batch sent to Spec is told the same, one after the other, the
        // second from the queue; a code task that didn't finish left none.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.task_history.click_checkbox(0, false);
                this.task_history.click_checkbox(1, false);
                this.send_selected_to(SendMode::Spec, window, cx);
                assert_eq!(this.queue.len(), 1);
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the batch sent to Spec", |this| {
            !this.working.any() && this.queue.is_empty() && this.tasks.len() == 8
        });
        crate::harness::use_program_for_test(None);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[6].sent.code_task, Some(told.clone()));
            assert_eq!(
                this.tasks[7].sent.code_task,
                Some(CodeTask {
                    prompt: "Half of it.".into(),
                    result: None,
                })
            );
        });
        told_spec(&run(3), answer.trim_end());
        let none = run(4);
        assert!(
            none.ends_with(&format!(
                "The prompt the code task was sent:\n\nHalf of it.\n\n\
                 What the code task said it built, its final output:\n\n{}",
                system_prompts::NO_CODE_RESULT
            )),
            "{none}"
        );

        // Each remembers its code task in the history, and loads back with it.
        let restored: Vec<Option<CodeTask>> = prompt_history::load(&dir)
            .into_iter()
            .map(|saved| PromptTask::restore(saved).sent.code_task)
            .collect();
        assert_eq!(restored.len(), 5);
        assert_eq!(
            restored.iter().filter(|told| told.is_some()).count(),
            4,
            "{restored:?}"
        );
        assert!(restored.contains(&Some(told)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every prompt sent in one conversation, whatever its mode, is sent the
    /// same system prompt, byte for byte, so the harness's prompt cache holds:
    /// the project's. What changes from prompt to prompt, its mode's
    /// instructions with its understanding file, and a chain step's handoff,
    /// heads its message instead. A question is sent the same system prompt.
    /// A Freeform prompt has no instructions; carrying on a conversation, it
    /// keeps the conversation's system prompt, and starting one, has none.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn one_conversation_keeps_one_system_prompt(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("one-system-prompt", &prompt_mode, cx);
        let (script, runs) = recording_harness(&dir, true);
        crate::harness::use_program_for_test(Some(script));
        let send = |text: &str, mode: SendMode, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                prompt_mode.update(cx, |this, cx| {
                    this.chat_input
                        .update(cx, |input, _| input.set_post_build_update(false));
                    this.send(text.into(), mode, Vec::new(), window, cx)
                })
            })
            .unwrap();
        };
        let tasks_done = |n: usize, cx: &mut TestAppContext| {
            run_until(cx, &prompt_mode, "the tasks", move |this| {
                !this.working.any()
                    && this.tasks.len() == n
                    && this.tasks.iter().all(|task| !task.status.is_active())
            });
        };

        send("Change the code.", SendMode::Code, cx);
        tasks_done(1, cx);
        send("Change the spec.", SendMode::Spec, cx);
        tasks_done(2, cx);
        // A chain: its spec step, then its code step, handed the spec step.
        send("Change both.", SendMode::Both, cx);
        tasks_done(4, cx);
        send("Why is it so?", SendMode::Ask, cx);
        run_until(cx, &prompt_mode, "the question", |this| {
            this.asks.len() + this.answers.len() > 0
                && this.asks.iter().all(|ask| !ask.task.status.is_active())
        });
        send("Just this, as typed.", SendMode::Freeform, cx);
        tasks_done(5, cx);

        let project = hidden_anchor::project_system_prompt(&dir).unwrap().unwrap();
        let runs_seen: Vec<(String, Option<String>)> =
            (0..6).map(|n| recorded_run(&runs, n)).collect();
        for (n, (_, system)) in runs_seen.iter().enumerate() {
            assert_eq!(
                system.as_deref(),
                Some(project.as_str()),
                "run {n} was sent another system prompt"
            );
        }
        assert!(!project.contains(".understanding.md"), "{project}");
        assert!(!project.contains("We're working on the code "), "{project}");
        assert!(
            !project.contains("fluency"),
            "the system prompt holds the fluency"
        );

        // Each task's own instructions, with its understanding file, head its
        // message, and the prompt follows.
        let understanding = |message: &str| {
            let instructions = instructions_in(message);
            assert!(
                instructions.contains(".suspense/history/")
                    && instructions.contains(".understanding.md, and keep it current"),
                "no understanding file: {instructions}"
            );
            instructions.to_string()
        };
        let (code, _) = &runs_seen[0];
        assert!(understanding(code).starts_with("We're working on the code "));
        assert!(code.ends_with("\n\nChange the code."), "{code}");
        let (spec, _) = &runs_seen[1];
        assert!(understanding(spec).starts_with("We're working on the spec "));
        let (chain, _) = &runs_seen[2];
        assert!(understanding(chain).contains("first step changes the spec only"));
        let (step, _) = &runs_seen[3];
        let handed = understanding(step);
        assert!(handed.starts_with("We're working on the code "), "{handed}");
        assert!(handed.contains("second step of a chain"), "{handed}");
        assert!(handed.ends_with("Said run 2."), "{handed}");
        // Each has its own understanding file.
        assert_ne!(understanding(code), understanding(spec));
        // The question's are Ask's, with no understanding file.
        let (asked, _) = &runs_seen[4];
        let instructions = instructions_in(asked);
        assert!(instructions.starts_with("We're only asking a question "));
        assert!(!instructions.contains(".understanding.md"));
        // None repeats the spec reading. Only Spec and the chain's spec
        // step, which write Piton, point at the fluency file, and only once
        // it has been written, as a spec build writes it where piton runs.
        let fluency = crate::piton_fluency::file(&dir).exists();
        assert!(fluency, "the spec builds wrote no fluency file");
        for (n, (message, _)) in runs_seen.iter().enumerate().take(5) {
            assert!(
                !message.contains("Before executing anything, read the spec"),
                "run {n} repeats the spec reading"
            );
            let points = instructions_in(message).contains(".suspense/fluency.md once");
            assert_eq!(
                points,
                fluency && matches!(n, 1 | 2),
                "run {n} points at the fluency file or doesn't as it should"
            );
        }
        // Freeform, carrying on the conversation, has no instructions.
        assert_eq!(runs_seen[5].0, "Just this, as typed.");
        // Code, the chain's code step, and the question are told not to read
        // the spec's source; Spec and the chain's spec step aren't.
        let spec_root = crate::project_tree::Locations::read(&dir).spec.unwrap();
        let unread = format!("Read(/{}/**)", spec_root.display());
        for n in 0..6 {
            let denied =
                std::fs::read_to_string(runs.join(format!("{n}.denied"))).unwrap_or_default();
            assert_eq!(
                denied.contains(&unread),
                matches!(n, 0 | 3 | 4),
                "run {n} was told {denied:?}"
            );
        }

        // Starting a new conversation, a Freeform prompt has no system prompt.
        prompt_mode.update(cx, |this, cx| this.new_conversation(cx));
        send("Fresh.", SendMode::Freeform, cx);
        tasks_done(6, cx);
        crate::harness::use_program_for_test(None);
        assert_eq!(recorded_run(&runs, 6), ("Fresh.".to_string(), None));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A chain is sent as its steps: the Chain task writes the spec, then the
    /// same prompt goes to Code, the spec built again first, told what the
    /// spec step said; with Post-Build Spec Update on, the code step then
    /// goes to Spec, as a Code task sent to Spec does. Each step knows the
    /// one it was sent from.
    #[gpui_kit::test]
    async fn chains_are_sent_as_their_steps(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("chain-steps", &prompt_mode, cx);
        // A harness that keeps each run's system prompt and message, and says
        // which run it was before it is done.
        let (script, runs) = recording_harness(&dir, true);
        crate::harness::use_program_for_test(Some(script));
        // The instructions heading each step's message.
        let run = |n: usize| instructions_in(&recorded_run(&runs, n).0).to_string();
        let send = |post_build: bool, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                prompt_mode.update(cx, |this, cx| {
                    this.chat_input
                        .update(cx, |input, _| input.set_post_build_update(post_build));
                    this.send("Make it so.".into(), SendMode::Both, Vec::new(), window, cx)
                })
            })
            .unwrap();
        };

        // Without a post-build spec update, the chain ends with its code step.
        send(false, cx);
        run_until(cx, &prompt_mode, "the chain's code step", |this| {
            !this.working.any() && this.tasks.len() == 2 && this.tasks[1].status == TaskStatus::Done
        });
        let spec = run(0);
        assert!(spec.contains("first step changes the spec only"), "{spec}");
        let code = run(1);
        assert!(code.starts_with("We're working on the code "), "{code}");
        assert!(code.contains("second step of a chain"), "{code}");
        assert!(
            code.ends_with(
                "The prompt the chain was sent:\n\nMake it so.\n\n\
                 What the spec step said it changed, its final output:\n\nSaid run 0."
            ),
            "{code}"
        );
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[0].sent.mode, Some(SendMode::Both));
            let step = &this.tasks[1].sent;
            assert_eq!(step.mode, Some(SendMode::Code));
            assert_eq!(step.sent_from.as_deref(), Some(this.tasks[0].name.as_ref()));
            assert_eq!(
                step.code_task
                    .as_ref()
                    .and_then(|task| task.result.as_deref()),
                Some("Said run 0.")
            );
            assert!(!step.post_build_update);
        });

        // With one, the code step goes on to Spec, told what it did.
        send(true, cx);
        run_until(
            cx,
            &prompt_mode,
            "the chain's post-build spec update",
            |this| {
                !this.working.any()
                    && this.tasks.len() == 5
                    && this.tasks[4].status == TaskStatus::Done
            },
        );
        crate::harness::use_program_for_test(None);
        let update = run(4);
        assert!(update.starts_with("We're working on the spec "), "{update}");
        assert!(
            update.ends_with(
                "The prompt the code task was sent:\n\nMake it so.\n\n\
                 What the code task said it built, its final output:\n\nSaid run 3."
            ),
            "{update}"
        );
        prompt_mode.read_with(cx, |this, _| {
            assert!(this.tasks[2].sent.post_build_update);
            assert!(this.tasks[3].sent.post_build_update);
            let step = &this.tasks[4].sent;
            assert_eq!(step.mode, Some(SendMode::Spec));
            assert_eq!(step.sent_from.as_deref(), Some(this.tasks[3].name.as_ref()));
            assert!(!step.post_build_update);
        });
        // Each step is kept in the history as it was sent, in whatever order
        // tasks sent in the same second load.
        let mut restored: Vec<(Option<SendMode>, bool)> = prompt_history::load(&dir)
            .into_iter()
            .map(|saved| {
                let task = PromptTask::restore(saved);
                (task.sent.mode, task.sent.post_build_update)
            })
            .collect();
        restored.sort_by_key(|(mode, post_build)| (mode.map(SendMode::key), *post_build));
        assert_eq!(
            restored,
            [
                (Some(SendMode::Code), false),
                (Some(SendMode::Code), true),
                (Some(SendMode::Both), false),
                (Some(SendMode::Both), true),
                (Some(SendMode::Spec), false),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
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
        while prompt_mode.read_with(cx, |this, _| this.working.any()) {
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

    /// A stand-in harness in `dir` that keeps, for each run in turn, the
    /// system prompt it was given with `--append-system-prompt` in
    /// `runs/N.system`, none when it was given none, the rules it was given
    /// with `--disallowedTools` in `runs/N.denied`, and the message it was
    /// sent in `runs/N`; then, when it `finishes`, reports the conversation
    /// `s1` and says which run it was, else stops at once. Returns the script
    /// and the runs directory.
    #[cfg(unix)]
    fn recording_harness(dir: &std::path::Path, finishes: bool) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let runs = dir.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let script = dir.join("recording-harness.sh");
        let reply = if finishes {
            "echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             echo '{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}}'\n\
             echo '{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Said run '$n'.\"}}}'\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done.\"}'\n"
        } else {
            "exit 0\n"
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 n=$(ls {runs} | grep -v '\\.' | wc -l | tr -d ' ')\n\
                 file={runs}/$n\n\
                 : > \"$file\"\n\
                 fed=no\n\
                 while [ $# -gt 0 ]; do\n\
                 \x20 if [ \"$1\" = --append-system-prompt ]; then printf '%s' \"$2\" > \"$file.system\"; fi\n\
                 \x20 if [ \"$1\" = --input-format ]; then fed=yes; fi\n\
                 \x20 case \"$1\" in --disallowedTools=*) printf '%s' \"${{1#--disallowedTools=}}\" > \"$file.denied\";; esac\n\
                 \x20 shift\n\
                 done\n\
                 if [ $fed = yes ]; then IFS= read -r line; printf '%s' \"$line\" > \"$file\"; else cat > \"$file\"; fi\n\
                 {reply}",
                runs = runs.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, runs)
    }

    /// Run `n` of a [`recording_harness`]: the message it was sent, as text,
    /// and the system prompt it was given, if any.
    fn recorded_run(runs: &std::path::Path, n: usize) -> (String, Option<String>) {
        let sent = std::fs::read_to_string(runs.join(n.to_string())).unwrap();
        // Fed, the message is a stream-json user message.
        let message = match serde_json::from_str::<serde_json::Value>(&sent) {
            Ok(line) => line["message"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Err(_) => sent,
        };
        let system = std::fs::read_to_string(runs.join(format!("{n}.system"))).ok();
        (message, system)
    }

    /// The instructions heading a message, between their markers.
    fn instructions_in(message: &str) -> &str {
        let start = message
            .strip_prefix(&format!("{}\n", crate::system_prompts::INSTRUCTIONS_OPEN))
            .unwrap_or_else(|| panic!("no instructions block: {message}"));
        let end = start
            .find(&format!(
                "\n{}\n\n",
                crate::system_prompts::INSTRUCTIONS_CLOSE
            ))
            .unwrap_or_else(|| panic!("the instructions block isn't closed: {message}"));
        &start[..end]
    }

    /// A project that builds and compiles, with a stand-in harness that
    /// works on until it is stopped, and a note written for every task that
    /// finishes; the project's directory as prompt mode knows it.
    #[cfg(unix)]
    fn cancel_project(
        name: &str,
        prompt_mode: &Entity<PromptMode>,
        cx: &mut TestAppContext,
    ) -> std::path::PathBuf {
        fn summarize(_: &std::path::Path, asked: &str, _: &str) -> anyhow::Result<Option<String>> {
            Ok(Some(format!("Did {asked}")))
        }
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("piton.config.pi"),
            "use @piton/belay\nuse @piton/config\n\n\
             from @piton/belay import ClaudeCodeAdapter\n\n\
             export piton-config Project:\n    root: ./spec\n    frameworks:\n        - {Belay}\n\n\
             belay-config Belay:\n    codeRoot: ./src\n    adapters:\n        - {ClaudeCodeAdapter}\n",
        )
        .unwrap();
        std::fs::write(dir.join("spec/index.pi"), "export a: 1\n").unwrap();
        crate::harness::use_program_for_test(Some(crate::harness::tests::slow_harness(&dir)));
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| this.summarize = summarize);
        prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap())
    }

    /// Lets the referenced spec sidebar's slide settle, and the view be laid
    /// out beside it.
    fn settle_sidebar(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            std::thread::sleep(super::PANE_SLIDE_TIME);
            cx.run_until_parked();
        }
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    /// Runs prompt mode until `done`, failing after a while.
    fn run_until(
        cx: &mut TestAppContext,
        prompt_mode: &Entity<PromptMode>,
        what: &str,
        done: impl Fn(&PromptMode) -> bool,
    ) {
        let start = std::time::Instant::now();
        while !prompt_mode.read_with(cx, |this, _| done(this)) {
            if start.elapsed() > Duration::from_secs(30) {
                let tasks = prompt_mode.read_with(cx, |this, _| {
                    this.tasks
                        .iter()
                        .map(|task| {
                            let errors: Vec<&String> = task
                                .reply
                                .parts
                                .iter()
                                .filter_map(|part| match part {
                                    ReplyPart::Error(error) => Some(error),
                                    _ => None,
                                })
                                .collect();
                            format!("{} {errors:?}", task.status.label())
                        })
                        .collect::<Vec<_>>()
                });
                panic!("{what} never happened: {tasks:?}");
            }
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The most recent task still under way heads the view, whichever lane
    /// it runs in: as with two chains whose spec steps both finished before
    /// the first's code step, that code step heads it while it runs, not the
    /// second's finished spec step; the second's code step heads it once
    /// sent; and with nothing under way, the task that finished last does.
    #[gpui_kit::test]
    async fn the_most_recent_active_task_heads_the_view(cx: &mut TestAppContext) {
        let (prompt_mode, handle) = open(cx);
        cx.run_until_parked();
        let latest =
            |cx: &mut TestAppContext| prompt_mode.read_with(cx, |this, _| this.latest_ix());
        let push = |status: TaskStatus, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                this.tasks[ix].status = status;
                this.settle_latest();
                ix
            })
        };
        let finish = |ix: usize, cx: &mut TestAppContext| {
            prompt_mode.update(cx, |this, cx| {
                this.tasks[ix].status = TaskStatus::Done;
                cx.notify();
            });
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        };

        // The first chain's spec step, then the second's.
        let first_spec = push(TaskStatus::Running, cx);
        finish(first_spec, cx);
        // The first chain's code step starts; the second's spec step runs
        // beside it, sent last, so it heads the view.
        let first_code = push(TaskStatus::Running, cx);
        let second_spec = push(TaskStatus::Running, cx);
        assert_eq!(latest(cx), Some(second_spec));
        // The second's spec step finishes: the code step still running heads
        // the view, and the finished step is among the previous tasks.
        finish(second_spec, cx);
        assert_eq!(latest(cx), Some(first_code));
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(
                super::running_tasks(&this.tasks, crate::chat_input::Lanes::ALL)
                    .into_iter()
                    .map(|(ix, _)| ix)
                    .collect::<Vec<_>>(),
                vec![first_code]
            );
        });
        // The second chain's code step, sent once the lane is free, heads it.
        finish(first_code, cx);
        assert_eq!(latest(cx), Some(first_code), "the task that finished last");
        let second_code = push(TaskStatus::Running, cx);
        assert_eq!(latest(cx), Some(second_code));
        finish(second_code, cx);
        assert_eq!(latest(cx), Some(second_code));

        // Nothing under way: the task that finished most recently, even when
        // sent before another.
        let a = push(TaskStatus::Running, cx);
        let b = push(TaskStatus::Running, cx);
        finish(b, cx);
        finish(a, cx);
        assert_eq!(latest(cx), Some(a));
    }

    /// The latest task's header offers Cancel only while the task is under
    /// way: building, compiling, or running.
    #[gpui_kit::test]
    async fn only_a_task_under_way_offers_cancel(cx: &mut TestAppContext) {
        let dir =
            std::env::temp_dir().join(format!("suspense-cancel-shown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        for status in [
            TaskStatus::Building,
            TaskStatus::Compiling,
            TaskStatus::Running,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Unrecorded,
        ] {
            let ix = prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Fix it".into(), cx);
                this.tasks[ix].status = status;
                this.tasks[ix].cancel = Some(super::Cancel {
                    cancelled: Default::default(),
                    signal: None,
                    stop: None,
                });
                cx.notify();
                ix
            });
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find(("resend-latest", ix)).is_some());
                assert_eq!(
                    window.try_find(("cancel-latest", ix)).is_some(),
                    status.is_active(),
                    "Cancel is wrongly shown for a task {}",
                    status.label()
                );
            })
            .unwrap();
            // Over, so the next heads the view in its place.
            prompt_mode.update(cx, |this, _| this.tasks[ix].status = TaskStatus::Done);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Cancelling a running task stops its harness straight away, and all it
    /// started, mid-turn. The task keeps its output, its tool call still
    /// running reads "cancelled", and it reads "Cancelled", in the history
    /// too, with no commit note. The queue carries on with the next prompt,
    /// which carries on the same conversation.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn cancelling_a_run_keeps_its_output_and_the_queue_carries_on(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        if crate::piton_build::piton_missing() {
            return;
        }
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("cancel-run", &prompt_mode, cx);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("First".into(), SendMode::Code, Vec::new(), window, cx);
                this.send("Second".into(), SendMode::Code, Vec::new(), window, cx);
                assert_eq!(this.queue.len(), 1, "the second prompt was not queued");
            });
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the first run's tool call", |this| {
            this.tasks[0].status == TaskStatus::Running
                && this.tasks[0]
                    .reply
                    .parts
                    .iter()
                    .any(|part| matches!(part, ReplyPart::Tool(_)))
        });
        let grandchild = loop {
            if let Ok(pid) = std::fs::read_to_string(dir.join("grandchild"))
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let pid = prompt_mode.read_with(cx, |this, _| {
            this.tasks[0]
                .cancel
                .as_ref()
                .unwrap()
                .stop
                .as_ref()
                .unwrap()
                .pid()
                .unwrap()
        });

        // The referenced spec sidebar slides out as the task runs; once it
        // settles, the header is laid out beside it, its buttons in reach.
        settle_sidebar(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.click(("cancel-latest", 0usize), cx);
        })
        .unwrap();
        // Straight away: the task reads Cancelled, and its harness is gone.
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[0].status, TaskStatus::Cancelled);
            assert!(!this.tasks[0].can_cancel());
        });
        assert!(
            std::fs::metadata(format!("/proc/{pid}")).is_err(),
            "the harness was not ended at once"
        );
        assert!(
            crate::harness::tests::gone(grandchild),
            "what it started runs on"
        );

        run_until(
            cx,
            &prompt_mode,
            "the queue sending the next prompt",
            |this| this.tasks.len() == 2 && this.tasks[1].status == TaskStatus::Running,
        );
        prompt_mode.read_with(cx, |this, _| {
            let task = &this.tasks[0];
            assert_eq!(task.status, TaskStatus::Cancelled);
            assert!(this.queue.is_empty());
            let rows = task.reply.rows();
            assert!(
                matches!(
                    rows.as_slice(),
                    [OutputRow::Text("Working on it."), OutputRow::Tool(call)]
                        if call.state == ToolState::Cancelled
                ),
                "the output was not kept as it was: {} rows",
                rows.len()
            );
        });
        // The next task carries on the conversation the first reported.
        let start = std::time::Instant::now();
        // The first run began a conversation of its own.
        while !std::fs::read_to_string(dir.join("args"))
            .unwrap_or_default()
            .contains("--resume s1")
        {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the next task did not carry on the conversation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        settle_sidebar(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.click(("cancel-latest", 1usize), cx);
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the second task ending", |this| {
            !this.working.any()
        });
        crate::harness::use_program_for_test(None);

        // It reads Cancelled from the history, with its output.
        let saved = prompt_history::load(&dir);
        assert_eq!(saved.len(), 2);
        assert!(
            saved
                .iter()
                .all(|saved| saved.record.as_ref().unwrap().cancelled)
        );
        let first = saved
            .into_iter()
            .find(|saved| saved.text == "First")
            .unwrap();
        let restored = PromptTask::restore(first);
        assert_eq!(restored.status, TaskStatus::Cancelled);
        assert!(matches!(
            restored.reply.rows().as_slice(),
            [OutputRow::Text("Working on it."), OutputRow::Tool(call)]
                if call.state == ToolState::Cancelled
        ));
        // No commit note, however long it is given.
        std::thread::sleep(Duration::from_millis(300));
        cx.run_until_parked();
        assert!(
            crate::commit_notes::load(&dir).is_empty(),
            "a cancelled task added a note"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A task cancelled while the spec builds is abandoned there: its
    /// harness never runs, it reads "Cancelled", in the history too, and
    /// the harness is free for the next.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn cancelling_a_build_never_runs_the_harness(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        if crate::piton_build::piton_missing() {
            return;
        }
        let (prompt_mode, handle) = open(cx);
        let dir = cancel_project("cancel-build", &prompt_mode, cx);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send("Change it".into(), SendMode::Code, Vec::new(), window, cx);
                assert_eq!(this.tasks[0].status, TaskStatus::Building);
            });
            window.render_frame(cx);
            window.click(("cancel-latest", 0usize), cx);
        })
        .unwrap();
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[0].status, TaskStatus::Cancelled)
        });
        run_until(cx, &prompt_mode, "the task ending", |this| {
            !this.working.any()
        });
        // Long enough for a run to have started, had one been.
        std::thread::sleep(Duration::from_millis(300));
        cx.run_until_parked();
        crate::harness::use_program_for_test(None);
        assert!(!dir.join("ran").exists(), "the harness ran");
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[0].status, TaskStatus::Cancelled);
            assert!(this.tasks[0].compiled.is_none(), "it was compiled");
        });
        let saved = prompt_history::load(&dir);
        assert_eq!(saved.len(), 1);
        assert!(saved[0].record.as_ref().unwrap().cancelled);
        assert_eq!(
            PromptTask::restore(saved.into_iter().next().unwrap()).status,
            TaskStatus::Cancelled
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A project with no `piton.config.pi`, so any `piton build`, compile, or
    /// slice would fail, and a stand-in harness that records each run's
    /// arguments and the prompt it was given, one line per run, then
    /// finishes after a moment; the project's directory as prompt mode knows
    /// it.
    #[cfg(unix)]
    fn freeform_project(
        name: &str,
        prompt_mode: &Entity<PromptMode>,
        cx: &mut TestAppContext,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        fn summarize(_: &std::path::Path, asked: &str, _: &str) -> anyhow::Result<Option<String>> {
            Ok(Some(format!("Did {asked}")))
        }
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("harness.sh");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
echo "$@" >> {args}
IFS= read -r line
printf '%s\n' "$line" >> {stdin}
echo '{{"type":"system","subtype":"init","session_id":"s1"}}'
echo '{{"type":"result","subtype":"success","is_error":false,"result":"Done."}}'
"#,
                args = dir.join("args").display(),
                stdin = dir.join("stdin").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, _| this.summarize = summarize);
        prompt_mode.read_with(cx, |this, _| this.project_dir.clone().unwrap())
    }

    /// The prompt each run of [`freeform_project`]'s harness was given, in
    /// order, as the text of the user message it read.
    #[cfg(unix)]
    fn prompts_given(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("stdin"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let message: serde_json::Value = serde_json::from_str(line).unwrap();
                message["message"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// Images attached in the chat input are saved in the project's data as
    /// the prompt is sent, the hidden anchor keeping their paths, and given
    /// to the harness after the prompt's text; they are shown beneath the
    /// prompt in the latest task's header, listed in the raw prompt, and
    /// kept with the task in the history. Resending it, queueing it,
    /// editing it while queued, and sending a task to the other mode keep
    /// its images, and so does a message sent to the running task.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn attached_images_stay_with_the_prompt(cx: &mut TestAppContext) {
        use crate::attached_image::AttachedImage;
        use crate::chat_input::SendMode;
        use crate::harness::tests::image_blocks_of;
        // The stand-in harness is a real process, whose events arrive from
        // its own thread.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = freeform_project("images", &prompt_mode, cx);
        let png = crate::attached_image::tests::png;
        let (a, b) = (png(3, 2, "a"), png(5, 4, "b"));
        let image_a = AttachedImage::from_bytes(a.clone(), Some("a.png".into())).unwrap();
        let image_b = AttachedImage::from_bytes(b.clone(), None).unwrap();
        let chat_input = prompt_mode.read_with(cx, |this, _| this.chat_input.clone());
        let given = |line: usize| {
            let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap_or_default();
            let message: serde_json::Value =
                serde_json::from_str(stdin.lines().nth(line).expect("no such run")).unwrap();
            image_blocks_of(&message)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>()
        };
        let press_send = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                #[cfg(target_os = "macos")]
                window.press("cmd-enter", cx);
                #[cfg(not(target_os = "macos"))]
                window.press("ctrl-enter", cx);
            })
            .unwrap();
        };

        // Sent from the Freeform tab, with an image, text, and an image.
        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input.select_tab(4, window, cx);
                assert_eq!(input.mode(), SendMode::Freeform);
                input.attach_image(image_a.clone(), cx);
                input.attach_text("note".into(), cx);
                input.attach_image(image_b.clone(), cx);
                input.set_text_for_test("Look", window, cx);
            });
            chat_input.update(cx, |input, cx| input.focus_editor_for_test(window, cx));
        })
        .unwrap();
        press_send(cx);
        run_until(cx, &prompt_mode, "the task done", |this| {
            !this.working.any() && this.tasks.len() == 1
        });
        let paths = prompt_mode.read_with(cx, |this, _| this.tasks[0].sent.attached_images.clone());
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .all(|path| path.starts_with(".suspense/images/") && dir.join(path).is_file()),
            "{paths:?}"
        );
        assert_eq!(std::fs::read(dir.join(&paths[1])).unwrap(), b);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[0].sent.attached_text, ["note"]);
            assert_eq!(this.tasks[0].status, TaskStatus::Done);
        });
        assert_eq!(given(0), [a.clone(), b.clone()]);
        // Never as text in the prompt.
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        let first: serde_json::Value = serde_json::from_str(stdin.lines().next().unwrap()).unwrap();
        let text = first["message"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains(".suspense/images"), "{text}");
        // Kept in the history, and restored with the task.
        let saved = prompt_history::load(&dir)
            .into_iter()
            .find(|saved| saved.text == "Look")
            .unwrap();
        assert_eq!(saved.anchor.attached_images, paths);
        assert_eq!(PromptTask::restore(saved).sent.attached_images, paths);
        // In its message in the Freeform chat, and in the raw prompt.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .within(("question", 0usize))
                    .try_find(("prompt-image", 1usize))
                    .is_some()
            );
            prompt_mode.update(cx, |this, cx| this.open_raw_prompt(0, window, cx));
        })
        .unwrap();
        let sections = crate::raw_prompt::last_opened()
            .unwrap()
            .read_with(cx, |view, _| view.prompt().sections.clone());
        assert_eq!(sections.last().unwrap().title, "Attached images");
        assert_eq!(
            sections
                .last()
                .unwrap()
                .copied()
                .map(|text| text.to_string()),
            Some(paths.join("\n"))
        );
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();

        // Resent, it goes with its images again.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.resend(|this| &this.tasks, 0, window, cx)
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the resend done", |this| {
            !this.working.any() && this.tasks.len() == 2
        });
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[1].sent.attached_images, paths)
        });
        assert_eq!(given(1), [a.clone(), b.clone()]);

        // Queued while the harness works, it keeps them, shown beneath it in
        // the queue, and editing it brings them back into the chat input.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.working = crate::chat_input::Lanes::ALL;
                this.queue_expanded = true;
                this.resend(|this| &this.tasks, 0, window, cx);
                assert_eq!(this.queue[0].images, paths);
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the queued prompt saved", |this| {
            this.queue[0].saved.is_some()
        });
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(
                this.queue[0].saved.as_ref().unwrap().anchor.attached_images,
                paths
            )
        });
        let id = prompt_mode.read_with(cx, |this, _| this.queue[0].id);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("queued-images", 0usize)).is_some());
            prompt_mode.update(cx, |this, cx| this.edit_queued(id, window, cx));
        })
        .unwrap();
        let attached = chat_input.read_with(cx, |input, _| {
            input
                .attachments()
                .iter()
                .map(|attachment| match &attachment.attached {
                    crate::chat_input::Attaching::Text(text) => text.clone(),
                    crate::chat_input::Attaching::Image(image) => {
                        format!("{:?}", image.image.bytes.len())
                    }
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(
            attached,
            ["note".to_string(), a.len().to_string(), b.len().to_string()]
        );
        press_send(cx);
        run_until(cx, &prompt_mode, "the edit saved", |this| {
            this.editing_queued.is_none() && this.queue[0].saved.is_some()
        });
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.queue[0].images, paths);
            assert_eq!(
                this.queue[0].saved.as_ref().unwrap().anchor.attached_images,
                paths
            );
        });
        // Saved once each, however often they were sent.
        assert_eq!(
            std::fs::read_dir(crate::attached_image::images_dir(&dir))
                .unwrap()
                .count(),
            2
        );
        prompt_mode.update(cx, |this, cx| {
            this.working = crate::chat_input::Lanes::NONE;
            this.send_next(cx);
        });
        run_until(cx, &prompt_mode, "the queued prompt done", |this| {
            !this.working.any() && this.queue.is_empty() && this.tasks.len() == 3
        });
        assert_eq!(given(2), [a.clone(), b.clone()]);

        // A Code task sent to Spec keeps its images.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Code it".into(), cx);
                this.tasks[ix].sent = super::SentAs {
                    mode: Some(SendMode::Code),
                    attached_images: paths.clone(),
                    ..Default::default()
                };
                this.tasks[ix].mode = Some(SendMode::Code);
                this.working = crate::chat_input::Lanes::ALL;
                this.send_to_other_mode(|this| &this.tasks, ix, window, cx);
                let queued = this.queue.last().unwrap();
                assert_eq!(queued.text.as_ref(), "Code it");
                assert_eq!(queued.images, paths);
                this.queue.clear();
                this.working = crate::chat_input::Lanes::NONE;
            })
        })
        .unwrap();
        cx.run_until_parked();

        // A message sent to the running task carries its images.
        std::fs::remove_file(dir.join("stdin")).ok();
        crate::harness::use_program_for_test(Some(crate::harness::tests::recording_harness(
            &dir, 2,
        )));
        let crate::harness::Run { events, feed, .. } = crate::harness::send_task(
            "First".into(),
            None,
            Vec::new(),
            None,
            dir.clone(),
            Default::default(),
        );
        crate::harness::use_program_for_test(None);
        let feed = feed.unwrap();
        let start = std::time::Instant::now();
        while !feed.is_open() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the run never started"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.tasks.last_mut().unwrap().feed = Some(feed.clone());
                this.working = crate::chat_input::Lanes::ALL;
                this.send_to_task(
                    "And this".into(),
                    SendMode::Freeform,
                    crate::hidden_anchor::Attached {
                        text: Vec::new(),
                        images: vec![paths[1].clone()],
                    },
                    window,
                    cx,
                );
            })
        })
        .unwrap();
        // Sent off the main thread, once the executor runs.
        let start = std::time::Instant::now();
        while std::fs::read_to_string(dir.join("stdin"))
            .unwrap_or_default()
            .lines()
            .count()
            < 2
        {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the message was never sent"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        let events = futures::executor::block_on(futures::StreamExt::collect::<Vec<_>>(events));
        assert!(
            events.iter().any(|event| matches!(
                event,
                HarnessEvent::Sent { text, .. } if text == "And this"
            )),
            "{events:?}"
        );
        assert_eq!(given(1), [b.clone()]);
        prompt_mode.update(cx, |this, _| {
            this.working = crate::chat_input::Lanes::NONE;
            this.tasks.last_mut().unwrap().feed = None;
        });

        // A question takes images as a task does.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send_attached(
                    "Why?".into(),
                    SendMode::Ask,
                    crate::hidden_anchor::Attached {
                        text: Vec::new(),
                        images: paths.clone(),
                    },
                    window,
                    cx,
                );
                assert_eq!(this.asks.last().unwrap().task.sent.attached_images, paths);
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the question over", |this| {
            this.asks.iter().all(|ask| !ask.task.status.is_active())
        });
        crate::harness::use_program_for_test(None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Freeform prompt goes to the harness exactly as it was typed, with
    /// the text attached to it, and nothing else: no system prompt, no spec
    /// built or prompt compiled (the project has no config, so either would
    /// fail), no slices though the Slice toggle is on, and no understanding
    /// file. Otherwise it is a task: one sent while the harness works
    /// queues, it carries on the tasks' conversation, heads the message list
    /// with no hidden anchor named, adds a commit note, and is saved in the
    /// history as Freeform, from which it restores, and resends, as
    /// Freeform.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn freeform_prompts_go_to_the_harness_as_typed(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        // The stand-in harness is a real process, whose events arrive from
        // its own thread.
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open(cx);
        let dir = freeform_project("freeform", &prompt_mode, cx);
        let typed = "Fix @{Button.color} and ${Name}: {1 + 2}\n    - keep `this`\n// as is";
        let attached = vec!["some context".to_string()];
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.chat_input
                    .update(cx, |input, _| input.set_slices(true));
                this.send(
                    typed.into(),
                    SendMode::Freeform,
                    attached.clone(),
                    window,
                    cx,
                );
                let task = this.tasks.last().unwrap();
                assert_ne!(task.status, TaskStatus::Building, "it builds the spec");
                assert_eq!(task.sent.mode, Some(SendMode::Freeform));
                assert!(!task.sent.sliced, "it was sent sliced");
                // Sent while the harness works, it queues.
                this.send(
                    "Then this".into(),
                    SendMode::Freeform,
                    Vec::new(),
                    window,
                    cx,
                );
                assert_eq!(this.tasks.len(), 1, "the second didn't queue");
                assert_eq!(this.queue.len(), 1);
            });
        })
        .unwrap();
        run_until(cx, &prompt_mode, "both prompts running", |this| {
            !this.working.any() && this.queue.is_empty() && this.tasks.len() == 2
        });

        let sent = hidden_anchor::with_attached_text(typed, &attached);
        assert_eq!(prompts_given(&dir), [sent.clone(), "Then this".to_string()]);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(
            !args.contains("--append-system-prompt"),
            "a system prompt was sent: {args}"
        );
        // The second carried on the tasks' conversation.
        assert!(
            args.lines().nth(1).unwrap().contains("--resume s1"),
            "{args}"
        );
        prompt_mode.read_with(cx, |this, _| {
            for task in &this.tasks {
                assert_eq!(task.status, TaskStatus::Done);
                assert_eq!(task.sent.mode, Some(SendMode::Freeform));
                assert!(
                    !task
                        .reply
                        .parts
                        .iter()
                        .any(|part| matches!(part, ReplyPart::Error(_))),
                    "it failed, or built the spec"
                );
            }
            // It keeps the prompt as it was sent.
            assert_eq!(this.tasks[0].compiled.as_ref().unwrap().markdown, sent);
        });
        // Shown as a chat, both prompts messages with Resend beneath them,
        // with no header and no hidden anchor named.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("question", 0usize)).is_some());
            assert!(window.try_find(("resend-question", 1usize)).is_some());
            assert!(window.try_find("task-header").is_none());
            assert!(window.try_find(("prompt-anchor", 1usize)).is_none());
            assert!(window.try_find(("send-to-other-latest", 1usize)).is_none());
        })
        .unwrap();

        // Saved in the history as Freeform, unsliced, with no system prompt
        // or understanding file, and restored as it was sent.
        let history = hidden_anchor::history_dir(&dir);
        let understanding = std::fs::read_dir(&history)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("understanding")
            })
            .count();
        assert_eq!(understanding, 0, "an understanding file was written");
        let first = prompt_history::load(&dir)
            .into_iter()
            .find(|saved| saved.text == typed)
            .unwrap();
        assert_eq!(first.anchor.mode, Some(SendMode::Freeform));
        assert!(!first.anchor.sliced);
        assert!(first.anchor.system_prompt.is_none());
        assert_eq!(first.anchor.attached_text, attached);
        let restored = PromptTask::restore(first);
        assert_eq!(restored.sent.mode, Some(SendMode::Freeform));
        assert_eq!(restored.sent.attached_text, attached);
        assert_eq!(restored.compiled.as_ref().unwrap().markdown, sent);

        // Finished well, each added a commit note.
        run_until(cx, &prompt_mode, "the commit notes", |_| {
            crate::commit_notes::load(&dir).len() == 2
        });

        // Resent, it goes as Freeform again, just as it was typed.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.resend(|this| &this.tasks, 0, window, cx)
            });
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the resent prompt running", |this| {
            !this.working.any() && this.tasks.len() == 3
        });
        crate::harness::use_program_for_test(None);
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(this.tasks[2].sent.mode, Some(SendMode::Freeform));
            assert_eq!(this.tasks[2].status, TaskStatus::Done);
        });
        assert_eq!(prompts_given(&dir)[2], sent);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Freeform task is never offered to Spec or Code: not from the latest
    /// task's header, and not in a batch of selected previous tasks.
    #[gpui_kit::test]
    async fn freeform_tasks_are_never_sent_to_another_mode(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        let dir =
            std::env::temp_dir().join(format!("suspense-freeform-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (prompt_mode, handle) = open(cx);
        cx.update(|cx| ProjectDirectory::set(dir.clone(), cx));
        cx.run_until_parked();
        prompt_mode.update(cx, |this, cx| {
            for mode in [SendMode::Freeform, SendMode::Freeform] {
                let ix = this.push_task(format!("Task {}", this.tasks.len()).into(), cx);
                this.tasks[ix].sent.mode = Some(mode);
                this.tasks[ix].mode = Some(mode);
                this.tasks[ix].status = TaskStatus::Done;
                assert_eq!(super::other_mode(&this.tasks[ix]), None);
            }
            this.task_history.click_checkbox(0, false);
            assert!(this.selected_for(SendMode::Spec).is_empty());
            assert!(this.selected_for(SendMode::Code).is_empty());
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("resend-question", 1usize)).is_some());
            assert!(window.try_find(("send-to-other-latest", 1usize)).is_none());
            prompt_mode.update(cx, |this, cx| {
                this.send_selected_to(SendMode::Spec, window, cx);
                this.send_selected_to(SendMode::Code, window, cx);
                this.send_to_other_mode(|this| &this.tasks, 1, window, cx);
                assert_eq!(this.tasks.len(), 2, "a Freeform task was sent on");
                assert!(this.queue.is_empty());
            });
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Prompt mode in a window that shows dialogs over it, as the main
    /// window does.
    fn open_with_dialogs(cx: &mut TestAppContext) -> (Entity<PromptMode>, AnyWindowHandle) {
        struct WithDialogs(Entity<PromptMode>);
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
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut prompt_mode = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| PromptMode::new(window, cx));
            prompt_mode = Some(view.clone());
            let shown = cx.new(|_| WithDialogs(view));
            Root::new(shown, window, cx)
        });
        (prompt_mode.unwrap(), window.into())
    }

    /// The raw prompt modal on the latest task, opened from its header, as a
    /// test reads it.
    fn raw_prompt_of_latest(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        prompt_mode: &Entity<PromptMode>,
    ) -> crate::raw_prompt::RawPrompt {
        let ix = prompt_mode.read_with(cx, |this, _| this.tasks.len() - 1);
        prompt_mode.read_with(cx, |this, _| {
            let task = &this.tasks[ix];
            assert!(
                task.compiled.is_some(),
                "it didn't compile: {} {:?}",
                task.status.label(),
                task.reply
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        ReplyPart::Error(error) => Some(error),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            );
        });
        // Once the referenced spec sidebar is out of the way.
        settle_sidebar(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(("raw-prompt-latest", ix), cx);
            window.render_frame(cx);
            assert!(window.has_active_dialog(cx), "the raw prompt didn't open");
        })
        .unwrap();
        let view = crate::raw_prompt::last_opened().expect("no raw prompt was opened");
        view.read_with(cx, |view, _| view.prompt().clone())
    }

    /// The raw prompt `task` restored from the history shows.
    fn raw_prompt_of(task: &PromptTask) -> crate::raw_prompt::RawPrompt {
        let compiled = task.compiled.as_ref().expect("it didn't compile");
        crate::raw_prompt::RawPrompt::new(
            task.mode,
            compiled.anchor.clone(),
            &compiled.markdown,
            task.given.as_ref(),
        )
    }

    /// The latest task's header has a button beside Resend that opens the
    /// raw prompt modal, disabled until the task has compiled. Opening it
    /// opens or closes nothing else in the header; it shows the task's mode,
    /// hidden anchor, and harness beneath its title, and each section's Copy
    /// button copies its text; Escape closes it.
    #[gpui_kit::test]
    async fn raw_prompt_opens_once_the_task_has_compiled(cx: &mut TestAppContext) {
        use crate::agent::Agent;
        use crate::chat_input::SendMode;
        use crate::raw_prompt::Given;
        let (prompt_mode, handle) = open_with_dialogs(cx);
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Fix it".into(), cx);
            this.tasks[ix].mode = Some(SendMode::Code);
            cx.notify();
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let button = window.find(("raw-prompt-latest", 0usize)).bounds();
            let resend = window.find(("resend-latest", 0usize)).bounds();
            assert!(
                (button.center().y - resend.center().y).abs() < gpui_kit::px(2.),
                "{button:?} isn't beside Resend at {resend:?}"
            );
            window.click(("raw-prompt-latest", 0usize), cx);
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx), "it opened before compiling");
        })
        .unwrap();

        let sliced = "Fix it, compiled.\n\n<!-- slices -->";
        prompt_mode.update(cx, |this, cx| {
            this.tasks[0].given = Some(Given {
                harness: Agent::Claude,
                system_prompt: Some("Be brief.\n\nVery.".into()),
                instructions: None,
                resumed: false,
            });
            this.show_compiled(0, "Prompt_a".into(), sliced.into(), cx);
        });
        let before = prompt_mode.read_with(cx, |this, _| {
            (this.tasks[0].slices_open, this.task_history.expanded)
        });
        let prompt = raw_prompt_of_latest(cx, handle, &prompt_mode);
        let sections: Vec<_> = prompt
            .sections
            .iter()
            .map(|section| (section.title, section.copied().map(|text| text.to_string())))
            .collect();
        assert_eq!(
            sections,
            [
                ("System prompt", Some("Be brief.\n\nVery.".to_string())),
                ("User prompt", Some(sliced.to_string())),
            ]
        );
        assert_eq!(prompt.subtitle(), "Code · Prompt_a · Claude Code");
        prompt_mode.read_with(cx, |this, _| {
            assert_eq!(
                (this.tasks[0].slices_open, this.task_history.expanded),
                before,
                "opening it changed the header"
            );
        });
        cx.update_window(handle, |_, window, cx| {
            for id in ["raw-prompt", "raw-prompt-title", "raw-prompt-list"] {
                assert!(window.try_find(id).is_some(), "no {id}");
            }
            // Most of the window's height.
            let modal = window.find("raw-prompt").bounds();
            assert!(
                modal.size.height > window.viewport_size().height * 0.7,
                "{modal:?} is short"
            );
            window.click(("raw-prompt-copy-button", 1usize), cx);
        })
        .unwrap();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(sliced.to_string())
        );
        cx.update_window(handle, |_, window, cx| {
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(!window.has_active_dialog(cx), "Escape didn't close it");
        })
        .unwrap();
    }

    /// Whatever it holds, the modal lays out only the text in view.
    #[gpui_kit::test]
    async fn raw_prompt_lays_out_only_what_is_in_view(cx: &mut TestAppContext) {
        use crate::agent::Agent;
        use crate::raw_prompt::Given;
        let (prompt_mode, handle) = open_with_dialogs(cx);
        let long: String = (0..20_000).map(|n| format!("line {n}\n")).collect();
        prompt_mode.update(cx, |this, cx| {
            let ix = this.push_task("Long".into(), cx);
            this.tasks[ix].given = Some(Given {
                harness: Agent::Claude,
                system_prompt: Some(long.clone()),
                instructions: None,
                resumed: false,
            });
            this.show_compiled(ix, "Prompt_a".into(), "Short.".into(), cx);
        });
        let prompt = raw_prompt_of_latest(cx, handle, &prompt_mode);
        assert_eq!(
            prompt.sections[0].copied().map(|text| text.to_string()),
            Some(long)
        );
        cx.update_window(handle, |_, window, _| {
            assert!(window.try_find(("raw-prompt-text", 1usize)).is_some());
            assert!(
                window.try_find(("raw-prompt-text", 1000usize)).is_none(),
                "every row was laid out"
            );
        })
        .unwrap();
    }

    /// A Code task sent to Spec shows, in its raw prompt, the system prompt
    /// exactly as the harness received it, every placeholder filled in, the
    /// spec-reading prompt, the Piton fluency, and the code-to-spec prompt with
    /// the code task's prompt and output in place, and the compiled prompt
    /// the harness received. Restored from the history, it shows the same;
    /// from a record saved before the system prompt was kept, "Not recorded".
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn raw_prompt_shows_the_system_prompt_as_sent(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use crate::raw_prompt::NOT_RECORDED;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open_with_dialogs(cx);
        let dir = cancel_project("raw-prompt", &prompt_mode, cx);
        // A harness that is done at once, keeping the system prompt it was
        // given.
        let given = dir.join("given");
        let script = dir.join("recording-harness.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 : > {given}\n\
                 while [ $# -gt 0 ]; do\n\
                 \x20 if [ \"$1\" = --append-system-prompt ]; then printf '%s' \"$2\" > {given}; fi\n\
                 \x20 shift\n\
                 done\n\
                 exit 0\n",
                given = given.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                let ix = this.push_task("Make it blue.".into(), cx);
                this.tasks[ix].sent = super::SentAs {
                    mode: Some(SendMode::Code),
                    attached_text: Vec::new(),
                    attached_images: Vec::new(),
                    sliced: false,
                    code_task: None,
                    sent_from: None,
                    post_build_update: false,
                };
                this.tasks[ix].mode = Some(SendMode::Code);
                for event in [
                    HarnessEvent::TextStarted,
                    HarnessEvent::TextDelta("Made it blue.".into()),
                    HarnessEvent::Finished {
                        is_error: false,
                        result: String::new(),
                    },
                ] {
                    this.tasks[ix].reply.apply(event);
                }
                this.tasks[ix].status = TaskStatus::Done;
                this.send_to_other_mode(|this| &this.tasks, ix, window, cx);
            })
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the code task sent to Spec", |this| {
            !this.working.any() && this.tasks.len() == 2
        });
        crate::harness::use_program_for_test(None);

        let sent = std::fs::read_to_string(&given).unwrap();
        let prompt = raw_prompt_of_latest(cx, handle, &prompt_mode);
        assert_eq!(prompt.sections.len(), 2);
        assert_eq!(prompt.sections[0].title, "System prompt");
        let system_prompt = prompt.sections[0].copied().unwrap().to_string();
        assert_eq!(system_prompt, sent, "not the system prompt sent");
        // The system prompt is the project's: the spec reading, and nothing
        // of the task's mode or what it was handed, nor the fluency.
        for injected in [
            "Before executing anything, read the spec it touches",
            "Read the spec from its compiled reference",
        ] {
            assert!(
                system_prompt.contains(injected),
                "no {injected:?}: {system_prompt}"
            );
        }
        for apart in [
            "We're working on the spec ",
            "Make it blue.",
            ".understanding.md",
            "Piton Fluency",
            "fluency.md",
        ] {
            assert!(
                !system_prompt.contains(apart),
                "{apart:?} in the system prompt"
            );
        }
        // The message heads the prompt with its mode's instructions and what
        // it was handed.
        let message = prompt.sections[1].copied().unwrap().to_string();
        let instructions = instructions_in(&message);
        for injected in [
            "We're working on the spec ",
            "This prompt was first sent to change the code",
            "Make it blue.",
            "Made it blue.",
            ".understanding.md",
        ] {
            assert!(
                instructions.contains(injected),
                "no {injected:?}: {instructions}"
            );
        }
        assert!(
            !instructions.contains("Piton Fluency"),
            "the fluency itself is sent"
        );
        // Sent to Spec, it is pointed at the fluency file, once written.
        if crate::piton_fluency::file(&dir).exists() {
            assert!(
                instructions.contains(".suspense/fluency.md once"),
                "not pointed at the fluency file: {instructions}"
            );
        }
        for placeholder in [
            "CODE_LOCATION",
            "SPEC_LOCATION",
            "HARNESS_DIRECTORY",
            "SPEC_READING",
            "PITON_FLUENCY",
            "PITON_FLUENCY_FILE",
            "UNDERSTANDING_FILE",
            "CODE_PROMPT",
            "CODE_RESULT",
        ] {
            for text in [&system_prompt, &message] {
                assert!(
                    !text.contains(&format!("${{{placeholder}}}")),
                    "{placeholder} was left: {text}"
                );
            }
        }
        let compiled = prompt_mode.read_with(cx, |this, _| {
            this.tasks[1].compiled.as_ref().unwrap().markdown.clone()
        });
        assert_eq!(prompt.sections[1].title, "User prompt");
        assert!(message.ends_with(&format!("\n\n{compiled}")), "{message}");
        assert!(
            prompt.subtitle().starts_with("Spec · Prompt_"),
            "{}",
            prompt.subtitle()
        );
        assert!(prompt.subtitle().ends_with(" · Claude Code"));

        // Restored from the history, the same.
        let saved = || {
            prompt_history::load(&dir)
                .into_iter()
                .find(|saved| saved.anchor.mode == Some(SendMode::Spec))
                .unwrap()
        };
        assert_eq!(raw_prompt_of(&PromptTask::restore(saved())), prompt);
        // Saved before the system prompt was kept, it says so.
        let record_file = std::fs::read_dir(hidden_anchor::history_dir(&dir))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .unwrap();
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&record_file).unwrap()).unwrap();
        assert_eq!(record["systemPrompt"].as_str(), Some(sent.as_str()));
        assert_eq!(record["harness"].as_str(), Some("claude"));
        let record_map = record.as_object_mut().unwrap();
        record_map.remove("systemPrompt");
        record_map.remove("harness");
        std::fs::write(&record_file, serde_json::to_string(&record).unwrap()).unwrap();
        let old = raw_prompt_of(&PromptTask::restore(saved()));
        assert_eq!(old.sections[0].text, Err(NOT_RECORDED));
        // Nor were its instructions, so it shows the prompt alone.
        assert_eq!(
            old.sections[1].copied().map(|text| text.to_string()),
            Some(compiled)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Freeform task's raw prompt says there was no system prompt, and
    /// shows the prompt as typed.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn raw_prompt_of_a_freeform_task_has_no_system_prompt(cx: &mut TestAppContext) {
        use crate::chat_input::SendMode;
        use crate::raw_prompt::NO_SYSTEM_PROMPT;
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open_with_dialogs(cx);
        let dir = freeform_project("raw-prompt-freeform", &prompt_mode, cx);
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send(
                    "Just this.".into(),
                    SendMode::Freeform,
                    Vec::new(),
                    window,
                    cx,
                )
            });
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the prompt running", |this| {
            !this.working.any() && this.tasks.len() == 1
        });
        crate::harness::use_program_for_test(None);
        // Its chat has no header to open it from; it opens all the same.
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| this.open_raw_prompt(0, window, cx));
        })
        .unwrap();
        let prompt = crate::raw_prompt::last_opened()
            .expect("no raw prompt was opened")
            .read_with(cx, |view, _| view.prompt().clone());
        assert_eq!(prompt.sections[0].text, Err(NO_SYSTEM_PROMPT));
        assert_eq!(
            prompt.sections[1].copied().map(|text| text.to_string()),
            Some(prompts_given(&dir)[0].clone())
        );
        let saved = prompt_history::load(&dir).pop().unwrap();
        assert_eq!(raw_prompt_of(&PromptTask::restore(saved)), prompt);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// For Codex, which takes no system prompt of its own, the raw prompt is
    /// the one text it was given, the system prompt ahead of the prompt, just
    /// as it read it.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn raw_prompt_for_codex_is_the_one_prompt_it_was_given(cx: &mut TestAppContext) {
        use crate::agent::{self, Agent};
        use crate::chat_input::SendMode;
        use std::os::unix::fs::PermissionsExt as _;
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.executor().allow_parking();
        let (prompt_mode, handle) = open_with_dialogs(cx);
        let dir = cancel_project("raw-prompt-codex", &prompt_mode, cx);
        // A harness that keeps what it read, and is done once it exits.
        let given = dir.join("given");
        let script = dir.join("codex.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat > {}\nexit 0\n", given.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::harness::use_program_for_test(Some(script));
        agent::set(Agent::Codex).unwrap();
        cx.update_window(handle, |_, window, cx| {
            prompt_mode.update(cx, |this, cx| {
                this.send(
                    "Make it blue.".into(),
                    SendMode::Code,
                    Vec::new(),
                    window,
                    cx,
                )
            });
        })
        .unwrap();
        run_until(cx, &prompt_mode, "the task running", |this| {
            !this.working.any() && this.tasks.len() == 1
        });
        crate::harness::use_program_for_test(None);
        let prompt = raw_prompt_of_latest(cx, handle, &prompt_mode);
        agent::set(Agent::Claude).unwrap();
        let sent = std::fs::read_to_string(&given).unwrap();
        assert!(sent.starts_with("<system-prompt>\n"), "{sent}");
        assert_eq!(prompt.sections.len(), 1);
        assert_eq!(prompt.sections[0].title, "Prompt");
        assert_eq!(
            prompt.sections[0].copied().map(|text| text.to_string()),
            Some(sent)
        );
        assert!(
            prompt.subtitle().ends_with(" · Codex"),
            "{}",
            prompt.subtitle()
        );
        let saved = prompt_history::load(&dir).pop().unwrap();
        assert_eq!(raw_prompt_of(&PromptTask::restore(saved)), prompt);
        std::fs::remove_dir_all(&dir).ok();
    }
}
