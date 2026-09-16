//! The divergence panel: runs an analysis of how far the open project's code
//! and spec have diverged, showing its build step and both agents' output
//! tables, side by side, as they run, then the score, an
//! overall report, and the source and spec trees, where a selected file shows
//! its measures and its connections to files on the other side, strongest
//! first. Each report is saved with the project, and the latest shows when the
//! panel opens, rather than analyzing again.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt as _;

use anyhow::Result;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::divergence::{
    self, AgentReply, Build, Cancel, FileResult, Files, Report, RunAgent, SavedReport, Side,
    Verdict, percent, strength_label, tree_rows,
};
use crate::harness::{self, HarnessEvent};
use crate::prompt_mode::{Reply, output_table};
use crate::scrollbar::{self, SetLock};

actions!(suspense, [AnalyzeDivergence]);

/// Emitted when the panel is closed.
pub struct CloseDivergence;

/// Emitted to hide the panel while the analysis carries on.
pub struct MinimizeDivergence;

/// How a step of the analysis is going.
#[derive(Clone, Debug, PartialEq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed(SharedString),
}

/// The steps of an analysis, in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Build,
    Code,
    Spec,
}

/// How tall the panel's header is.
const HEADER_HEIGHT: Pixels = px(44.);

/// Where the agents' output tables number from, apart from prompt mode's.
const OUTPUT_IX: usize = usize::MAX / 4;

/// How many files the report lists as best and least well defined, and as
/// diverging most.
const LISTED_FILES: usize = 5;

impl Step {
    const ALL: [Step; 3] = [Step::Build, Step::Code, Step::Spec];

    /// Which agent the step runs, for the agents' steps.
    fn agent(self) -> Option<usize> {
        match self {
            Step::Build => None,
            Step::Code => Some(0),
            Step::Spec => Some(1),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Step::Build => "Building the spec",
            Step::Code => "Analyzing the code",
            Step::Spec => "Analyzing the spec",
        }
    }
}

pub struct DivergenceView {
    project_dir: PathBuf,
    /// Where reports are saved.
    reports_dir: PathBuf,
    build: Build,
    agent: RunAgent,
    steps: [StepState; 3],
    /// Whether the build failed, so the compiled spec may be out of date.
    build_failed: bool,
    /// Why the files couldn't be listed, if they couldn't.
    error: Option<SharedString>,
    files: Option<Files>,
    /// What each agent has streamed, as a task's reply, code agent first.
    replies: [Reply; 2],
    /// Each agent's output table's scroll, and whether it follows the output.
    output_scrolls: [ScrollHandle; 2],
    output_locked: [bool; 2],
    /// The saved reports, newest first, and which of them is showing.
    reports: Vec<SavedReport>,
    shown: Option<usize>,
    /// Whether the Report tab is showing, rather than `side`'s tree.
    report_tab: bool,
    side: Side,
    selected: Option<String>,
    report_scroll: ScrollHandle,
    tree_scroll: ScrollHandle,
    details_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// Stops the running analysis's agents.
    cancel: Arc<Cancel>,
    _run: Task<()>,
}

impl EventEmitter<CloseDivergence> for DivergenceView {}
impl EventEmitter<MinimizeDivergence> for DivergenceView {}

impl Focusable for DivergenceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for DivergenceView {
    fn drop(&mut self) {
        // Closed while it runs, the agents stop.
        self.cancel.cancel();
    }
}

impl DivergenceView {
    /// A panel for the project in `project_dir`, showing its latest saved
    /// report, or analyzing it straight away when there is none.
    pub fn new(project_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let reports_dir = divergence::reports_dir(&project_dir);
        Self::with_runners(
            project_dir,
            reports_dir,
            crate::piton_build::build,
            divergence::run_agent,
            cx,
        )
    }

    /// As [`Self::new`], with reports saved in `reports_dir`, building and
    /// running agents with `build` and `agent`.
    pub fn with_runners(
        project_dir: PathBuf,
        reports_dir: PathBuf,
        build: Build,
        agent: RunAgent,
        cx: &mut Context<Self>,
    ) -> Self {
        let reports = divergence::load_reports(&reports_dir);
        let mut this = Self {
            project_dir,
            reports_dir,
            build,
            agent,
            steps: Step::ALL.map(|_| StepState::Pending),
            build_failed: false,
            error: None,
            files: None,
            replies: Default::default(),
            output_scrolls: Default::default(),
            output_locked: [true; 2],
            reports,
            shown: None,
            report_tab: true,
            side: Side::Source,
            selected: None,
            report_scroll: ScrollHandle::new(),
            tree_scroll: ScrollHandle::new(),
            details_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            cancel: Arc::default(),
            _run: Task::ready(()),
        };
        if this.reports.is_empty() {
            this.analyze(cx);
        } else {
            this.steps = Step::ALL.map(|_| StepState::Done);
            this.show_saved(0, cx);
        }
        this
    }

    /// The report showing.
    pub fn report(&self) -> Option<&Report> {
        self.saved().map(|saved| &saved.report)
    }

    fn saved(&self) -> Option<&SavedReport> {
        self.shown.and_then(|ix| self.reports.get(ix))
    }

    #[cfg(test)]
    pub fn reports(&self) -> &[SavedReport] {
        &self.reports
    }

    #[cfg(test)]
    pub fn report_tab(&self) -> bool {
        self.report_tab
    }

    /// Shows the saved report at `ix` on the Report tab, with the source file
    /// that diverges most selected for when a tree is shown.
    pub fn show_saved(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.reports.len() {
            return;
        }
        self.shown = Some(ix);
        self.report_tab = true;
        self.side = Side::Source;
        self.selected = self.reports[ix]
            .report
            .sources
            .iter()
            .filter_map(|file| Some((file.divergence?, &file.path)))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, path)| path.clone());
        self.report_scroll.set_offset(point(px(0.), px(0.)));
        self.tree_scroll.set_offset(point(px(0.), px(0.)));
        self.details_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    #[cfg(test)]
    pub fn side(&self) -> Side {
        self.side
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    #[cfg(test)]
    pub fn step(&self, step: Step) -> &StepState {
        &self.steps[step as usize]
    }

    /// Holds the analysis as if its agents were still running, for tests
    /// elsewhere; `false` lets it finish.
    #[cfg(test)]
    pub fn hold_running_for_test(&mut self, running: bool, cx: &mut Context<Self>) {
        let state = if running {
            StepState::Running
        } else {
            StepState::Done
        };
        self.steps[Step::Code as usize] = state.clone();
        self.steps[Step::Spec as usize] = state;
        cx.notify();
    }

    /// Whether the analysis is still running.
    pub fn is_running(&self) -> bool {
        self.running()
    }

    #[cfg(test)]
    pub fn reply(&self, step: Step) -> Option<&Reply> {
        step.agent().map(|ix| &self.replies[ix])
    }

    /// Folds `lines` `step`'s agent streamed into its reply.
    fn stream_lines(&mut self, step: Step, lines: Vec<String>, cx: &mut Context<Self>) {
        let Some(ix) = step.agent() else {
            return;
        };
        let reply = &mut self.replies[ix];
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

    /// Ends `step`'s agent's reply, if it didn't end itself, with why.
    fn end_reply(&mut self, step: Step, error: Option<String>) {
        let Some(ix) = step.agent() else {
            return;
        };
        let reply = &mut self.replies[ix];
        if !reply.is_done() {
            let why = error.unwrap_or_else(|| "The harness stopped without a result.".into());
            if let Some(error) = reply.apply(HarnessEvent::Failed(why)) {
                reply.push_error(error);
            }
        }
    }

    fn running(&self) -> bool {
        self.steps
            .iter()
            .any(|state| matches!(state, StepState::Running | StepState::Pending))
            && self.error.is_none()
    }

    fn set_step(&mut self, step: Step, state: StepState, cx: &mut Context<Self>) {
        self.steps[step as usize] = state;
        cx.notify();
    }

    /// Starts a fresh analysis: builds the spec, then runs both agents at
    /// once, and merges what they found.
    pub fn analyze(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = Arc::default();
        self.steps = Step::ALL.map(|_| StepState::Pending);
        self.build_failed = false;
        self.error = None;
        self.files = None;
        self.replies = Default::default();
        self.output_scrolls = Default::default();
        self.output_locked = [true; 2];
        self.shown = None;
        self.selected = None;
        self.side = Side::Source;
        cx.notify();

        let (project_dir, build, agent, cancel) = (
            self.project_dir.clone(),
            self.build,
            self.agent,
            self.cancel.clone(),
        );
        self._run = cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn({
                    let project_dir = project_dir.clone();
                    async move { divergence::list_files(&project_dir) }
                })
                .await;
            let files = match listed {
                Ok(files) => files,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(format!("{err:#}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            this.update(cx, |this, cx| {
                this.files = Some(files.clone());
                this.set_step(Step::Build, StepState::Running, cx)
            })
            .ok();
            let built = cx
                .background_spawn({
                    let project_dir = project_dir.clone();
                    async move { build(&project_dir) }
                })
                .await;
            let build_failed = !matches!(built, Ok(ref outcome) if outcome.success);
            let updated = this.update(cx, |this, cx| {
                this.build_failed = build_failed;
                this.set_step(Step::Build, StepState::Done, cx);
                this.set_step(Step::Code, StepState::Running, cx);
                this.set_step(Step::Spec, StepState::Running, cx);
            });
            if updated.is_err() {
                return;
            }

            // Each agent streams its lines back as it runs.
            let (lines, streamed) = futures::channel::mpsc::unbounded::<(Step, String)>();
            let run = |side: Side, step: Step| {
                let (project_dir, cancel, prompt, lines) = (
                    project_dir.clone(),
                    cancel.clone(),
                    divergence::prompt(side, &files),
                    lines.clone(),
                );
                cx.background_spawn(async move {
                    let reply = agent(&project_dir, &prompt, &cancel, &|line| {
                        lines.unbounded_send((step, line)).ok();
                    })?;
                    divergence::parse_reply(&reply)
                })
            };
            let (code, spec) = (run(Side::Source, Step::Code), run(Side::Spec, Step::Spec));
            // Once both agents are over, their senders are gone and the
            // stream ends.
            drop(lines);
            let mut streamed = streamed;
            while let Some(first) = streamed.next().await {
                // Whatever else has arrived meanwhile goes in the same redraw.
                let mut batch = vec![first];
                while let Ok(next) = streamed.try_recv() {
                    batch.push(next);
                }
                let updated = this.update(cx, |this, cx| {
                    for step in [Step::Code, Step::Spec] {
                        let lines: Vec<String> = batch
                            .iter()
                            .filter(|(of, _)| *of == step)
                            .map(|(_, line)| line.clone())
                            .collect();
                        if !lines.is_empty() {
                            this.stream_lines(step, lines, cx);
                        }
                    }
                });
                if updated.is_err() {
                    return;
                }
            }
            let (code, spec): (Result<AgentReply>, Result<AgentReply>) = futures::join!(code, spec);

            this.update(cx, |this, cx| {
                let state = |result: &Result<AgentReply>| match result {
                    Ok(_) => StepState::Done,
                    Err(err) => StepState::Failed(format!("{err:#}").into()),
                };
                this.steps[Step::Code as usize] = state(&code);
                this.steps[Step::Spec as usize] = state(&spec);
                this.end_reply(
                    Step::Code,
                    code.as_ref().err().map(|err| format!("{err:#}")),
                );
                this.end_reply(
                    Step::Spec,
                    spec.as_ref().err().map(|err| format!("{err:#}")),
                );
                if code.is_ok() || spec.is_ok() {
                    let mut saved = SavedReport {
                        made: divergence::now(),
                        report: divergence::merge(
                            &files.sources,
                            &files.specs,
                            code.as_ref().ok(),
                            spec.as_ref().ok(),
                        ),
                        notes: this.notes(),
                    };
                    // Analyses are costly, so each report is kept.
                    if let Err(err) = divergence::save_report(&this.reports_dir, &saved) {
                        saved
                            .notes
                            .push(format!("The report couldn't be saved: {err:#}"));
                    }
                    this.reports.insert(0, saved);
                    this.show_saved(0, cx);
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        cx.emit(CloseDivergence);
    }

    /// Shows the Report tab.
    pub fn show_report_tab(&mut self, cx: &mut Context<Self>) {
        self.report_tab = true;
        cx.notify();
    }

    /// Shows `side`'s tree, keeping the selection if it was already that side.
    pub fn set_side(&mut self, side: Side, cx: &mut Context<Self>) {
        self.report_tab = false;
        if self.side != side {
            self.side = side;
            self.selected = None;
            self.tree_scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    pub fn select(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected = Some(path);
        self.details_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Follows a connection to `path` on the other side: that tree, with the
    /// file selected and scrolled into view.
    pub fn follow(&mut self, path: String, cx: &mut Context<Self>) {
        self.open_file(self.side.other(), path, cx);
    }

    /// Shows `side`'s tree with `path` selected and scrolled into view.
    pub fn open_file(&mut self, side: Side, path: String, cx: &mut Context<Self>) {
        self.report_tab = false;
        self.side = side;
        let row = tree_rows(&self.tree_paths())
            .iter()
            .position(|row| row.file.as_deref() == Some(path.as_str()));
        self.select(path, cx);
        if let Some(row) = row {
            self.tree_scroll.scroll_to_item(row);
        }
    }

    fn tree_paths(&self) -> Vec<String> {
        self.report()
            .map(|report| {
                report
                    .files(self.side)
                    .iter()
                    .map(|file| file.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn verdict_color(percent: u32, cx: &App) -> Hsla {
        let theme = cx.theme();
        match Verdict::of(percent) {
            Verdict::Aligned => theme.success,
            Verdict::Drifting => theme.warning,
            Verdict::Diverged => theme.danger,
        }
    }

    /// How well defined a percentage is, coloured as the score would be were
    /// it how badly defined.
    fn definition_color(percent: u32, cx: &App) -> Hsla {
        Self::verdict_color(100 - percent.min(100), cx)
    }

    /// A bar as long as `value`, from 0 to 1, in `color`.
    fn bar(value: f32, color: Hsla, cx: &App) -> Div {
        div().h(px(4.)).rounded_full().bg(cx.theme().muted).child(
            div()
                .h_full()
                .rounded_full()
                .bg(color)
                .w(relative(value.clamp(0., 1.))),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let score = (!self.running())
            .then(|| self.report().and_then(Report::score))
            .flatten();
        let made = self.saved().map(|saved| saved.made);
        h_flex()
            .flex_none()
            // As tall whatever it shows, running or with results.
            .h(HEADER_HEIGHT)
            .gap_3()
            .pl_4()
            .pr_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::GitCompareArrows)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().font_semibold().child("Divergence"))
            .when_some(score, |header, score| {
                let color = Self::verdict_color(score, cx);
                header.child(gpui_kit::TestSupportExt::test_support(
                    h_flex()
                        .id("divergence-score")
                        .h_full()
                        .gap_3()
                        .pl_3()
                        .border_l_1()
                        .border_color(theme.border)
                        // Read as how aligned it is: 92% aligned.
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(color)
                                .child(format!("{}% aligned", 100 - score.min(100))),
                        )
                        .children(made.map(|made| {
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(divergence::ago(made, divergence::now()))
                        })),
                ))
            })
            .child(div().flex_1())
            .child(
                Button::new("divergence-analyze-again")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Analyze again")
                    .disabled(self.running())
                    .on_click(cx.listener(|this, _, _, cx| this.analyze(cx))),
            )
            .child(
                Button::new("divergence-minimize")
                    .ghost()
                    .small()
                    .icon(IconName::Minus)
                    .tooltip("Minimize; the analysis keeps running")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MinimizeDivergence))),
            )
            .child(
                Button::new("divergence-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close, stopping any analysis still running")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
    }

    /// A step's icon: a spinner while it runs, a tick once done, or a cross if
    /// it failed.
    fn step_icon(state: &StepState, cx: &App) -> AnyElement {
        let theme = cx.theme();
        match state {
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
        }
    }

    /// A step's heading: its icon and name, muted before it starts, and why it
    /// failed if it did.
    fn step_heading(&self, step: Step, cx: &App) -> Div {
        let theme = cx.theme();
        let state = &self.steps[step as usize];
        v_flex()
            .gap_0p5()
            .child(
                h_flex().gap_2().child(Self::step_icon(state, cx)).child(
                    div()
                        .when(*state == StepState::Pending, |label| {
                            label.text_color(theme.muted_foreground)
                        })
                        .child(step.label()),
                ),
            )
            .when_some(
                match state {
                    StepState::Failed(why) => Some(why.clone()),
                    _ => None,
                },
                |heading, why| {
                    heading.child(div().pl_6().text_sm().text_color(theme.danger).child(why))
                },
            )
    }

    /// The analysis as it runs: the build step along the top, and beneath it
    /// both agents' output tables, side by side.
    fn render_steps(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let top = v_flex()
            .id("divergence-steps")
            .flex_none()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .children(self.error.clone().map(|error| {
                div()
                    .text_color(theme.danger)
                    .child(format!("Couldn't analyze the project: {error}"))
            }))
            .child(self.step_heading(Step::Build, cx));
        v_flex().size_full().child(top).child(
            h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_agent(Step::Code, cx))
                .child(self.render_agent(Step::Spec, cx)),
        )
    }

    /// An agent's progress: its heading, and its output as a task's table,
    /// following the latest output while locked to the bottom.
    fn render_agent(&self, step: Step, cx: &mut Context<Self>) -> AnyElement {
        let ix = step.agent().unwrap_or_default();
        let theme = cx.theme();
        let scroll = &self.output_scrolls[ix];
        let locked = self.output_locked[ix];
        if locked {
            scroll.scroll_to_bottom();
        }
        let output = div()
            .id(("divergence-output", ix))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px_4()
            .pb_3()
            .child(output_table(
                OUTPUT_IX - ix,
                &self.replies[ix],
                None,
                None,
                cx,
            ));
        // Lets UI tests find the output; inert in normal builds.
        let output = gpui_kit::TestSupportExt::test_support(output);
        let this = cx.entity().downgrade();
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                if this.output_locked[ix] != locked {
                    this.output_locked[ix] = locked;
                    if locked {
                        this.output_scrolls[ix].scroll_to_bottom();
                    }
                    cx.notify();
                }
            })
            .ok();
        });
        let column = v_flex()
            .id(("divergence-agent", ix))
            .w(relative(0.5))
            .h_full()
            .min_w_0()
            .when(step == Step::Code, |column| {
                column.border_r_1().border_color(theme.border)
            })
            .child(self.step_heading(step, cx).flex_none().px_4().py_2())
            .child(div().flex_1().min_h_0().child(scrollbar::with_scrollbar(
                format!("divergence-output-{ix}"),
                scroll,
                output,
                true,
                Some((locked, toggle)),
                cx,
            )));
        // Lets UI tests find the column; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(column).into_any_element()
    }

    /// Notes about the results: a failed build, a side cut short, or an agent
    /// that failed.
    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.build_failed {
            notes.push("piton build failed, so the compiled spec may be out of date.".into());
        }
        if let Some(files) = &self.files {
            if files.sources_truncated {
                notes.push(format!(
                    "Only the first {} source files were analyzed.",
                    divergence::MAX_FILES
                ));
            }
            if files.specs_truncated {
                notes.push(format!(
                    "Only the first {} spec files were analyzed.",
                    divergence::MAX_FILES
                ));
            }
        }
        for step in [Step::Code, Step::Spec] {
            if let StepState::Failed(why) = &self.steps[step as usize] {
                notes.push(format!("{} failed: {why}", step.label()));
            }
        }
        notes
    }

    fn render_tree(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let files = report.files(self.side);
        let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
        let rows = tree_rows(&paths).into_iter().enumerate().map(|(ix, row)| {
            let indent = px(16. + 14. * row.depth as f32);
            let Some(path) = row.file else {
                return h_flex()
                    .id(("divergence-folder", ix))
                    .gap_1p5()
                    .pl(indent)
                    .py_0p5()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::FolderOpen).xsmall())
                    .child(row.name)
                    .into_any_element();
            };
            let divergence = files
                .iter()
                .find(|file| file.path == path)
                .and_then(|file| file.divergence);
            let selected = self.selected.as_deref() == Some(path.as_str());
            let meter = match divergence {
                Some(value) => {
                    let percent = (value * 100.).round() as u32;
                    let color = Self::verdict_color(percent, cx);
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(
                            div()
                                .w(px(48.))
                                .h(px(4.))
                                .rounded_full()
                                .bg(theme.muted)
                                .child(div().h_full().rounded_full().bg(color).w(relative(value))),
                        )
                        .child(
                            div()
                                .w(px(36.))
                                .text_right()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{percent}%")),
                        )
                        .into_any_element()
                }
                None => div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Not analyzed")
                    .into_any_element(),
            };
            let target = path.clone();
            let row = h_flex()
                .id(("divergence-file", ix))
                .gap_2()
                .pl(indent)
                .pr_2()
                .py_0p5()
                .cursor_pointer()
                .when(selected, |row| row.bg(theme.list_active))
                .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(target.clone(), cx)))
                .child(
                    Icon::new(IconName::File)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(row.name))
                .child(meter);
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row).into_any_element()
        });
        let list = v_flex()
            .id("divergence-tree")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.tree_scroll)
            .py_1()
            .children(rows);
        scrollbar::with_scrollbar("divergence-tree", &self.tree_scroll, list, true, None, cx)
    }

    fn render_details(&self, report: &Report, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(path) = &self.selected else {
            return div()
                .p_6()
                .text_color(theme.muted_foreground)
                .child("Select a file to see what it's connected to.")
                .into_any_element();
        };
        let file = report
            .files(self.side)
            .iter()
            .find(|file| &file.path == path);
        let analyzed = file.is_some_and(|file| file.divergence.is_some());
        let other = self.side.other();
        let connections = report.connections_of(self.side, path);

        // A measure: its name, a bar, and its percentage, or "Not analyzed".
        let measure = |id: &'static str, name: &str, value: Option<f32>, color: Hsla| {
            let row = h_flex()
                .id(id)
                .gap_3()
                .child(
                    div()
                        .w(px(180.))
                        .flex_none()
                        .text_sm()
                        .child(name.to_string()),
                )
                .child(match value {
                    Some(value) => h_flex()
                        .flex_1()
                        .gap_2()
                        .child(Self::bar(value, color, cx).flex_1())
                        .child(
                            div()
                                .w(px(40.))
                                .text_right()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}%", percent(value))),
                        )
                        .into_any_element(),
                    None => div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Not analyzed")
                        .into_any_element(),
                });
            gpui_kit::TestSupportExt::test_support(row)
        };
        let divergence = file.and_then(|file| file.divergence);
        let definition = file.and_then(|file| file.definition);
        let measures = v_flex()
            .gap_1p5()
            .child(measure(
                "divergence-influence",
                "Influence",
                analyzed.then(|| report.influence(self.side, path)),
                theme.primary,
            ))
            .child(measure(
                "divergence-diverges",
                match self.side {
                    Side::Source => "Diverges from the spec",
                    Side::Spec => "Implementation diverges",
                },
                divergence,
                Self::verdict_color(divergence.map_or(0, percent), cx),
            ))
            .child(measure(
                "divergence-definition",
                "Definition",
                definition,
                Self::definition_color(definition.map_or(0, percent), cx),
            ));

        let gaps = file.map(|file| file.gaps.clone()).unwrap_or_default();
        let gaps_title = match self.side {
            Side::Source => "Filled in by the implementation",
            Side::Spec => "Gaps in the spec",
        };
        let gaps = v_flex()
            .id("divergence-gaps")
            .gap_1()
            .child(div().pt_2().font_medium().child(gaps_title))
            .when(gaps.is_empty() && analyzed, |list| {
                list.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("None found"),
                )
            })
            .children(gaps.into_iter().map(|gap| {
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .child(div().text_color(theme.muted_foreground).child("•"))
                    .child(div().flex_1().min_w_0().child(gap))
            }));

        let rows = connections.iter().enumerate().map(|(ix, connection)| {
            let target = connection.end(other).to_string();
            let strength = connection.strength;
            let row = v_flex()
                .id(("divergence-connection", ix))
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|row| row.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.follow(target.clone(), cx)))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme.mono_font_family.clone())
                                .child(connection.end(other).to_string()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} · {}%",
                                    strength_label(strength),
                                    percent(strength)
                                )),
                        )
                        .children(connection.divergence.map(|value| {
                            div()
                                .flex_none()
                                .text_sm()
                                .text_color(Self::verdict_color(percent(value), cx))
                                .child(format!("Diverges {}%", percent(value)))
                        })),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(6.))
                        .rounded_full()
                        .bg(theme.muted)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(theme.primary.opacity(0.35 + 0.65 * strength))
                                .w(relative(strength)),
                        ),
                )
                .children((!connection.notes.is_empty()).then(|| {
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(connection.notes.join(" · "))
                }));
            // Lets UI tests find the connection; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row)
        });
        let body = v_flex()
            .id("divergence-details")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.details_scroll)
            .gap_3()
            .p_4()
            .child(
                div()
                    .font_semibold()
                    .font_family(theme.mono_font_family.clone())
                    .child(path.clone()),
            )
            .children(
                file.map(|file| file.summary.clone())
                    .filter(|summary| !summary.is_empty())
                    .map(|summary| div().child(summary)),
            )
            .child(measures)
            .child(gpui_kit::TestSupportExt::test_support(gaps))
            .child(div().pt_2().font_medium().child(match connections.len() {
                0 => format!("No connected {} files", other.label().to_lowercase()),
                1 => format!("1 connected {} file", other.label().to_lowercase()),
                n => format!("{n} connected {} files", other.label().to_lowercase()),
            }))
            .children(rows);
        scrollbar::with_scrollbar(
            "divergence-details",
            &self.details_scroll,
            body,
            true,
            None,
            cx,
        )
    }

    /// The Report tab: the figures, the agents' assessments, the well and less
    /// well defined areas and files, the files that diverge most, the saved
    /// reports, and notes about the analysis.
    fn render_report(&self, saved: &SavedReport, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let report = &saved.report;
        // A section, and a part of one.
        let heading = |text: &str| div().text_lg().font_semibold().child(text.to_string());
        let subheading = |text: &str| {
            div()
                .text_sm()
                .font_medium()
                .text_color(theme.muted_foreground)
                .child(text.to_string())
        };
        // Two halves side by side, top aligned.
        let columns = |left: AnyElement, right: AnyElement| {
            h_flex()
                .items_start()
                .gap_8()
                .child(v_flex().flex_1().min_w_0().gap_2().child(left))
                .child(v_flex().flex_1().min_w_0().gap_2().child(right))
        };
        let muted = |text: &str| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text.to_string())
        };

        // A figure: its value, large, above what it is.
        let figure = |label: &str, value: Option<u32>, color: Option<Hsla>| {
            v_flex()
                .min_w(px(110.))
                .gap_0p5()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xl()
                        .font_bold()
                        .when_some(color, |value, color| value.text_color(color))
                        .child(value.map_or("—".to_string(), |value| format!("{value}%"))),
                )
                .child(muted(label))
        };
        let [aligned, drifting, diverged] = report.verdict_counts();
        let score = report.score();
        let definition = |side| report.definition(side);
        let figures = h_flex()
            .id("divergence-figures")
            .flex_wrap()
            .gap_2()
            .child(figure(
                "Aligned",
                report.alignment(),
                score.map(|score| Self::verdict_color(score, cx)),
            ))
            .child(figure(
                "Source definition",
                definition(Side::Source),
                definition(Side::Source).map(|value| Self::definition_color(value, cx)),
            ))
            .child(figure(
                "Spec definition",
                definition(Side::Spec),
                definition(Side::Spec).map(|value| Self::definition_color(value, cx)),
            ))
            .child(figure(
                "Source coverage",
                report.coverage(Side::Source),
                None,
            ))
            .child(figure("Spec coverage", report.coverage(Side::Spec), None))
            .child(
                v_flex()
                    .gap_0p5()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .text_xl()
                            .font_bold()
                            .child(div().text_color(theme.success).child(aligned.to_string()))
                            .child(div().text_color(theme.warning).child(drifting.to_string()))
                            .child(div().text_color(theme.danger).child(diverged.to_string())),
                    )
                    .child(muted("Aligned · Drifting · Diverged")),
            );

        let assessment = |side: Side, text: &str| {
            v_flex()
                .gap_1()
                .child(subheading(side.label()))
                .child(if text.is_empty() {
                    muted("No assessment").into_any_element()
                } else {
                    div().child(text.to_string()).into_any_element()
                })
        };

        let areas = |areas: &[divergence::Area]| {
            v_flex().gap_2().children(areas.iter().map(|area| {
                v_flex()
                    .child(div().font_medium().child(area.name.clone()))
                    .children((!area.why.is_empty()).then(|| muted(&area.why)))
            }))
        };
        // Files, each a row that opens it in its tree.
        let files = |id: &'static str,
                     files: Vec<(Side, &FileResult)>,
                     value: fn(&FileResult) -> f32,
                     color: fn(u32, &App) -> Hsla,
                     cx: &mut Context<Self>| {
            v_flex().children(files.into_iter().enumerate().map(|(ix, (side, file))| {
                let (path, amount) = (file.path.clone(), percent(value(file)));
                // Its text lines up with the heading above; only its hover
                // reaches out past it.
                let row = h_flex()
                    .id((id, ix))
                    .gap_2()
                    .mx_neg_2()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_file(side, path.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .child(file.path.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(color(amount, cx))
                            .child(format!("{amount}%")),
                    );
                gpui_kit::TestSupportExt::test_support(row)
            }))
        };
        let definition_of = |file: &FileResult| file.definition.unwrap_or_default();
        let divergence_of = |file: &FileResult| file.divergence.unwrap_or_default();
        let definition_color = |percent: u32, cx: &App| Self::definition_color(percent, cx);
        let verdict_color = |percent: u32, cx: &App| Self::verdict_color(percent, cx);
        let best = files(
            "divergence-best",
            report.by_definition(true, LISTED_FILES),
            definition_of,
            definition_color,
            cx,
        );
        let least = files(
            "divergence-least",
            report.by_definition(false, LISTED_FILES),
            definition_of,
            definition_color,
            cx,
        );
        let most = files(
            "divergence-most",
            report.most_diverged(LISTED_FILES),
            divergence_of,
            verdict_color,
            cx,
        );

        let now = divergence::now();
        let saved_rows = self.reports.iter().enumerate().map(|(ix, other)| {
            let showing = self.shown == Some(ix);
            let score = other.report.score();
            let row = h_flex()
                .id(("divergence-saved", ix))
                .gap_3()
                .mx_neg_2()
                .px_2()
                .py_0p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(showing, |row| row.bg(theme.list_active))
                .when(!showing, |row| row.hover(|row| row.bg(theme.list_hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.show_saved(ix, cx)))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .child(divergence::ago(other.made, now)),
                )
                .children(score.map(|score| {
                    div()
                        .text_sm()
                        .text_color(Self::verdict_color(score, cx))
                        .child(format!("{}% aligned", 100 - score.min(100)))
                }));
            gpui_kit::TestSupportExt::test_support(row)
        });

        let body = v_flex()
            .id("divergence-report")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.report_scroll)
            .gap_2()
            .p_4()
            .child(gpui_kit::TestSupportExt::test_support(figures))
            .child(
                v_flex()
                    .gap_2()
                    .pt_4()
                    .child(heading("Assessment"))
                    .child(columns(
                        assessment(Side::Source, &report.code_assessment).into_any_element(),
                        assessment(Side::Spec, &report.spec_assessment).into_any_element(),
                    )),
            )
            .child(
                v_flex()
                    .gap_3()
                    .pt_4()
                    .child(columns(
                        v_flex()
                            .gap_2()
                            .child(heading("Well defined"))
                            .when(report.well_defined.is_empty(), |column| {
                                column.child(muted("No areas named"))
                            })
                            .child(areas(&report.well_defined))
                            .into_any_element(),
                        v_flex()
                            .gap_2()
                            .child(heading("Less well defined"))
                            .when(report.less_defined.is_empty(), |column| {
                                column.child(muted("No areas named"))
                            })
                            .child(areas(&report.less_defined))
                            .into_any_element(),
                    ))
                    .child(columns(
                        v_flex()
                            .gap_1()
                            .child(subheading("Best defined files"))
                            .child(best)
                            .into_any_element(),
                        v_flex()
                            .gap_1()
                            .child(subheading("Least defined files"))
                            .child(least)
                            .into_any_element(),
                    )),
            )
            .child(
                div().pt_4().child(columns(
                    v_flex()
                        .gap_2()
                        .child(heading("Most diverged"))
                        .child(most)
                        .into_any_element(),
                    v_flex()
                        .gap_2()
                        .child(heading("Saved reports"))
                        .child(v_flex().children(saved_rows))
                        .into_any_element(),
                )),
            )
            .when(!saved.notes.is_empty(), |body| {
                body.child(div().pt_4().child(heading("Notes")))
                    .children(saved.notes.iter().map(|note| {
                        div()
                            .text_sm()
                            .text_color(theme.warning)
                            .child(note.clone())
                    }))
            });
        // Lets UI tests find the report; inert in normal builds.
        let body = gpui_kit::TestSupportExt::test_support(body);
        scrollbar::with_scrollbar(
            "divergence-report",
            &self.report_scroll,
            body,
            true,
            None,
            cx,
        )
    }

    fn render_results(&self, saved: &SavedReport, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let report = &saved.report;
        // A tab: its label, underlined while selected.
        let tab = |id: &'static str, label: &'static str, selected: bool| {
            let tab = div()
                .id(id)
                .h_full()
                .flex()
                .items_center()
                .px_1()
                .border_b_2()
                .cursor_pointer()
                .when(selected, |tab| {
                    tab.border_color(theme.primary)
                        .text_color(theme.foreground)
                        .font_medium()
                })
                .when(!selected, |tab| {
                    tab.border_color(transparent_black())
                        .text_color(theme.muted_foreground)
                        .hover(|tab| tab.text_color(theme.foreground))
                })
                .child(label);
            // Lets UI tests find the tab; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(tab)
        };
        let tabs = h_flex()
            .flex_none()
            .h(px(36.))
            .gap_2()
            // With each tab's own padding, labels line up with the content.
            .px_3()
            .text_sm()
            .border_b_1()
            .border_color(theme.border)
            .child(
                tab("divergence-tab-report", "Report", self.report_tab)
                    .on_click(cx.listener(|this, _, _, cx| this.show_report_tab(cx))),
            )
            .child(
                tab(
                    "divergence-side-source",
                    Side::Source.label(),
                    !self.report_tab && self.side == Side::Source,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_side(Side::Source, cx))),
            )
            .child(
                tab(
                    "divergence-side-spec",
                    Side::Spec.label(),
                    !self.report_tab && self.side == Side::Spec,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_side(Side::Spec, cx))),
            );
        let body = if self.report_tab {
            self.render_report(saved, cx)
        } else {
            h_flex()
                .size_full()
                .child(
                    div()
                        .w(relative(0.4))
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(self.render_tree(report, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_details(report, cx)),
                )
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Render for DivergenceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match (self.saved(), self.running()) {
            (Some(saved), false) => self.render_results(&saved.clone(), cx).into_any_element(),
            _ => self.render_steps(cx).into_any_element(),
        };
        let view = v_flex()
            .id("divergence")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body));
        // Lets UI tests find the view; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(view)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use anyhow::{Result, bail};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{DivergenceView, Step, StepState};
    use crate::divergence::{Cancel, Side};
    use crate::piton_build::BuildOutcome;

    fn built(_: &Path) -> Result<BuildOutcome> {
        Ok(BuildOutcome {
            success: true,
            files: Vec::new(),
            report: String::new(),
        })
    }

    /// Answers as the agents would, about this project's own files.
    fn agent(_: &Path, prompt: &str, _: &Cancel, on_line: &dyn Fn(String)) -> Result<String> {
        // A tool call, and the start of some text, as the harness streams them.
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"t1","name":"Read"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","input":{"file_path":"src/main.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Here it is."}}}"#,
            r#"{"type":"result","is_error":false,"result":"Here it is."}"#,
        ] {
            on_line(line.to_string());
        }
        Ok(if prompt.contains("Source files to analyze") {
            r#"Sure: {"assessment": "The code follows the spec.",
                "wellDefined": [{"name": "Scrolling", "why": "Every part is described."}],
                "lessDefined": [{"name": "Startup", "why": "Barely described."}],
                "files": [
                {"path": "src/scrollbar.rs", "divergence": 0.1, "definition": 0.9, "summary": "As described.",
                 "links": [{"path": "spec/ui/components/scrollbar/index.pi", "strength": 0.9, "divergence": 0.1, "note": "the column"}]},
                {"path": "src/main.rs", "divergence": 0.7, "definition": 0.2, "summary": "Mostly unspecified.",
                 "gaps": ["the window size", "logging"],
                 "links": [{"path": "spec/index.pi", "strength": 0.2, "note": "the entry"},
                           {"path": "spec/ui/components/scrollbar/index.pi", "strength": 0.5}]}
            ]}"#
        } else {
            r#"{"assessment": "The spec is mostly complete.", "files": [
                {"path": "spec/ui/components/scrollbar/index.pi", "divergence": 0.3, "definition": 0.8,
                 "gaps": ["the thumb's minimum size"],
                 "links": [{"path": "src/scrollbar.rs", "strength": 0.7}]}
            ]}"#
        }
        .to_string())
    }

    fn failing_spec_agent(
        project_dir: &Path,
        prompt: &str,
        cancel: &Cancel,
        on_line: &dyn Fn(String),
    ) -> Result<String> {
        if prompt.contains("Spec files to analyze") {
            bail!("the harness stopped");
        }
        agent(project_dir, prompt, cancel, on_line)
    }

    /// A fresh folder for a test's saved reports.
    fn reports_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("suspense-divergence-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn open(
        cx: &mut TestAppContext,
        reports: &Path,
        run: crate::divergence::RunAgent,
    ) -> (gpui_kit::Entity<DivergenceView>, gpui_kit::AnyWindowHandle) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::project_directory::ProjectDirectory::set(env!("CARGO_MANIFEST_DIR").into(), cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let divergence = cx.new(|cx| {
                DivergenceView::with_runners(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")),
                    reports.to_path_buf(),
                    built,
                    run,
                    cx,
                )
            });
            view = Some(divergence.clone());
            Root::new(divergence, window, cx)
        });
        (view.unwrap(), window.into())
    }

    /// Once both agents are done, the score shows, the source file that
    /// diverges most is selected with its connections, and following a
    /// connection shows that file in the spec tree.
    #[gpui_kit::test]
    async fn shows_the_score_trees_and_connections(cx: &mut TestAppContext) {
        let reports = reports_dir("score");
        let (view, handle) = open(cx, &reports, agent);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("divergence-score").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            for step in [Step::Build, Step::Code, Step::Spec] {
                assert_eq!(view.step(step), &StepState::Done, "{step:?}");
            }
            let report = view.report().unwrap();
            // (0.1 + 0.7 + 0.3) / 3
            assert_eq!(report.score(), Some(37));
            assert_eq!(view.selected(), Some("src/main.rs"));
            let connections = report.connections_of(Side::Source, "src/main.rs");
            assert_eq!(
                connections
                    .iter()
                    .map(|c| c.spec.as_str())
                    .collect::<Vec<_>>(),
                ["spec/ui/components/scrollbar/index.pi", "spec/index.pi"]
            );
            let strong = report.connections_of(Side::Spec, "spec/ui/components/scrollbar/index.pi");
            assert_eq!(strong[0].source, "src/scrollbar.rs");
            assert!((strong[0].strength - 0.8).abs() < 1e-4);
        });

        // What each agent streamed is its reply: the tool call and the text.
        view.read_with(cx, |view, _| {
            for step in [Step::Code, Step::Spec] {
                let reply = view.reply(step).unwrap();
                assert_eq!(reply.row_count(), 2, "{step:?}");
                assert!(reply.is_done(), "{step:?}");
            }
        });
        // While the agents run, their tables show side by side.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("divergence-agent", 0usize)).is_none());
        })
        .unwrap();
        view.update(cx, |view, cx| view.hold_running_for_test(true, cx));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let code = window.find(("divergence-agent", 0usize)).bounds();
            let spec = window.find(("divergence-agent", 1usize)).bounds();
            assert!(
                spec.left() >= code.right() - gpui_kit::px(1.)
                    && (code.top() - spec.top()).abs() < gpui_kit::px(1.),
                "{code:?} and {spec:?} aren't side by side"
            );
            assert!((code.size.width - spec.size.width).abs() < gpui_kit::px(2.));
            for ix in [0usize, 1] {
                assert!(window.try_find(("divergence-output", ix)).is_some());
                assert!(
                    window
                        .try_find(format!("divergence-output-{ix}-scroll-lock"))
                        .is_some()
                );
            }
        })
        .unwrap();
        view.update(cx, |view, cx| view.hold_running_for_test(false, cx));

        // The Report tab shows first; the source tree keeps the selection.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("divergence-report").is_some());
            window.click("divergence-side-source", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            for measure in [
                "divergence-influence",
                "divergence-diverges",
                "divergence-definition",
                "divergence-gaps",
            ] {
                assert!(window.try_find(measure).is_some(), "{measure}");
            }
            assert!(window.try_find(("divergence-connection", 1usize)).is_some());
            window.click(("divergence-connection", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.side(), Side::Spec);
            assert_eq!(
                view.selected(),
                Some("spec/ui/components/scrollbar/index.pi")
            );
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("divergence-side-source", cx);
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.side(), Side::Source);
            assert_eq!(view.selected(), None);
        });
        std::fs::remove_dir_all(&reports).ok();
    }

    /// How many times the counting agent has run.
    static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting_agent(
        project_dir: &Path,
        prompt: &str,
        cancel: &Cancel,
        on_line: &dyn Fn(String),
    ) -> Result<String> {
        RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        agent(project_dir, prompt, cancel, on_line)
    }

    /// A report is saved once the analysis is over, and a panel opened again
    /// shows it on the Report tab without analyzing; its lists open files in
    /// their trees, and analyzing again saves another.
    #[gpui_kit::test]
    async fn reports_are_saved_and_shown_again(cx: &mut TestAppContext) {
        let reports = reports_dir("saved");
        let (first, handle) = open(cx, &reports, agent);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("divergence-score").is_some()
        })
        .await;
        let saved = first.read_with(cx, |view, _| view.reports().to_vec());
        assert_eq!(saved.len(), 1);
        assert_eq!(crate::divergence::load_reports(&reports), saved);
        cx.update_window(handle, |_, window, _| window.remove_window())
            .unwrap();

        RUNS.store(0, std::sync::atomic::Ordering::SeqCst);
        let (view, handle) = open(cx, &reports, counting_agent);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(RUNS.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(!view.is_running());
            assert!(view.report_tab());
            assert_eq!(view.report(), Some(&saved[0].report));
            let report = view.report().unwrap();
            assert_eq!(report.code_assessment, "The code follows the spec.");
            assert_eq!(report.well_defined[0].name, "Scrolling");
            assert_eq!(report.definition(Side::Source), Some(55));
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("divergence-score").is_some());
            assert!(window.try_find("divergence-figures").is_some());
            assert!(window.try_find(("divergence-saved", 0usize)).is_some());
            assert!(window.try_find(("divergence-most", 0usize)).is_some());
            // The least well defined file is src/main.rs.
            window.click(("divergence-least", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.report_tab());
            assert_eq!(view.side(), Side::Source);
            assert_eq!(view.selected(), Some("src/main.rs"));
        });

        view.update(cx, |view, cx| view.analyze(cx));
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("divergence-score").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            assert_eq!(RUNS.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert_eq!(view.reports().len(), 2);
        });
        assert_eq!(crate::divergence::load_reports(&reports).len(), 2);
        std::fs::remove_dir_all(&reports).ok();
    }

    /// An agent that fails is marked so, and what the other found still shows.
    #[gpui_kit::test]
    async fn one_failed_agent_still_shows_the_other(cx: &mut TestAppContext) {
        let reports = reports_dir("failed");
        let (view, handle) = open(cx, &reports, failing_spec_agent);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("divergence-score").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            assert!(matches!(view.step(Step::Spec), StepState::Failed(_)));
            // Its table ends with why.
            let reply = view.reply(Step::Spec).unwrap();
            assert!(reply.is_done());
            assert_eq!(reply.row_count(), 1);
            assert_eq!(view.step(Step::Code), &StepState::Done);
            let report = view.report().unwrap();
            assert!(report.specs.iter().all(|file| file.divergence.is_none()));
            // (0.1 + 0.7) / 2
            assert_eq!(report.score(), Some(40));
            // The report is saved with a note that the spec agent failed.
            assert!(
                view.reports()[0]
                    .notes
                    .iter()
                    .any(|note| note.contains("failed"))
            );
        });
        std::fs::remove_dir_all(&reports).ok();
    }
}
