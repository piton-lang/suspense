//! The Rescope panel: has the harness look through the spec for concepts it
//! repeats while its output shows, then lists them ranked, each with every
//! place the spec describes it, to check the ones to refactor into scopes of
//! their own and send that to the harness. It can be minimized while it looks.

use std::collections::BTreeSet;
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

use crate::checkbox::checkbox;
use crate::divergence::{self, Cancel, RunAgent, percent};
use crate::harness::{self, HarnessEvent};
use crate::rescope::{self, Concept};
use crate::scrollbar::{self, SetLock};
use crate::task_table::{self, Reply, TableView, TaskTable};

actions!(suspense, [Rescope]);

/// Emitted when the panel is closed.
pub struct CloseRescope;

/// Emitted to hide the panel while it keeps looking.
pub struct MinimizeRescope;

/// Emitted with the prompt refactoring the concepts checked, to send.
pub struct RefactorConcepts(pub String);

/// Where the harness's output table numbers from, apart from others'.
const OUTPUT_IX: usize = usize::MAX / 16;

/// How much a concept has to be worth to start checked.
const CHECKED_WORTH: f32 = 0.5;

/// How a step is going.
#[derive(Clone, Debug, PartialEq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed(SharedString),
}

pub struct RescopeView {
    project_dir: PathBuf,
    agent: RunAgent,
    /// Reading the spec, and looking through it.
    steps: [StepState; 2],
    reply: Reply,
    output_table: TaskTable,
    output_locked: bool,
    /// The concepts found, once looked for, and which are checked and
    /// selected, by index.
    concepts: Option<Vec<Concept>>,
    checked: BTreeSet<usize>,
    selected: Option<usize>,
    list_scroll: ScrollHandle,
    details_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    cancel: Arc<Cancel>,
    _run: Task<()>,
}

impl EventEmitter<CloseRescope> for RescopeView {}
impl EventEmitter<MinimizeRescope> for RescopeView {}
impl EventEmitter<RefactorConcepts> for RescopeView {}

impl Focusable for RescopeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for RescopeView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl RescopeView {
    pub fn new(project_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_agent(project_dir, divergence::run_agent, cx)
    }

    /// As [`Self::new`], looking with `agent`.
    pub fn with_agent(project_dir: PathBuf, agent: RunAgent, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            project_dir,
            agent,
            steps: [StepState::Pending, StepState::Pending],
            reply: Reply::default(),
            output_table: TaskTable::new(),
            output_locked: true,
            concepts: None,
            checked: BTreeSet::new(),
            selected: None,
            list_scroll: ScrollHandle::new(),
            details_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            cancel: Arc::default(),
            _run: Task::ready(()),
        };
        this.look(cx);
        this
    }

    #[cfg(test)]
    pub fn concepts(&self) -> Option<&[Concept]> {
        self.concepts.as_deref()
    }

    #[cfg(test)]
    pub fn checked(&self) -> &BTreeSet<usize> {
        &self.checked
    }

    #[cfg(test)]
    pub fn step_failed(&self) -> bool {
        matches!(self.steps[1], StepState::Failed(_))
    }

    /// Whether it is still looking.
    pub fn is_running(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, StepState::Pending | StepState::Running))
            && !self
                .steps
                .iter()
                .any(|step| matches!(step, StepState::Failed(_)))
    }

    /// Looks through the spec afresh.
    pub fn look(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = Arc::default();
        self.steps = [StepState::Running, StepState::Pending];
        self.reply = Reply::default();
        self.output_locked = true;
        self.concepts = None;
        self.checked.clear();
        self.selected = None;
        cx.notify();

        let (project_dir, agent, cancel) =
            (self.project_dir.clone(), self.agent, self.cancel.clone());
        self._run = cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn({
                    let project_dir = project_dir.clone();
                    async move { divergence::list_spec_files(&project_dir) }
                })
                .await;
            let (spec_root, files) = match listed {
                Ok(listed) => listed,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.steps[0] = StepState::Failed(format!("{err:#}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let prompt = rescope::search_prompt(&spec_root, &files);
            if this
                .update(cx, |this, cx| {
                    this.steps = [StepState::Done, StepState::Running];
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let (lines, mut streamed) = futures::channel::mpsc::unbounded::<String>();
            let search = cx.background_spawn(async move {
                let reply = agent(&project_dir, &prompt, &cancel, &|line| {
                    lines.unbounded_send(line).ok();
                })?;
                rescope::parse_reply(&reply)
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
            this.update(cx, |this, cx| this.found(found, cx)).ok();
        });
    }

    /// Folds lines the harness streamed into its reply.
    fn stream_lines(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        for line in lines {
            let events = serde_json::from_str::<serde_json::Value>(&line)
                .map(|event| harness::parse(&event))
                .unwrap_or_default();
            for event in std::iter::once(HarnessEvent::Output(line)).chain(events) {
                if let Some(error) = self.reply.apply(event) {
                    self.reply.push_error(error);
                }
            }
        }
        cx.notify();
    }

    fn found(&mut self, found: anyhow::Result<Vec<Concept>>, cx: &mut Context<Self>) {
        match found {
            Ok(concepts) => {
                self.steps[1] = StepState::Done;
                self.checked = concepts
                    .iter()
                    .enumerate()
                    .filter(|(_, concept)| concept.worth >= CHECKED_WORTH)
                    .map(|(ix, _)| ix)
                    .collect();
                self.selected = (!concepts.is_empty()).then_some(0);
                self.concepts = Some(concepts);
            }
            Err(err) => {
                let why = format!("{err:#}");
                if !self.reply.is_done()
                    && let Some(error) = self.reply.apply(HarnessEvent::Failed(why.clone()))
                {
                    self.reply.push_error(error);
                }
                self.steps[1] = StepState::Failed(why.into());
            }
        }
        cx.notify();
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.details_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    pub fn set_checked(&mut self, ix: usize, checked: bool, cx: &mut Context<Self>) {
        if checked {
            self.checked.insert(ix);
        } else {
            self.checked.remove(&ix);
        }
        cx.notify();
    }

    /// Hands the prompt refactoring the checked concepts on to be sent.
    pub fn refactor(&mut self, cx: &mut Context<Self>) {
        let Some(concepts) = &self.concepts else {
            return;
        };
        let chosen: Vec<Concept> = self
            .checked
            .iter()
            .filter_map(|ix| concepts.get(*ix).cloned())
            .collect();
        if !chosen.is_empty() {
            cx.emit(RefactorConcepts(rescope::refactor_prompt(&chosen)));
        }
    }

    fn step_heading(&self, ix: usize, label: &'static str, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let state = &self.steps[ix];
        let icon: AnyElement = match state {
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
        v_flex()
            .id(("rescope-step", ix))
            .gap_0p5()
            .child(
                h_flex().gap_2().child(icon).child(
                    div()
                        .when(*state == StepState::Pending, |label| {
                            label.text_color(theme.muted_foreground)
                        })
                        .child(label),
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
            .map(gpui_kit::TestSupportExt::test_support)
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .h(px(44.))
            .gap_3()
            .pl_4()
            .pr_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::Shapes)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().font_semibold().child("Rescope"))
            .child(div().flex_1())
            .child(
                Button::new("rescope-again")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Look again")
                    .tooltip("Look through the spec for repeated concepts afresh")
                    .disabled(self.is_running())
                    .on_click(cx.listener(|this, _, _, cx| this.look(cx))),
            )
            .child(
                Button::new("rescope-minimize")
                    .ghost()
                    .small()
                    .icon(IconName::Minus)
                    .tooltip("Minimize; the search keeps running")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MinimizeRescope))),
            )
            .child(
                Button::new("rescope-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close, stopping any search still running")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel.cancel();
                        cx.emit(CloseRescope);
                    })),
            )
    }

    fn render_looking(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let this = cx.entity().downgrade();
        let reply_of = task_table::reply_of({
            let this = this.clone();
            move |cx| Some(&this.upgrade()?.read(cx).reply)
        });
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                this.output_locked = locked;
                if locked {
                    this.output_table.scroll_to_end();
                }
                cx.notify();
            })
            .ok();
        });
        let output = self.output_table.render(
            &self.reply,
            reply_of,
            TableView {
                id: "rescope-output".into(),
                scrollbar: "rescope-output".into(),
                table: OUTPUT_IX,
                open: None,
                steps: None,
                lock: Some((self.output_locked, toggle)),
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
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.step_heading(0, "Reading the spec", cx))
                    .child(self.step_heading(1, "Looking for repeated concepts", cx)),
            )
            .child(div().flex_1().min_h_0().child(output))
            .into_any_element()
    }

    fn render_list(&self, concepts: &[Concept], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rows = concepts.iter().enumerate().map(|(ix, concept)| {
            let selected = self.selected == Some(ix);
            let row = h_flex()
                .id(("rescope-row", ix))
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .when(selected, |row| row.bg(theme.list_active))
                .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
                .child(
                    checkbox(("rescope-check", ix), "")
                        .checked(self.checked.contains(&ix))
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_checked(ix, *checked, cx)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(concept.name.clone()),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(
                            div()
                                .w(px(48.))
                                .h(px(4.))
                                .rounded_full()
                                .bg(theme.muted)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(theme.primary)
                                        .w(relative(concept.worth)),
                                ),
                        )
                        .child(
                            div()
                                .w(px(36.))
                                .text_right()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}%", percent(concept.worth))),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(132.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} places · {} scopes",
                            concept.places.len(),
                            concept.scope_count()
                        )),
                );
            gpui_kit::TestSupportExt::test_support(row)
        });
        let list = v_flex()
            .id("rescope-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .py_1()
            .children(rows);
        scrollbar::with_scrollbar("rescope-list", &self.list_scroll, list, true, None, cx)
    }

    fn render_details(&self, concepts: &[Concept], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(concept) = self.selected.and_then(|ix| concepts.get(ix)) else {
            return div()
                .p_6()
                .text_color(theme.muted_foreground)
                .child("Select a concept to see where the spec describes it.")
                .into_any_element();
        };
        let places = concept.places.iter().enumerate().map(|(ix, place)| {
            v_flex()
                .id(("rescope-place", ix))
                .gap_0p5()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_none().font_medium().child(place.scope.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(place.file.clone()),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .child(place.excerpt.clone()),
                )
                .map(gpui_kit::TestSupportExt::test_support)
        });
        let body = v_flex()
            .id("rescope-details")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.details_scroll)
            .gap_2()
            .p_4()
            .child(div().text_lg().font_semibold().child(concept.name.clone()))
            .child(div().child(concept.concept.clone()))
            .when(!concept.why.is_empty(), |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(concept.why.clone()),
                )
            })
            .child(
                div()
                    .pt_2()
                    .font_medium()
                    .child("Where the spec describes it"),
            )
            .children(places);
        scrollbar::with_scrollbar(
            "rescope-details",
            &self.details_scroll,
            body,
            true,
            None,
            cx,
        )
    }

    fn render_results(&self, concepts: &[Concept], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let count = self.checked.len();
        let body = if concepts.is_empty() {
            div()
                .id("rescope-none")
                .size_full()
                .p_6()
                .text_color(theme.muted_foreground)
                .child("The spec repeats no concepts worth a scope of their own.")
                .map(gpui_kit::TestSupportExt::test_support)
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .child(
                    div()
                        .w(relative(0.5))
                        .h_full()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(self.render_list(concepts, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_details(concepts, cx)),
                )
                .into_any_element()
        };
        let theme = cx.theme();
        let footer = h_flex()
            .flex_none()
            .gap_3()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(match count {
                        0 => "No concepts checked".to_string(),
                        1 => "1 concept checked".to_string(),
                        n => format!("{n} concepts checked"),
                    }),
            )
            .child(
                Button::new("rescope-refactor")
                    .primary()
                    .label("Refactor")
                    .tooltip(
                        "Have the harness refactor each checked concept into a scope of its own",
                    )
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.refactor(cx))),
            );
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(body))
            .child(footer)
            .into_any_element()
    }
}

impl Render for RescopeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.concepts.clone() {
            Some(concepts) => self.render_results(&concepts, cx),
            None => self.render_looking(cx),
        };
        let view = v_flex()
            .id("rescope")
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
    use std::time::Duration;

    use anyhow::{Result, bail};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{RefactorConcepts, RescopeView};
    use crate::divergence::Cancel;

    /// Finds tooltips in two scopes, worth checking, and spinners, not.
    pub fn agent(_: &Path, prompt: &str, _: &Cancel, on_line: &dyn Fn(String)) -> Result<String> {
        assert!(prompt.contains("spec/a/index.pi"), "{prompt}");
        on_line(r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#.into());
        Ok(r#"{"concepts": [
            {"name": "SpinnerScope", "concept": "A spinner.", "worth": 0.3,
             "places": [{"file": "spec/a/index.pi", "scope": "AScope", "excerpt": "spins"}]},
            {"name": "TooltipScope", "concept": "A tooltip on hover.", "worth": 0.8, "why": "Drifted.",
             "places": [
                {"file": "spec/a/index.pi", "scope": "AScope", "excerpt": "shows a tooltip"},
                {"file": "spec/b/index.pi", "scope": "BScope", "excerpt": "a tooltip too"}
             ]}
        ]}"#
        .into())
    }

    fn failing(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> Result<String> {
        bail!("the harness stopped")
    }

    fn nothing(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> Result<String> {
        Ok(r#"{"concepts": []}"#.into())
    }

    fn open(
        cx: &mut TestAppContext,
        dir: &Path,
        run: crate::divergence::RunAgent,
    ) -> (gpui_kit::Entity<RescopeView>, gpui_kit::AnyWindowHandle) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::project_directory::ProjectDirectory::set(dir.to_path_buf(), cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let rescope = cx.new(|cx| RescopeView::with_agent(dir.to_path_buf(), run, cx));
            view = Some(rescope.clone());
            Root::new(rescope, window, cx)
        });
        (view.unwrap(), window.into())
    }

    /// The concepts found are ranked, those worth half or more checked and the
    /// first selected with where it's described; Refactor hands on a prompt
    /// for the checked concepts only.
    #[gpui_kit::test]
    async fn ranks_concepts_and_refactors_those_checked(cx: &mut TestAppContext) {
        let (dir, _) = crate::generate_skills::fixture("rescope");
        let (view, handle) = open(cx, &dir, agent);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("rescope-refactor").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            let names: Vec<&str> = view
                .concepts()
                .unwrap()
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            assert_eq!(names, ["TooltipScope", "SpinnerScope"]);
            assert_eq!(view.checked().iter().collect::<Vec<_>>(), [&0]);
            assert!(!view.is_running());
        });
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let sent = sent.clone();
            cx.subscribe(&view, move |_, RefactorConcepts(prompt), _| {
                sent.borrow_mut().push(prompt.clone())
            })
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            assert!(window.try_find(("rescope-place", 1usize)).is_some());
            window.click("rescope-refactor", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let sent = sent.borrow();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("TooltipScope: A tooltip on hover."));
        assert!(!sent[0].contains("SpinnerScope"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A harness that fails says why and offers nothing; one that finds
    /// nothing says so.
    #[gpui_kit::test]
    async fn failing_or_finding_nothing_is_said(cx: &mut TestAppContext) {
        let (dir, _) = crate::generate_skills::fixture("rescope-failed");
        let (view, _) = open(cx, &dir, failing);
        for _ in 0..50 {
            cx.run_until_parked();
            if view.read_with(cx, |view, _| view.step_failed()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        view.read_with(cx, |view, _| {
            assert!(view.step_failed());
            assert!(view.concepts().is_none());
            assert!(!view.is_running());
        });

        let (_, handle) = open(cx, &dir, nothing);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("rescope-none").is_some()
        })
        .await;
        std::fs::remove_dir_all(&dir).ok();
    }
}
