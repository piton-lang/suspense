//! The Run panel: finds how the project is run, with the harness, saving the
//! targets it finds, or runs one of them, streaming what it prints. It holds
//! one of the two at a time, and can be minimized while either goes on.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::console_text::ConsoleLine;
use crate::divergence::{self, Cancel, RunAgent};
use crate::harness::{self, HarnessEvent};
use crate::measured_list::{MeasuredList, RenderRow};
use crate::rescope_view::StepState;
use crate::run_targets::{self, Running, Target};
use crate::scrollbar::{self, SetLock};
use crate::task_table::{self, Reply, TableView, TaskTable};

/// Emitted when the panel is closed.
pub struct CloseRun;

/// Emitted to hide the panel while what it holds goes on.
pub struct MinimizeRun;

/// Emitted with the targets found, once saved.
pub struct TargetsFound(pub Vec<Target>);

/// How often a running target's output is gathered.
pub(crate) const POLL: std::time::Duration = std::time::Duration::from_millis(40);

/// Where the harness's output table numbers from, apart from others'.
const OUTPUT_IX: usize = usize::MAX / 32;

/// How a target's run is going.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunStatus {
    Running,
    /// Being stopped: asked to end, everything it started given time to,
    /// and killed after; not yet stopped while any of it still runs.
    Stopping,
    /// Ended with its exit code, or with none when stopped by a signal.
    Exited(Option<i32>),
    /// Stopped: every process it started has exited.
    Stopped,
    /// Couldn't be started.
    Failed,
}

// The panel holds one of these at a time, so the difference in size costs
// nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
enum Holding {
    Finding {
        step: StepState,
        reply: Reply,
        output_table: TaskTable,
        output_locked: bool,
        found: Option<Vec<Target>>,
    },
    Running {
        ix: usize,
        target: Target,
        status: RunStatus,
        /// What it printed, a line each, read as a terminal shows it.
        lines: Vec<ConsoleLine>,
        rows: MeasuredList,
        locked: bool,
    },
}

/// How far the panel's body is inset from its edges, on every side.
const BODY_INSET: Pixels = px(16.);

pub struct RunView {
    project_dir: PathBuf,
    agent: RunAgent,
    holding: Holding,
    focus_handle: FocusHandle,
    cancel: Arc<Cancel>,
    running: Option<Arc<Running>>,
    _work: Task<()>,
}

impl EventEmitter<CloseRun> for RunView {}
impl EventEmitter<MinimizeRun> for RunView {}
impl EventEmitter<TargetsFound> for RunView {}

impl Focusable for RunView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for RunView {
    fn drop(&mut self) {
        self.stop_all();
    }
}

impl RunView {
    /// A panel finding how the project in `project_dir` is run.
    pub fn finding(project_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::finding_with(project_dir, divergence::run_agent, cx)
    }

    /// As [`Self::finding`], looking with `agent`.
    pub fn finding_with(project_dir: PathBuf, agent: RunAgent, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(project_dir, agent, cx);
        this.find(cx);
        this
    }

    /// A panel running `target`, the `ix`th of the project's.
    pub fn running(
        project_dir: PathBuf,
        ix: usize,
        target: Target,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::empty(project_dir, divergence::run_agent, cx);
        this.run(ix, target, cx);
        this
    }

    fn empty(project_dir: PathBuf, agent: RunAgent, cx: &mut Context<Self>) -> Self {
        Self {
            project_dir,
            agent,
            holding: Holding::Finding {
                step: StepState::Pending,
                reply: Reply::default(),
                output_table: TaskTable::new(),
                output_locked: true,
                found: None,
            },
            focus_handle: cx.focus_handle(),
            cancel: Arc::default(),
            running: None,
            _work: Task::ready(()),
        }
    }

    /// Whether what it holds is still going.
    /// The project it finds or runs targets for.
    pub fn project_dir(&self) -> &std::path::Path {
        &self.project_dir
    }

    /// Whether it can be stopped: a target running, with its Stop button.
    /// While it can, it can't be closed, only minimized or stopped.
    pub fn can_stop(&self) -> bool {
        matches!(
            &self.holding,
            Holding::Running { status, .. }
                if matches!(*status, RunStatus::Running | RunStatus::Stopping)
        )
    }

    pub fn is_running(&self) -> bool {
        match &self.holding {
            Holding::Finding { step, .. } => {
                matches!(step, StepState::Pending | StepState::Running)
            }
            Holding::Running { status, .. } => {
                matches!(*status, RunStatus::Running | RunStatus::Stopping)
            }
        }
    }

    /// The target it runs, while one is running.
    pub fn running_target(&self) -> Option<usize> {
        match &self.holding {
            Holding::Running {
                ix,
                status: RunStatus::Running,
                ..
            } => Some(*ix),
            _ => None,
        }
    }

    /// What the running activity lists it as, while it goes on.
    pub fn job_title(&self) -> Option<SharedString> {
        if !self.is_running() {
            return None;
        }
        Some(match &self.holding {
            Holding::Finding { .. } => "Finding how to run".into(),
            Holding::Running { target, .. } => format!("Running {}", target.name).into(),
        })
    }

    #[cfg(all(test, unix))]
    pub fn status(&self) -> Option<RunStatus> {
        match &self.holding {
            Holding::Running { status, .. } => Some(*status),
            _ => None,
        }
    }

    #[cfg(all(test, unix))]
    pub fn lines(&self) -> Vec<SharedString> {
        match &self.holding {
            Holding::Running { lines, .. } => lines.iter().map(|line| line.text.clone()).collect(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn found(&self) -> Option<&[Target]> {
        match &self.holding {
            Holding::Finding { found, .. } => found.as_deref(),
            _ => None,
        }
    }

    /// Stops whatever it holds, a target stopped as Stop does, carrying on
    /// in the background until all of it has ended.
    fn stop_all(&mut self) {
        self.cancel.cancel();
        if let Some(running) = self.running.take() {
            running.stop_later();
        }
    }

    /// Stops the target running, if one is: it reads "Stopped" once every
    /// process it started has exited, its Stop button spinning until then.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(running) = &self.running {
            running.stop_later();
            if let Holding::Running { status, .. } = &mut self.holding
                && *status == RunStatus::Running
            {
                *status = RunStatus::Stopping;
            }
            cx.notify();
        }
    }

    /// Looks for the project's targets afresh, stopping whatever it held.
    pub fn find(&mut self, cx: &mut Context<Self>) {
        self.stop_all();
        self.cancel = Arc::default();
        self.holding = Holding::Finding {
            step: StepState::Running,
            reply: Reply::default(),
            output_table: TaskTable::new(),
            output_locked: true,
            found: None,
        };
        cx.notify();
        let (project_dir, agent, cancel) =
            (self.project_dir.clone(), self.agent, self.cancel.clone());
        self._work = cx.spawn(async move |this, cx| {
            let prompt = run_targets::find_prompt(&project_dir);
            let (lines, mut streamed) = futures::channel::mpsc::unbounded::<String>();
            let search = cx.background_spawn(async move {
                let reply = agent(&project_dir, &prompt, &cancel, &|line| {
                    lines.unbounded_send(line).ok();
                })?;
                let targets = run_targets::parse_reply(&reply)?;
                run_targets::save(&project_dir, &targets)?;
                anyhow::Ok(targets)
            });
            while let Some(first) = streamed.next().await {
                let mut batch = vec![first];
                while let Ok(next) = streamed.try_recv() {
                    batch.push(next);
                }
                if this
                    .update(cx, |this, cx| this.stream_lines(batch, cx))
                    .is_err()
                {
                    return;
                }
            }
            let found = search.await;
            this.update(cx, |this, cx| this.found_targets(found, cx))
                .ok();
        });
    }

    fn stream_lines(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        let Holding::Finding { reply, .. } = &mut self.holding else {
            return;
        };
        for line in lines {
            let events = serde_json::from_str::<serde_json::Value>(&line)
                .map(|event| harness::parse(&event))
                .unwrap_or_default();
            for event in std::iter::once(HarnessEvent::Output(line)).chain(events) {
                if let Some(error) = reply.apply(event) {
                    reply.push_error(error);
                }
            }
        }
        cx.notify();
    }

    fn found_targets(&mut self, found: anyhow::Result<Vec<Target>>, cx: &mut Context<Self>) {
        let Holding::Finding {
            step,
            reply,
            found: kept,
            ..
        } = &mut self.holding
        else {
            return;
        };
        match found {
            Ok(targets) => {
                *step = StepState::Done;
                *kept = Some(targets.clone());
                cx.emit(TargetsFound(targets));
            }
            Err(err) => {
                let why = format!("{err:#}");
                if !reply.is_done()
                    && let Some(error) = reply.apply(HarnessEvent::Failed(why.clone()))
                {
                    reply.push_error(error);
                }
                *step = StepState::Failed(why.into());
            }
        }
        cx.notify();
    }

    /// Runs `target`, the `ix`th, in place of whatever it held.
    pub fn run(&mut self, ix: usize, target: Target, cx: &mut Context<Self>) {
        self.stop_all();
        let rows = MeasuredList::new(task_table::OVERDRAW);
        self.holding = Holding::Running {
            ix,
            target: target.clone(),
            status: RunStatus::Running,
            lines: Vec::new(),
            rows,
            locked: true,
        };
        cx.notify();

        let running = match Running::start(&self.project_dir, &target.command) {
            Ok(running) => Arc::new(running),
            Err(err) => {
                self.push_lines(vec![format!("{err:#}")], cx);
                if let Holding::Running { status, .. } = &mut self.holding {
                    *status = RunStatus::Failed;
                }
                return;
            }
        };
        // Kept for quitting to stop, whatever becomes of this panel.
        crate::run_targets::keep(&running);
        self.running = Some(running.clone());
        // What it prints is gathered a few times a second, so a flood of
        // output is drawn in batches.
        self._work = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let exited = running.exited();
                let lines = running.take_printed();
                let still = this
                    .update(cx, |this, cx| {
                        // Replaced by another run meanwhile.
                        if !this
                            .running
                            .as_ref()
                            .is_some_and(|r| Arc::ptr_eq(r, &running))
                        {
                            return false;
                        }
                        if !lines.is_empty() {
                            this.push_lines(lines, cx);
                        }
                        // Being stopped, it reads "Stopped" only once all of
                        // it has exited.
                        if running.is_stopping() {
                            if !running.is_stopped() {
                                return true;
                            }
                            this.running = None;
                            if let Holding::Running { status, .. } = &mut this.holding
                                && *status == RunStatus::Stopping
                            {
                                *status = RunStatus::Stopped;
                            }
                            cx.notify();
                            return false;
                        }
                        let Some(code) = exited else {
                            return true;
                        };
                        // Ended by itself: what it left running in its tree
                        // is stopped too.
                        running.stop_later();
                        this.running = None;
                        if let Holding::Running { status, .. } = &mut this.holding
                            && *status == RunStatus::Running
                        {
                            *status = RunStatus::Exited(code);
                        }
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !still {
                    return;
                }
            }
        });
    }

    /// The target it ran, to run again.
    fn run_again(&mut self, cx: &mut Context<Self>) {
        if let Holding::Running { ix, target, .. } = &self.holding {
            let (ix, target) = (*ix, target.clone());
            self.run(ix, target, cx);
        }
    }

    fn push_lines(&mut self, new: Vec<String>, cx: &mut Context<Self>) {
        let Holding::Running {
            lines,
            rows,
            locked,
            ..
        } = &mut self.holding
        else {
            return;
        };
        let was = lines.len();
        // Read once, as it arrives.
        lines.extend(new.iter().map(|line| ConsoleLine::parse(line)));
        rows.splice(was..was, lines.len() - was);
        if *locked {
            rows.scroll_to_end();
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (icon, title): (IconName, SharedString) = match &self.holding {
            Holding::Finding { .. } => (IconName::Search, "Run".into()),
            Holding::Running { target, .. } => {
                (target.kind.icon(), format!("Run · {}", target.name).into())
            }
        };
        let status = match &self.holding {
            Holding::Running { status, .. } => Some(match status {
                RunStatus::Running => h_flex()
                    .gap_1p5()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().small())
                    .child("Running"),
                RunStatus::Stopping => h_flex()
                    .gap_1p5()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().small())
                    .child("Stopping…"),
                RunStatus::Exited(Some(0)) => {
                    h_flex().text_color(theme.success).child("Exited with 0")
                }
                RunStatus::Exited(Some(code)) => h_flex()
                    .text_color(theme.danger)
                    .child(format!("Exited with {code}")),
                RunStatus::Exited(None) | RunStatus::Stopped => {
                    h_flex().text_color(theme.muted_foreground).child("Stopped")
                }
                RunStatus::Failed => h_flex().text_color(theme.danger).child("Couldn't start"),
            }),
            Holding::Finding { .. } => None,
        };
        let running = self.is_running();
        let actions: AnyElement = match &self.holding {
            Holding::Finding { .. } => Button::new("run-find-again")
                .ghost()
                .small()
                .icon(IconName::RefreshCw)
                .label("Find again")
                .tooltip("Look through the project for how to run it afresh")
                .disabled(running)
                .on_click(cx.listener(|this, _, _, cx| this.find(cx)))
                .into_any_element(),
            Holding::Running { status, .. } if running => {
                let stopping = *status == RunStatus::Stopping;
                Button::new("run-stop")
                    .ghost()
                    .small()
                    .icon(IconName::CircleStop)
                    .label("Stop")
                    .loading(stopping)
                    .disabled(stopping)
                    .tooltip(if stopping {
                        "Stopping it, and everything it started"
                    } else {
                        "Stop it, and everything it started"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                    .into_any_element()
            }
            Holding::Running { .. } => Button::new("run-again")
                .ghost()
                .small()
                .icon(IconName::Play)
                .label("Run again")
                .tooltip("Run it afresh")
                .on_click(cx.listener(|this, _, _, cx| this.run_again(cx)))
                .into_any_element(),
        };
        let minimize_tip = match &self.holding {
            Holding::Finding { .. } => "Minimize; the search keeps going",
            Holding::Running { .. } => "Minimize; it keeps running",
        };
        h_flex()
            .flex_none()
            .h(px(44.))
            .gap_3()
            .pl_4()
            .pr_1()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(icon).small().text_color(theme.muted_foreground))
            .child(div().font_semibold().child(title))
            .children(status.map(|status| status.text_sm()))
            .child(div().flex_1())
            .child(actions)
            .child(
                Button::new("run-minimize")
                    .ghost()
                    .small()
                    .icon(IconName::Minus)
                    .tooltip(minimize_tip)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MinimizeRun))),
            )
            // What can be stopped can't be closed: only minimized or stopped.
            .when(!self.can_stop(), |this| {
                this.child(
                    Button::new("run-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip("Close, stopping anything still going")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.stop_all();
                            cx.emit(CloseRun);
                        })),
                )
            })
    }

    fn render_finding(&self, cx: &mut Context<Self>) -> AnyElement {
        let Holding::Finding {
            step,
            reply,
            output_table,
            output_locked,
            found,
        } = &self.holding
        else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        if let Some(found) = found {
            let mono = theme.mono_font_family.clone();
            return v_flex()
                .id("run-found")
                .size_full()
                .gap_2()
                .p(BODY_INSET)
                .overflow_y_scroll()
                .child(
                    div().text_sm().text_color(theme.muted_foreground).child(
                        "Saved to .suspense/run.json. The Code tab now has a button for each.",
                    ),
                )
                .children(found.iter().enumerate().map(|(ix, target)| {
                    h_flex()
                        .id(("run-found-target", ix))
                        .gap_3()
                        .py_1()
                        .child(Icon::new(target.kind.icon()).small())
                        .child(div().flex_none().font_medium().child(target.name.clone()))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_family(mono.clone())
                                .text_color(theme.muted_foreground)
                                .child(target.command.clone()),
                        )
                }))
                .into_any_element();
        }
        let this = cx.entity().downgrade();
        let reply_of = task_table::reply_of({
            let this = this.clone();
            move |cx| match &this.upgrade()?.read(cx).holding {
                Holding::Finding { reply, .. } => Some(reply),
                _ => None,
            }
        });
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                if let Holding::Finding {
                    output_locked,
                    output_table,
                    ..
                } = &mut this.holding
                {
                    *output_locked = locked;
                    if locked {
                        output_table.scroll_to_end();
                    }
                }
                cx.notify();
            })
            .ok();
        });
        let output = output_table.render(
            reply,
            reply_of,
            TableView {
                id: "run-find-output".into(),
                scrollbar: "run-find-output".into(),
                table: OUTPUT_IX,
                open: None,
                steps: None,
                lock: Some((*output_locked, toggle)),
                padding: Edges::all(BODY_INSET),
                max_height: None,
            },
            cx,
        );
        let icon: AnyElement = match step {
            StepState::Pending => div().size_4().into_any_element(),
            StepState::Running => Spinner::new().small().into_any_element(),
            StepState::Done => Icon::new(IconName::Check)
                .small()
                .text_color(theme.success)
                .into_any_element(),
            StepState::Failed(_) => Icon::new(IconName::X)
                .small()
                .text_color(theme.danger)
                .into_any_element(),
        };
        let why = match step {
            StepState::Failed(why) => Some(why.clone()),
            _ => None,
        };
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("run-find-step")
                    .flex_none()
                    .gap_0p5()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(h_flex().gap_2().child(icon).child("Looking at the project"))
                    .when_some(why, |step, why| {
                        step.child(div().pl_6().text_sm().text_color(theme.danger).child(why))
                    }),
            )
            .child(div().flex_1().min_h_0().child(output))
            .into_any_element()
    }

    fn render_running(&self, cx: &mut Context<Self>) -> AnyElement {
        let Holding::Running {
            target,
            rows,
            locked,
            ..
        } = &self.holding
        else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let (mono, well) = (
            theme.mono_font_family.clone(),
            crate::theme::palette(cx).well,
        );
        let this = cx.entity().downgrade();
        let render: RenderRow = Rc::new({
            let this = this.clone();
            let mono = mono.clone();
            move |ix, _, cx| {
                let line = this
                    .upgrade()
                    .and_then(|this| match &this.read(cx).holding {
                        Holding::Running { lines, .. } => lines.get(ix).cloned(),
                        _ => None,
                    })
                    .unwrap_or_default();
                let highlights = line.highlights(cx);
                div()
                    .px_4()
                    .text_sm()
                    .font_family(mono.clone())
                    .child(StyledText::new(line.text).with_highlights(highlights))
                    .into_any_element()
            }
        });
        let toggle: SetLock = Rc::new(move |lock, _, cx| {
            this.update(cx, |this, cx| {
                if let Holding::Running { locked, rows, .. } = &mut this.holding {
                    *locked = lock;
                    if lock {
                        rows.scroll_to_end();
                    }
                }
                cx.notify();
            })
            .ok();
        });
        let log = div()
            .id("run-output")
            .size_full()
            .py(BODY_INSET)
            .child(rows.element(render));
        let log = gpui_kit::TestSupportExt::test_support(log);
        v_flex()
            .size_full()
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_sm()
                    .font_family(mono)
                    .text_color(theme.muted_foreground)
                    .child(target.command.clone()),
            )
            // The well is the log's surface, beneath its scroll column too,
            // which has no colour of its own.
            .child(div().flex_1().min_h_0().bg(gpui_kit::rgb(well)).child(
                scrollbar::with_scrollbar(
                    "run-output",
                    rows,
                    log,
                    true,
                    Some((*locked, toggle)),
                    cx,
                ),
            ))
            .into_any_element()
    }
}

impl Render for RunView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.holding {
            Holding::Finding { .. } => self.render_finding(cx),
            Holding::Running { .. } => self.render_running(cx),
        };
        let view = v_flex()
            .id("run")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body));
        gpui_kit::TestSupportExt::test_support(view)
    }
}

#[cfg(test)]
pub mod tests {
    use std::path::Path;

    use anyhow::Result;
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext as _, TestAppContext};

    #[cfg(unix)]
    use super::RunStatus;
    use super::{RunView, TargetsFound};
    use crate::divergence::Cancel;
    use crate::run_targets::{self, Kind, Target};

    /// Finds a way to run a Rust project, and to test it.
    pub fn agent(_: &Path, prompt: &str, _: &Cancel, _: &dyn Fn(String)) -> Result<String> {
        assert!(prompt.contains("shell command"), "{prompt}");
        Ok(r#"{"targets": [
            {"name": "Run", "command": "cargo run", "kind": "run"},
            {"name": "Test", "command": "cargo test", "kind": "test"}
        ]}"#
        .into())
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Finding saves the targets with the project and says what was found.
    #[gpui_kit::test]
    async fn finding_saves_the_targets(cx: &mut TestAppContext) {
        let dir = dir("run-find");
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::project_directory::ProjectDirectory::set(dir.clone(), cx);
        });
        let found = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut view = None;
        cx.add_window(|window, cx| {
            let run = cx.new(|cx| RunView::finding_with(dir.clone(), agent, cx));
            cx.subscribe(&run, {
                let found = found.clone();
                move |_, _, TargetsFound(targets), _| *found.borrow_mut() = targets.clone()
            })
            .detach();
            view = Some(run.clone());
            Root::new(run, window, cx)
        });
        let view = view.unwrap();
        for _ in 0..100 {
            cx.run_until_parked();
            if view.read_with(cx, |view, _| view.found().is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let names = |targets: &[Target]| targets.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            view.read_with(cx, |view, _| names(view.found().unwrap())),
            ["Run", "Test"]
        );
        assert_eq!(names(&found.borrow()), ["Run", "Test"]);
        assert_eq!(run_targets::load(&dir)[1].kind, Kind::Test);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Running a target streams what it prints and says how it ended; one
    /// that runs on can be stopped.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn running_streams_output_and_stops(cx: &mut TestAppContext) {
        let dir = dir("run-target");
        cx.update(gpui_kit::init);
        let target = |command: &str| Target {
            name: "Run".into(),
            command: command.into(),
            kind: Kind::Run,
            release: false,
        };
        let mut view = None;
        cx.add_window(|window, cx| {
            let run = cx.new(|cx| RunView::running(
                dir.clone(),
                0,
                target(r#"printf '\033[1;31mhi\033[0m\n'; test -n "$CLICOLOR_FORCE" || echo uncoloured; exit 2"#),
                cx,
            ));
            view = Some(run.clone());
            Root::new(run, window, cx)
        });
        let view = view.unwrap();
        let settle = |cx: &mut TestAppContext| {
            for _ in 0..200 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                cx.executor().advance_clock(super::POLL);
                cx.run_until_parked();
                if !view.read_with(cx, |view, _| view.is_running()) {
                    return;
                }
            }
        };
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.status(), Some(RunStatus::Exited(Some(2))));
            // Clean, the command asked for colour, its colour kept as a run.
            assert_eq!(view.lines(), ["hi"]);
            let super::Holding::Running { lines, .. } = &view.holding else {
                panic!("not running");
            };
            assert_eq!(lines[0].runs.len(), 1);
            assert_eq!(view.job_title(), None);
        });

        view.update(cx, |view, cx| view.run(0, target("sleep 30"), cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.running_target(), Some(0));
            assert_eq!(view.job_title().as_deref(), Some("Running Run"));
        });
        view.update(cx, |view, cx| view.stop(cx));
        // Stopping until everything it started has exited, never "Stopped"
        // before.
        view.read_with(cx, |view, _| {
            assert_eq!(view.status(), Some(RunStatus::Stopping));
            assert!(view.can_stop());
        });
        settle(cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.status()),
            Some(RunStatus::Stopped)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A command that ends by itself, leaving a server running behind it,
    /// leaves nothing of it running.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn what_an_ended_command_left_running_is_stopped(cx: &mut TestAppContext) {
        let dir = dir("run-leftovers");
        cx.update(gpui_kit::init);
        let target = Target {
            name: "Run".into(),
            command: "sleep 60 & echo $!".into(),
            kind: Kind::Run,
            release: false,
        };
        let mut view = None;
        cx.add_window(|window, cx| {
            let run = cx.new(|cx| RunView::running(dir.clone(), 0, target, cx));
            view = Some(run.clone());
            Root::new(run, window, cx)
        });
        let view = view.unwrap();
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.executor().advance_clock(super::POLL);
            cx.run_until_parked();
            if !view.read_with(cx, |view, _| view.is_running()) {
                break;
            }
        }
        assert_eq!(
            view.read_with(cx, |view, _| view.status()),
            Some(RunStatus::Exited(Some(0)))
        );
        let pid: i32 = view.read_with(cx, |view, _| view.lines()[0].parse().unwrap());
        let started = std::time::Instant::now();
        while unsafe { libc::kill(pid, 0) == 0 }
            && started.elapsed() < std::time::Duration::from_secs(5)
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(unsafe { libc::kill(pid, 0) != 0 }, "{pid} was left running");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The log's scroll column has no colour of its own: it sits on the log's
    /// own surface, the well, as the log does, its line down both of its
    /// sides laid over the well.
    #[cfg(unix)]
    #[gpui_kit::test]
    async fn the_logs_scroll_column_is_on_the_logs_surface(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{point, px};
        let dir = dir("run-column");
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let run = cx.new(|cx| {
                RunView::running(
                    dir.clone(),
                    0,
                    Target {
                        name: "Run".into(),
                        command: "seq 1 5".into(),
                        kind: Kind::Run,
                        release: false,
                    },
                    cx,
                )
            });
            view = Some(run.clone());
            Root::new(run, window, cx)
        });
        let (view, handle): (_, gpui_kit::AnyWindowHandle) = (view.unwrap(), window.into());
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.executor().advance_clock(super::POLL);
            cx.run_until_parked();
            if !view.read_with(cx, |view, _| view.is_running()) {
                break;
            }
        }
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
                let well = crate::theme::palette(cx).well;
                use gpui_kit::component::ActiveTheme as _;
                let raised = crate::scrollbar::scroll_colors(cx.theme().is_dark()).raised;
                let raised: gpui_kit::Rgba = crate::theme::color(well).blend(raised).into();
                let raised = [raised.r, raised.g, raised.b]
                    .into_iter()
                    .fold(0u32, |rgb, c| rgb << 8 | (c * 255.).round() as u32);
                let near = |a: u32, b: u32| {
                    (0..3).all(|ix| {
                        (((a >> (ix * 8)) & 0xff) as i32 - ((b >> (ix * 8)) & 0xff) as i32).abs()
                            <= 1
                    })
                };
                let frame = crate::frame_image::Frame::of(window);
                let log = window.find("run-output").bounds();
                let column = window.find("run-output-scroll-column").bounds();
                // Down to the down button: the lock button beneath it is
                // pressed while the log follows its output.
                let bottom = window.find("run-output-scroll-down").bounds().bottom();
                let mut y = column.top() + px(0.5);
                while y < bottom {
                    let c = frame.at(point(log.right() - px(4.), y));
                    assert_eq!(
                        c, well,
                        "{mode:?}: {c:06x} at {y:?} in the log, not the well"
                    );
                    for x in [column.left() + px(0.5), column.right() - px(0.5)] {
                        let c = frame.at(point(x, y));
                        assert!(
                            near(c, raised),
                            "{mode:?}: {c:06x} at ({x:?}, {y:?}), not {raised:06x} over the well"
                        );
                    }
                    y += px(7.);
                }
            })
            .unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
