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

/// How wide the bar down a body task tab's left edge is, in its mode's
/// colour.
const MODE_BAR: Pixels = px(2.);

/// The bar in a Tasks tab row's mode's colour, as the TasksTabScope's rows
/// say: a pill this wide, its corners rounded this much, set in from the
/// row's left and from its top and bottom by these.
const ROW_BAR: Pixels = px(4.);
const ROW_BAR_RADIUS: Pixels = px(2.);
const ROW_BAR_LEFT: Pixels = px(4.);
const ROW_BAR_INSET: Pixels = px(6.);

/// How far a chain's steps are set in beneath its row.
const STEP_INDENT: Pixels = px(16.);

/// How long the prompt of an open queued task rests after a change before
/// it is saved.
const SAVE_REST: Duration = Duration::from_millis(500);

/// The most rows an open queued task's prompt grows to: all of it, since
/// it is shown whole while it is edited.
const PROMPT_MAX_ROWS: usize = 100_000;

/// How many lines, as they wrap, an open task's prompt shows while clipped,
/// and how tall each is.
const CLIPPED_LINES: f32 = 8.;
const PROMPT_LINE: Pixels = px(20.);

/// One of the Tasks tab's rows, as what it shows, each [`ROW_HEIGHT`]
/// tall.
enum TimelineRow {
    /// A group's heading, with its label.
    Heading(Group, String),
    /// A day's divider within Previous: its label and how many tasks it
    /// shows.
    Day(String, usize),
    /// A group with nothing in it, as its id and what it says.
    Empty(&'static str, &'static str),
    /// A chain's row, as its head and all its steps.
    Chain(usize, Vec<usize>),
    /// A task's, as its index, its step's kind in a chain, and whether it is
    /// one of the previous tasks.
    Task(usize, Option<StepKind>, bool),
    /// A queued prompt's, by its place in the queue.
    Queued(usize),
    /// A step of a running chain, beneath the chain's row, by the chain's
    /// first step.
    Step(usize, StepRow),
}

/// Which row a row of the list is, whatever its place: what the list's
/// scroll is kept on, so rows moving above it never shift the view.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKey {
    Heading(Group),
    Day(String),
    Empty(&'static str),
    Chain(usize),
    Task(usize),
    Queued(usize),
    Step(usize, &'static str, usize),
}

/// A running chain's step, as its row beneath the chain shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StepRow {
    /// Sent: the task, and what it does in the chain.
    Sent(usize, StepKind),
    /// Waiting in its lane's queue, by its place there.
    Queued(usize),
    /// To be sent once the step before it finishes: what it does, the mode
    /// whose colour it reads in, and whether it follows the step running,
    /// "Next", rather than one after that, "Waiting".
    Next(&'static str, SendMode, bool),
}

/// Where a chain stands as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChainState {
    /// One of its steps is under way now, or, an earlier step of it having
    /// run, one waits in the queue.
    Running,
    /// Neither: it reads as its steps ended.
    Over,
}

/// The width of a row's lead column, holding a Previous row's checkbox
/// or a Queued row's grip, and of its status column after it, as the
/// TasksTabScope's rows say: kept whether or not anything is in them, so
/// every row's parts line up.
const LEAD_COLUMN: Pixels = px(20.);
const STATUS_COLUMN: Pixels = px(20.);

/// A column of a row `width` wide, holding `child`, centred, if anything.
fn column(width: Pixels, child: Option<AnyElement>) -> Div {
    h_flex()
        .flex_none()
        .w(width)
        .justify_center()
        .children(child)
}

/// What a day's divider reads, `date` seen from `today`: "Today",
/// "Yesterday", the weekday for the rest of the past week, else the date,
/// with the year once it isn't this one.
fn day_label(date: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    use chrono::Datelike as _;
    match (today - date).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => date.format("%A").to_string(),
        _ if date.year() == today.year() => date.format("%-d %B").to_string(),
        _ => date.format("%-d %B %Y").to_string(),
    }
}

/// A day's divider within Previous: its label, small and muted, set in to
/// line up with the rows' text, then, muted, how many tasks it shows.
fn day_divider(label: String, count: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    gpui_kit::TestSupportExt::test_support(h_flex().id(SharedString::from(format!("tasks-day-{label}"))))
        .flex_none()
        .w_full()
        .h(ROW_HEIGHT)
        .gap_2()
        .pl(px(10.))
        .pr_3()
        .bg(theme.background)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(column(LEAD_COLUMN, None))
        .child(column(STATUS_COLUMN, None))
        .child(div().font_semibold().child(label))
        .child(div().child(count.to_string()))
        .into_any_element()
}

/// The small muted mark at the right of a session's first row, where it
/// started a new conversation.
fn new_conversation_mark(id: (&'static str, usize), cx: &App) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .child(
            Icon::new(IconName::MessageSquare)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .tooltip(|window, cx| {
            gpui_kit::component::tooltip::Tooltip::new("Started a new conversation").build(window, cx)
        })
        .into_any_element()
}

/// What a heading takes besides its controls once its title and count
/// have given way: its padding, chevron and status columns, and gaps.
const HEADING_FIXED: Pixels = px(96.);

/// Room kept for the filters' button when measuring the batch actions.
const FILTER_WIDTH: Pixels = px(72.);

/// The list's own room beneath its last row.
const BOTTOM_PADDING: Pixels = px(8.);

/// `n` as "1st", "2nd", "3rd", "4th".
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The start of `text`, as much as a clipped prompt can show and one line
/// more: its first lines, each cut where it would wrap past them all.
fn first_lines(text: &SharedString) -> SharedString {
    const LINES: usize = CLIPPED_LINES as usize + 1;
    const LINE_CHARS: usize = 120 * LINES;
    let lines: Vec<&str> = text.split('\n').take(LINES).collect();
    let cut = |line: &str| match line.char_indices().nth(LINE_CHARS) {
        Some((at, _)) => line[..at].to_string(),
        None => line.to_string(),
    };
    let start: Vec<String> = lines.iter().map(|line| cut(line)).collect();
    let start = start.join("\n");
    if start.len() == text.len() { text.clone() } else { start.into() }
}

/// The timeline's groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Previous,
    Running,
    Queued,
}

/// How many rows either side of those in view the Tasks tab draws too.
pub(super) const OVERSCAN_ROWS: usize = 4;

/// What the right sidebar holds: the Tasks list alone, as the
/// RightSidebarScope says; the Run panels are the left sidebar's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SidebarTab {
    /// Every task of the project, as the TasksTabScope says.
    #[default]
    Tasks,
}

/// A task opened in the Tasks tab: one sent, by its place among the tasks,
/// or one queued, by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Whether the Previous group shows its rows; collapsed as the project
    /// opens, then kept as it is left.
    pub(super) previous_expanded: bool,
    /// Whether the Running and the Queued groups show their rows; expanded
    /// as the project opens, then kept as they are left.
    pub(super) running_expanded: bool,
    pub(super) queued_expanded: bool,
    /// The list has been shown for the project in this launch; shown the
    /// first time, it scrolls to what runs.
    pub(super) shown: Cell<bool>,
    /// The row at the top of the list's view last frame, and how far into
    /// it the view began: the scroll is kept on it as rows above it come
    /// and go.
    anchor: RefCell<Option<(RowKey, Pixels)>>,
    /// A task opened from elsewhere, whose row the list scrolls into view.
    pub(super) reveal: Cell<Option<usize>>,
    /// Each task opened in the tab, as it was left: its details' scroll and
    /// whether its prompt showed whole, kept for as long as the app runs.
    kept: HashMap<Opened, (Point<Pixels>, bool)>,
    /// Where the list was left scrolled last frame, and whether that was its
    /// bottom, so a list left at its bottom stays there as it grows.
    pub(super) left_at: Cell<Option<(Pixels, bool)>>,
    /// How wide Previous's batch actions were last laid out unfolded.
    batch_width: Rc<Cell<Pixels>>,
    /// The open task's prompt shows whole, past its 8 lines; and, for a
    /// queued one, it is being edited, shown whole as it is typed in.
    pub(super) prompt_expanded: bool,
    prompt_editing: bool,
    /// How tall the open task's prompt is laid out whole, and where its
    /// Prompt section was drawn, to scroll back to it as it is clipped.
    prompt_height: Rc<Cell<Pixels>>,
    prompt_top: Rc<Cell<Option<Pixels>>>,
    /// The open task's details, scrolled.
    pub(super) details_scroll: ScrollHandle,
    /// The day the day dividers were last labelled on, and what redraws
    /// them once midnight passes.
    labelled_on: Cell<Option<chrono::NaiveDate>>,
    midnight: RefCell<Task<()>>,
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
    /// The kind of time hovered in the open task's Time section.
    pub(super) time_hover: Option<crate::time_breakdown::Kind>,
}

impl TasksTab {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            open: None,
            scroll: ScrollHandle::new(),
            previous_expanded: false,
            running_expanded: true,
            queued_expanded: true,
            shown: Cell::new(false),
            anchor: RefCell::new(None),
            reveal: Cell::new(None),
            kept: HashMap::new(),
            left_at: Cell::new(None),
            batch_width: Rc::new(Cell::new(px(0.))),
            prompt_expanded: false,
            prompt_editing: false,
            prompt_height: Rc::new(Cell::new(px(0.))),
            prompt_top: Rc::new(Cell::new(None)),
            details_scroll: ScrollHandle::new(),
            labelled_on: Cell::new(None),
            midnight: RefCell::new(Task::ready(())),
            drafts: HashSet::new(),
            editor: None,
            step_editor: None,
            focus: cx.focus_handle(),
            filter_menu: false,
            filter_button: Rc::new(Cell::new(None)),
            time_hover: None,
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
pub(super) fn status_mark(status: TaskStatus, cx: &App) -> AnyElement {
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
pub(super) fn mode_bar(mode: Option<SendMode>, cx: &App) -> Div {
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

/// The bar in `mode`'s colour at a Tasks tab row's left: a short rounded
/// pill, inset from the row's edges, so the bars of rows one above another
/// never touch.
/// The one bar in the Chain colour a chain's rows share, in place of each
/// row's own, as the TasksTabScope's rows say: drawn in each of its rows,
/// set in from the top of the `first` and the bottom of the `last`, and
/// otherwise running to the row's edges, so the rows' bars join unbroken.
fn chain_bar(first: bool, last: bool, cx: &App) -> Div {
    let inset = |end: bool| if end { ROW_BAR_INSET } else { px(0.) };
    div()
        .absolute()
        .left(ROW_BAR_LEFT)
        .top(inset(first))
        .bottom(inset(last))
        .w(ROW_BAR)
        .when(first, |bar| bar.rounded_t(ROW_BAR_RADIUS))
        .when(last, |bar| bar.rounded_b(ROW_BAR_RADIUS))
        .bg(chat_input::mode_color(SendMode::Both, cx))
}

fn row_bar(mode: Option<SendMode>, cx: &App) -> Div {
    let color = mode.map_or(cx.theme().muted_foreground, |mode| {
        chat_input::mode_color(mode, cx)
    });
    div()
        .absolute()
        .left(ROW_BAR_LEFT)
        .top(ROW_BAR_INSET)
        .bottom(ROW_BAR_INSET)
        .w(ROW_BAR)
        .rounded(ROW_BAR_RADIUS)
        .bg(color)
}

/// A section's heading, as the TasksTabScope says: a row on the raised
/// surface, a chevron, right while `expanded` is off and down while on, in
/// the lead column, its `title` and `count` set in to line up with the
/// rows' text, the title giving way first where it is narrow, then its
/// `actions` at its right, never giving way. Its chevron calls `toggle`;
/// a click anywhere else but its actions calls `clicked`.
#[allow(clippy::too_many_arguments)]
fn section_heading(
    id: &'static str,
    title: &'static str,
    count: String,
    expanded: bool,
    actions: Vec<AnyElement>,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
    clicked: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    // Clicks on the actions are theirs alone.
    let actions = actions.into_iter().enumerate().map(|(ix, action)| {
        div()
            .id((id, ix))
            .flex_none()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(action)
    });
    let chevron = div()
        .id((id, 1000usize))
        .flex_none()
        .w(LEAD_COLUMN)
        .flex()
        .justify_center()
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            toggle(window, cx)
        })
        .child(
            Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                .xsmall()
                .text_color(muted),
        );
    // Lets UI tests find it; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(h_flex().id(id))
        .flex_none()
        .w_full()
        .h(ROW_HEIGHT)
        .overflow_hidden()
        .gap_2()
        .pl(px(10.))
        .pr_3()
        .bg(theme.tab_bar)
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(muted)
        .cursor_pointer()
        .on_click(move |_, window, cx| clicked(window, cx))
        .child(chevron)
        .child(column(STATUS_COLUMN, None))
        // The title gives way first, then the count; never the actions.
        .child(div().flex_shrink(100.).min_w_0().truncate().font_semibold().child(title))
        .child(div().flex_shrink(1.).min_w_0().truncate().child(count))
        .child(div().flex_1())
        .children(actions)
        .into_any_element()
}

/// Where a list `content` tall in a view `height` tall shows from, its
/// offset from the top `offset`: clamped to its ends, and rounded to whole
/// device pixels at `scale`. It scrolls only once it overflows the view by a
/// whole pixel. A list `left_at` its bottom last frame, and not scrolled
/// since, stays at its bottom, however it has grown or the view has changed.
/// With whether it is at its top and at its bottom.
fn clamped_top(
    offset: Pixels,
    content: Pixels,
    height: Pixels,
    scale: f32,
    left_at: Option<(Pixels, bool)>,
    at_laid_out_bottom: bool,
) -> (Pixels, bool, (bool, bool)) {
    let scale = if scale > 0. { scale } else { 1. };
    let overflow = content - height;
    // Its end, at a whole device pixel no further than the content goes.
    let max = if overflow >= px(1.) {
        px((f32::from(overflow) * scale).floor() / scale)
    } else {
        px(0.)
    };
    // Left at its bottom and not scrolled since: as it was left, or as the
    // layout put it at its bottom.
    let kept_at_bottom =
        left_at.is_some_and(|(left, bottom)| bottom && (left == offset || at_laid_out_bottom));
    let top = if kept_at_bottom {
        max
    } else {
        px((f32::from(-offset) * scale).round() / scale).clamp(px(0.), max)
    };
    let at_bottom = max > px(0.) && top >= max;
    (top, at_bottom, (top <= px(0.), max <= px(0.) || at_bottom))
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
pub(super) fn section(label: &'static str, cx: &App) -> Div {
    v_flex().w_full().gap_1().px_3().py_2().child(
        div()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(label),
    )
}

impl PromptMode {
    /// Redraws the day dividers once midnight passes, so "Today" reads
    /// "Yesterday" without the tab being opened again.
    fn follow_midnight(&self, cx: &mut Context<Self>) {
        let now = chrono::Local::now();
        let today = now.date_naive();
        if self.tasks_tab.labelled_on.replace(Some(today)) == Some(today) {
            return;
        }
        let midnight = today
            .succ_opt()
            .and_then(|day| day.and_hms_opt(0, 0, 1))
            .and_then(|at| at.and_local_timezone(chrono::Local).earliest());
        let wait = midnight
            .and_then(|at| (at - now).to_std().ok())
            .unwrap_or(Duration::from_secs(3600));
        *self.tasks_tab.midnight.borrow_mut() = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        });
    }

    /// The day, in local time, the task at `ix` was sent, where it can be
    /// told.
    fn sent_on(&self, ix: usize) -> Option<chrono::NaiveDate> {
        let task = self.tasks.get(ix)?;
        let at = match task.asked_at {
            Some(secs) => std::time::UNIX_EPOCH + Duration::from_secs(secs),
            None => task.span()?.0,
        };
        Some(chrono::DateTime::<chrono::Local>::from(at).date_naive())
    }

    /// The tasks the Previous group lists: every task that has run, none
    /// under way, nor any step of a chain running or queued.
    pub(super) fn previous_ixs(&self) -> Vec<usize> {
        let held: Vec<usize> = self
            .chains_where(|this, members| this.chain_state(members) != ChainState::Over)
            .into_iter()
            .flat_map(|(_, members)| members)
            .collect();
        (0..self.tasks.len())
            .filter(|&ix| !self.tasks[ix].status.is_active() && !held.contains(&ix))
            .collect()
    }

    /// Every chain, by its first step, with its steps, whose state `keep`
    /// keeps.
    fn chains_where(&self, keep: impl Fn(&Self, &[usize]) -> bool) -> Vec<(usize, Vec<usize>)> {
        let layout = chain_layout(&self.tasks);
        (0..layout.order.len())
            .filter_map(|place| {
                let step = layout.steps[place].filter(|step| step.pos == 0)?;
                let members = layout.members(step);
                keep(self, members).then(|| (layout.order[place], members.to_vec()))
            })
            .collect()
    }

    /// Where the chain of `members`, its steps joined by which each was sent
    /// from, stands, as the PromptEditor's chained tasks say: read only from
    /// its steps as they truly are, never from what it might still do.
    /// Running exactly while one of its steps is under way in this
    /// application now, which no step from the history is after a restart;
    /// else Queued while one of its steps waits in the queue; else over.
    pub(super) fn chain_state(&self, members: &[usize]) -> ChainState {
        let steps: Vec<&PromptTask> = members.iter().filter_map(|&ix| self.tasks.get(ix)).collect();
        // Under way, or between its steps while this session is still to
        // send the next: never over while a step it will run is to come.
        if steps.iter().any(|task| task.status.is_active() || task.goes_on || task.held_for_fix) {
            return ChainState::Running;
        }
        // A step of it waiting in the queue, its first step having run: the
        // chain is under way, though that step is queued. A chain whose
        // first step still waits is a queued prompt, not yet a chain.
        if self.chain_queued_step(members).is_some() {
            ChainState::Running
        } else {
            ChainState::Over
        }
    }

    /// Where in the queue a step of the chain of `members` waits, if one
    /// does: sent on from one of its steps, told what that did, or added
    /// to it.
    fn chain_queued_step(&self, members: &[usize]) -> Option<usize> {
        let steps: Vec<&PromptTask> = members.iter().filter_map(|&ix| self.tasks.get(ix)).collect();
        let head = members.first().and_then(|&ix| self.tasks.get(ix));
        self.queue.iter().position(|item| {
            let anchor = item.saved.as_ref().map(|saved| &saved.anchor);
            let added = anchor
                .and_then(|anchor| anchor.added_step.as_ref())
                .zip(head)
                .is_some_and(|((chain, _), head)| chain.as_str() == head.name.as_ref());
            let sent_on = item.sent_from.as_deref().is_some_and(|from| {
                steps.iter().any(|task| task.name.as_ref() == from)
            }) && anchor.is_none_or(|anchor| anchor.code_task.is_some());
            added || sent_on
        })
    }

    /// The rows of a running chain's steps, in the order they run: those
    /// sent, then its own still to come, the one waiting in the queue and
    /// those not yet sent, then the steps added to it not yet sent; every
    /// step it will run, from the moment it is sent.
    fn chain_step_rows(&self, head: usize, members: &[usize]) -> Vec<StepRow> {
        let layout = chain_layout(&self.tasks);
        let mut rows: Vec<StepRow> = members
            .iter()
            .map(|&ix| StepRow::Sent(ix, layout.step_of(ix).map_or(StepKind::Spec, |step| step.kind)))
            .collect();
        rows.extend(self.chain_pending_steps(head, members));
        // Then the steps added to it, not yet sent or queued.
        let queued_place = self
            .chain_queued_step(members)
            .and_then(|at| self.queue.get(at))
            .and_then(|item| item.saved.as_ref())
            .and_then(|saved| saved.anchor.added_step.as_ref())
            .map(|(_, place)| *place);
        if let Some(chain) = self.tasks.get(head) {
            for (place, step) in chain.added_steps.iter().enumerate() {
                if self.added_step_task(head, place).is_none()
                    && Some(place) != queued_place
                    && !step.prompt.trim().is_empty()
                {
                    let label = if step.mode == SendMode::Spec { "Added spec" } else { "Added code" };
                    let next = Self::follows_running(&rows, members, &self.tasks);
                    rows.push(StepRow::Next(label, step.mode, next));
                }
            }
        }
        rows
    }

    /// Whether a step not yet sent, after `rows`, follows the step running:
    /// the last sent is under way, and nothing waits between them.
    fn follows_running(rows: &[StepRow], members: &[usize], tasks: &[PromptTask]) -> bool {
        matches!(rows.last(), Some(StepRow::Sent(..)))
            && members
                .last()
                .and_then(|&ix| tasks.get(ix))
                .is_some_and(|task| task.status.is_active())
    }

    /// A running chain's own steps still to come: the one waiting in the
    /// queue, then those not yet sent, its code step and its spec follow-up
    /// when it asks for one, each "Next" while it follows the step running
    /// and "Waiting" after that.
    fn chain_pending_steps(&self, head: usize, members: &[usize]) -> Vec<StepRow> {
        let sent: Vec<StepRow> = members.iter().map(|&ix| StepRow::Sent(ix, StepKind::Spec)).collect();
        let mut rows = Vec::new();
        let post_build = self.tasks.get(head).is_some_and(|chain| chain.sent.post_build_update);
        let last = members.last().and_then(|&ix| self.tasks.get(ix));
        let own: Vec<(&'static str, SendMode)> = match self.chain_queued_step(members) {
            Some(at) => {
                rows.push(StepRow::Queued(at));
                // A code step queued with a post-build update has its
                // follow-up still to come.
                let item = &self.queue[at];
                let follow = item.mode == Some(SendMode::Code)
                    && item.saved.as_ref().map_or(post_build, |saved| saved.anchor.post_build_update);
                if follow { vec![("Spec follow-up", SendMode::Spec)] } else { Vec::new() }
            }
            None => match last.and_then(|last| chain_next(last)).map(|(_, sending)| sending) {
                Some(Sending::Now(SendMode::Code, _, _, _, _, follow, _)) => {
                    let mut own = vec![("Code", SendMode::Code)];
                    if follow {
                        own.push(("Spec follow-up", SendMode::Spec));
                    }
                    own
                }
                Some(Sending::Now(SendMode::Spec, ..)) => vec![("Spec follow-up", SendMode::Spec)],
                _ => Vec::new(),
            },
        };
        for (label, mode) in own {
            let mut all = sent.clone();
            all.extend(rows.iter().copied());
            let next = Self::follows_running(&all, members, &self.tasks);
            rows.push(StepRow::Next(label, mode, next));
        }
        rows
    }

    /// For a chain that is over without sending every step it might have,
    /// what was never sent, as its parent says it, muted.
    fn never_sent(&self, members: &[usize]) -> Option<&'static str> {
        let &last = members.last()?;
        let task = self.tasks.get(last)?;
        if task.status != TaskStatus::Done {
            return None;
        }
        let Some((_, next)) = chain_next(task) else {
            // Its own steps sent, a step added to it still to send.
            return self.next_added_step(last).map(|_| "added step never sent");
        };
        match next {
            Sending::Now(SendMode::Code, ..) => Some("code step never sent"),
            Sending::Now(SendMode::Spec, ..) => Some("spec follow-up never sent"),
            _ => Some("next step never sent"),
        }
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
        self.keep_open_details();
        self.tasks_tab.open = Some(open);
        // As it was left when last open, scrolled where it was and its
        // prompt clipped or whole; never being edited.
        let (scrolled, expanded) = self.tasks_tab.kept.get(&open).copied().unwrap_or_default();
        self.tasks_tab.details_scroll.set_offset(scrolled);
        self.tasks_tab.prompt_expanded = expanded;
        self.tasks_tab.prompt_editing = false;
        self.tasks_tab.prompt_height.set(px(0.));
        match open {
            Opened::Task(ix) => {
                // A previous task opened from elsewhere shows in its group.
                if self.tasks.get(ix).is_some_and(|task| !task.status.is_active()) {
                    self.tasks_tab.previous_expanded = true;
                }
                // Its output opens in the body: in Chat while Chat shows it,
                // else in a tab of its own.
                self.open_task(ix, cx);
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
        self.keep_open_details();
        self.tasks_tab.open = None;
        // Let go, it goes on as the queue does.
        self.auto_send_next(cx);
        cx.notify();
    }

    /// Keeps the open task's details as they are left, to show them so
    /// when it is opened again.
    pub(super) fn keep_open_details(&mut self) {
        if let Some(open) = self.tasks_tab.open {
            let kept = (self.tasks_tab.details_scroll.offset(), self.tasks_tab.prompt_expanded);
            self.tasks_tab.kept.insert(open, kept);
        }
    }

    /// Which row `row` is, whatever its place in the list.
    fn row_key(&self, row: &TimelineRow) -> RowKey {
        match row {
            TimelineRow::Heading(group, _) => RowKey::Heading(*group),
            TimelineRow::Day(label, _) => RowKey::Day(label.clone()),
            TimelineRow::Empty(id, _) => RowKey::Empty(id),
            TimelineRow::Chain(head, _) => RowKey::Chain(*head),
            TimelineRow::Task(ix, ..) => RowKey::Task(*ix),
            TimelineRow::Queued(at) => RowKey::Queued(self.queue.get(*at).map_or(usize::MAX, |item| item.id)),
            TimelineRow::Step(head, step) => match step {
                StepRow::Sent(ix, _) => RowKey::Step(*head, "sent", *ix),
                StepRow::Queued(at) => {
                    RowKey::Step(*head, "queued", self.queue.get(*at).map_or(usize::MAX, |item| item.id))
                }
                StepRow::Next(label, ..) => RowKey::Step(*head, label, 0),
            },
        }
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
        let mut label = count.to_string();
        if shown.is_some() {
            let showing = previous.iter().filter(|&&ix| visible(ix)).count();
            label.push_str(&format!(" · {showing} shown"));
        }
        rows.push(TimelineRow::Heading(Group::Previous, label));
        // Collapsed, its heading alone.
        if count > 0 && self.tasks_tab.previous_expanded {
            let layout = chain_layout(&self.tasks);
            // Each item shown, a chain by its first step, with how many
            // tasks it shows.
            let mut items: Vec<(TimelineRow, usize, usize)> = Vec::new();
            for place in 0..layout.order.len() {
                let ix = layout.order[place];
                match layout.steps[place] {
                    // A chain: one row; its steps show once it is opened.
                    // One running or queued is listed with those.
                    Some(step) if step.pos == 0 => {
                        let all = layout.members(step);
                        if self.chain_state(all) != ChainState::Over {
                            continue;
                        }
                        let shown = all
                            .iter()
                            .filter(|&&member| !self.tasks[member].status.is_active() && visible(member))
                            .count();
                        if shown > 0 {
                            items.push((TimelineRow::Chain(ix, all.to_vec()), ix, shown));
                        }
                    }
                    Some(_) => {}
                    None => {
                        if !self.tasks[ix].status.is_active() && visible(ix) {
                            items.push((TimelineRow::Task(ix, None, true), ix, 1));
                        }
                    }
                }
            }
            if items.is_empty() {
                rows.push(TimelineRow::Empty(
                    "no-filtered-tasks",
                    "No previous tasks match these filters",
                ));
            }
            // Under the day each was sent, a chain its first step's, each
            // day headed by its divider, counting what it shows.
            let today = chrono::Local::now().date_naive();
            let mut day: Option<(chrono::NaiveDate, usize, usize)> = None;
            for (row, ix, shown) in items {
                let date = self.sent_on(ix).unwrap_or(today);
                match &mut day {
                    Some((on, _, n)) if *on == date => *n += shown,
                    _ => {
                        if let Some((on, at, n)) = day.take() {
                            rows[at] = TimelineRow::Day(day_label(on, today), n);
                        }
                        day = Some((date, rows.len(), shown));
                        rows.push(TimelineRow::Day(String::new(), 0));
                    }
                }
                rows.push(row);
            }
            if let Some((on, at, n)) = day {
                rows[at] = TimelineRow::Day(day_label(on, today), n);
            }
        }
        // Running, in either lane: every task under way, a chain still in
        // progress as one row, all its steps, done and to come, beneath it.
        let chains = self.chains_where(|this, members| this.chain_state(members) == ChainState::Running);
        let running: Vec<usize> = (0..self.tasks.len())
            .filter(|&ix| {
                self.tasks[ix].status.is_active()
                    && !chains.iter().any(|(_, members)| members.contains(&ix))
            })
            .collect();
        let running_at = rows.len();
        rows.push(TimelineRow::Heading(
            Group::Running,
            (running.len() + chains.len()).to_string(),
        ));
        let running_shown = self.tasks_tab.running_expanded;
        if running_shown && running.is_empty() && chains.is_empty() {
            rows.push(TimelineRow::Empty("tasks-running-empty", "Nothing running"));
        }
        // In the order they were sent, a chain by its first step.
        let mut items: Vec<(usize, Option<Vec<usize>>)> = running.into_iter().map(|ix| (ix, None)).collect();
        items.extend(chains.into_iter().map(|(head, members)| (head, Some(members))));
        items.sort_by_key(|(ix, _)| *ix);
        // A running chain is expanded: its row, then each of its steps.
        let mut held_in_chains = Vec::new();
        for (ix, members) in items {
            let Some(members) = members else {
                if running_shown {
                    rows.push(TimelineRow::Task(ix, None, false));
                }
                continue;
            };
            let steps = self.chain_step_rows(ix, &members);
            held_in_chains.extend(steps.iter().filter_map(|step| match step {
                StepRow::Queued(at) => Some(*at),
                _ => None,
            }));
            if running_shown {
                rows.push(TimelineRow::Chain(ix, members));
                rows.extend(steps.into_iter().map(|step| TimelineRow::Step(ix, step)));
            }
        }
        // Queued, in its order, reordered by dragging.
        // Queued, in its order, reordered by dragging; a running chain's
        // step is listed with its chain, though it keeps its place.
        let listed: Vec<usize> = (0..self.queue.len()).filter(|at| !held_in_chains.contains(at)).collect();
        let queued_at = rows.len();
        rows.push(TimelineRow::Heading(
            Group::Queued,
            listed.len().to_string(),
        ));
        let queued_shown = self.tasks_tab.queued_expanded;
        if queued_shown && listed.is_empty() {
            rows.push(TimelineRow::Empty("tasks-queued-empty", "No queued prompts"));
        }
        let first_queued = rows.len();
        if queued_shown {
            rows.extend(listed.into_iter().map(TimelineRow::Queued));
        }
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
        // Not scrolled since the last frame, it stays on the row it was on
        // at the top of the view, however rows above it came and went;
        // left at its top, it stays there.
        let left_at = self.tasks_tab.left_at.get();
        if let Some((left, bottom)) = left_at
            && !bottom
            && left < px(0.)
            && scroll.offset().y == left
            && let Some((key, into)) = self.tasks_tab.anchor.borrow().clone()
            && let Some(row) = rows.iter().position(|row| self.row_key(row) == key)
        {
            let kept = ROW_HEIGHT * row as f32 + into;
            if -kept != left {
                scroll.set_offset(point(scroll.offset().x, -kept));
            }
        }
        // A task opened from elsewhere: its row brought into view, beneath
        // the headings pinned above it.
        if let Some(ix) = self.tasks_tab.reveal.take()
            && let Some(row) = rows.iter().position(|row| match row {
                TimelineRow::Task(at, ..) => *at == ix,
                TimelineRow::Chain(_, all) => all.contains(&ix),
                _ => false,
            })
        {
            let view = match scroll.bounds().size.height {
                height if height > px(0.) => height,
                _ => window.viewport_size().height,
            };
            let place = ROW_HEIGHT * row as f32;
            let top = -scroll.offset().y;
            let headings = rows.iter().filter(|row| matches!(row, TimelineRow::Heading(..))).count();
            let room = ROW_HEIGHT * headings as f32;
            if place < top + room || place + ROW_HEIGHT > top + view - room {
                let to = (place - room).max(px(0.));
                scroll.set_offset(point(px(0.), -to));
                self.tasks_tab.left_at.set(None);
            }
        }
        // Until the list has been laid out once, the window's height.
        let height = match scroll.bounds().size.height {
            height if height > px(0.) => height,
            _ => window.viewport_size().height,
        };
        // Clamped to its ends, in whole device pixels, before anything is
        // drawn, so nothing is ever drawn past an end and put back after.
        // Its rows and its bottom padding, in the area beneath the tab row
        // it is drawn in.
        let content = ROW_HEIGHT * rows.len() as f32 + BOTTOM_PADDING;
        let laid_out_bottom = scroll.max_offset().y > px(0.)
            && scroll.offset().y <= -scroll.max_offset().y + px(0.5);
        let top = clamped_top(
            scroll.offset().y,
            content,
            height,
            window.scale_factor(),
            self.tasks_tab.left_at.get(),
            laid_out_bottom,
        );
        if scroll.offset().y != -top.0 {
            scroll.set_offset(point(scroll.offset().x, -top.0));
        }
        // At its bottom, it is laid out at its bottom in this frame, however
        // the tab was resized since the last, so its last row shows whole;
        // enough rows drawn for whatever height it is given.
        let at_bottom = top.1;
        if at_bottom {
            scroll.scroll_to_bottom();
        }
        self.tasks_tab.left_at.set(Some((-top.0, top.1)));
        // The row now at the top of the view, to keep the scroll on.
        let top_row = ((top.0 / ROW_HEIGHT).floor() as usize).min(rows.len().saturating_sub(1));
        *self.tasks_tab.anchor.borrow_mut() = rows
            .get(top_row)
            .map(|row| (self.row_key(row), top.0 - ROW_HEIGHT * top_row as f32));
        let at_end = top.2;
        let top = top.0;
        let drawn_from = if at_bottom {
            (content - height.max(window.viewport_size().height)).max(px(0.))
        } else {
            top
        };
        let first = ((drawn_from / ROW_HEIGHT).floor() as usize).saturating_sub(OVERSCAN_ROWS);
        let last = (((top + height) / ROW_HEIGHT).ceil() as usize + OVERSCAN_ROWS).min(rows.len());
        let first = first.min(last);

        // Every heading always in view: one whose place has left the
        // visible rows above, beneath the headings stacked above it, is
        // pinned at the top, in order; one whose place lies below the
        // visible rows, above those stacked beneath it, at the bottom. None
        // pushes another out.
        let headings: Vec<usize> = (0..rows.len())
            .filter(|&row| matches!(rows.get(row), Some(TimelineRow::Heading(..))))
            .collect();
        let n = headings.len();
        let place = |row: usize| ROW_HEIGHT * row as f32;
        let mut pins: Vec<(usize, Pixels)> = Vec::new();
        for (k, &row) in headings.iter().enumerate() {
            let at_top = top + ROW_HEIGHT * k as f32;
            let at_bottom = top + height - ROW_HEIGHT * (n - k) as f32;
            if place(row) < at_top {
                pins.push((row, at_top));
            } else if place(row) > at_bottom {
                pins.push((row, at_bottom));
            }
        }
        let pinned_at = |row: usize| pins.iter().find(|(pinned, _)| *pinned == row).map(|(_, y)| *y);
        // The day of the Previous rows in view, pinned just below Previous's
        // heading while Previous's rows are what is in view beneath it,
        // until the next day's divider, or the next heading, pushes it out.
        let boundary = |row: usize| {
            matches!(rows.get(row), Some(TimelineRow::Heading(..) | TimelineRow::Day(..)))
        };
        let below_previous = top + ROW_HEIGHT;
        let pinned_day = headings
            .first()
            .filter(|&&row| matches!(rows.get(row), Some(TimelineRow::Heading(Group::Previous, _))))
            .filter(|&&row| pinned_at(row).is_some_and(|y| y == top))
            .and_then(|&heading| {
                (heading + 1..rows.len())
                    .take_while(|&row| place(row) < below_previous)
                    .filter(|&row| matches!(rows.get(row), Some(TimelineRow::Day(..))))
                    .last()
            })
            .map(|row| {
                let next = (row + 1..rows.len()).find(|&next| boundary(next));
                let pushed = next.map_or(px(0.), |next| {
                    (place(next) - below_previous - ROW_HEIGHT).min(px(0.))
                });
                (row, pushed)
            })
            .filter(|(_, pushed)| *pushed > -ROW_HEIGHT);
        self.follow_midnight(cx);
        let queued: Vec<usize> = rows[first..last]
            .iter()
            .filter_map(|row| match row {
                TimelineRow::Queued(at) => Some(*at),
                _ => None,
            })
            .collect();
        let mut queued_rows = self.render_queued_rows(&queued, cx).into_iter();
        let mut drawn: Vec<AnyElement> = Vec::with_capacity(last - first + 2);
        drawn.push(div().flex_none().h(ROW_HEIGHT * first as f32).into_any_element());
        let mut pinned_headings: Vec<(Pixels, AnyElement)> = Vec::new();
        let mut pinned_divider = None;
        for (at, row) in rows[first..last].iter().enumerate() {
            drawn.push(match row {
                TimelineRow::Day(label, count) if pinned_day.is_some_and(|(row, _)| row == first + at) => {
                    pinned_divider = Some(day_divider(label.clone(), *count, cx));
                    div().flex_none().h(ROW_HEIGHT).into_any_element()
                }
                TimelineRow::Day(label, count) => day_divider(label.clone(), *count, cx),
                // Pinned, it is drawn there alone, an empty row in its place.
                TimelineRow::Heading(..) if pinned_at(first + at).is_some() => {
                    div().flex_none().h(ROW_HEIGHT).into_any_element()
                }
                TimelineRow::Heading(group, label) => {
                    self.render_section_heading(*group, label.clone(), None, cx)
                }
                TimelineRow::Empty(id, text) => group_empty(id, text, cx),
                TimelineRow::Chain(head, all) => {
                    // Its steps beneath it, the bar runs on into them.
                    let steps_follow = matches!(rows.get(first + at + 1), Some(TimelineRow::Step(h, _)) if h == head);
                    self.render_chain_row(*head, all, steps_follow, cx)
                }
                TimelineRow::Task(ix, kind, previous) => {
                    self.render_task_row(*ix, *kind, *previous, cx)
                }
                TimelineRow::Step(head, step) => {
                    let last_step = !matches!(rows.get(first + at + 1), Some(TimelineRow::Step(h, _)) if h == head);
                    self.render_step_row(*head, *step, Some((false, last_step)), cx)
                }
                TimelineRow::Queued(_) => match queued_rows.next() {
                    Some(row) => row,
                    None => div().into_any_element(),
                },
            });
        }
        drawn.push(
            div()
                .flex_none()
                .h(ROW_HEIGHT * (rows.len() - last) as f32 + BOTTOM_PADDING)
                .into_any_element(),
        );
        // Pinned, each heading is drawn there alone, whether or not its row
        // is among those drawn, its place in the list kept empty.
        for &(row, y) in &pins {
            if let Some(TimelineRow::Heading(group, label)) = rows.get(row) {
                let section_start = place(row)
                    - ROW_HEIGHT * headings.iter().position(|&h| h == row).unwrap_or(0) as f32;
                pinned_headings.push((
                    y,
                    self.render_section_heading(*group, label.clone(), Some(section_start), cx),
                ));
            }
        }
        let listed: Rc<Vec<usize>> = Rc::new(
            rows.iter()
                .filter_map(|row| match row {
                    TimelineRow::Queued(at) => Some(*at),
                    _ => None,
                })
                .collect(),
        );
        let drag_scroll = self.queue_drag_driver(first_queued, listed, cx);
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
            .child(
                // Scrolling further at an end goes no further, nor is anything
                // drawn anew: the list never takes it.
                v_flex()
                    .w_full()
                    .flex_none()
                    .on_scroll_wheel(move |event, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height()).y;
                        let (at_top, at_bottom) = at_end;
                        if (delta > px(0.) && at_top) || (delta < px(0.) && at_bottom) {
                            cx.stop_propagation();
                        }
                    })
                    .relative()
                    .children(rows)
                    // Over the rows scrolling beneath it, opaque, moving none.
                    .children(pinned_day.zip(pinned_divider).map(|((_, pushed), divider)| {
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(top + ROW_HEIGHT + pushed)
                            .child(divider)
                    }))
                    .children(pinned_headings.into_iter().map(|(y, heading)| {
                        div().absolute().left_0().right_0().top(y).child(heading)
                    })),
            );
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

    /// A section's heading, with its own controls, wherever it is pinned:
    /// Previous's selection, batch actions, and filters, Running's "Cancel
    /// all" while more than one task runs, Queued's "Send next" and
    /// auto-send switch. Its chevron expands or collapses that section
    /// alone; a click elsewhere on it does too in its place, and, pinned,
    /// scrolls to `pinned`, where its section starts.
    fn render_section_heading(
        &self,
        group: Group,
        count: String,
        pinned: Option<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, title, expanded, actions) = match group {
            Group::Previous => {
                let mut actions = self.render_selection_actions(cx);
                // The filters' button, while there is a task to filter.
                let filter = (!self.previous_ixs().is_empty()).then(|| self.render_filter_button(cx));
                // Where even the title and count giving way leaves too
                // little room, as they were last laid out, the batch
                // actions fold into an "N selected" menu.
                let room = self.tasks_tab.scroll.bounds().size.width
                    - HEADING_FIXED
                    - if filter.is_some() { FILTER_WIDTH } else { px(0.) };
                let fold = actions.len() > 1
                    && self.tasks_tab.batch_width.get() > px(0.)
                    && self.tasks_tab.batch_width.get() > room;
                if fold {
                    actions = vec![self.render_selection_menu(cx)];
                } else if !actions.is_empty() {
                    let width = self.tasks_tab.batch_width.clone();
                    let all = h_flex()
                        .flex_none()
                        .gap_2()
                        .on_prepaint(move |bounds, _, _| width.set(bounds.size.width))
                        .children(std::mem::take(&mut actions));
                    actions = vec![all.into_any_element()];
                }
                actions.extend(filter);
                ("tasks-previous", "Previous", self.tasks_tab.previous_expanded, actions)
            }
            Group::Running => ("tasks-running", "Running", self.tasks_tab.running_expanded, self.running_controls(cx)),
            Group::Queued => ("tasks-queued", "Queued", self.tasks_tab.queued_expanded, self.queue_controls(cx)),
        };
        // Pinned twice over, at the top and bottom, ids stay apart.
        let id = match pinned {
            Some(_) => match group {
                Group::Previous => "tasks-previous-pinned",
                Group::Running => "tasks-running-pinned",
                Group::Queued => "tasks-queued-pinned",
            },
            None => id,
        };
        let toggle = {
            let this = cx.entity().downgrade();
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |this, cx| {
                    let open = match group {
                        Group::Previous => &mut this.tasks_tab.previous_expanded,
                        Group::Running => &mut this.tasks_tab.running_expanded,
                        Group::Queued => &mut this.tasks_tab.queued_expanded,
                    };
                    *open = !*open;
                    cx.notify();
                })
                .ok();
            }
        };
        let clicked: Box<dyn Fn(&mut Window, &mut App)> = match pinned {
            Some(start) => {
                let this = cx.entity().downgrade();
                Box::new(move |_, cx| {
                    this.update(cx, |this, cx| {
                        let scroll = &this.tasks_tab.scroll;
                        scroll.set_offset(point(scroll.offset().x, -start.max(px(0.))));
                        this.tasks_tab.left_at.set(None);
                        cx.notify();
                    })
                    .ok();
                })
            }
            None => Box::new(toggle.clone()),
        };
        section_heading(id, title, count, expanded, actions, toggle, clicked, cx)
    }

    /// Along the Running heading: "Cancel all", while more than one task
    /// runs.
    fn running_controls(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let running = self.tasks.iter().filter(|task| task.can_cancel()).count();
        if running < 2 {
            return Vec::new();
        }
        vec![Button::new("cancel-all-running")
            .ghost()
            .xsmall()
            .label("Cancel all")
            .tooltip(format!("Cancel all {running} running tasks"))
            .on_click(cx.listener(|this, _, _, cx| {
                let running: Vec<usize> = (0..this.tasks.len())
                    .filter(|&ix| this.tasks[ix].can_cancel())
                    .collect();
                for ix in running {
                    this.cancel_task(ix, cx);
                }
            }))
            .into_any_element()]
    }

    /// The batch actions folded into one "N selected" menu, as a narrow
    /// Previous heading shows them.
    fn render_selection_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.task_history.selected.iter().filter(|&&ix| ix < self.tasks.len()).count();
        let sends: Vec<(SendMode, usize)> = [SendMode::Spec, SendMode::Code]
            .into_iter()
            .map(|to| (to, self.selected_for(to).len()))
            .filter(|(_, n)| *n > 0)
            .collect();
        let this = cx.entity().downgrade();
        Button::new("history-selection-menu")
            .ghost()
            .xsmall()
            .label(format!("{count} selected"))
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, _| {
                for &(to, n) in &sends {
                    let this = this.clone();
                    menu = menu.item(PopupMenuItem::new(format!("Send {n} to {}", to.label())).on_click(
                        move |_, window, cx| {
                            this.update(cx, |this, cx| this.send_selected_to(to, window, cx)).ok();
                        },
                    ));
                }
                let this = this.clone();
                menu.item(PopupMenuItem::new("Clear").on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| {
                        this.task_history.clear_selection();
                        cx.notify();
                    })
                    .ok();
                }))
            })
            .into_any_element()
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
            // A chain's step has no bar of its own: the chain's runs by it.
            .when(step.is_none(), |row| row.child(row_bar(task.mode, cx)))
            .child(column(LEAD_COLUMN, checkbox))
            .child(column(STATUS_COLUMN, Some(status_mark(task.status, cx))))
            .children(label)
            .child(div().flex_1().min_w_0().truncate().child(first_line(&task.text)))
            // How fast it is writing, beside its time, while it runs.
            .children(
                rate_label(("tasks-tab-rate", ix), task, None, cx).filter(|_| task.status.is_active()),
            )
            .children(
                task.took()
                    .map(|took| took_label(("tasks-tab-took", ix), took, cx)),
            )
            .children(
                (previous && task.new_conversation)
                    .then(|| new_conversation_mark(("tasks-tab-new-conversation", ix), cx)),
            );
        // Lets UI tests find the row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }

    /// A chain's row: a chevron that shows or hides its steps, its status as
    /// a whole, its prompt's first line, how many steps it has, and how long
    /// they took together. Clicking it opens the chain.
    fn render_chain_row(
        &self,
        head: usize,
        members: &[usize],
        steps_follow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let steps: Vec<&PromptTask> = members.iter().filter_map(|&ix| self.tasks.get(ix)).collect();
        let state = self.chain_state(members);
        let never_sent = (state == ChainState::Over).then(|| self.never_sent(members)).flatten();
        let open = self.tasks_tab.open == Some(Opened::Task(head));
        let text = first_line(&self.tasks[head].text);
        let tip = queued_tooltip(&self.tasks[head].text);
        // Its own steps and those added, sent or still to come.
        let unsent = (0..self.tasks[head].added_steps.len())
            .filter(|&place| self.added_step_task(head, place).is_none())
            .count();
        let count = steps.len() + unsent;
        // Among the previous tasks, its checkbox selects its steps for the
        // batch actions.
        let checkbox = (state == ChainState::Over).then(|| {
            let members: Vec<usize> = members.to_vec();
            let selected = members.iter().all(|ix| self.task_history.selected.contains(ix));
            let this = cx.entity().downgrade();
            history_checkbox(head, selected, move |_, cx| {
                let members = members.clone();
                this.update(cx, |this, cx| {
                    let history = &mut this.task_history;
                    if selected {
                        for ix in &members {
                            history.selected.remove(ix);
                        }
                    } else {
                        history.selected.extend(members.iter().copied());
                    }
                    history.last_selected = Some(head);
                    cx.notify();
                })
                .ok();
            })
        });
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
            // The chain's bar, running on into its steps beneath it.
            .child(chain_bar(true, !steps_follow, cx))
            .child(column(LEAD_COLUMN, checkbox))
            .child(column(
                STATUS_COLUMN,
                Some(match state {
                    // Under way, though its step now is queued.
                    ChainState::Running => status_mark(TaskStatus::Running, cx),
                    ChainState::Over => status_mark(chain_status(&steps, never_sent.is_some()), cx),
                }),
            ))
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
            .children(never_sent.map(|what| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(what)
            }))
            .children(
                chain_took(&steps, state == ChainState::Running)
                    .map(|took| took_label(("tasks-tab-chain-took", head), took, cx)),
            )
            .children(
                self.tasks[head]
                    .new_conversation
                    .then(|| new_conversation_mark(("tasks-tab-chain-new-conversation", head), cx)),
            );
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }

    /// A running chain's step's row, beneath the chain's: in the same
    /// columns, its text set in further, labelled by what it does in that
    /// step's colour, then its status, its own mark or spinner, "Queued"
    /// with its place, or "Next", and its time. Clicking it opens the chain.
    fn render_step_row(
        &self,
        head: usize,
        step: StepRow,
        bar: Option<(bool, bool)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (id, label, mode, status, text, note, took) = match step {
            StepRow::Sent(ix, kind) => {
                let task = &self.tasks[ix];
                let (label, mode) = kind.label();
                (
                    ix,
                    label,
                    mode,
                    Some(status_mark(task.status, cx)),
                    first_line(&task.text),
                    None,
                    task.took().map(|took| {
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .children(
                                rate_label(("tasks-tab-step-rate", ix), task, None, cx)
                                    .filter(|_| task.status.is_active()),
                            )
                            .child(took_label(("tasks-tab-step-took", ix), took, cx))
                            .into_any_element()
                    }),
                )
            }
            StepRow::Queued(at) => {
                let item = &self.queue[at];
                let (label, mode) = self.queued_label(item).unwrap_or(("Step", SendMode::Code));
                let place = div().text_xs().text_color(muted).child(format!("{}.", at + 1));
                (
                    self.tasks.len() + at,
                    label,
                    mode,
                    Some(place.into_any_element()),
                    first_line(&item.text),
                    Some(format!("Queued, {} in its lane's queue", ordinal(at + 1))),
                    None,
                )
            }
            StepRow::Next(label, mode, next) => (
                usize::MAX / 2 + head * 64 + label.len() * 2 + usize::from(next),
                label,
                mode,
                None,
                first_line(&self.tasks[head].text),
                Some(if next { "Next" } else { "Waiting" }.to_string()),
                None,
            ),
        };
        let open = self.tasks_tab.open == Some(Opened::Task(head));
        let row = h_flex()
            .id(("tasks-tab-step", id))
            .relative()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .gap_2()
            .pl(STEP_INDENT + px(10.))
            .pr_3()
            .text_sm()
            .cursor_pointer()
            .hover(|row| row.bg(theme.list_hover))
            .when(open, |row| row.bg(theme.list_active))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_in_tab(Opened::Task(head), window, cx)
            }))
            // Its part of the chain's bar, its mode in its label's colour.
            .children(bar.map(|(first, last)| chain_bar(first, last, cx)))
            .child(column(LEAD_COLUMN, None))
            .child(column(STATUS_COLUMN, status))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_semibold()
                    .text_color(chat_input::mode_color(mode, cx))
                    .child(label),
            )
            .child(div().flex_1().min_w_0().truncate().child(text))
            .children(note.map(|note| div().flex_none().text_xs().text_color(muted).child(note)))
            .children(took);
        gpui_kit::TestSupportExt::test_support(row).into_any_element()
    }

    /// The queued prompts' rows, in the queue's order: each its grip to drag
    /// it by, its place, its prompt's first line, whether it starts a new
    /// conversation, and buttons to edit it in the chat input or cancel it.
    /// Clicking one opens it here.
    fn render_queued_rows(&self, shown: &[usize], cx: &mut Context<Self>) -> Vec<AnyElement> {
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
            .filter(|(ix, _)| shown.contains(ix))
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
                    .child(row_bar(item.mode, cx))
                    // Dragged by its grip, which never opens it.
                    .child(
                        div()
                            .id(("queued-grip-box", ix))
                            .flex_none()
                            .w(LEAD_COLUMN)
                            .flex()
                            .justify_center()
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
                    .child(column(
                        STATUS_COLUMN,
                        Some(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{}.", ix + 1))
                                .into_any_element(),
                        ),
                    ))
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
    fn queue_drag_driver(&self, first: usize, listed: Rc<Vec<usize>>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let (queue_gap, saved) = (self.queue_gap.clone(), self.queue_saved_marks());
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
                        (0..listed.len())
                            .find_map(|pos| drag_gap(listed[pos], item(first + pos)?, at, true))
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
                            .overflow_y_scroll()
                            .track_scroll(&self.tasks_tab.details_scroll),
                    )
                    .children(body),
                ),
            )
            .into_any_element()
    }

    /// The Prompt section holding `prompt`, where it was drawn kept, to
    /// scroll back to it.
    fn prompt_section(&self, prompt: AnyElement, cx: &mut Context<Self>) -> Div {
        let top = self.tasks_tab.prompt_top.clone();
        section("Prompt", cx).child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| top.set(Some(bounds.origin.y)))
                .child(prompt),
        )
    }

    /// Whether the open task's prompt, laid out whole, runs past its 8
    /// lines.
    fn prompt_is_long(&self) -> bool {
        self.tasks_tab.prompt_height.get() > PROMPT_LINE * CLIPPED_LINES + px(0.5)
    }

    /// An open task's prompt as typed: past 8 lines, as they wrap, its first
    /// 8, the last fading out, with "Read more" beneath, which shows it
    /// whole, then reads "Show less". For a queued task, `editing` it, a
    /// click on the text shows it whole in its editor.
    fn clipped_prompt(&self, text: SharedString, editing: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (accent, background, border) = (theme.accent, theme.background, theme.border);
        let long = self.prompt_is_long();
        let expanded = self.tasks_tab.prompt_expanded;
        let clipped = long && !expanded;
        let height = self.tasks_tab.prompt_height.clone();
        // While clipped, only its first lines are laid out, however long it
        // is: one more than it shows, each no longer than can fill them,
        // enough to tell that it runs past them.
        let text = if expanded { text } else { first_lines(&text) };
        // Laid out, its height measured, then clipped to 8 lines.
        let whole = div()
            .w_full()
            .line_height(PROMPT_LINE)
            .whitespace_normal()
            .on_prepaint(move |bounds, window, _| {
                if height.get() != bounds.size.height {
                    height.set(bounds.size.height);
                    window.refresh();
                }
            })
            .child(text);
        let shown = gpui_kit::TestSupportExt::test_support(div().id("tasks-tab-prompt"))
            .relative()
            .w_full()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(background)
            .text_sm()
            .child(
                div()
                    .w_full()
                    .when(clipped, |text| text.max_h(PROMPT_LINE * CLIPPED_LINES).overflow_hidden())
                    .child(whole),
            )
            // The last line shown fades into the background.
            .when(clipped, |shown| {
                shown.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(8.))
                        .h(PROMPT_LINE)
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(background.opacity(0.), 0.),
                            linear_color_stop(background, 1.),
                        )),
                )
            })
            .when_some(editing, |shown, _| {
                shown.cursor_text().on_click(cx.listener(|this, _, window, cx| {
                    this.tasks_tab.prompt_editing = true;
                    if let Some(editor) = &this.tasks_tab.editor {
                        editor.input.update(cx, |input, cx| input.focus(window, cx));
                    }
                    cx.notify();
                }))
            });
        let link = long.then(|| {
            gpui_kit::TestSupportExt::test_support(h_flex().id("tasks-tab-read-more"))
                .gap_1()
                .text_xs()
                .text_color(accent)
                .cursor_pointer()
                .child(if expanded { "Show less" } else { "Read more" })
                .child(
                    Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown })
                        .xsmall()
                        .text_color(accent),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_prompt_clip(cx)))
        });
        v_flex().w_full().gap_1().child(shown).children(link).into_any_element()
    }

    /// Shows the open task's prompt whole, or clips it again, scrolling the
    /// details back to the Prompt section if they had scrolled past it.
    fn toggle_prompt_clip(&mut self, cx: &mut Context<Self>) {
        self.tasks_tab.prompt_expanded = !self.tasks_tab.prompt_expanded;
        if !self.tasks_tab.prompt_expanded {
            let scroll = &self.tasks_tab.details_scroll;
            if let Some(top) = self.tasks_tab.prompt_top.get() {
                let view_top = scroll.bounds().origin.y;
                if top < view_top {
                    let offset = scroll.offset();
                    scroll.set_offset(point(offset.x, (offset.y + (view_top - top)).min(px(0.))));
                }
            }
        }
        cx.notify();
    }

    /// An open task that has run, or is running: as it was sent, its
    /// figures, and what can be done with it.
    fn render_sent_details(&self, ix: usize, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let task = &self.tasks[ix];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Its status and how long it took, on one line, first.
        let status = section("Status", cx).child(
            h_flex()
                .gap_2()
                .child(task.status.tag(cx))
                .children(task.took().map(|took| took_label(("tasks-tab-status-took", ix), took, cx))),
        );
        let mut glance = section("Details", cx);
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
        let prompt = self
            .prompt_section(self.clipped_prompt(task.text.clone(), None, cx), cx)
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
        if task.can_pause() {
            actions = actions.child(
                Button::new(("pause-previous", ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Pause)
                    .label("Pause")
                    .tooltip("Pause this task")
                    .on_click(cx.listener(move |this, _, _, cx| this.pause_task(ix, cx))),
            );
        }
        if task.status == TaskStatus::Paused {
            actions = actions.child(
                Button::new(("resume-previous", ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Play)
                    .label("Resume")
                    .tooltip("Carry on this task from where it left off")
                    .on_click(cx.listener(move |this, _, _, cx| this.resume_task(ix, cx))),
            );
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
                    .tooltip("Show its output in the body")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_task(ix, cx);
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
            )
            // What the harness was given, exactly, once it has compiled.
            .child(raw_prompt_button(
                ("raw-prompt-latest", ix),
                task.compiled.is_some(),
                cx.listener(move |this, _, window, cx| this.open_raw_prompt(ix, window, cx)),
            ));
        // Status, its prompt, a chain's steps, then the rest.
        let mut sections = vec![status.into_any_element(), prompt.into_any_element()];
        if task.sent.mode == Some(SendMode::Both) {
            sections.push(self.render_chain_steps(ix, cx));
        }
        // Where its time went, once it has started.
        sections.extend(self.render_time_section(ix, cx));
        sections.extend([
            glance.into_any_element(),
            parameters.into_any_element(),
            section("Actions", cx).child(actions).into_any_element(),
        ]);
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
        let status = section("Status", cx).child(
            div().text_sm().child(if self.tasks_tab.drafts.contains(&id) {
                "Queued, waiting for Send"
            } else if saving {
                "Saving…"
            } else {
                "Queued, held while open"
            }),
        );
        let mut glance = section("Details", cx)
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
        // Shown as text, clipped while long, until clicked into to be
        // edited, shown whole in its editor.
        let text = fields.as_ref().map(|fields| fields.text.clone()).unwrap_or_default();
        let editor = match editor {
            Some(editor) if !self.tasks_tab.prompt_editing => {
                let _ = editor;
                Some(self.clipped_prompt(text.into(), Some(id), cx))
            }
            editor => editor,
        };
        let mut prompt = self.prompt_section(div().w_full().children(editor).into_any_element(), cx);
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
        // Status, its prompt, a chain's steps, then the rest.
        let mut sections = vec![status.into_any_element(), prompt.into_any_element()];
        if mode == Some(SendMode::Both) {
            sections.push(self.render_queued_chain_note(cx));
        }
        sections.extend([
            glance.into_any_element(),
            parameters.into_any_element(),
            section("Actions", cx).child(actions).into_any_element(),
        ]);
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
    pub(super) fn chain_step_ixs(&self, head: usize) -> Vec<(usize, StepKind)> {
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
        // Its rows, one bar in the Chain colour running down all of them,
        // across the gaps between, however many there are.
        let mut steps = v_flex().relative().w_full().gap_1().child(chain_bar(true, true, cx));
        let sent = self.chain_step_ixs(head);
        for &(ix, kind) in &sent {
            steps = steps.child(self.render_task_row(ix, Some(kind), false, cx));
        }
        // Running, every step it will run is listed from the moment it is
        // sent: its own still to come, each changing in place as it is sent.
        let members: Vec<usize> = sent.iter().map(|(ix, _)| *ix).collect();
        let running = self.chain_state(&members) == ChainState::Running;
        let pending = if running { self.chain_pending_steps(head, &members) } else { Vec::new() };
        // The first added step still to send follows the step running only
        // while none of the chain's own waits between them.
        let added_follows = running
            && pending.is_empty()
            && Self::follows_running(
                &members.iter().map(|&ix| StepRow::Sent(ix, StepKind::Spec)).collect::<Vec<_>>(),
                &members,
                &self.tasks,
            );
        for step in pending {
            steps = steps.child(self.render_step_row(head, step, None, cx));
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
                        // While the chain runs, whether it follows the step
                        // running or waits behind another.
                        .when(running, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(if nth == 0 && added_follows { "Next" } else { "Waiting" }),
                            )
                        })
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
        // The bar runs down the steps alone, never past them to Add step.
        section("Steps", cx)
            .child(steps)
            .child(div().pt_1().child(add))
            .into_any_element()
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

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::clamped_top;

    /// The list is clamped to its ends in whole device pixels, scrolls only
    /// once it overflows by a whole pixel, and stays at its bottom as it
    /// grows once left there.
    /// A day reads "Today", "Yesterday", its weekday within the past week,
    /// else its date, with the year once it isn't this one.
    #[test]
    fn days_are_labelled_from_today() {
        use super::day_label;
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let today = day(2026, 10, 11);
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(day(2026, 10, 10), today), "Yesterday");
        assert_eq!(day_label(day(2026, 10, 6), today), "Tuesday");
        assert_eq!(day_label(day(2026, 10, 3), today), "3 October");
        assert_eq!(day_label(day(2025, 9, 28), today), "28 September 2025");
    }

    #[test]
    fn the_list_stays_within_its_ends() {
        // Past the top, or past the bottom, it is at that end.
        assert_eq!(clamped_top(px(40.), px(280.), px(100.), 2., None, false), (px(0.), false, (true, false)));
        assert_eq!(clamped_top(px(-500.), px(280.), px(100.), 2., None, false), (px(180.), true, (false, true)));
        // Whole device pixels: halves at a scale of 2, wholes at 1.
        assert_eq!(clamped_top(px(-10.3), px(280.), px(100.), 2., None, false).0, px(10.5));
        assert_eq!(clamped_top(px(-10.3), px(280.), px(100.), 1., None, false).0, px(10.));
        // An end short of a whole device pixel stops at the pixel before.
        assert_eq!(clamped_top(px(-500.), px(280.4), px(100.), 1., None, false).0, px(180.));
        // Overflowing by less than a pixel, it doesn't scroll at all.
        assert_eq!(clamped_top(px(-0.6), px(100.6), px(100.), 2., None, false), (px(0.), false, (true, true)));
        // Left at its bottom, it stays there as rows are added; scrolled
        // since, it stays where it was scrolled to.
        let left = Some((px(-180.), true));
        assert_eq!(clamped_top(px(-180.), px(336.), px(100.), 2., left, false).0, px(236.));
        assert_eq!(clamped_top(px(-150.), px(336.), px(100.), 2., left, false).0, px(150.));
        // At its top, it stays at its top.
        assert_eq!(clamped_top(px(0.), px(336.), px(100.), 2., Some((px(0.), false)), false).0, px(0.));
    }
}
