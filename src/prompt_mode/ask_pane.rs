//! The Ask conversation (see the AskConversationScope): while the chat
//! input's Ask tab is selected, the body splits in two above the chat input,
//! the tab bar and the selected tab's contents on the left, and on the right
//! every question of the project, read as a chat. Each question sits in a
//! tinted box against the pane's right edge, its answer beneath it at the
//! pane's full width, and a divider marks where each conversation begins.
//!
//! An answer reads as a chat reply rather than a task's table: its reply
//! text after the harness's last tool call as markdown, with everything the
//! harness did before it summed up in one muted line that expands into the
//! steps, or, while it runs, a status line in that line's place.
//!
//! The whole conversation is one virtualized list: each question is a few
//! rows of it, its box, its summary or status line, each step shown, each
//! piece of its answer, and its end, so however many questions there are,
//! and however long their answers, only the rows in view are laid out.

use super::*;
use gpui_kit::component::button::ButtonCustomVariant;
use crate::task_table::ChatSegment;

/// The share of the width above the chat input the pane starts at, and the
/// least and most its edge can be dragged to.
pub(super) const ASK_SPLIT_SHARE: f32 = 0.5;
const MIN_ASK_SPLIT_SHARE: f32 = 0.25;
const MAX_ASK_SPLIT_SHARE: f32 = 0.75;

/// How wide the strip along the pane's left edge that resizes it is.
const ASK_SPLIT_HANDLE: Pixels = px(6.);

/// How tall the pane's header is: as tall as the body's tab bar, level with
/// it.
const ASK_HEADER_HEIGHT: Pixels = px(32.);

/// How far apart questions are, and how far beneath its question an answer
/// starts.
const QUESTION_GAP: Pixels = px(16.);
const ANSWER_GAP: Pixels = px(8.);

/// How much of Ask's colour tints a question's box.
const QUESTION_TINT: f32 = 0.12;

/// Dragged by the edge between the body's left side and the pane to resize
/// the split.
pub(super) struct AskSplitResize;

/// A prompt in a chat: a question saved with the project, by its place
/// among the saved questions, or asked since, by its id; or a Freeform task,
/// by its place among the tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum QuestionKey {
    Answer(usize),
    Ask(usize),
    Task(usize),
}

/// Which chat a list shows: the Ask conversation, or the Freeform chat in
/// the task view (see the MessageList's freeform).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Chat {
    Ask,
    Freeform,
}

impl Chat {
    /// The mode whose colour tints its prompts.
    fn mode(self) -> SendMode {
        match self {
            Chat::Ask => SendMode::Ask,
            Chat::Freeform => SendMode::Freeform,
        }
    }
}

impl QuestionKey {
    /// Numbers its table apart from every other, in its rows' element ids and
    /// its markdown's keys.
    fn table(self) -> usize {
        match self {
            Self::Answer(ix) => ASK_HISTORY_IX + ix,
            Self::Ask(id) => ASK_IX - id,
            Self::Task(ix) => ix,
        }
    }

    /// Its element ids, apart from every other question's.
    fn id(self) -> usize {
        self.table()
    }
}

/// What a row of a chat's list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneRow {
    /// A prompt's box, headed by a divider when it begins a conversation.
    Question(usize),
    /// A message sent to its run while it worked, by its row of the reply,
    /// in a box of its own.
    Sent(usize, usize),
    /// A segment's line summing up the steps before its answer, or, while
    /// the run is on its last segment, its status line; nothing when there
    /// is neither.
    Line(usize, usize),
    /// A step before an answer, by its row of the reply, shown while its
    /// segment's steps are expanded.
    Step(usize, usize),
    /// A piece of an answer, by its row of the reply.
    Answer(usize, usize),
    /// Its end: "No answer." for a finished one with nothing to show, the
    /// error it failed with, or that it was stopped; for a task, the files
    /// it changed beneath.
    End(usize),
    /// After the last prompt: the divider New conversation put in, if the
    /// next question starts one, and the margin ending the list.
    Tail,
}

/// What a step or a piece of an answer shows: a tool call's name and most
/// telling argument, or reply text as markdown, and where its state is kept.
enum Piece {
    Tool(String, Option<String>),
    Text(MarkdownKey, SharedString),
    /// A notice, a muted line that opens onto its details: the notice, its
    /// reply's number, and its part of the reply.
    Notice(task_table::Notice, u64, usize),
}

/// How many rows a prompt takes besides its segments': its box and its end.
const QUESTION_ROWS: usize = 2;

/// How a prompt's reply is laid out: each segment of it, and whether its
/// steps are shown.
#[derive(Clone, Debug, PartialEq)]
struct AnswerLayout {
    segments: Vec<(ChatSegment, bool)>,
}

impl Default for AnswerLayout {
    fn default() -> Self {
        Self {
            segments: vec![(ChatSegment::default(), false)],
        }
    }
}

impl AnswerLayout {
    fn segment_items((segment, shown): &(ChatSegment, bool)) -> usize {
        usize::from(segment.sent.is_some())
            + 1
            + if *shown { segment.rows.steps.len() } else { 0 }
            + segment.rows.answer.len()
    }

    fn items(&self) -> usize {
        QUESTION_ROWS + self.segments.iter().map(Self::segment_items).sum::<usize>()
    }

    /// The last segment, which a running prompt's status line heads.
    fn last(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }

    /// The first piece of the answer of segment `seg`, if any.
    fn first_answer(&self, seg: usize) -> Option<usize> {
        self.segments.get(seg)?.0.rows.answer.first().copied()
    }

    fn row_at(&self, question: usize, at: usize) -> PaneRow {
        if at == 0 {
            return PaneRow::Question(question);
        }
        let mut at = at - 1;
        for (seg, entry) in self.segments.iter().enumerate() {
            let (segment, shown) = entry;
            if let Some(sent) = segment.sent {
                if at == 0 {
                    return PaneRow::Sent(question, sent);
                }
                at -= 1;
            }
            if at == 0 {
                return PaneRow::Line(question, seg);
            }
            at -= 1;
            let steps = if *shown { segment.rows.steps.len() } else { 0 };
            if at < steps {
                return PaneRow::Step(question, segment.rows.steps[at]);
            }
            at -= steps;
            if at < segment.rows.answer.len() {
                return PaneRow::Answer(question, segment.rows.answer[at]);
            }
            at -= segment.rows.answer.len();
        }
        PaneRow::End(question)
    }
}

/// What a question showed, as last laid out, so its rows are measured again
/// once that changes.
#[derive(Clone, Debug, PartialEq)]
struct Look {
    starts: bool,
    status: TaskStatus,
    reply: (u64, u64),
    answer: AnswerLayout,
    /// Its prompt cards expanded.
    expanded: Vec<String>,
}

/// What the list was last laid out for.
#[derive(Default)]
struct PaneLaidOut {
    keys: Vec<QuestionKey>,
    looks: Vec<Option<Look>>,
    markdown: u64,
    pending: bool,
}

/// The Ask conversation of one project, kept while the application runs.
pub(super) struct AskPane {
    rows: MeasuredList,
    laid_out: RefCell<PaneLaidOut>,
    /// Its scroll stays at the bottom, following each question asked and
    /// each answer as it grows.
    pub(super) locked: bool,
    /// The question to scroll to once the pane is next laid out.
    reveal: Cell<Option<QuestionKey>>,
    /// The question whose answer's start to scroll to the top once the pane
    /// is next laid out.
    reveal_answer: Cell<Option<QuestionKey>>,
    /// The user scrolled it since a question was last asked.
    pub(super) scrolled: bool,
}

impl AskPane {
    pub(super) fn new() -> Self {
        Self {
            rows: MeasuredList::new(task_table::OVERDRAW),
            laid_out: RefCell::default(),
            // It opens scrolled to the bottom, on the newest question.
            locked: true,
            reveal: Cell::new(None),
            reveal_answer: Cell::new(None),
            scrolled: false,
        }
    }

    /// Scrolls to `key` once it is next laid out, leaving the bottom.
    pub(super) fn reveal(&mut self, key: QuestionKey) {
        self.locked = false;
        self.reveal.set(Some(key));
    }

    #[cfg(test)]
    pub(super) fn rows(&self) -> &MeasuredList {
        &self.rows
    }
}

/// Where each question's rows start, what row `ix` shows, and how each
/// question's answer is laid out.
struct PaneLayout {
    starts: Vec<usize>,
    answers: Vec<AnswerLayout>,
}

impl PaneLayout {
    fn row_at(&self, ix: usize) -> PaneRow {
        let question = match self.starts.binary_search(&ix) {
            Ok(question) => question,
            Err(0) => return PaneRow::Tail,
            Err(next) => next - 1,
        };
        let Some(answer) = self.answers.get(question) else {
            return PaneRow::Tail;
        };
        let at = ix - self.starts[question];
        if at >= answer.items() {
            return PaneRow::Tail;
        }
        answer.row_at(question, at)
    }
}

/// `secs`, seconds since the Unix epoch, in local time as `format` has it.
fn local_time(secs: u64, format: &str) -> Option<String> {
    let at = chrono::DateTime::from_timestamp(i64::try_from(secs).ok()?, 0)?;
    Some(at.with_timezone(&chrono::Local).format(format).to_string())
}

/// Now, in seconds since the Unix epoch.
pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// How long a prompt card's Edit button reads "Edited" after it is pressed.
const SENT_TO_PROMPT_FOR: Duration = Duration::from_secs(2);

/// Which of a prompt card's buttons was pressed: Edit, or a send button, by
/// the mode it sends in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    Edit,
    Send(SendMode),
}

impl CardAction {
    /// What it is known by among the card's buttons.
    fn key(self, card: &str) -> String {
        match self {
            CardAction::Edit => format!("{card}/edit"),
            CardAction::Send(mode) => format!("{card}/{}", mode_slug(mode)),
        }
    }
}

/// A mode as a `suspense-prompt` block names it.
fn mode_slug(mode: SendMode) -> &'static str {
    match mode {
        SendMode::Code => "code",
        SendMode::Both => "chain",
        SendMode::Spec => "spec",
        SendMode::Ask => "ask",
        SendMode::Freeform => "freeform",
    }
}

impl PromptMode {
    /// Whether the prompt card `card` of the question `key` had its button
    /// for `action` pressed: Edit just now, a send button ever.
    pub(super) fn sent_to_prompt(&self, key: QuestionKey, card: &str, action: CardAction) -> bool {
        match action {
            CardAction::Edit => self
                .sent_to_prompt
                .get(&format!("{}/{}", key.id(), action.key(card)))
                .is_some_and(|at| at.elapsed() < SENT_TO_PROMPT_FOR),
            CardAction::Send(_) => self
                .question(key)
                .is_some_and(|task| task.sent_prompts.contains(&action.key(card))),
        }
    }

    /// Whether the prompt card `card` of the question `key` is expanded.
    pub(super) fn card_expanded(&self, key: QuestionKey, card: &str) -> bool {
        self.expanded_cards.contains(&(key.id(), card.to_string()))
    }

    /// Expands the prompt card `card` of the question `key`, or collapses it,
    /// leaving the pane scrolled where it is.
    pub(super) fn toggle_card(&mut self, key: QuestionKey, card: String, cx: &mut Context<Self>) {
        let card = (key.id(), card);
        if !self.expanded_cards.remove(&card) {
            self.expanded_cards.insert(card);
        }
        // Held where it is rather than following the bottom.
        self.ask_pane.locked = false;
        self.freeform_pane.locked = false;
        cx.notify();
    }

    /// The question `key`'s answer finished, however it ended: the pane
    /// scrolls the answer's start to its top, unless the user scrolled it
    /// meanwhile or a question beneath it is still answering.
    pub(super) fn answer_finished(&mut self, key: QuestionKey) {
        if self.ask_pane.scrolled {
            return;
        }
        let keys = self.question_keys();
        let Some(at) = keys.iter().position(|k| *k == key) else {
            return;
        };
        let beneath_answering = keys[at + 1..]
            .iter()
            .any(|k| self.question(*k).is_some_and(|task| task.status.is_active()));
        if beneath_answering {
            return;
        }
        self.ask_pane.locked = false;
        self.ask_pane.reveal_answer.set(Some(key));
    }

    /// Does what a prompt card's button `action` does with the prompt `text`
    /// it holds, of `mode`, as the AskConversationScope's suggested prompts
    /// say. Edit puts it in the chat input, in `mode`'s tab, or Chain's for
    /// a card of no known mode, sending nothing; a send button sends it at
    /// once in its own mode, as though written on that tab and sent, with
    /// the Slice toggle as it stands and nothing attached, leaving what is
    /// being written alone.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn card_prompt(
        &mut self,
        key: QuestionKey,
        card: String,
        text: String,
        mode: Option<SendMode>,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            CardAction::Edit => {
                let mode = mode.unwrap_or(SendMode::Both);
                self.chat_input
                    .update(cx, |input, cx| input.put_prompt(text, mode, window, cx));
                self.sent_to_prompt
                    .insert(format!("{}/{}", key.id(), action.key(&card)), Instant::now());
                // It reads as done for a while, then as before.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SENT_TO_PROMPT_FOR).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            CardAction::Send(send) => {
                // Sent once from each button, for good.
                let sent = action.key(&card);
                let Some(task) = self.question_mut(key) else {
                    return;
                };
                if !task.sent_prompts.insert(sent.clone()) {
                    return;
                }
                let name = task.name.to_string();
                if let Some(project_dir) = self.project_dir.clone() {
                    cx.background_spawn(async move {
                        let file = match key {
                            QuestionKey::Task(_) => prompt_history::history_file(&project_dir, &name),
                            _ => prompt_history::ask_file(&project_dir, &name),
                        };
                        if let Some(file) = file
                            && let Err(err) = prompt_history::save_sent_prompt(&file, &sent)
                        {
                            eprintln!("could not keep the prompt sent: {err:#}");
                        }
                    })
                    .detach();
                }
                self.send_attached(text, send, Attached::default(), window, cx)
            }
        }
        cx.notify();
    }

    /// Asks `answer`, picked on the card `card` asking `question` in the
    /// answer of the question `key`, as the next question, carrying on its
    /// conversation; the pick is kept with that question, so the card is
    /// answered once.
    pub(super) fn pick_answer(
        &mut self,
        key: QuestionKey,
        card: String,
        question: &str,
        answer: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.question_mut(key) else {
            return;
        };
        if task.picked.contains_key(&card) {
            return;
        }
        task.picked.insert(card.clone(), answer.clone());
        let name = task.name.to_string();
        if let Some(project_dir) = self.project_dir.clone() {
            let picked = answer.clone();
            cx.background_spawn(async move {
                if let Some(file) = prompt_history::ask_file(&project_dir, &name)
                    && let Err(err) = prompt_history::save_picked(&file, &card, &picked)
                {
                    eprintln!("could not keep the answer picked: {err:#}");
                }
            })
            .detach();
        }
        let text = crate::answer_blocks::picked_question(question, &answer);
        self.send_as(
            text,
            SendMode::Ask,
            Attached::default(),
            false,
            None,
            None,
            false,
            None,
            window,
            cx,
        );
        cx.notify();
    }
    /// Every question of the project, oldest first: those saved with it, then
    /// those asked since.
    fn question_keys(&self) -> Vec<QuestionKey> {
        (0..self.answers.len())
            .map(QuestionKey::Answer)
            .chain(self.asks.iter().map(|ask| QuestionKey::Ask(ask.id)))
            .collect()
    }

    /// The Freeform chat's prompts, oldest first: the latest task and the
    /// Freeform tasks sent straight before it, back to the last task of
    /// another mode, or to the first that started a new conversation. None
    /// while the latest task isn't Freeform.
    pub(super) fn freeform_keys(&self) -> Vec<QuestionKey> {
        self.latest_ix()
            .map(|latest| self.freeform_keys_to(latest))
            .unwrap_or_default()
    }

    /// The Freeform chat ending with the task at `latest`, as
    /// [`Self::freeform_keys`] gives the latest's.
    pub(super) fn freeform_keys_to(&self, latest: usize) -> Vec<QuestionKey> {
        let mut keys = Vec::new();
        if latest >= self.tasks.len() {
            return keys;
        }
        for (ix, task) in self.tasks[..=latest].iter().enumerate().rev() {
            if task.mode != Some(SendMode::Freeform) {
                break;
            }
            keys.push(QuestionKey::Task(ix));
            if task.new_conversation {
                break;
            }
        }
        keys.reverse();
        keys
    }

    /// The pane `chat` is shown in.
    fn chat_pane(&self, chat: Chat) -> &AskPane {
        match chat {
            Chat::Ask => &self.ask_pane,
            Chat::Freeform => &self.freeform_pane,
        }
    }

    fn chat_pane_mut(&mut self, chat: Chat) -> &mut AskPane {
        match chat {
            Chat::Ask => &mut self.ask_pane,
            Chat::Freeform => &mut self.freeform_pane,
        }
    }

    /// The task or question `key` names, to change.
    pub(super) fn question_mut(&mut self, key: QuestionKey) -> Option<&mut PromptTask> {
        match key {
            QuestionKey::Answer(ix) => self.answers.get_mut(ix),
            QuestionKey::Ask(id) => self
                .asks
                .iter_mut()
                .find(|ask| ask.id == id)
                .map(|ask| &mut ask.task),
            QuestionKey::Task(ix) => self.tasks.get_mut(ix),
        }
    }

    pub(super) fn question(&self, key: QuestionKey) -> Option<&PromptTask> {
        match key {
            QuestionKey::Answer(ix) => self.answers.get(ix),
            QuestionKey::Ask(id) => self
                .asks
                .iter()
                .find(|ask| ask.id == id)
                .map(|ask| &ask.task),
            QuestionKey::Task(ix) => self.tasks.get(ix),
        }
    }

    /// Whether each question begins a conversation: the first, and each whose
    /// record says it started one. The Freeform chat marks none.
    fn conversation_starts(&self, keys: &[QuestionKey]) -> Vec<bool> {
        if keys
            .first()
            .is_some_and(|key| matches!(key, QuestionKey::Task(_)))
        {
            return vec![false; keys.len()];
        }
        keys.iter()
            .enumerate()
            .map(|(ix, key)| {
                ix == 0
                    || self
                        .question(*key)
                        .is_some_and(|task| task.new_conversation)
            })
            .collect()
    }

    /// Brings the question `id` into view: the Ask tab selected, its pane
    /// scrolled to the question.
    pub fn reveal_question(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_input
            .update(cx, |input, cx| input.select_mode(SendMode::Ask, window, cx));
        self.on_ask_tab = true;
        self.ask_pane.reveal(QuestionKey::Ask(id));
        cx.notify();
    }

    /// Stops the question `id`, if it is still running, leaving the others
    /// running. Its run's record is saved as far as it went.
    pub(super) fn stop_ask(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(ask) = self.asks.iter_mut().find(|ask| ask.id == id)
            && ask.task.status.is_active()
        {
            ask.task.cancel();
            ask.task.end();
            ask.stopped.store(true, Ordering::SeqCst);
            // Dropping its run stops it, and saves what came of it.
            ask._run = Task::ready(());
            self.answer_finished(QuestionKey::Ask(id));
            cx.notify();
        }
    }

    /// Asks the question `key` again, as it was asked; it comes in at the
    /// bottom.
    pub(super) fn resend_question(
        &mut self,
        key: QuestionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A Freeform task is resent as any task is.
        if let QuestionKey::Task(ix) = key {
            self.resend(|this| &this.tasks, ix, window, cx);
            return;
        }
        let Some(task) = self.question(key) else {
            return;
        };
        let (text, sent) = (task.text.to_string(), task.sent.clone());
        // Named after it, rather than titled again.
        let named_after = Some(task.name.to_string());
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
            SendMode::Ask,
            sent.attached(),
            sent.sliced,
            None,
            None,
            false,
            named_after,
            window,
            cx,
        );
    }

    /// Tells the list what changed since it was last laid out: questions
    /// asked or loaded, and each question whose answer, steps shown, status,
    /// or divider changed, or whose markdown finished parsing.
    fn lay_out_chat(&self, chat: Chat, keys: &[QuestionKey], cx: &App) -> PaneLayout {
        let pane = self.chat_pane(chat);
        let rows = &pane.rows;
        let mut laid_out = pane.laid_out.borrow_mut();
        let old_items = |looks: &[Option<Look>], ix: usize| {
            looks[ix]
                .as_ref()
                .map_or(QUESTION_ROWS, |look| look.answer.items())
        };
        if laid_out.keys != keys {
            let kept = laid_out.keys.len();
            // The list holds its tail from the first time it is laid out.
            if rows.count() > 0 && kept <= keys.len() && keys[..kept] == laid_out.keys[..] {
                // Asked since: new questions come in above the tail.
                let tail = rows.count().saturating_sub(1);
                let added = keys.len() - kept;
                rows.splice(tail..tail, added * QUESTION_ROWS);
                laid_out.looks.extend((0..added).map(|_| None));
            } else {
                rows.reset(keys.len() * QUESTION_ROWS + 1);
                laid_out.looks = vec![None; keys.len()];
                laid_out.pending = false;
            }
            laid_out.keys = keys.to_vec();
        }
        // Markdown that has finished parsing is measured again.
        let parsed: HashSet<usize> = MarkdownStates::changed_since(&mut laid_out.markdown, cx)
            .into_iter()
            .map(|key| key.table)
            .collect();
        let starts_conversation = self.conversation_starts(keys);
        let mut base = 0;
        let mut starts = Vec::with_capacity(keys.len());
        let mut answers = Vec::with_capacity(keys.len());
        for (ix, key) in keys.iter().enumerate() {
            starts.push(base);
            let old = old_items(&laid_out.looks, ix);
            let Some(task) = self.question(*key) else {
                let answer = laid_out.looks[ix]
                    .as_ref()
                    .map(|look| look.answer.clone())
                    .unwrap_or_default();
                base += old;
                answers.push(answer);
                continue;
            };
            let answer = AnswerLayout {
                segments: task
                    .reply
                    .chat_segments()
                    .into_iter()
                    .enumerate()
                    .map(|(seg, segment)| {
                        let shown = self.chat_steps_shown.contains(&(key.id(), seg));
                        (segment, shown)
                    })
                    .collect(),
            };
            let look = Look {
                starts: starts_conversation[ix],
                status: task.status,
                reply: task.reply.version(),
                answer: answer.clone(),
                expanded: {
                    let mut expanded: Vec<String> = self
                        .expanded_cards
                        .iter()
                        .filter(|(question, _)| *question == key.id())
                        .map(|(_, card)| card.clone())
                        .collect();
                    expanded.sort();
                    expanded
                },
            };
            let items = answer.items();
            if items != old {
                rows.splice(base..base + old, items);
            } else if laid_out.looks[ix].as_ref() != Some(&look) || parsed.contains(&key.table()) {
                rows.remeasure(base..base + items);
            } else if task.status.is_active() {
                // While it runs, its status line follows the harness.
                rows.remeasure(base + 1..base + items);
            }
            laid_out.looks[ix] = Some(look);
            base += items;
            answers.push(answer);
        }
        let pending = chat == Chat::Ask && self.ask_new_pending;
        if laid_out.pending != pending {
            rows.remeasure(base..base + 1);
            laid_out.pending = pending;
        }
        PaneLayout { starts, answers }
    }

    /// The divider heading the conversation begun at `at`, in seconds since
    /// the Unix epoch, or, with none, the one New conversation put in at the
    /// bottom before the next question is asked.
    fn conversation_divider(id: usize, at: Option<u64>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let color = chat_input::mode_color(SendMode::Ask, cx);
        let line = || div().flex_1().h(px(1.)).bg(color.opacity(0.6));
        let when = at.and_then(|at| local_time(at, "%-d %b %Y, %H:%M"));
        let divider = h_flex()
            .id(("ask-conversation", id))
            .w_full()
            .items_center()
            .gap_2()
            .py_1()
            .text_xs()
            .child(line())
            .child(
                h_flex()
                    .flex_none()
                    .gap_1p5()
                    .child(div().text_color(color).child("New conversation"))
                    .children(
                        when.map(|when| div().text_color(theme.muted_foreground).child(when)),
                    ),
            )
            .child(line());
        // Lets UI tests find the divider; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(divider).into_any_element()
    }

    /// A question's box, against the pane's right edge: the question as
    /// typed, with anything it was sent with beneath it, then when it was
    /// asked and, once it is over, Resend.
    #[allow(clippy::too_many_arguments)]
    fn question_row(
        &self,
        chat: Chat,
        key: QuestionKey,
        task: &PromptTask,
        first: bool,
        starts: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = key.id();
        let tint = Hsla {
            a: QUESTION_TINT,
            ..chat_input::mode_color(chat.mode(), cx)
        };
        let attached_text = task.sent.attached_text.iter().map(|text| {
            let lines = text.lines().count();
            h_flex()
                .gap_1p5()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::TextQuote).xsmall().flex_none())
                .child(div().min_w_0().truncate().child(first_line(text)))
                .when(lines > 1, |row| {
                    row.child(div().flex_none().child(format!("{lines} lines")))
                })
        });
        let bubble = div()
            .id(("question", id))
            .max_w(relative(0.8))
            .min_w_0()
            .overflow_hidden()
            .rounded(px(6.))
            .bg(theme.secondary)
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .bg(tint)
                    .child(div().min_w_0().child(task.text.clone()))
                    .children(attached_text)
                    .children(task_images(id, task, Some(self.file_opener(cx)), cx)),
            );
        // Lets UI tests find the question; inert in normal builds.
        let bubble = gpui_kit::TestSupportExt::test_support(bubble);
        let asked = task.asked_at.and_then(|at| local_time(at, "%H:%M"));
        let done = !task.status.is_active();
        let this = cx.entity().downgrade();
        let footer = h_flex()
            .justify_end()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(asked)
            .when(done, |footer| {
                footer.child(resend_button(("resend-question", id)).on_click(
                    move |_, window, cx| {
                        this.update(cx, |this, cx| this.resend_question(key, window, cx))
                            .ok();
                    },
                ))
            })
            // A Freeform task is cancelled from beneath its prompt while it
            // runs.
            .when_some(
                match key {
                    QuestionKey::Task(ix) if task.can_cancel() => Some(ix),
                    _ => None,
                },
                |footer, ix| {
                    footer.child(
                        cancel_button(("cancel-freeform", ix))
                            .on_click(cx.listener(move |this, _, _, cx| this.cancel_task(ix, cx))),
                    )
                },
            );
        v_flex()
            .w_full()
            .px_4()
            .pt(if first { px(12.) } else { QUESTION_GAP })
            .gap_1()
            .when(starts, |row| {
                row.child(
                    div()
                        .pb_2()
                        .child(Self::conversation_divider(id, task.asked_at, cx)),
                )
            })
            .child(h_flex().w_full().justify_end().child(bubble))
            .child(footer)
            .pb(ANSWER_GAP)
            .into_any_element()
    }

    /// A question's line above its answer: while it runs, a spinner, the
    /// latest thing the harness did, and Stop; once over, the steps before
    /// its answer summed up, beside a chevron that expands them; nothing
    /// when there were none.
    fn answer_line(
        &self,
        key: QuestionKey,
        task: &PromptTask,
        answer: &AnswerLayout,
        seg: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = key.id();
        let muted = cx.theme().muted_foreground;
        // Only the segment the run is on shows its status.
        if task.status.is_active() && seg == answer.last() {
            let stop = match key {
                QuestionKey::Ask(ask) => Some(
                    Button::new(("stop-ask", ask))
                        .ghost()
                        .xsmall()
                        .icon(IconName::CircleStop)
                        .label("Stop")
                        .tooltip("Stop this question, leaving the others running")
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_ask(ask, cx))),
                ),
                QuestionKey::Answer(_) | QuestionKey::Task(_) => None,
            };
            let row = h_flex()
                .id(("question-status", id))
                .w_full()
                .px_4()
                .pb_1()
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(muted)
                .child(div().flex_none().child(Spinner::new().small()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(latest_row(id, &task.reply, cx)),
                )
                .children(stop);
            return gpui_kit::TestSupportExt::test_support(row).into_any_element();
        }
        let Some((segment, shown)) = answer.segments.get(seg) else {
            return div().into_any_element();
        };
        if segment.rows.steps.is_empty() {
            return div().into_any_element();
        }
        let shown = *shown;
        let toggle = h_flex()
            .id(("answer-steps", id))
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .text_sm()
            .text_color(muted)
            .child(
                Icon::new(if shown {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall(),
            )
            .child(task.reply.steps_summary(&segment.rows.steps))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.chat_steps_shown.remove(&(id, seg)) {
                    this.chat_steps_shown.insert((id, seg));
                }
                cx.notify();
            }));
        div()
            .id(("answer-segment", seg))
            .w_full()
            .px_4()
            .pb_1()
            .child(gpui_kit::TestSupportExt::test_support(toggle))
            .into_any_element()
    }

    /// A message sent to a prompt's run while it worked, in a box of its own
    /// on the right, laid out as a prompt's is.
    fn sent_row(chat: Chat, key: QuestionKey, ix: usize, text: &str, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let tint = Hsla {
            a: QUESTION_TINT,
            ..chat_input::mode_color(chat.mode(), cx)
        };
        let bubble = div()
            .id(("sent-message", ix))
            .max_w(relative(0.8))
            .min_w_0()
            .overflow_hidden()
            .rounded(px(6.))
            .bg(theme.secondary)
            .child(div().px_3().py_2().bg(tint).child(text.to_string()));
        div()
            .id(("sent-of", key.id()))
            .w_full()
            .px_4()
            .pt_2()
            .pb(ANSWER_GAP)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .child(gpui_kit::TestSupportExt::test_support(bubble)),
            )
            .into_any_element()
    }

    /// What row `ix` of `reply` shows as a step or a piece of an answer.
    fn piece_of(key: QuestionKey, reply: &Reply, ix: usize, cx: &App) -> Option<Piece> {
        match reply.row(ix)? {
            OutputRow::Notice(notice) => Some(Piece::Notice(
                notice.clone(),
                reply.version().0,
                reply.part_of_row(ix)?,
            )),
            OutputRow::Tool(call) => {
                let project_dir = ProjectDirectory::get(cx);
                Some(Piece::Tool(
                    call.name.clone(),
                    task_table::tool_argument(call, project_dir.as_deref()),
                ))
            }
            _ => {
                let (key, shown) = task_table::reply_markdown(key.table(), reply, ix)?;
                Some(Piece::Text(key, shown))
            }
        }
    }

    /// A step before a question's answer, row `ix` of its reply, as a
    /// compact muted line: a tool call's name, then its most telling
    /// argument; reply text as muted markdown.
    fn answer_step(
        key: QuestionKey,
        ix: usize,
        piece: Piece,
        open: &OpenFile,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, mono) = (theme.muted_foreground, theme.mono_font_family.clone());
        let step =
            match piece {
                Piece::Notice(notice, reply, part) => {
                    task_table::notice_view(key.table(), reply, part, ix, &notice, cx)
                }
                Piece::Tool(name, argument) => h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(div().flex_none().child(name))
                    .children(argument.map(|argument| {
                        div().min_w_0().truncate().font_family(mono).child(argument)
                    }))
                    .into_any_element(),
                Piece::Text(markdown, shown) => div()
                    .min_w_0()
                    .child(task_table::prepared_markdown(
                        markdown,
                        shown,
                        Some(open),
                        cx,
                    ))
                    .into_any_element(),
            };
        let step = div()
            .id(("answer-step", ix))
            .w_full()
            .min_w_0()
            .pl(px(18.))
            .py_0p5()
            .text_sm()
            .text_color(muted)
            .child(step);
        div()
            .id(("answer-steps-of", key.id()))
            .w_full()
            .px_4()
            .child(gpui_kit::TestSupportExt::test_support(step))
            .into_any_element()
    }

    /// A piece of a question's answer, row `ix` of its reply, as markdown in
    /// the body's text colour, a paragraph after the one before it.
    /// With `cards`, the prompt mode whose Ask conversation it is, the
    /// prompts and questions the answer hands back are shown as cards.
    fn answer_piece(
        key: QuestionKey,
        ix: usize,
        piece: Piece,
        first: bool,
        open: &OpenFile,
        cards: Option<WeakEntity<PromptMode>>,
        cx: &mut App,
    ) -> AnyElement {
        let foreground = cx.theme().foreground;
        let text = match piece {
            Piece::Text(markdown, shown)
                if cards.is_some() && crate::answer_blocks::has_blocks(&shown) =>
            {
                let cards = cards.unwrap();
                Some(Self::answer_with_cards(
                    key, ix, markdown, &shown, open, cards, cx,
                ))
            }
            Piece::Text(markdown, shown) => Some(
                task_table::prepared_markdown(markdown, shown, Some(open), cx).into_any_element(),
            ),
            Piece::Tool(..) | Piece::Notice(..) => None,
        };
        let piece = div()
            .id(("answer-row", ix))
            .w_full()
            .min_w_0()
            .text_color(foreground)
            .children(text);
        div()
            .id(("answer-of", key.id()))
            .w_full()
            .px_4()
            .when(!first, |row| row.pt_2())
            .child(gpui_kit::TestSupportExt::test_support(piece))
            .into_any_element()
    }

    /// Reply text holding blocks the answer hands back: its markdown as ever,
    /// and each block, where it stands, as a card, as the
    /// AskConversationScope's suggested prompts and asking back say.
    fn answer_with_cards(
        key: QuestionKey,
        ix: usize,
        markdown: MarkdownKey,
        shown: &str,
        open: &OpenFile,
        this: WeakEntity<PromptMode>,
        cx: &mut App,
    ) -> AnyElement {
        use crate::answer_blocks::{AnswerPart, split};
        let (done, picked) = this
            .upgrade()
            .and_then(|this| {
                let this = this.read(cx);
                let task = this.question(key)?;
                Some((task.reply.is_done(), task.picked.clone()))
            })
            .unwrap_or_default();
        let parts = split(shown);
        let mut children = Vec::new();
        for (part_ix, part) in parts.into_iter().enumerate() {
            // Each card is known by its row and place in it.
            let card = format!("{ix}:{part_ix}");
            let element = match part {
                AnswerPart::Markdown(text) => {
                    let key = MarkdownKey {
                        kind: MarkdownKind::AnswerPart,
                        table: markdown.table,
                        row: markdown.row * CARD_PARTS + part_ix,
                    };
                    task_table::prepared_markdown(key, text.into(), Some(open), cx)
                        .into_any_element()
                }
                AnswerPart::Prompt { mode, text, closed } => {
                    // Usable once its block has closed, or the answer is over.
                    let ready = closed || done;
                    prompt_card(&this, key, &card, mode, text, ready, cx)
                }
                AnswerPart::Question {
                    question,
                    answers,
                    closed,
                } => {
                    let ready = closed || done;
                    let chosen = picked.get(&card).cloned();
                    let question_key = MarkdownKey {
                        kind: MarkdownKind::AnswerPart,
                        table: markdown.table,
                        row: markdown.row * CARD_PARTS + part_ix,
                    };
                    question_card(
                        &this,
                        key,
                        &card,
                        question_key,
                        question,
                        answers,
                        ready,
                        chosen,
                        open,
                        cx,
                    )
                }
            };
            children.push(element);
        }
        v_flex()
            .w_full()
            .gap_2()
            .children(children)
            .into_any_element()
    }

    /// A question's end: the error it failed with, "Stopped" once stopped,
    /// or "No answer." when it finished with nothing to show.
    fn answer_end(
        &self,
        key: QuestionKey,
        task: &PromptTask,
        answer: &AnswerLayout,
        with_changed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let end = v_flex()
            .id(("question-end", key.id()))
            .w_full()
            .px_4()
            .gap_1();
        let end = match task.status {
            TaskStatus::Failed => {
                let errors = task.reply.errors();
                let errors: Vec<SharedString> = if errors.is_empty() {
                    vec!["Failed".into()]
                } else {
                    errors
                        .into_iter()
                        .map(|error| error.to_string().into())
                        .collect()
                };
                // With what has to be done before it can go, to do it.
                let action = task.reply.action().map(|action| {
                    let table = match key {
                        QuestionKey::Ask(id) => super::ASK_ACTION_BASE + id,
                        QuestionKey::Task(ix) => ix,
                        QuestionKey::Answer(ix) => super::ANSWER_ACTION_BASE + ix,
                    };
                    div().pt_1().child(task_table::action_button(
                        ("question-action", key.id()),
                        action,
                        table,
                    ))
                });
                end.pt_2()
                    .text_color(theme.danger)
                    .children(errors)
                    .children(action)
            }
            TaskStatus::Cancelled => {
                end.pt_2()
                    .text_color(theme.muted_foreground)
                    .child(match key {
                        QuestionKey::Task(_) => "Cancelled",
                        _ => "Stopped",
                    })
            }
            TaskStatus::Done
                if answer
                    .segments
                    .last()
                    .is_none_or(|(segment, _)| segment.rows.answer.is_empty()) =>
            {
                end.text_color(theme.muted_foreground).child("No answer.")
            }
            _ => end,
        };
        // A task's files changed while it ran, beneath its reply.
        let changed = match key {
            QuestionKey::Task(ix) if with_changed => {
                self.changed_files_of(ix, id_of_latest(ix), cx)
            }
            _ => None,
        };
        let end = end.children(changed.map(|changed| div().pt_2().child(changed)));
        gpui_kit::TestSupportExt::test_support(end).into_any_element()
    }

    /// The pane: its header, and the conversation beneath it, or, with no
    /// question asked yet, a note saying so.
    pub(super) fn render_ask_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let keys = self.question_keys();
        let theme = cx.theme();
        let (border, muted, ring) = (theme.border, theme.muted_foreground, theme.ring);
        let (tab_bar, background) = (theme.tab_bar, theme.background);
        let header = h_flex()
            .id("ask-pane-header")
            .flex_none()
            .h(ASK_HEADER_HEIGHT)
            .px(px(12.))
            .items_center()
            .justify_between()
            .bg(tab_bar)
            .border_b_1()
            .border_color(border)
            .child("Questions")
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(keys.len().to_string()),
            );
        let header = gpui_kit::TestSupportExt::test_support(header);
        let content = if keys.is_empty() {
            let empty = div()
                .id("ask-pane-empty")
                .text_color(muted)
                .text_center()
                .child("No questions yet. Ask one below.");
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                .child(gpui_kit::TestSupportExt::test_support(empty))
                .into_any_element()
        } else {
            self.render_conversation(&keys, cx)
        };
        let handle = div()
            .id("ask-split-resize")
            .group("ask-split-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(-ASK_SPLIT_HANDLE / 2.)
            .w(ASK_SPLIT_HANDLE)
            .flex()
            .justify_center()
            .cursor_col_resize()
            .on_prepaint(|bounds, _, cx| {
                crate::hit_areas::register_resize("ask-split-resize".into(), bounds, cx)
            })
            .child(
                div()
                    .h_full()
                    .w(px(2.))
                    .group_hover("ask-split-resize", |line| line.bg(ring)),
            )
            .on_drag(AskSplitResize, |_, _, _, cx| cx.new(|_| EmptyView));
        let handle = gpui_kit::TestSupportExt::test_support(handle);
        let pane = v_flex()
            .id("ask-pane")
            .relative()
            .flex_none()
            .h_full()
            .w(relative(self.ask_split_share))
            .min_w_0()
            .bg(background)
            .border_l_1()
            .border_color(border)
            // A drag that selects some of a question's or answer's text
            // offers to copy it or attach it to the prompt, once the
            // selection has settled.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|_, event: &MouseUpEvent, window, cx| {
                    let position = event.position;
                    cx.defer_in(window, move |this, window, cx| {
                        let text = TextSelection::selected_text(window, cx);
                        if !text.trim().is_empty() && this.on_ask_tab {
                            this.selection_popover = Some((position, text));
                            cx.notify();
                        }
                    });
                }),
            )
            .child(header)
            .child(content)
            .child(handle);
        gpui_kit::TestSupportExt::test_support(pane).into_any_element()
    }

    /// Every question and its answer, oldest first, as one list that scrolls,
    /// laying out only what is in view.
    fn render_conversation(&self, keys: &[QuestionKey], cx: &mut Context<Self>) -> AnyElement {
        self.render_chat(Chat::Ask, keys, cx)
    }

    /// The Freeform chat the task view shows while the latest task was sent
    /// in Freeform: each prompt and its reply, oldest first, laid out as the
    /// Ask conversation is. None while the latest task isn't Freeform.
    pub(super) fn render_freeform_chat(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let keys = self.freeform_keys();
        (!keys.is_empty()).then(|| self.render_chat(Chat::Freeform, &keys, cx))
    }

    /// The Freeform chat ending with the task at `ix`, as a task's tab shows
    /// it; none for a task not sent in Freeform.
    pub(super) fn render_freeform_chat_to(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let keys = self.freeform_keys_to(ix);
        (!keys.is_empty()).then(|| self.render_chat(Chat::Freeform, &keys, cx))
    }

    /// `chat`'s prompts, `keys`, and their replies, oldest first, as one list
    /// that scrolls, laying out only what is in view.
    fn render_chat(&self, chat: Chat, keys: &[QuestionKey], cx: &mut Context<Self>) -> AnyElement {
        let layout = Rc::new(self.lay_out_chat(chat, keys, cx));
        let pane = self.chat_pane(chat);
        let rows = &pane.rows;
        if let Some(key) = pane.reveal.take()
            && let Some(ix) = keys.iter().position(|k| *k == key)
        {
            rows.state().scroll_to(ListOffset {
                item_ix: layout.starts[ix],
                offset_in_item: px(0.),
            });
        } else if let Some(key) = pane.reveal_answer.take()
            && let Some(ix) = keys.iter().position(|k| *k == key)
        {
            // Its answer's start, just past the question's box.
            rows.state().scroll_to(ListOffset {
                item_ix: layout.starts[ix] + 1,
                offset_in_item: px(0.),
            });
        } else if pane.locked {
            rows.scroll_to_end();
        }
        let keys: Rc<Vec<QuestionKey>> = Rc::new(keys.to_vec());
        let starts = Rc::new(self.conversation_starts(&keys));
        let this = cx.entity().downgrade();
        let open_file = self.file_opener(cx);
        let render: RenderRow = Rc::new(move |ix, _window, cx| {
            let Some(entity) = this.upgrade() else {
                return div().into_any_element();
            };
            let row = layout.row_at(ix);
            let question = |question: usize, cx: &App| -> Option<(QuestionKey, bool)> {
                let key = *keys.get(question)?;
                entity.read(cx).question(key)?;
                Some((key, starts[question]))
            };
            let answer = |q: usize| layout.answers.get(q).cloned().unwrap_or_default();
            match row {
                PaneRow::Question(q) => {
                    let Some((key, starts)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    entity.update(cx, |this, cx| {
                        let Some(task) = this.question(key) else {
                            return div().into_any_element();
                        };
                        this.question_row(chat, key, task, q == 0, starts, cx)
                    })
                }
                PaneRow::Sent(q, row) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    let text = entity.read(cx).question(key).and_then(|task| {
                        match task.reply.row(row)? {
                            OutputRow::Sent(text) => Some(text.to_string()),
                            _ => None,
                        }
                    });
                    match text {
                        Some(text) => Self::sent_row(chat, key, row, &text, cx),
                        None => div().into_any_element(),
                    }
                }
                PaneRow::Line(q, seg) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    let answer = answer(q);
                    entity.update(cx, |this, cx| {
                        let Some(task) = this.question(key) else {
                            return div().into_any_element();
                        };
                        this.answer_line(key, task, &answer, seg, cx)
                    })
                }
                PaneRow::Step(q, row) | PaneRow::Answer(q, row) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    let piece = entity
                        .read(cx)
                        .question(key)
                        .and_then(|task| Self::piece_of(key, &task.reply, row, cx));
                    let Some(piece) = piece else {
                        return div().into_any_element();
                    };
                    if let PaneRow::Step(..) = layout.row_at(ix) {
                        return Self::answer_step(key, row, piece, &open_file, cx);
                    }
                    let first = layout.answers.get(q).is_some_and(|answer| {
                        (0..answer.segments.len()).any(|seg| answer.first_answer(seg) == Some(row))
                    });
                    // In the Ask conversation, what the answer hands back is
                    // shown as cards.
                    let cards = Some(entity.downgrade());
                    Self::answer_piece(key, row, piece, first, &open_file, cards, cx)
                }
                PaneRow::End(q) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    let answer = answer(q);
                    entity.update(cx, |this, cx| {
                        let Some(task) = this.question(key) else {
                            return div().into_any_element();
                        };
                        this.answer_end(key, task, &answer, true, cx)
                    })
                }
                PaneRow::Tail => {
                    let pending = chat == Chat::Ask && entity.read(cx).ask_new_pending;
                    div()
                        .w_full()
                        .px_4()
                        .pt(if pending { QUESTION_GAP } else { px(0.) })
                        .pb(px(12.))
                        .when(pending, |tail| {
                            tail.child(Self::conversation_divider(usize::MAX, None, cx))
                        })
                        .into_any_element()
                }
            }
        });
        let (scroll_id, scrollbar_id) = match chat {
            Chat::Ask => ("ask-pane-scroll", "ask-pane"),
            Chat::Freeform => ("freeform-chat", "freeform-chat-scrollbar"),
        };
        let list = div()
            .id(scroll_id)
            .flex_1()
            .min_h_0()
            .size_full()
            .child(rows.element(render));
        let list = gpui_kit::TestSupportExt::test_support(list);
        let this = cx.entity().downgrade();
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                let pane = this.chat_pane_mut(chat);
                if pane.locked != locked {
                    pane.locked = locked;
                    pane.scrolled = true;
                    cx.notify();
                }
            })
            .ok();
        });
        div()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(scrollbar::with_scrollbar(
                scrollbar_id,
                rows,
                list,
                true,
                Some((pane.locked, toggle)),
                cx,
            ))
            .into_any_element()
    }

    /// Locks the Freeform chat to its bottom, as sending a prompt does.
    pub(super) fn lock_freeform_chat(&mut self) {
        self.freeform_pane.locked = true;
    }

    /// Resizes the split so the pane's left edge is at `x`, within `bounds`,
    /// the space both sides share.
    pub(super) fn drag_ask_split(
        &mut self,
        x: Pixels,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if bounds.size.width <= px(0.) {
            return;
        }
        self.ask_split_share = ((bounds.right() - x) / bounds.size.width)
            .clamp(MIN_ASK_SPLIT_SHARE, MAX_ASK_SPLIT_SHARE);
        cx.notify();
    }
}

/// How many parts a row's text can be split into, each kept apart by its
/// markdown's key.
const CARD_PARTS: usize = 1024;

/// How rounded a card's corners are.
const CARD_RADIUS: Pixels = px(6.);

/// How much of its mode's colour tints a card.
const CARD_TINT: f32 = 0.08;

/// The icon a mode's prompt card is headed by.
fn mode_icon(mode: Option<SendMode>) -> IconName {
    match mode {
        Some(SendMode::Code) => IconName::Code,
        Some(SendMode::Both) => IconName::Link,
        Some(SendMode::Spec) => IconName::BookOpen,
        Some(SendMode::Ask) => IconName::MessageCircleQuestionMark,
        Some(SendMode::Freeform) => IconName::Sparkles,
        None => IconName::MessageCircleQuestionMark,
    }
}

/// A card's box: the theme's raised surface, faintly tinted `color`,
/// rounded, across the answer's width.
fn card_box(id: impl Into<ElementId>, color: Hsla, cx: &App) -> Stateful<Div> {
    let raised = crate::theme::color(crate::theme::palette(cx).raised);
    v_flex()
        .id(id)
        .w_full()
        .p_3()
        .gap_2()
        .rounded(CARD_RADIUS)
        .bg(raised.blend(Hsla {
            a: CARD_TINT,
            ..color
        }))
}

/// A send button's label, before and after it is pressed, and its tooltip.
fn send_labels(mode: SendMode) -> (&'static str, &'static str, &'static str) {
    match mode {
        SendMode::Code => ("Send to Code", "Sent to Code", "Send this prompt as a Code task"),
        SendMode::Both => ("Send to Chain", "Sent to Chain", "Send this prompt as a Chain task"),
        SendMode::Spec => ("Send to Spec", "Sent to Spec", "Send this prompt as a Spec task"),
        SendMode::Ask => ("Ask", "Asked", "Ask this prompt as a question"),
        SendMode::Freeform => (
            "Send to Freeform",
            "Sent to Freeform",
            "Send this prompt as a Freeform task",
        ),
    }
}

/// A prompt an answer hands back, as a card: headed by its mode, its text as
/// written, and Copy, Edit, and a button sending it to each of Code, Chain,
/// and Spec, usable once `ready`.
fn prompt_card(
    this: &WeakEntity<PromptMode>,
    key: QuestionKey,
    card: &str,
    mode: Option<SendMode>,
    text: String,
    ready: bool,
    cx: &mut App,
) -> AnyElement {
    let color = chat_input::mode_color(mode.unwrap_or(SendMode::Ask), cx);
    let title = match mode {
        Some(SendMode::Both) => "Chain prompt".to_string(),
        Some(mode) => format!("{} prompt", mode.label()),
        None => "Prompt".to_string(),
    };
    let done = |action: CardAction, cx: &App| {
        this.upgrade()
            .is_some_and(|this| this.read(cx).sent_to_prompt(key, card, action))
    };
    let id = SharedString::from(format!("prompt-card-{card}"));
    let copy = {
        let text = text.clone();
        Button::new(SharedString::from(format!("prompt-card-copy-{card}")))
            .ghost()
            .small()
            .icon(IconName::Copy)
            .label("Copy")
            .disabled(!ready)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            })
            .into_any_element()
    };
    // A button doing `action`, and whether it was pressed just now.
    let action_button = |id: String, action: CardAction, cx: &mut App| {
        let pressed = done(action, cx);
        let (this, card, text) = (this.clone(), card.to_string(), text.clone());
        // A send button pressed is disabled for good.
        let spent = pressed && matches!(action, CardAction::Send(_));
        let button = Button::new(SharedString::from(id))
            .small()
            .disabled(!ready || spent)
            .on_click(move |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.card_prompt(key, card.clone(), text.clone(), mode, action, window, cx)
                })
                .ok();
            });
        (button, pressed)
    };
    let (edit, edited) = action_button(format!("prompt-card-edit-{card}"), CardAction::Edit, cx);
    let edit = edit
        .ghost()
        .icon(if edited {
            IconName::Check
        } else {
            IconName::Pencil
        })
        .label(if edited { "Edited" } else { "Edit" })
        .tooltip("Put this prompt in the chat input to edit before sending")
        .into_any_element();
    // Ask's or Freeform's own first, then each task mode, the card's own
    // filled in its colour and the rest ghosts.
    let lead = mode.filter(|mode| matches!(mode, SendMode::Ask | SendMode::Freeform));
    let sends = lead
        .into_iter()
        .chain([SendMode::Code, SendMode::Both, SendMode::Spec])
        .map(|send| {
            let (label, sent_label, tooltip) = send_labels(send);
            let send_color = chat_input::mode_color(send, cx);
            let (button, sent) = action_button(
                format!("prompt-card-send-{}-{card}", mode_slug(send)),
                CardAction::Send(send),
                cx,
            );
            let button = button
                .icon(if sent {
                    IconName::Check
                } else {
                    IconName::ArrowRight
                })
                .label(if sent { sent_label } else { label })
                .tooltip(tooltip);
            if mode == Some(send) {
                // Light text on a dark colour, dark on a light one.
                let foreground = if send_color.l > 0.6 {
                    gpui_kit::black()
                } else {
                    gpui_kit::white()
                };
                button
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .color(send_color)
                            .hover(send_color.opacity(0.9))
                            .active(send_color.opacity(0.8))
                            .foreground(foreground),
                    )
                    // gpui-kit keeps only a fifth of a custom button's
                    // resting colour, so the colour is given to the button
                    // itself.
                    .bg(send_color)
                    .text_color(foreground)
                    .into_any_element()
            } else {
                button.ghost().text_color(send_color).into_any_element()
            }
        })
        .collect::<Vec<_>>();
    // Collapsed to its first line until expanded, from its header.
    let expanded = this
        .upgrade()
        .is_some_and(|this| this.read(cx).card_expanded(key, card));
    let toggle = {
        let (this, card) = (this.clone(), card.to_string());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            this.update(cx, |this, cx| this.toggle_card(key, card.clone(), cx))
                .ok();
        }
    };
    let header = h_flex()
        .id(SharedString::from(format!("prompt-card-header-{card}")))
        .w_full()
        .gap_1p5()
        .cursor_pointer()
        .text_xs()
        .font_medium()
        .text_color(color)
        .child(Icon::new(mode_icon(mode)).xsmall())
        .child(div().flex_1().child(title))
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall(),
        )
        .on_click(toggle);
    let shown = if expanded {
        div()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .child(text)
    } else {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or_default();
        let first = if lines.next().is_some() {
            format!("{first}…")
        } else {
            first.to_string()
        };
        div().w_full().min_w_0().truncate().child(first)
    };
    let card_box = card_box(id, color, cx)
        .child(gpui_kit::TestSupportExt::test_support(header))
        .child(
            gpui_kit::TestSupportExt::test_support(
                shown.id(SharedString::from(format!("prompt-card-text-{card}"))),
            )
            .text_color(cx.theme().foreground),
        )
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .justify_end()
                .gap_1()
                .child(copy)
                .child(edit)
                .children(sends),
        );
    gpui_kit::TestSupportExt::test_support(card_box).into_any_element()
}

/// A question an answer asks back, as a card: the question as markdown, and
/// a button per answer offered, the one picked, if any, shown chosen.
#[allow(clippy::too_many_arguments)]
fn question_card(
    this: &WeakEntity<PromptMode>,
    key: QuestionKey,
    card: &str,
    markdown: MarkdownKey,
    question: String,
    answers: Vec<String>,
    ready: bool,
    chosen: Option<String>,
    open: &OpenFile,
    cx: &mut App,
) -> AnyElement {
    let color = chat_input::mode_color(SendMode::Ask, cx);
    let muted = cx.theme().muted_foreground;
    let id = SharedString::from(format!("question-card-{card}"));
    let text = task_table::prepared_markdown(markdown, question.clone().into(), Some(open), cx);
    let buttons = answers.into_iter().enumerate().map(|(ix, answer)| {
        let picked = chosen.as_deref() == Some(answer.as_str());
        let (this, card, question) = (this.clone(), card.to_string(), question.clone());
        let label = answer.clone();
        Button::new(SharedString::from(format!(
            "question-card-answer-{card}-{ix}"
        )))
        .small()
        .label(label)
        .selected(picked)
        .when(picked, |button| button.text_color(color))
        .when(chosen.is_some() && !picked, |button| {
            button.text_color(muted)
        })
        .disabled(!ready || chosen.is_some())
        .on_click(move |_, window, cx| {
            this.update(cx, |this, cx| {
                this.pick_answer(key, card.clone(), &question, answer.clone(), window, cx)
            })
            .ok();
        })
    });
    let card_box = card_box(id, color, cx)
        .child(div().w_full().min_w_0().child(text))
        .child(h_flex().flex_wrap().gap_1().children(buttons));
    gpui_kit::TestSupportExt::test_support(card_box).into_any_element()
}
