//! The Ask conversation (see the AskConversationScope): while the chat
//! input's Ask tab is selected, the body splits in two above the chat input,
//! the tab bar and the selected tab's contents on the left, and on the right
//! every question of the project, read as a chat. Each question sits in a
//! tinted box against the pane's right edge, its answer beneath it at the
//! pane's full width, and a divider marks where each conversation begins.
//!
//! The whole conversation is one virtualized list: each question is a few
//! rows of it, its box, its status line, its table's header, each of its
//! table's rows, and its end, so however many questions there are, and
//! however long their answers, only the rows in view are laid out.

use super::*;

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

/// A question in the pane: saved with the project, by its place among the
/// saved questions, or asked since, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuestionKey {
    Answer(usize),
    Ask(usize),
}

impl QuestionKey {
    /// Numbers its table apart from every other, in its rows' element ids and
    /// its markdown's keys.
    fn table(self) -> usize {
        match self {
            Self::Answer(ix) => ASK_HISTORY_IX + ix,
            Self::Ask(id) => ASK_IX - id,
        }
    }

    /// Its element ids, apart from every other question's.
    fn id(self) -> usize {
        self.table()
    }
}

/// What a row of the pane's list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneRow {
    /// A question's box, headed by a divider when it begins a conversation.
    Question(usize),
    /// Its status line: while it runs, the latest thing the harness did.
    Status(usize),
    /// Its table's header, or "No output." for a finished one with none.
    TableHeader(usize),
    /// A row of its table.
    TableRow(usize, usize),
    /// The end of its table.
    End(usize),
    /// After the last question: the divider New conversation put in, if the
    /// next question starts one, and the margin ending the list.
    Tail,
}

/// How many rows a question takes besides its table's: its box, status
/// line, table header, and end.
const QUESTION_ROWS: usize = 4;

/// What a question's box, status line, and table header showed, as last laid
/// out, so they are measured again once that changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    starts: bool,
    status: TaskStatus,
    empty: bool,
}

/// What the list was last laid out for.
#[derive(Default)]
struct PaneLaidOut {
    keys: Vec<QuestionKey>,
    tables: Vec<TableSync>,
    looks: Vec<Option<Look>>,
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
}

impl AskPane {
    pub(super) fn new() -> Self {
        Self {
            rows: MeasuredList::new(task_table::OVERDRAW),
            laid_out: RefCell::default(),
            // It opens scrolled to the bottom, on the newest question.
            locked: true,
            reveal: Cell::new(None),
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
/// question's table is laid out.
struct PaneLayout {
    starts: Vec<usize>,
    tables: Vec<TableLayout>,
}

impl PaneLayout {
    fn row_at(&self, ix: usize) -> PaneRow {
        let question = match self.starts.binary_search(&ix) {
            Ok(question) => question,
            Err(0) => return PaneRow::Tail,
            Err(next) => next - 1,
        };
        let Some(table) = self.tables.get(question) else {
            return PaneRow::Tail;
        };
        let at = ix - self.starts[question];
        let items = table.items();
        match at {
            0 => PaneRow::Question(question),
            1 => PaneRow::Status(question),
            2 => PaneRow::TableHeader(question),
            at if at < items + 3 => PaneRow::TableRow(question, at - 3),
            at if at == items + 3 => PaneRow::End(question),
            _ => PaneRow::Tail,
        }
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

impl PromptMode {
    /// Every question of the project, oldest first: those saved with it, then
    /// those asked since.
    fn question_keys(&self) -> Vec<QuestionKey> {
        (0..self.answers.len())
            .map(QuestionKey::Answer)
            .chain(self.asks.iter().map(|ask| QuestionKey::Ask(ask.id)))
            .collect()
    }

    fn question(&self, key: QuestionKey) -> Option<&PromptTask> {
        match key {
            QuestionKey::Answer(ix) => self.answers.get(ix),
            QuestionKey::Ask(id) => self
                .asks
                .iter()
                .find(|ask| ask.id == id)
                .map(|ask| &ask.task),
        }
    }

    /// Whether each question begins a conversation: the first, and each whose
    /// record says it started one.
    fn conversation_starts(&self, keys: &[QuestionKey]) -> Vec<bool> {
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
            cx.notify();
        }
    }

    /// Asks the question `key` again, as it was asked; it comes in at the
    /// bottom.
    fn resend_question(&mut self, key: QuestionKey, window: &mut Window, cx: &mut Context<Self>) {
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
    /// asked or loaded, what their tables hold, and what each question's
    /// box, status line, and header show.
    fn lay_out_ask_pane(&self, keys: &[QuestionKey], cx: &App) -> PaneLayout {
        let rows = &self.ask_pane.rows;
        let mut laid_out = self.ask_pane.laid_out.borrow_mut();
        if laid_out.keys != keys {
            let kept = laid_out.keys.len();
            // The list holds its tail from the first time it is laid out.
            if rows.count() > 0 && kept <= keys.len() && keys[..kept] == laid_out.keys[..] {
                // Asked since: new questions come in above the tail.
                let tail = rows.count().saturating_sub(1);
                let added = keys.len() - kept;
                rows.splice(tail..tail, added * QUESTION_ROWS);
                laid_out
                    .tables
                    .extend((0..added).map(|_| TableSync::default()));
                laid_out.looks.extend((0..added).map(|_| None));
            } else {
                rows.reset(keys.len() * QUESTION_ROWS + 1);
                laid_out.tables = keys.iter().map(|_| TableSync::default()).collect();
                laid_out.looks = vec![None; keys.len()];
                laid_out.pending = false;
            }
            laid_out.keys = keys.to_vec();
        }
        let starts_conversation = self.conversation_starts(keys);
        let mut base = 0;
        let mut starts = Vec::with_capacity(keys.len());
        let mut tables = Vec::with_capacity(keys.len());
        for (ix, key) in keys.iter().enumerate() {
            starts.push(base);
            let Some(task) = self.question(*key) else {
                let layout = laid_out.tables[ix]
                    .layout()
                    .unwrap_or(TableLayout::of(&Reply::default(), None));
                tables.push(layout);
                base += QUESTION_ROWS + layout.items();
                continue;
            };
            let table = key.table();
            let layout = TableLayout::of(&task.reply, Some(self.steps_shown.contains(&table)));
            laid_out.tables[ix].update(rows, base + 3, table, &task.reply, layout, cx);
            let look = Look {
                starts: starts_conversation[ix],
                status: task.status,
                empty: task.reply.row_count() == 0,
            };
            if laid_out.looks[ix] != Some(look) {
                rows.remeasure(base..base + 3);
                let end = base + 3 + layout.items();
                rows.remeasure(end..end + 1);
                laid_out.looks[ix] = Some(look);
            }
            // While it runs, its status line follows the harness.
            if task.status.is_active() {
                rows.remeasure(base + 1..base + 2);
            }
            tables.push(layout);
            base += QUESTION_ROWS + layout.items();
        }
        let pending = self.ask_new_pending;
        if laid_out.pending != pending {
            rows.remeasure(base..base + 1);
            laid_out.pending = pending;
        }
        PaneLayout { starts, tables }
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
    fn question_row(
        &self,
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
            ..chat_input::mode_color(SendMode::Ask, cx)
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
                    .children(task_images(id, task, cx)),
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
            });
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
            .into_any_element()
    }

    /// A question's status line: while it runs, a spinner, the latest thing
    /// the harness did, and Stop; once over, its status unless it is done.
    fn question_status(
        &self,
        key: QuestionKey,
        task: &PromptTask,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = key.id();
        if task.status.is_active() {
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
                QuestionKey::Answer(_) => None,
            };
            let row = h_flex()
                .id(("question-status", id))
                .w_full()
                .px_4()
                .pt(ANSWER_GAP)
                .gap_2()
                .items_center()
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
        if task.status == TaskStatus::Done {
            return div().into_any_element();
        }
        div()
            .w_full()
            .px_4()
            .pt(ANSWER_GAP)
            .child(
                div()
                    .id(("task-status", id))
                    .flex_none()
                    .child(task.status.tag(cx)),
            )
            .into_any_element()
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
        let layout = Rc::new(self.lay_out_ask_pane(keys, cx));
        let rows = &self.ask_pane.rows;
        if let Some(key) = self.ask_pane.reveal.take()
            && let Some(ix) = keys.iter().position(|k| *k == key)
        {
            rows.state().scroll_to(ListOffset {
                item_ix: layout.starts[ix],
                offset_in_item: px(0.),
            });
        } else if self.ask_pane.locked {
            rows.scroll_to_end();
        }
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let (table_background, radius) = (theme.tokens.table, theme.radius);
        let keys: Rc<Vec<QuestionKey>> = Rc::new(keys.to_vec());
        let starts = Rc::new(self.conversation_starts(&keys));
        let this = cx.entity().downgrade();
        let open_file = self.file_opener(cx);
        // Its tables tighten as the pane narrows, counting its padding.
        let density = task_table::Density::of_list(rows, px(32.));
        // Each question's table rows, finding its reply wherever it is kept.
        let tables: Rc<Vec<RenderRow>> = Rc::new(
            keys.iter()
                .zip(&layout.tables)
                .map(|(&key, &table_layout)| {
                    let reply_of = task_table::reply_of({
                        let this = this.clone();
                        move |cx| {
                            let prompt_mode = this.upgrade()?.read(cx);
                            prompt_mode.question(key).map(|task| &task.reply)
                        }
                    });
                    let steps = steps(key.table(), &self.steps_shown, &cx.entity());
                    task_table::table_rows(
                        key.table(),
                        reply_of,
                        table_layout,
                        Some(open_file.clone()),
                        Some(steps),
                        density,
                        cx,
                    )
                })
                .collect(),
        );
        let render: RenderRow = Rc::new(move |ix, window, cx| {
            let Some(entity) = this.upgrade() else {
                return div().into_any_element();
            };
            let row = layout.row_at(ix);
            let inset = || div().w_full().px_4();
            let question = |question: usize, cx: &App| -> Option<(QuestionKey, bool)> {
                let key = *keys.get(question)?;
                entity.read(cx).question(key)?;
                Some((key, starts[question]))
            };
            match row {
                PaneRow::Question(q) => {
                    let Some((key, starts)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    entity.update(cx, |this, cx| {
                        let Some(task) = this.question(key) else {
                            return div().into_any_element();
                        };
                        this.question_row(key, task, q == 0, starts, cx)
                    })
                }
                PaneRow::Status(q) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    entity.update(cx, |this, cx| {
                        let Some(task) = this.question(key) else {
                            return div().into_any_element();
                        };
                        this.question_status(key, task, cx)
                    })
                }
                PaneRow::TableHeader(q) => {
                    let Some((key, _)) = question(q, cx) else {
                        return div().into_any_element();
                    };
                    let empty = entity
                        .read(cx)
                        .question(key)
                        .is_none_or(|task| task.reply.row_count() == 0);
                    if empty {
                        return inset()
                            .pt(ANSWER_GAP)
                            .text_color(muted)
                            .child("No output.")
                            .into_any_element();
                    }
                    inset()
                        .pt(ANSWER_GAP)
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
                PaneRow::TableRow(q, row) => {
                    let Some(rows) = tables.get(q) else {
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
                PaneRow::End(q) => {
                    let has_table = layout.tables.get(q).is_some_and(|table| table.items() > 0);
                    let id = keys.get(q).map_or(0, |key| key.id());
                    let end = inset().id(("question-end", id)).when(has_table, |end| {
                        end.child(
                            div()
                                .h(radius.max(px(1.)))
                                .rounded_b(radius)
                                .border_b_1()
                                .border_x_1()
                                .border_color(border)
                                .bg(table_background),
                        )
                    });
                    gpui_kit::TestSupportExt::test_support(end).into_any_element()
                }
                PaneRow::Tail => {
                    let pending = entity.read(cx).ask_new_pending;
                    inset()
                        .pt(if pending { QUESTION_GAP } else { px(0.) })
                        .pb(px(12.))
                        .when(pending, |tail| {
                            tail.child(Self::conversation_divider(usize::MAX, None, cx))
                        })
                        .into_any_element()
                }
            }
        });
        let list = div()
            .id("ask-pane-scroll")
            .flex_1()
            .min_h_0()
            .size_full()
            .child(rows.element(render))
            .child(task_table::follow_density(rows, density, px(32.)));
        let list = gpui_kit::TestSupportExt::test_support(list);
        let this = cx.entity().downgrade();
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                if this.ask_pane.locked != locked {
                    this.ask_pane.locked = locked;
                    cx.notify();
                }
            })
            .ok();
        });
        div()
            .flex_1()
            .min_h_0()
            .child(scrollbar::with_scrollbar(
                "ask-pane",
                rows,
                list,
                true,
                Some((self.ask_pane.locked, toggle)),
                cx,
            ))
            .into_any_element()
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
