//! The right sidebar's Tasks tab (see the TasksTabScope): every task of the
//! project at a glance, read top to bottom as a timeline in three groups,
//! what has run, what is running, and what is queued. Clicking a task opens
//! it in the tab, where it can be looked into, a queued one edited and
//! adjusted in place, and a chain given more steps.
//!
//! The tab is one scrolling column, each task a row 28 pixels tall: a bar in
//! its mode's colour, its status or place in the queue, its prompt's first
//! line, and how long it took.

use super::*;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::DropdownMenu as _;

/// How tall a row of the tab is, as a queued prompt's is.
const ROW_HEIGHT: Pixels = QUEUED_ROW_HEIGHT;

/// How wide the bar down a row's left edge is, in its mode's colour.
const MODE_BAR: Pixels = px(2.);

/// How far a chain's steps are set in beneath its row.
const STEP_INDENT: Pixels = px(16.);

/// How long the prompt of an open queued task rests after a change before
/// it is saved.
const SAVE_REST: Duration = Duration::from_millis(500);

/// The most rows an open queued task's prompt grows to before it scrolls.
const PROMPT_MAX_ROWS: usize = 12;

/// One of the Tasks tab's rows, as what it shows, each [`ROW_HEIGHT`]
/// tall.
enum TimelineRow {
    /// A group's heading, with its label.
    Heading(Group, String),
    /// A group with nothing in it, as its id and what it says.
    Empty(&'static str, &'static str),
    /// A chain's row, as its head and all its steps.
    Chain(usize, Vec<usize>),
    /// A task's, as its index, its step's kind in a chain, and whether it is
    /// one of the previous tasks.
    Task(usize, Option<StepKind>, bool),
    /// A queued prompt's, the next in the queue.
    Queued,
}

/// The timeline's groups.
enum Group {
    Previous,
    Running,
    Queued,
}

/// How many rows either side of those in view the Tasks tab draws too.
pub(super) const OVERSCAN_ROWS: usize = 4;

/// The right sidebar's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SidebarTab {
    /// The referenced files, the understanding, and the subagents of the
    /// task selected.
    #[default]
    Run,
    /// Every task of the project, as the TasksTabScope says.
    Tasks,
}

/// A task opened in the Tasks tab: one sent, by its place among the tasks,
/// or one queued, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Opened {
    Task(usize),
    Queued(usize),
}

/// An open queued task's prompt, edited in place and saved as it rests.
struct QueuedEditor {
    id: usize,
    input: Entity<TextareaState>,
    _changed: Subscription,
    _save: Task<()>,
}

/// A step added to a chain, its prompt being typed: the chain's first step,
/// the step's place among those added, and its prompt's input.
struct StepEditor {
    head: usize,
    place: usize,
    input: Entity<TextareaState>,
    _changed: Subscription,
    _save: Task<()>,
}

/// One open project's Tasks tab, kept with the project's other work.
pub(super) struct TasksTab {
    /// The task open in the tab, in the list's place.
    pub(super) open: Option<Opened>,
    pub(super) scroll: ScrollHandle,
    /// The chains whose steps are shown beneath them, by their first step.
    pub(super) expanded_chains: HashSet<usize>,
    /// Whether the Previous group shows its rows; collapsed as the project
    /// opens, then kept as it is left.
    pub(super) previous_expanded: bool,
    /// A task opened here shows its output in the message list, with a bar
    /// back to the latest task.
    pub(super) viewing: bool,
    /// The tab was on screen last frame; shown afresh, it scrolls to what
    /// runs.
    pub(super) shown: Cell<bool>,
    /// Queued prompts put there by "Edit and resend", held until "Send" is
    /// clicked.
    pub(super) drafts: HashSet<usize>,
    editor: Option<QueuedEditor>,
    step_editor: Option<StepEditor>,
    pub(super) focus: FocusHandle,
    /// Whether the previous tasks' filter menu is open.
    pub(super) filter_menu: bool,
    /// Where the filter button was last drawn, the menu opening beneath it.
    pub(super) filter_button: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl TasksTab {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            open: None,
            scroll: ScrollHandle::new(),
            expanded_chains: HashSet::new(),
            previous_expanded: false,
            viewing: false,
            shown: Cell::new(false),
            drafts: HashSet::new(),
            editor: None,
            step_editor: None,
            focus: cx.focus_handle(),
            filter_menu: false,
            filter_button: Rc::new(Cell::new(None)),
        }
    }

    /// Whether the queued prompt `id` is held, never sent: open here, being
    /// changed, or put here by "Edit and resend" and not yet sent.
    pub(super) fn holds(&self, id: usize) -> bool {
        self.open == Some(Opened::Queued(id)) || self.drafts.contains(&id)
    }
}

/// What a queued prompt holds that can be changed in the tab.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct QueuedFields {
    pub(super) text: String,
    pub(super) mode: SendMode,
    pub(super) attached: Attached,
    pub(super) sliced: bool,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
}

/// A task's status as a row shows it: a spinner while it is under way, then
/// a tick, a cross, or a dash.
fn status_mark(status: TaskStatus, cx: &App) -> AnyElement {
    let theme = cx.theme();
    if status.is_active() {
        return Spinner::new().xsmall().into_any_element();
    }
    let (icon, color) = match status {
        TaskStatus::Done => (IconName::Check, Hue::Green.of(crate::theme::palette(cx))),
        TaskStatus::Failed => (IconName::X, Hue::Red.of(crate::theme::palette(cx))),
        _ => (IconName::Minus, theme.muted_foreground),
    };
    Icon::new(icon).xsmall().text_color(color).into_any_element()
}

/// The bar down a row's left edge, in `mode`'s colour.
fn mode_bar(mode: Option<SendMode>, cx: &App) -> Div {
    let color = mode.map_or(cx.theme().muted_foreground, |mode| {
        chat_input::mode_color(mode, cx)
    });
    div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(MODE_BAR)
        .bg(color)
}

/// A group's heading: its label, muted, then what goes along it.
fn group_heading(id: &'static str, label: String, actions: Vec<AnyElement>, cx: &App) -> AnyElement {
    group_heading_in(h_flex().id(id), label, actions, cx)
}

/// A group's heading, as [`group_heading`], drawn in `heading`.
fn group_heading_in(heading: Stateful<Div>, label: String, actions: Vec<AnyElement>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    // Lets UI tests find it; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(heading)
        .flex_none()
        .w_full()
        .h(ROW_HEIGHT)
        .overflow_hidden()
        .gap_2()
        .px_3()
        .bg(theme.tab_bar)
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        // Cut short where its actions need the room.
        .child(div().flex_1().min_w_0().truncate().child(label))
        .children(actions.into_iter().map(|action| div().flex_none().child(action)))
        .into_any_element()
}

/// A muted line standing in for a group with nothing in it.
fn group_empty(id: &'static str, text: &'static str, cx: &App) -> AnyElement {
    gpui_kit::TestSupportExt::test_support(div().id(id))
        .flex_none()
        .h(ROW_HEIGHT)
        .px_3()
        .flex()
        .items_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// A lane's name, as a figure reads it.
fn lane_label(mode: Option<SendMode>) -> &'static str {
    let lanes = Lanes::of(mode);
    match (lanes.code, lanes.spec) {
        (true, true) => "Both lanes",
        (true, false) => "Code lane",
        (false, true) => "Spec lane",
        (false, false) => "None, as a question",
    }
}

/// A labelled figure of an open task's at-a-glance section.
fn figure(label: &'static str, value: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .min_h(px(22.))
        .gap_2()
        .text_sm()
        .child(
            div()
                .flex_none()
                .w(px(110.))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(value))
        .into_any_element()
}

/// A section of an open task, headed by a small muted label.
fn section(label: &'static str, cx: &App) -> Div {
    v_flex().w_full().gap_1().px_3().py_2().child(
        div()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label),
    )
}

impl PromptMode {
    /// The tasks the Previous group lists: every task that has run, none
    /// under way.
    fn previous_ixs(&self) -> Vec<usize> {
        (0..self.tasks.len())
            .filter(|&ix| !self.tasks[ix].status.is_active())
            .collect()
    }

    /// The tab, as the TasksTabScope says: the timeline, or the task open in
    /// its place.
    pub(super) fn render_tasks_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let opened = self.tasks_tab.open.filter(|open| self.opened_exists(*open));
        let content = match opened {
            Some(open) => self.render_task_details(open, window, cx),
            None => self.render_task_timeline(window, cx),
        };
        // Lets UI tests find the tab; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(div().id("tasks-tab"))
            .size_full()
            .track_focus(&self.tasks_tab.focus)
            // Esc goes back to the list as it was left.
            .on_action(cx.listener(|this, _: &crate::main_window::Dismiss, window, cx| {
                if this.tasks_tab.filter_menu {
                    this.tasks_tab.filter_menu = false;
                    cx.notify();
                } else if this.tasks_tab.open.is_some() {
                    this.close_task_in_tab(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .child(content)
            .children(opened.is_none().then(|| self.render_filter_menu(cx)).flatten())
            .into_any_element()
    }

    /// Whether `open` is still there to show.
    fn opened_exists(&self, open: Opened) -> bool {
        match open {
            Opened::Task(ix) => ix < self.tasks.len(),
            Opened::Queued(id) => self.queue.iter().any(|item| item.id == id),
        }
    }

    /// Opens `open` in the tab: a task that has run, or is running, also
    /// shows its output in the message list; a queued one is held while it
    /// is open, its prompt ready to change.
    pub(super) fn open_in_tab(&mut self, open: Opened, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_prompt(window, cx);
        self.save_step_prompt(cx);
        self.tasks_tab.editor = None;
        self.tasks_tab.step_editor = None;
        self.tasks_tab.open = Some(open);
        match open {
            Opened::Task(ix) => {
                // A previous task opened from elsewhere shows in its group.
                if self.tasks.get(ix).is_some_and(|task| !task.status.is_active()) {
                    self.tasks_tab.previous_expanded = true;
                }
                self.tasks_tab.viewing = true;
                self.select_task(ix, cx);
            }
            Opened::Queued(id) => {
                let text = self
                    .queue
                    .iter()
                    .find(|item| item.id == id)
                    .map(|item| match &item.saved {
                        Some(saved) => saved.text.clone(),
                        None => item.text.to_string(),
                    })
                    .unwrap_or_default();
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, PROMPT_MAX_ROWS)
                        .default_value(text)
                });
                let changed = cx.subscribe_in(
                    &input,
                    window,
                    move |this, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            this.prompt_changed_in_tab(window, cx);
                        }
                    },
                );
                self.tasks_tab.editor = Some(QueuedEditor {
                    id,
                    input,
                    _changed: changed,
                    _save: Task::ready(()),
                });
            }
        }
        self.tasks_tab.focus.focus(window, cx);
        cx.notify();
    }

    /// Back to the list, as it was left: a queued task open is let go, what
    /// was typed in it saved first.
    pub(super) fn close_task_in_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_prompt(window, cx);
        self.save_step_prompt(cx);
        self.tasks_tab.editor = None;
        self.tasks_tab.step_editor = None;
        self.tasks_tab.open = None;
        // Let go, it goes on as the queue does.
        self.auto_send_next(cx);
        cx.notify();
    }

    /// The selected task's output back to the latest task's, scrolled where
    /// it was left.
    pub(super) fn back_to_latest(&mut self, cx: &mut Context<Self>) {
        self.tasks_tab.viewing = false;
        if let Some(latest) = self.true_latest_ix() {
            self.select_task(latest, cx);
        }
        self.selected_task = None;
        cx.notify();
    }

    /// The prompt of the open queued task changed: saved once it rests.
    fn prompt_changed_in_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.tasks_tab.editor.as_mut() else {
            return;
        };
        editor._save = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_REST).await;
            this.update_in(cx, |this, window, cx| this.save_open_prompt(window, cx))
                .ok();
        });
    }

    /// Saves the open queued task's prompt, if it changed.
    fn save_open_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.tasks_tab.editor else {
            return;
        };
        let (id, text) = (editor.id, editor.input.read(cx).value().to_string());
        let current = self
            .queue
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.text.to_string());
        if current.is_some_and(|current| current != text) && !text.trim().is_empty() {
            self.change_queued(id, move |fields| fields.text = text, window, cx);
        }
    }

    /// What the queued prompt `id` holds that can be changed, once saved.
    pub(super) fn queued_fields(&self, id: usize) -> Option<QueuedFields> {
        let saved = self.queue.iter().find(|item| item.id == id)?.saved.as_ref()?;
        Some(QueuedFields {
            text: saved.text.clone(),
            mode: anchor_mode(&saved.anchor).unwrap_or(SendMode::Both),
            attached: Attached {
                text: saved.anchor.attached_text.clone(),
                images: saved.anchor.attached_images.clone(),
                files: saved.anchor.attached_files.clone(),
            },
            sliced: saved.anchor.sliced,
            model: saved.anchor.model.clone(),
            effort: saved.anchor.effort.clone(),
        })
    }

    /// Changes the queued prompt `id` with `change`, saved with the project
    /// in its place, as saving a queued prompt's edit is. Its mode may move
    /// it to another lane, in its place in the queue. One still being saved
    /// is left as it is.
    pub(super) fn change_queued(
        &mut self,
        id: usize,
        change: impl FnOnce(&mut QueuedFields),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(mut fields), Some(project_dir)) = (self.queued_fields(id), self.project_dir.clone())
        else {
            return;
        };
        let before = fields.clone();
        change(&mut fields);
        if fields == before {
            return;
        }
        let Some(item) = self.queue.iter_mut().find(|item| item.id == id) else {
            return;
        };
        let Some(old) = item.saved.take() else {
            return;
        };
        let old_text = std::mem::replace(&mut item.text, fields.text.clone().into());
        let old_images = std::mem::replace(&mut item.images, fields.attached.images.clone());
        let old_files = std::mem::replace(&mut item.files, fields.attached.files.clone());
        let same_mode = anchor_mode(&old.anchor) == Some(fields.mode);
        let sent_from = old.anchor.sent_from.clone().filter(|_| same_mode);
        let post_build_update = same_mode && old.anchor.post_build_update;
        item.sent_from = sent_from.clone();
        let old_mode = item.mode.replace(fields.mode);
        cx.notify();
        let lsp = self.chat_input.read(cx).lsp();
        let save = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let code_task = old.anchor.code_task.clone().filter(|_| same_mode);
                match resolve_anchor(
                    &fields.text,
                    fields.mode,
                    fields.attached,
                    fields.sliced,
                    code_task,
                    lsp,
                    &project_dir,
                ) {
                    Ok(mut anchor) => {
                        anchor.sent_from = sent_from;
                        anchor.post_build_update = post_build_update;
                        anchor.new_conversation = old.anchor.new_conversation;
                        anchor.added_steps = old.anchor.added_steps.clone();
                        // A Freeform prompt goes to the harness's own.
                        if fields.mode != SendMode::Freeform {
                            anchor.model = fields.model;
                            anchor.effort = fields.effort;
                        }
                        prompt_queue::replace(old.file.clone(), anchor, fields.text)
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
                        if let Ok(saved) = &saved {
                            prompt_queue::remove(&saved.file).ok();
                        }
                        return;
                    };
                    match saved {
                        Ok(saved) => {
                            item.saved = Some(saved);
                            if let Some(ix) = this.queue.iter().position(|item| item.id == id) {
                                this.keep_new_conversation(ix);
                            }
                        }
                        Err((old, err)) => {
                            item.text = old_text;
                            item.images = old_images;
                            item.files = old_files;
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

    /// "Edit and resend": the task at `ix` put back as a new queued prompt,
    /// as it was sent, open here ready to change, sent once "Send" is
    /// clicked.
    fn edit_and_resend(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.get(ix) else {
            return;
        };
        let (text, sent) = (task.text.to_string(), task.sent.clone());
        let mode = sent.mode.unwrap_or_else(|| self.chat_input.read(cx).mode());
        let named_after = Some(task.name.to_string());
        let code_task = sent.code_task.clone().filter(|_| hands_on(mode));
        self.enqueue_at(
            QueuePlace::End,
            text,
            true,
            mode,
            sent.attached(),
            sent.sliced,
            code_task,
            sent.sent_from.clone(),
            sent.post_build_update,
            named_after,
            cx,
        );
        let id = self.next_queue_id;
        if self.queue.iter().any(|item| item.id == id) {
            self.tasks_tab.drafts.insert(id);
            self.open_in_tab(Opened::Queued(id), window, cx);
        }
    }

    /// "Send", on a prompt "Edit and resend" put in the queue: let go, it is
    /// sent at once while its lane is free, or else waits its turn.
    fn send_draft(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_prompt(window, cx);
        self.tasks_tab.drafts.remove(&id);
        if self.tasks_tab.open == Some(Opened::Queued(id)) {
            self.tasks_tab.editor = None;
            self.tasks_tab.open = None;
        }
        if let Some(ix) = self.queue.iter().position(|item| item.id == id)
            && self.queue[ix].saved.is_some()
            && !self.working.overlaps(Lanes::of(self.queue[ix].mode))
        {
            let item = self.queue.remove(ix);
            if let Some(saved) = item.saved {
                self.start(item.text.to_string(), Sending::Queued(saved), cx);
            }
        } else {
            self.auto_send_next(cx);
        }
        cx.notify();
    }

    /// The list's rows, top to bottom, as what each shows rather than drawn:
    /// Previous, Running, and Queued, each headed by its label and count.
    /// With where Running's and Queued's headings are, and the first queued
    /// prompt's row.
    fn timeline_rows(&self) -> (Vec<TimelineRow>, usize, usize, usize) {
        let mut rows = Vec::new();
        let previous = self.previous_ixs();
        let shown = self.task_visibility();
        let visible = |ix: usize| shown.as_ref().is_none_or(|shown| shown.get(ix) != Some(&false));
        // Previous, with its batch actions along its heading and its
        // filters beneath it.
        let count = previous.len();
        let mut label = match count {
            0 => "No previous tasks".to_string(),
            1 => "1 previous task".to_string(),
            n => format!("{n} previous tasks"),
        };
        if shown.is_some() {
            let showing = previous.iter().filter(|&&ix| visible(ix)).count();
            label.push_str(&format!(" · {showing} shown"));
        }
        rows.push(TimelineRow::Heading(Group::Previous, label));
        // Collapsed, its heading alone.
        if count > 0 && self.tasks_tab.previous_expanded {
            let layout = chain_layout(&self.tasks);
            let mut listed = 0;
            for place in 0..layout.order.len() {
                let ix = layout.order[place];
                match layout.steps[place] {
                    // A chain: its row, then its steps while shown.
                    Some(step) if step.pos == 0 => {
                        let all = layout.members(step);
                        let shown_member = |member: usize| {
                            !self.tasks[member].status.is_active() && visible(member)
                        };
                        if !all.iter().any(|&member| shown_member(member)) {
                            continue;
                        }
                        listed += 1;
                        rows.push(TimelineRow::Chain(ix, all.to_vec()));
                        if self.tasks_tab.expanded_chains.contains(&ix) {
                            for (pos, &member) in all.iter().enumerate() {
                                if shown_member(member) {
                                    let kind = layout.steps[step.start + pos].map(|step| step.kind);
                                    rows.push(TimelineRow::Task(member, kind, true));
                                }
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        if !self.tasks[ix].status.is_active() && visible(ix) {
                            listed += 1;
                            rows.push(TimelineRow::Task(ix, None, true));
                        }
                    }
                }
            }
            if listed == 0 {
                rows.push(TimelineRow::Empty(
                    "no-filtered-tasks",
                    "No previous tasks match these filters",
                ));
            }
        }
        // Running, in either lane.
        let running: Vec<usize> = (0..self.tasks.len())
            .filter(|&ix| self.tasks[ix].status.is_active())
            .collect();
        let running_at = rows.len();
        rows.push(TimelineRow::Heading(
            Group::Running,
            format!("Running · {}", running.len()),
        ));
        if running.is_empty() {
            rows.push(TimelineRow::Empty("tasks-running-empty", "Nothing running"));
        }
        rows.extend(running.into_iter().map(|ix| TimelineRow::Task(ix, None, false)));
        // Queued, in its order, reordered by dragging.
        let queued_at = rows.len();
        rows.push(TimelineRow::Heading(
            Group::Queued,
            format!("Queued · {}", self.queue.len()),
        ));
        if self.queue.is_empty() {
            rows.push(TimelineRow::Empty("tasks-queued-empty", "No queued prompts"));
        }
        let first_queued = rows.len();
        rows.extend((0..self.queue.len()).map(|_| TimelineRow::Queued));
        (rows, running_at, queued_at, first_queued)
    }

    /// The list, virtualized: only the rows in view, and a few either side,
    /// are laid out and drawn, each [`ROW_HEIGHT`] tall, with room kept for
    /// the rest above and below them, so where every row lies, and the
    /// scrollbar, follow from the rows' count alone.
    fn render_task_timeline(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (rows, running_at, queued_at, first_queued) = self.timeline_rows();
        let scroll = &self.tasks_tab.scroll;

        // Shown afresh, what runs is brought into view, or the queue's start
        // when nothing does, jumping straight there.
        if !self.tasks_tab.shown.replace(true) {
            let at = if self.tasks.iter().any(|task| task.status.is_active()) {
                running_at
            } else {
                queued_at
            };
            scroll.set_offset(point(px(0.), -(ROW_HEIGHT * at as f32)));
        }
        // Until the list has been laid out once, the window's height.
        let height = match scroll.bounds().size.height {
            height if height > px(0.) => height,
            _ => window.viewport_size().height,
        };
        let top = (-scroll.offset().y).max(px(0.));
        let first = ((top / ROW_HEIGHT).floor() as usize).saturating_sub(OVERSCAN_ROWS);
        let last = (((top + height) / ROW_HEIGHT).ceil() as usize + OVERSCAN_ROWS).min(rows.len());
        let first = first.min(last);

        let queued = first.max(first_queued).saturating_sub(first_queued)
            ..last.max(first_queued).saturating_sub(first_queued);
        let mut queued_rows = self.render_queued_rows(queued.clone(), cx).into_iter();
        let mut drawn: Vec<AnyElement> = Vec::with_capacity(last - first + 2);
        drawn.push(div().flex_none().h(ROW_HEIGHT * first as f32).into_any_element());
        for row in &rows[first..last] {
            drawn.push(match row {
                TimelineRow::Heading(group, label) => {
                    let (id, actions) = match group {
                        Group::Previous => {
                            let mut actions = self.render_selection_actions(cx);
                            // The filters' button, while there is a task to
                            // filter, at the heading's right.
                            if !self.previous_ixs().is_empty() {
                                actions.push(self.render_filter_button(cx));
                            }
                            drawn.push(self.render_previous_heading(label.clone(), actions, cx));
                            continue;
                        }
                        Group::Running => ("tasks-running", Vec::new()),
                        Group::Queued => ("tasks-queued", self.queue_controls(cx)),
                    };
                    group_heading(id, label.clone(), actions, cx)
                }
                TimelineRow::Empty(id, text) => group_empty(id, text, cx),
                TimelineRow::Chain(head, all) => self.render_chain_row(*head, all, cx),
                TimelineRow::Task(ix, kind, previous) => {
                    self.render_task_row(*ix, *kind, *previous, cx)
                }
                TimelineRow::Queued => match queued_rows.next() {
                    Some(row) => row,
                    None => div().into_any_element(),
                },
            });
        }
        drawn.push(
            div()
                .flex_none()
                .h(ROW_HEIGHT * (rows.len() - last) as f32)
                .into_any_element(),
        );
        let drag_scroll = self.queue_drag_driver(first_queued, cx);
        let rows = drawn;
        let (queue_gap, follow) = (self.queue_gap.clone(), self.queue_drag_scroll.clone());
        let list = v_flex()
            .id("tasks-tab-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.tasks_tab.scroll)
            // Off the list, no gap is shown.
            .on_drag_move(move |event: &DragMoveEvent<QueuedDrag>, window, _| {
                follow.follow(event.event.position);
                if !event.bounds.contains(&event.event.position)
                    && let Some((id, Some(_))) = queue_gap.get()
                {
                    queue_gap.set(Some((id, None)));
                    window.refresh();
                }
            })
            .children(rows);
        v_flex()
            .relative()
            .size_full()
            .child(scrollbar::with_scrollbar(
                "tasks-tab-list",
                &self.tasks_tab.scroll,
                // Lets UI tests find the list; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(list),
                true,
                None,
                cx,
            ))
            .children(drag_scroll)
            .into_any_element()
    }

    /// The Previous group's heading: a chevron, right while it is collapsed
    /// and down while expanded, its label, and its batch actions and
    /// filters. Clicking it anywhere but those expands or collapses it.
    fn render_previous_heading(&self, label: String, actions: Vec<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.tasks_tab.previous_expanded;
        let muted = cx.theme().muted_foreground;
        // Clicks on the actions are theirs alone.
        let actions = actions
            .into_iter()
            .enumerate()
            .map(|(ix, action)| {
                div()
                    .id(("tasks-previous-action", ix))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(action)
                    .into_any_element()
            })
            .collect();
        let heading = h_flex()
            .id("tasks-previous")
            .cursor_pointer()
            .child(
                Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                    .xsmall()
                    .text_color(muted),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.tasks_tab.previous_expanded = !this.tasks_tab.previous_expanded;
                cx.notify();
            }));
        group_heading_in(heading, label, actions, cx)
    }

    /// Along the Queued heading: "Send next" while a lane is free, and the
    /// auto-send switch.
    fn queue_controls(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let lane_free = self
            .queue
            .iter()
            .any(|item| !self.working.overlaps(Lanes::of(item.mode)));
        let next_ready = self.next_sendable().is_some();
        let mut controls = Vec::new();
        if lane_free && !self.queue.is_empty() {
            controls.push(
                Button::new("send-next")
                    .primary()
                    .xsmall()
                    .label("Send next")
                    .disabled(!next_ready)
                    .on_click(cx.listener(|this, _, _, cx| this.send_next_clicked(cx)))
                    .into_any_element(),
            );
        }
        controls.push(
            Switch::new("auto-send")
                .xsmall()
                .label("Send next automatically")
                .checked(self.auto_send)
                .on_click(cx.listener(|this, auto_send: &bool, _, cx| {
                    this.set_auto_send(*auto_send, cx)
                }))
                .into_any_element(),
        );
        controls
    }

    /// A task's row: the bar in its mode's colour, a checkbox for one of
    /// the previous tasks, its status, its step's label when it is one of a
    /// chain's, its prompt's first line, and how long it took.
    fn render_task_row(
        &self,
        ix: usize,
        step: Option<StepKind>,
        previous: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let task = &self.tasks[ix];
        let theme = cx.theme();
        let open = self.tasks_tab.open == Some(Opened::Task(ix));
        let tip = queued_tooltip(&task.text);
        let checkbox = previous.then(|| {
            let selected = self.task_history.selected.contains(&ix);
            let this = cx.entity().downgrade();
            history_checkbox(ix, selected, move |window, cx| {
                let range = window.modifiers().shift;
                this.update(cx, |this, cx| {
                    this.task_history.click_checkbox(ix, range);
                    cx.notify();
                })
                .ok();
            })
        });
        let label = step.map(|step| {
            let (label, mode) = step.label();
            div()
                .flex_none()
                .text_xs()
                .font_semibold()
                .text_color(chat_input::mode_color(mode, cx))
                .child(label)
        });
        let row = h_flex()
            .id(("tasks-tab-task", ix))
            .relative()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl(if step.is_some() { STEP_INDENT + px(10.) } else { px(10.) })
            .pr_3()
            .text_sm()
            .cursor_pointer()
            .hover(|row| row.bg(theme.list_hover))
            .when(open, |row| row.bg(theme.list_active))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_in_tab(Opened::Task(ix), window, cx)
            }))
            .child(mode_bar(task.mode, cx).when(step.is_some(), |bar| bar.left(STEP_INDENT)))
            .children(checkbox)
            .child(div().flex_none().child(status_mark(task.status, cx)))
            .children(label)
            .child(div().flex_1().min_w_0().truncate().child(first_line(&task.text)))
            .children(
                task.took()
                    .map(|took| took_label(("tasks-tab-took", ix), took, cx)),
            );
        // Lets UI tests find the row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }

    /// A chain's row: a chevron that shows or hides its steps, its status as
    /// a whole, its prompt's first line, how many steps it has, and how long
    /// they took together. Clicking it opens the chain.
    fn render_chain_row(&self, head: usize, members: &[usize], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let steps: Vec<&PromptTask> = members.iter().filter_map(|&ix| self.tasks.get(ix)).collect();
        let expanded = self.tasks_tab.expanded_chains.contains(&head);
        let open = self.tasks_tab.open == Some(Opened::Task(head));
        let text = first_line(&self.tasks[head].text);
        let tip = queued_tooltip(&self.tasks[head].text);
        // Its own steps and those added, sent or still to come.
        let unsent = (0..self.tasks[head].added_steps.len())
            .filter(|&place| self.added_step_task(head, place).is_none())
            .count();
        let count = steps.len() + unsent;
        let row = h_flex()
            .id(("tasks-tab-chain", head))
            .relative()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl(px(10.))
            .pr_3()
            .text_sm()
            .cursor_pointer()
            .hover(|row| row.bg(theme.list_hover))
            .when(open, |row| row.bg(theme.list_active))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_in_tab(Opened::Task(head), window, cx)
            }))
            .child(mode_bar(Some(SendMode::Both), cx))
            .child(
                Button::new(("tasks-tab-chain-toggle", head))
                    .ghost()
                    .xsmall()
                    .icon(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .tooltip(if expanded { "Hide its steps" } else { "Show its steps" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.tasks_tab.expanded_chains.remove(&head) {
                            this.tasks_tab.expanded_chains.insert(head);
                        }
                        cx.notify();
                    })),
            )
            .child(div().flex_none().child(status_mark(chain_status(&steps), cx)))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if count == 1 {
                        "1 step".to_string()
                    } else {
                        format!("{count} steps")
                    }),
            )
            .children(chain_took(&steps).map(|took| took_label(("tasks-tab-chain-took", head), took, cx)));
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }

    /// The queued prompts' rows, in the queue's order: each its grip to drag
    /// it by, its place, its prompt's first line, whether it starts a new
    /// conversation, and buttons to edit it in the chat input or cancel it.
    /// Clicking one opens it here.
    fn render_queued_rows(&self, shown: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let saved = self.queue_saved_marks();
        if !cx.has_active_drag() {
            self.queue_gap.set(None);
        }
        let (dragging, gap) = match self.queue_gap.get() {
            Some((id, gap)) => (Some(id), gap),
            None => (None, None),
        };
        let queue_len = self.queue.len();
        self.queue
            .iter()
            .enumerate()
            .skip(shown.start)
            .take(shown.len())
            .map(|(ix, item)| {
                let id = item.id;
                let open = self.tasks_tab.open == Some(Opened::Queued(id));
                let editing = self.editing_queued == Some(id) || open;
                // Prompts sharing a context are joined by a line between
                // their switches.
                let joins_above = ix > 0 && !item.new_conversation;
                let joins_below = self
                    .queue
                    .get(ix + 1)
                    .is_some_and(|next| !next.new_conversation);
                let segment = |joined: bool| {
                    div()
                        .flex_1()
                        .w(px(1.))
                        .when(joined, |line| line.bg(border))
                };
                let new_conversation = v_flex()
                    .id(("new-conversation-queued-box", ix))
                    .flex_none()
                    .self_stretch()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                let tip = queued_tooltip(&item.text);
                let row = h_flex()
                    .id(("queued-prompt", ix))
                    .relative()
                    .flex_none()
                    .w_full()
                    .h(ROW_HEIGHT)
                    .gap_2()
                    .pl(px(10.))
                    .pr_3()
                    .text_sm()
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_in_tab(Opened::Queued(id), window, cx)
                    }))
                    .when(editing, |row| row.bg(theme.list_active))
                    // The one dragged stays in its place, dimmed.
                    .when(dragging == Some(id), |row| row.opacity(0.5))
                    .on_drag_move({
                        let (queue_gap, saved) = (self.queue_gap.clone(), saved.clone());
                        move |event: &DragMoveEvent<QueuedDrag>, window, cx| {
                            let Some(at) = drag_gap(ix, event.bounds, event.event.position, true)
                            else {
                                return;
                            };
                            let dragged = event.drag(cx).id;
                            let from = saved.iter().position(|&(id, _)| id == dragged);
                            let gap = from
                                .and_then(|from| gap_target(from, at))
                                .filter(|&to| queue_droppable(&saved, dragged, to))
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
                    .child(mode_bar(item.mode, cx))
                    // Dragged by its grip, which never opens it.
                    .child(
                        div()
                            .id(("queued-grip-box", ix))
                            .flex_none()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                gpui_kit::TestSupportExt::test_support(div().id(("queued-grip", ix)))
                                    .when(item.saved.is_some(), |grip| {
                                        let queue_gap = self.queue_gap.clone();
                                        grip.cursor_grab().on_drag(
                                            QueuedDrag {
                                                id,
                                                text: first_line(&item.text),
                                            },
                                            move |drag, _, _, cx| {
                                                queue_gap.set(Some((drag.id, None)));
                                                cx.new(|_| drag.clone())
                                            },
                                        )
                                    })
                                    .child(Icon::new(IconName::GripVertical).xsmall().text_color(
                                        if item.saved.is_some() {
                                            muted
                                        } else {
                                            transparent_black()
                                        },
                                    )),
                            ),
                    )
                    // Its place in the queue, as its status.
                    .child(
                        div()
                            .flex_none()
                            .min_w_5()
                            .text_color(muted)
                            .child(format!("{}.", ix + 1)),
                    )
                    .children(self.queued_label(item).map(|(label, mode)| {
                        div()
                            .id(("queued-lane", ix))
                            .flex_none()
                            .text_xs()
                            .font_semibold()
                            .text_color(chat_input::mode_color(mode, cx))
                            .child(label)
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(first_line(&item.text)))
                    .when(!item.images.is_empty(), |row| {
                        row.child(
                            gpui_kit::TestSupportExt::test_support(h_flex().id(("queued-images", ix)))
                                .flex_none()
                                .gap_1()
                                .text_xs()
                                .text_color(muted)
                                .child(Icon::new(IconName::Image).xsmall().text_color(muted))
                                .child(item.images.len().to_string()),
                        )
                    })
                    .when(!item.files.is_empty(), |row| {
                        row.child(
                            gpui_kit::TestSupportExt::test_support(h_flex().id(("queued-files", ix)))
                                .flex_none()
                                .gap_1()
                                .text_xs()
                                .text_color(muted)
                                .child(Icon::new(IconName::Paperclip).xsmall().text_color(muted))
                                .child(item.files.len().to_string()),
                        )
                    })
                    .when(item.saved.is_none(), |row| {
                        row.child(div().flex_none().child(Spinner::new().xsmall()))
                    })
                    .child(new_conversation)
                    .child(
                        h_flex()
                            .id(("queued-buttons", ix))
                            .flex_none()
                            .gap_1()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                Button::new(("edit-queued", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Pencil)
                                    .tooltip("Edit this prompt in the chat input")
                                    .selected(self.editing_queued == Some(id))
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
                            ),
                    );
                gpui_kit::TestSupportExt::test_support(row).into_any_element()
            })
            .collect()
    }

    /// Where each queued prompt is and whether it is saved: it moves only
    /// past saved prompts.
    fn queue_saved_marks(&self) -> Rc<Vec<(usize, bool)>> {
        Rc::new(
            self.queue
                .iter()
                .map(|item| (item.id, item.saved.is_some()))
                .collect(),
        )
    }

    /// Scrolls the tab near its edges while a queued prompt is dragged, the
    /// gap shown chosen anew each frame from the prompts now under the
    /// pointer, the first of them the column's `first` item.
    fn queue_drag_driver(&self, first: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let (queue_gap, saved) = (self.queue_gap.clone(), self.queue_saved_marks());
        let queue_len = self.queue.len();
        let scroll = self.tasks_tab.scroll.clone();
        Some(self.queue_drag_scroll.driver(
            &self.tasks_tab.scroll,
            true,
            ROW_HEIGHT,
            cx.entity_id(),
            move |at, area, _| {
                // Where each row lies follows from its place, drawn or not.
                let item = |row: usize| {
                    Some(Bounds::new(
                        point(area.left(), area.top() + scroll.offset().y + ROW_HEIGHT * row as f32),
                        size(area.size.width, ROW_HEIGHT),
                    ))
                };
                let Some((dragged, shown)) = queue_gap.get() else {
                    return false;
                };
                let from = saved.iter().position(|&(id, _)| id == dragged);
                let gap = area
                    .contains(&at)
                    .then(|| {
                        (0..queue_len).find_map(|ix| drag_gap(ix, item(first + ix)?, at, true))
                    })
                    .flatten()
                    .filter(|&gap| {
                        from.and_then(|from| gap_target(from, gap))
                            .is_some_and(|to| queue_droppable(&saved, dragged, to))
                    });
                queue_gap.set(Some((dragged, gap)));
                gap != shown
            },
        )
        .into_any_element())
    }

    /// The task open in the tab's place: its mode and name along the top,
    /// with a back arrow, then what it is at a glance, its prompt, its
    /// parameters, its actions, and a chain's steps.
    fn render_task_details(&self, open: Opened, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (mode, name) = match open {
            Opened::Task(ix) => {
                let task = &self.tasks[ix];
                (task.mode, task.name.clone())
            }
            Opened::Queued(id) => {
                let item = self.queue.iter().find(|item| item.id == id);
                (
                    item.and_then(|item| item.mode),
                    item.and_then(|item| item.saved.as_ref())
                        .map(|saved| saved.anchor.name().to_string().into())
                        .unwrap_or_else(|| "Saving…".into()),
                )
            }
        };
        let mode_label: SharedString = match mode {
            Some(SendMode::Both) => "Chain".into(),
            Some(mode) => mode.label().into(),
            None => "Mode not known".into(),
        };
        let mode_color = mode.map_or(theme.muted_foreground, |mode| chat_input::mode_color(mode, cx));
        let top = h_flex()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .gap_2()
            .px_2()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("tasks-tab-back")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back to the tasks (Esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_task_in_tab(window, cx))),
            )
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .font_semibold()
                    .text_color(mode_color)
                    .child(mode_label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(name),
            );
        let body = match open {
            Opened::Task(ix) => self.render_sent_details(ix, cx),
            Opened::Queued(id) => self.render_queued_details(id, window, cx),
        };
        v_flex()
            .size_full()
            .child(top)
            .child(
                div().flex_1().min_h_0().relative().child(
                    // Lets UI tests find the details; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(
                        v_flex()
                            .id("tasks-tab-details")
                            .absolute()
                            .inset_0()
                            .overflow_y_scroll(),
                    )
                    .children(body),
                ),
            )
            .into_any_element()
    }

    /// An open task that has run, or is running: as it was sent, its
    /// figures, and what can be done with it.
    fn render_sent_details(&self, ix: usize, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let task = &self.tasks[ix];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut glance = section("At a glance", cx).child(figure(
            "Status",
            h_flex()
                .gap_2()
                .child(task.status.tag(cx))
                .children(task.took().map(|took| took_label(("tasks-tab-status-took", ix), took, cx))),
            cx,
        ));
        glance = glance.child(figure("Lane", lane_label(task.mode), cx));
        glance = glance.child(figure(
            "Conversation",
            if task.new_conversation {
                "Started a new one".to_string()
            } else {
                match &task.session {
                    Some(session) => format!("Carries on {}", session.chars().take(8).collect::<String>()),
                    None => "Carries on its lane's".to_string(),
                }
            },
            cx,
        ));
        let agent = crate::agent::of_project(self.project_dir.as_deref());
        glance = glance.child(figure(
            "Model and effort",
            format!(
                "{} · {}",
                crate::models::label(agent, task.model.as_deref()),
                crate::effort::label(agent, task.effort.as_deref())
            ),
            cx,
        ));
        if !task.status.is_active() || task.reply.row_count() > 0 {
            let calls = task.reply.tool_calls();
            glance = glance.child(figure("Tool calls", calls.to_string(), cx));
            let errors = task.reply.errors().len();
            if errors > 0 {
                glance = glance.child(figure(
                    "Errors",
                    div()
                        .text_color(Hue::Red.of(crate::theme::palette(cx)))
                        .child(errors.to_string()),
                    cx,
                ));
            }
            if let Some((tokens, cost)) = self.usage.spent_by(&task.name) {
                let spent = match cost {
                    Some(cost) => format!("{} · {}", chat_input::tokens_label(tokens), usage::cost_label(cost)),
                    None => chat_input::tokens_label(tokens),
                };
                glance = glance.child(figure("Tokens and cost", spent, cx));
            }
            if let Some(changed) = &task.changed {
                glance = glance.child(figure("Files changed", changed.files.len().to_string(), cx));
            }
        }
        // Its prompt, as it was typed, and what was attached to it.
        let attached = attachments_line(
            &task.sent.attached_text,
            &task.sent.attached_images,
            &task.sent.attached_files,
        );
        let prompt = section("Prompt", cx)
            .child(
                gpui_kit::TestSupportExt::test_support(div().id("tasks-tab-prompt"))
                    .w_full()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .text_sm()
                    .whitespace_normal()
                    .child(task.text.clone()),
            )
            .children(attached.map(|line| div().text_xs().text_color(muted).child(line)));
        // Its parameters, read as they were sent.
        let parameters = section("Parameters", cx)
            .child(figure(
                "Mode",
                match task.sent.mode {
                    Some(SendMode::Both) => "Chain",
                    Some(mode) => mode.label(),
                    None => "Not known",
                },
                cx,
            ))
            .child(figure("Model", crate::models::label(agent, task.model.as_deref()), cx))
            .child(figure("Effort", crate::effort::label(agent, task.effort.as_deref()), cx))
            .child(figure("Slice", if task.sent.sliced { "On" } else { "Off" }, cx))
            .child(figure(
                "New conversation",
                if task.new_conversation { "Yes" } else { "No" },
                cx,
            ));
        // What can be done with it.
        let this = cx.entity().downgrade();
        let mut actions = h_flex().flex_wrap().gap_1().child(
            resend_button(("resend-previous", ix))
                .label("Resend")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.resend(|this| &this.tasks, ix, window, cx)
                })),
        );
        if let Some((to, state)) = self.other_mode_state(task) {
            let (send, menu) = (this.clone(), this.clone());
            let name = task.name.clone();
            actions = actions.child(other_mode_button(
                ("send-to-other-previous", ix),
                to,
                state,
                move |_, window, cx| {
                    send.update(cx, |this, cx| {
                        this.send_to_other_mode(|this| &this.tasks, ix, window, cx)
                    })
                    .ok();
                },
                move |event, window, cx| {
                    menu.update(cx, |this, cx| {
                        this.open_mark_menu(name.clone(), event.position, window, cx)
                    })
                    .ok();
                },
                cx,
            ));
        }
        if task.can_cancel() {
            actions = actions.child(
                cancel_button(("cancel-previous", ix))
                    .label("Cancel")
                    .on_click(cx.listener(move |this, _, _, cx| this.cancel_task(ix, cx))),
            );
        }
        actions = actions
            .child(
                Button::new("tasks-tab-open-output")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelLeft)
                    .label("Open output")
                    .tooltip("Show its output in the message list")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tasks_tab.viewing = true;
                        this.select_task(ix, cx);
                        this.select_tab(None, cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("tasks-tab-edit-and-resend")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Pencil)
                    .label("Edit and resend")
                    .tooltip("Put it back in the queue as a new prompt, to change before sending")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.edit_and_resend(ix, window, cx)
                    })),
            );
        let mut sections = vec![
            glance.into_any_element(),
            prompt.into_any_element(),
            parameters.into_any_element(),
            section("Actions", cx).child(actions).into_any_element(),
        ];
        if task.sent.mode == Some(SendMode::Both) {
            sections.push(self.render_chain_steps(ix, cx));
        }
        sections
    }

    /// An open queued task, edited right there: its prompt typed in, its
    /// attachments removed, and its parameters changed, each change saved
    /// as it is made, held, never sent, while it is open.
    fn render_queued_details(&self, id: usize, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some((place, item)) = self.queue.iter().enumerate().find(|(_, item)| item.id == id) else {
            return Vec::new();
        };
        let fields = self.queued_fields(id);
        let saving = fields.is_none();
        let mode = fields.as_ref().map(|fields| fields.mode).or(item.mode);
        let agent = crate::agent::of_project(self.project_dir.as_deref());
        let mut glance = section("At a glance", cx)
            .child(figure(
                "Status",
                if self.tasks_tab.drafts.contains(&id) {
                    "Queued, waiting for Send"
                } else if saving {
                    "Saving…"
                } else {
                    "Queued, held while open"
                },
                cx,
            ))
            .child(figure("Lane", lane_label(mode), cx))
            .child(figure("Place in queue", format!("{} of {}", place + 1, self.queue.len()), cx))
            .child(figure(
                "Conversation",
                if item.new_conversation {
                    "Starts a new one"
                } else {
                    "Carries on its lane's"
                },
                cx,
            ));
        if let Some(fields) = &fields {
            glance = glance.child(figure(
                "Model and effort",
                format!(
                    "{} · {}",
                    crate::models::label(agent, fields.model.as_deref()),
                    crate::effort::label(agent, fields.effort.as_deref())
                ),
                cx,
            ));
        }
        // Its prompt, typed in here.
        let editor = self
            .tasks_tab
            .editor
            .as_ref()
            .filter(|editor| editor.id == id)
            .map(|editor| {
                gpui_kit::TestSupportExt::test_support(div().id("tasks-tab-prompt-editor"))
                    .w_full()
                    .child(Textarea::new(&editor.input))
                    .into_any_element()
            });
        let mut prompt = section("Prompt", cx).children(editor);
        if let Some(fields) = &fields {
            for (ix, text) in fields.attached.text.iter().enumerate() {
                prompt = prompt.child(attachment_row(
                    ("tasks-tab-remove-text", ix),
                    format!("Attached text: {}", first_line(text)),
                    cx.listener(move |this, _, window, cx| {
                        this.change_queued(id, move |fields| {
                            if ix < fields.attached.text.len() {
                                fields.attached.text.remove(ix);
                            }
                        }, window, cx)
                    }),
                    cx,
                ));
            }
            for (ix, image) in fields.attached.images.iter().enumerate() {
                let name = Path::new(image)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| image.clone());
                prompt = prompt.child(attachment_row(
                    ("tasks-tab-remove-image", ix),
                    format!("Image: {name}"),
                    cx.listener(move |this, _, window, cx| {
                        this.change_queued(id, move |fields| {
                            if ix < fields.attached.images.len() {
                                fields.attached.images.remove(ix);
                            }
                        }, window, cx)
                    }),
                    cx,
                ));
            }
            for (ix, file) in fields.attached.files.iter().enumerate() {
                let name = Path::new(file)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.clone());
                prompt = prompt.child(attachment_row(
                    ("tasks-tab-remove-file", ix),
                    format!("File: {name}"),
                    cx.listener(move |this, _, window, cx| {
                        this.change_queued(id, move |fields| {
                            if ix < fields.attached.files.len() {
                                fields.attached.files.remove(ix);
                            }
                        }, window, cx)
                    }),
                    cx,
                ));
            }
        }
        // Its parameters, each as the chat input shows it.
        let mut parameters = section("Parameters", cx);
        if let Some(fields) = fields.clone() {
            let this = cx.entity().downgrade();
            let mode_menu = {
                let this = this.clone();
                Button::new("tasks-tab-mode")
                    .ghost()
                    .xsmall()
                    .label(match fields.mode {
                        SendMode::Both => "Chain",
                        mode => mode.label(),
                    })
                    .text_color(chat_input::mode_color(fields.mode, cx))
                    .dropdown_caret(true)
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        for mode in [SendMode::Code, SendMode::Both, SendMode::Spec, SendMode::Ask, SendMode::Freeform] {
                            let this = this.clone();
                            let label = match mode {
                                SendMode::Both => "Chain",
                                mode => mode.label(),
                            };
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(mode == fields.mode)
                                    .on_click(move |_, window, cx| {
                                        this.update(cx, |this, cx| {
                                            this.change_queued(id, move |fields| fields.mode = mode, window, cx)
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        menu
                    })
            };
            parameters = parameters.child(figure("Mode", mode_menu, cx));
            let freeform = fields.mode == SendMode::Freeform;
            let model_menu = {
                let this = this.clone();
                let chosen = fields.model.clone();
                Button::new("tasks-tab-model")
                    .ghost()
                    .xsmall()
                    .label(crate::models::label(agent, chosen.as_deref()))
                    .dropdown_caret(true)
                    .disabled(freeform)
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        let entries = std::iter::once((None, "Default".to_string())).chain(
                            crate::models::available(agent)
                                .into_iter()
                                .map(|model| (Some(model.id), model.label)),
                        );
                        for (model, label) in entries {
                            let this = this.clone();
                            let picked = model.clone();
                            menu = menu.item(PopupMenuItem::new(label).checked(chosen == model).on_click(
                                move |_, window, cx| {
                                    let picked = picked.clone();
                                    this.update(cx, |this, cx| {
                                        this.change_queued(id, move |fields| fields.model = picked, window, cx)
                                    })
                                    .ok();
                                },
                            ));
                        }
                        menu
                    })
            };
            parameters = parameters.child(figure("Model", model_menu, cx));
            let levels = crate::effort::available(agent);
            let effort_menu = {
                let this = this.clone();
                let chosen = fields.effort.clone();
                Button::new("tasks-tab-effort")
                    .ghost()
                    .xsmall()
                    .label(crate::effort::label(agent, chosen.as_deref()))
                    .dropdown_caret(true)
                    .disabled(freeform || levels.is_empty())
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        let entries = std::iter::once((None, "Default".to_string())).chain(
                            crate::effort::available(agent)
                                .into_iter()
                                .map(|level| (Some(level.id), level.label)),
                        );
                        for (effort, label) in entries {
                            let this = this.clone();
                            let picked = effort.clone();
                            menu = menu.item(PopupMenuItem::new(label).checked(chosen == effort).on_click(
                                move |_, window, cx| {
                                    let picked = picked.clone();
                                    this.update(cx, |this, cx| {
                                        this.change_queued(id, move |fields| fields.effort = picked, window, cx)
                                    })
                                    .ok();
                                },
                            ));
                        }
                        menu
                    })
            };
            parameters = parameters.child(figure("Effort", effort_menu, cx));
            parameters = parameters
                .child(figure(
                    "Slice",
                    Button::new("tasks-tab-slice")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Scissors)
                        .label("Slice")
                        .selected(fields.sliced)
                        .disabled(freeform)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.change_queued(id, |fields| fields.sliced = !fields.sliced, window, cx)
                        })),
                    cx,
                ))
                .child(figure(
                    "New conversation",
                    Switch::new("tasks-tab-new-conversation")
                        .xsmall()
                        .checked(item.new_conversation)
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            this.toggle_queued_new_conversation(id, cx)
                        })),
                    cx,
                ));
        } else {
            parameters = parameters.child(div().text_sm().text_color(muted).child("Saving…"));
        }
        // What can be done with it.
        let mut actions = h_flex().flex_wrap().gap_1();
        if self.tasks_tab.drafts.contains(&id) {
            actions = actions.child(
                Button::new("tasks-tab-send-draft")
                    .primary()
                    .xsmall()
                    .label("Send")
                    .disabled(saving)
                    .on_click(cx.listener(move |this, _, window, cx| this.send_draft(id, window, cx))),
            );
        }
        actions = actions.child(
            cancel_button("tasks-tab-cancel-queued")
                .label("Cancel")
                .tooltip("Cancel this prompt")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.tasks_tab.drafts.remove(&id);
                    this.close_task_in_tab(window, cx);
                    this.cancel(id, window, cx);
                })),
        );
        let _ = window;
        let mut sections = vec![
            glance.into_any_element(),
            prompt.into_any_element(),
            parameters.into_any_element(),
            section("Actions", cx).child(actions).into_any_element(),
        ];
        if mode == Some(SendMode::Both) {
            sections.push(self.render_queued_chain_note(cx));
        }
        sections
    }

    /// A queued chain hasn't run yet: its steps are its own until it does.
    fn render_queued_chain_note(&self, cx: &mut Context<Self>) -> AnyElement {
        section("Steps", cx)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Its spec step, then its code step, once it is sent; more steps can be added once it has been."),
            )
            .into_any_element()
    }
}

/// What is attached to a prompt, in a line: how many pieces of text and
/// images; none with nothing attached.
fn attachments_line(text: &[String], images: &[String], files: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    match text.len() {
        0 => {}
        1 => parts.push("1 attached text".to_string()),
        n => parts.push(format!("{n} attached texts")),
    }
    match images.len() {
        0 => {}
        1 => parts.push("1 image".to_string()),
        n => parts.push(format!("{n} images")),
    }
    match files.len() {
        0 => {}
        1 => parts.push("1 file".to_string()),
        n => parts.push(format!("{n} files")),
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// A thing attached to an open queued task, with a button removing it.
fn attachment_row(
    id: (&'static str, usize),
    label: String,
    remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .gap_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(IconName::X)
                .tooltip("Remove it")
                .on_click(remove),
        )
        .into_any_element()
}

/// Whether the queued prompt `dragged` may be dropped at place `to`: only
/// past saved prompts.
pub(super) fn queue_droppable(saved: &[(usize, bool)], dragged: usize, to: usize) -> bool {
    saved
        .iter()
        .position(|&(id, _)| id == dragged)
        .is_some_and(|from| from != to && saved[from.min(to)..=from.max(to)].iter().all(|&(_, ok)| ok))
}

impl PromptMode {
    /// The task sent as the step at `place` among those added to the chain
    /// begun by the task at `head`, once it has been.
    pub(super) fn added_step_task(&self, head: usize, place: usize) -> Option<usize> {
        let name = self.tasks.get(head)?.name.to_string();
        self.tasks
            .iter()
            .position(|task| task.sent.added_step.as_ref() == Some(&(name.clone(), place)))
    }

    /// The chain the task at `ix` is a step of, by its first step, the
    /// Chain task, found through the steps each was sent from; none for a
    /// task of no chain.
    fn chain_head_of(&self, ix: usize) -> Option<usize> {
        let mut at = ix;
        // A chain is only so many steps long; a loop is no chain.
        for _ in 0..=self.tasks.len() {
            let task = self.tasks.get(at)?;
            if let Some((head, _)) = &task.sent.added_step {
                return self.tasks.iter().position(|task| task.name.as_ref() == head);
            }
            if task.sent.mode == Some(SendMode::Both) {
                return Some(at);
            }
            // Only a step handed on from the one before is of its chain.
            task.sent.code_task.as_ref()?;
            let from = task.sent.sent_from.as_deref()?;
            at = self.tasks[..at]
                .iter()
                .rposition(|task| task.name.as_ref() == from)?;
        }
        None
    }

    /// The step a chain goes on to once the task at `ix`, its last step so
    /// far, finished as Done, when that is one added to it: the chain's
    /// first step, and the added step's place. None while the chain's own
    /// steps go on, or when nothing is added after it, or its prompt is
    /// empty.
    pub(super) fn next_added_step(&self, ix: usize) -> Option<(usize, usize)> {
        let task = self.tasks.get(ix)?;
        if chain_next(task).is_some() {
            return None;
        }
        let head = self.chain_head_of(ix)?;
        let place = match &task.sent.added_step {
            Some((_, place)) => place + 1,
            None => 0,
        };
        let step = self.tasks[head].added_steps.get(place)?;
        (!step.prompt.trim().is_empty() && self.added_step_task(head, place).is_none())
            .then_some((head, place))
    }

    /// Sends the step at `place` among those added to the chain begun by
    /// the task at `head`, after the task at `after`, the step before it:
    /// in its own mode's lane and place, as every step of a chain is, told
    /// its own prompt, the chain's prompt as typed, and what the step
    /// before it replied.
    pub(super) fn send_added_step(&mut self, head: usize, place: usize, after: usize, cx: &mut Context<Self>) {
        let (Some(chain), Some(before)) = (self.tasks.get(head), self.tasks.get(after)) else {
            return;
        };
        let Some(step) = chain.added_steps.get(place).cloned() else {
            return;
        };
        let code_task = CodeTask {
            prompt: chain.text.to_string(),
            result: before.reply.final_output(),
        };
        let sending = Sending::Now(
            step.mode,
            Attached::default(),
            chain.sent.sliced,
            Some(code_task),
            Some(before.name.to_string()),
            false,
            Some(before.name.to_string()),
        );
        let stamp = chain.chain_stamp;
        self.pending_step = Some(PendingStep {
            added: (chain.name.to_string(), place),
            model: step.model,
            effort: step.effort,
        });
        self.chain_on(step.prompt, sending, stamp, cx);
        // Taken by the step as it is sent or queued.
        self.pending_step = None;
    }

    /// The chain's steps, its own and those added, in the order they run:
    /// the chain's own and added steps sent, then those still to come.
    fn chain_step_ixs(&self, head: usize) -> Vec<(usize, StepKind)> {
        let layout = chain_layout(&self.tasks);
        let Some(step) = layout.step_of(head) else {
            return vec![(head, StepKind::Spec)];
        };
        (0..step.len)
            .filter_map(|pos| {
                let place = step.start + pos;
                Some((*layout.order.get(place)?, layout.steps.get(place)?.as_ref()?.kind))
            })
            .collect()
    }

    /// The step the next added step goes after, once the chain is over: its
    /// last step, while it finished as Done and nothing of the chain is
    /// still going.
    fn chain_ready_after(&self, head: usize) -> Option<usize> {
        let steps = self.chain_step_ixs(head);
        if steps.iter().any(|&(ix, _)| self.tasks[ix].status.is_active())
            || self.tasks[head].held_for_fix
        {
            return None;
        }
        let &(last, _) = steps.last()?;
        // Its own steps all sent.
        (self.tasks[last].status == TaskStatus::Done && chain_next(&self.tasks[last]).is_none())
            .then_some(last)
    }

    /// Adds a step of `mode` after the chain's last, with the chain's model
    /// and effort, ready for its prompt to be typed.
    fn add_chain_step(&mut self, head: usize, mode: SendMode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chain) = self.tasks.get_mut(head) else {
            return;
        };
        chain.added_steps.push(AddedStep {
            mode,
            prompt: String::new(),
            model: chain.model.clone(),
            effort: chain.effort.clone(),
        });
        let place = chain.added_steps.len() - 1;
        self.save_added_steps(head, cx);
        self.edit_chain_step(head, place, window, cx);
    }

    /// Opens the prompt of the step at `place` added to the chain begun by
    /// the task at `head`, to be typed in.
    fn edit_chain_step(&mut self, head: usize, place: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.save_step_prompt(cx);
        let Some(step) = self.tasks.get(head).and_then(|chain| chain.added_steps.get(place)) else {
            return;
        };
        let text = step.prompt.clone();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, PROMPT_MAX_ROWS)
                .placeholder("What this step should do…")
                .default_value(text)
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let changed = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(editor) = this.tasks_tab.step_editor.as_mut()
            {
                editor._save = cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(SAVE_REST).await;
                    this.update(cx, |this, cx| this.save_step_prompt(cx)).ok();
                });
            }
        });
        self.tasks_tab.step_editor = Some(StepEditor {
            head,
            place,
            input,
            _changed: changed,
            _save: Task::ready(()),
        });
        cx.notify();
    }

    /// Keeps what was typed in the step being edited, with the chain.
    fn save_step_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.tasks_tab.step_editor else {
            return;
        };
        let (head, place) = (editor.head, editor.place);
        let text = editor.input.read(cx).value().to_string();
        let Some(step) = self
            .tasks
            .get_mut(head)
            .and_then(|chain| chain.added_steps.get_mut(place))
        else {
            return;
        };
        if step.prompt != text && self.added_step_task(head, place).is_none() {
            self.tasks[head].added_steps[place].prompt = text;
            self.save_added_steps(head, cx);
        }
    }

    /// Changes the step at `place` added to the chain begun at `head`, while
    /// it hasn't been sent.
    fn change_chain_step(&mut self, head: usize, place: usize, change: impl FnOnce(&mut AddedStep), cx: &mut Context<Self>) {
        if self.added_step_task(head, place).is_some() {
            return;
        }
        if let Some(step) = self.tasks.get_mut(head).and_then(|chain| chain.added_steps.get_mut(place)) {
            change(step);
            self.save_added_steps(head, cx);
        }
    }

    /// Moves the step at `place` added to the chain begun at `head` one
    /// place `up`, or down, among those not yet sent.
    fn move_chain_step(&mut self, head: usize, place: usize, up: bool, cx: &mut Context<Self>) {
        self.save_step_prompt(cx);
        let other = if up { place.checked_sub(1) } else { Some(place + 1) };
        let Some(other) = other.filter(|&other| {
            self.tasks[head].added_steps.len() > other
                && self.added_step_task(head, other).is_none()
                && self.added_step_task(head, place).is_none()
        }) else {
            return;
        };
        self.tasks[head].added_steps.swap(place, other);
        self.tasks_tab.step_editor = None;
        self.save_added_steps(head, cx);
    }

    /// Removes the step at `place` added to the chain begun at `head`, while
    /// it hasn't been sent.
    fn remove_chain_step(&mut self, head: usize, place: usize, cx: &mut Context<Self>) {
        if self.added_step_task(head, place).is_some()
            || (place + 1..self.tasks[head].added_steps.len())
                .any(|later| self.added_step_task(head, later).is_some())
        {
            return;
        }
        self.tasks_tab.step_editor = None;
        self.tasks[head].added_steps.remove(place);
        self.save_added_steps(head, cx);
    }

    /// Saves the steps added to the chain begun at `head` in its hidden
    /// anchor, in the history, so the chain keeps them across restarts.
    fn save_added_steps(&mut self, head: usize, cx: &mut Context<Self>) {
        let (Some(chain), Some(project_dir)) = (self.tasks.get(head), self.project_dir.clone()) else {
            return;
        };
        let (name, steps) = (chain.name.to_string(), chain.added_steps.clone());
        cx.background_spawn(async move {
            let saved = (|| -> Result<()> {
                let file = prompt_history::history_file(&project_dir, &name)
                    .ok_or_else(|| anyhow::anyhow!("the chain {name} isn't in the history"))?;
                let source = std::fs::read_to_string(&file)?;
                let (mut anchor, prompt) = HiddenAnchor::parse(&source)
                    .ok_or_else(|| anyhow::anyhow!("{} is no prompt", file.display()))?;
                anchor.added_steps = steps;
                std::fs::write(&file, anchor.source(&prompt))?;
                Ok(())
            })();
            if let Err(err) = saved {
                eprintln!("could not keep the chain's steps: {err:#}");
            }
        })
        .detach();
        cx.notify();
    }

    /// An open Chain task's steps, in the order they run, each a row with
    /// its status; those added and not yet sent can be typed in, moved, or
    /// removed; and "Add step" beneath them.
    pub(super) fn render_chain_steps(&self, head: usize, cx: &mut Context<Self>) -> AnyElement {
        let (muted, hover) = (cx.theme().muted_foreground, cx.theme().list_hover);
        let mut steps = section("Steps", cx);
        for (ix, kind) in self.chain_step_ixs(head) {
            steps = steps.child(self.render_task_row(ix, Some(kind), false, cx));
        }
        let chain = &self.tasks[head];
        let unsent: Vec<usize> = (0..chain.added_steps.len())
            .filter(|&place| self.added_step_task(head, place).is_none())
            .collect();
        let ready_after = self.chain_ready_after(head);
        let agent = crate::agent::of_project(self.project_dir.as_deref());
        for (nth, &place) in unsent.iter().enumerate() {
            let step = &chain.added_steps[place];
            let (label, color) = match step.mode {
                SendMode::Spec => ("Spec step", chat_input::mode_color(SendMode::Spec, cx)),
                _ => ("Code step", chat_input::mode_color(SendMode::Code, cx)),
            };
            let editing = self
                .tasks_tab
                .step_editor
                .as_ref()
                .filter(|editor| editor.head == head && editor.place == place);
            let prompt = match editing {
                Some(editor) => div()
                    .w_full()
                    .child(Textarea::new(&editor.input))
                    .into_any_element(),
                None => {
                    let shown: SharedString = if step.prompt.trim().is_empty() {
                        "Type what this step should do…".into()
                    } else {
                        step.prompt.clone().into()
                    };
                    div()
                        .id(("chain-step-prompt", place))
                        .w_full()
                        .p_1()
                        .rounded_md()
                        .cursor_text()
                        .text_sm()
                        .whitespace_normal()
                        .when(step.prompt.trim().is_empty(), |text| text.text_color(muted))
                        .hover(move |text| text.bg(hover))
                        .child(shown)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_chain_step(head, place, window, cx)
                        }))
                        .into_any_element()
                }
            };
            let can_send = nth == 0 && ready_after.is_some() && !step.prompt.trim().is_empty();
            let row = v_flex()
                .w_full()
                .gap_1()
                .py_1()
                .pl(px(10.))
                .relative()
                .child(mode_bar(Some(step.mode), cx))
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_semibold()
                                .text_color(color)
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(format!(
                                    "{} · {}",
                                    crate::models::label(agent, step.model.as_deref()),
                                    crate::effort::label(agent, step.effort.as_deref())
                                )),
                        )
                        .child(step_model_menu(head, place, step.model.clone(), cx))
                        .child(
                            Button::new(("chain-step-up", place))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ArrowUp)
                                .tooltip("Move it up")
                                .disabled(nth == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_chain_step(head, place, true, cx)
                                })),
                        )
                        .child(
                            Button::new(("chain-step-down", place))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ArrowDown)
                                .tooltip("Move it down")
                                .disabled(nth + 1 == unsent.len())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_chain_step(head, place, false, cx)
                                })),
                        )
                        .child(
                            Button::new(("chain-step-remove", place))
                                .ghost()
                                .xsmall()
                                .icon(IconName::X)
                                .tooltip("Remove this step")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_chain_step(head, place, cx)
                                })),
                        ),
                )
                .child(prompt)
                .when(can_send, |row| {
                    row.child(
                        div().child(
                            Button::new(("chain-step-send", place))
                                .primary()
                                .xsmall()
                                .label("Send step")
                                .tooltip("The chain is over: send this step after its last")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_step_prompt(cx);
                                    if let Some(after) = this.chain_ready_after(head) {
                                        this.send_added_step(head, place, after, cx);
                                    }
                                })),
                        ),
                    )
                });
            // Lets UI tests find the step; inert in normal builds.
            steps = steps.child(
                gpui_kit::TestSupportExt::test_support(div().id(("chain-step", place)))
                    .w_full()
                    .child(row),
            );
        }
        let this = cx.entity().downgrade();
        let add = Button::new("chain-add-step")
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .label("Add step")
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |menu, _, _| {
                let (code, spec) = (this.clone(), this.clone());
                menu.item(PopupMenuItem::new("Code step").on_click(move |_, window, cx| {
                    code.update(cx, |this, cx| this.add_chain_step(head, SendMode::Code, window, cx))
                        .ok();
                }))
                .item(PopupMenuItem::new("Spec step").on_click(move |_, window, cx| {
                    spec.update(cx, |this, cx| this.add_chain_step(head, SendMode::Spec, window, cx))
                        .ok();
                }))
            });
        steps.child(div().pt_1().child(add)).into_any_element()
    }
}

/// The menu changing the model of a step added to a chain, and its effort
/// where the harness takes one.
fn step_model_menu(head: usize, place: usize, chosen: Option<String>, cx: &mut Context<PromptMode>) -> AnyElement {
    let agent = crate::agent::of_project(cx.entity().read(cx).project_dir.as_deref());
    let this = cx.entity().downgrade();
    Button::new(("chain-step-model", place))
        .ghost()
        .xsmall()
        .icon(IconName::Settings2)
        .tooltip("Change its model and effort")
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
            let models = std::iter::once((None, "Default model".to_string())).chain(
                crate::models::available(agent)
                    .into_iter()
                    .map(|model| (Some(model.id), model.label)),
            );
            for (model, label) in models {
                let this = this.clone();
                let picked = model.clone();
                menu = menu.item(PopupMenuItem::new(label).checked(chosen == model).on_click(
                    move |_, _, cx| {
                        let picked = picked.clone();
                        this.update(cx, |this, cx| {
                            this.change_chain_step(head, place, |step| step.model = picked, cx)
                        })
                        .ok();
                    },
                ));
            }
            let levels = crate::effort::available(agent);
            if !levels.is_empty() {
                menu = menu.separator();
                let efforts = std::iter::once((None, "Default effort".to_string())).chain(
                    levels.into_iter().map(|level| (Some(level.id), level.label)),
                );
                for (effort, label) in efforts {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let picked = effort.clone();
                        this.update(cx, |this, cx| {
                            this.change_chain_step(head, place, |step| step.effort = picked, cx)
                        })
                        .ok();
                    }));
                }
            }
            menu
        })
        .into_any_element()
}
