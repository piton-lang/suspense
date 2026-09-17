//! The Generate Skills panel: maps the spec's scopes, has the harness rank
//! them for skills while its output shows, then lists them ranked, each with
//! its connections, to check the ones to write skills for and create them.

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
use crate::generate_skills::{self, Candidate, SpecMap};
use crate::harness::{self, HarnessEvent};
use crate::prompt_mode::{Reply, output_table};
use crate::scrollbar::{self, SetLock};

actions!(suspense, [GenerateSkills]);

/// Emitted when the panel is closed.
pub struct CloseGenerateSkills;

/// Where the agent's output table numbers from, apart from others'.
const OUTPUT_IX: usize = usize::MAX / 8;

/// How a step is going.
#[derive(Clone, Debug, PartialEq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed(SharedString),
}

pub struct GenerateSkillsView {
    project_dir: PathBuf,
    agent: RunAgent,
    /// Mapping the spec, and ranking it.
    steps: [StepState; 2],
    map: Option<SpecMap>,
    reply: Reply,
    output_scroll: ScrollHandle,
    output_locked: bool,
    /// The scopes offered, once ranked, and which are checked and selected.
    candidates: Option<Vec<Candidate>>,
    checked: BTreeSet<String>,
    selected: Option<String>,
    /// Notes about the results: a harness that couldn't rank, or what
    /// creating skills did.
    notes: Vec<String>,
    list_scroll: ScrollHandle,
    details_scroll: ScrollHandle,
    focus_handle: FocusHandle,
    cancel: Arc<Cancel>,
    _run: Task<()>,
}

impl EventEmitter<CloseGenerateSkills> for GenerateSkillsView {}

impl Focusable for GenerateSkillsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for GenerateSkillsView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl GenerateSkillsView {
    pub fn new(project_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_agent(project_dir, divergence::run_agent, cx)
    }

    /// As [`Self::new`], ranking with `agent`.
    pub fn with_agent(project_dir: PathBuf, agent: RunAgent, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            project_dir,
            agent,
            steps: [StepState::Pending, StepState::Pending],
            map: None,
            reply: Reply::default(),
            output_scroll: ScrollHandle::new(),
            output_locked: true,
            candidates: None,
            checked: BTreeSet::new(),
            selected: None,
            notes: Vec::new(),
            list_scroll: ScrollHandle::new(),
            details_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            cancel: Arc::default(),
            _run: Task::ready(()),
        };
        this.rank(cx);
        this
    }

    #[cfg(test)]
    pub fn candidates(&self) -> Option<&[Candidate]> {
        self.candidates.as_deref()
    }

    #[cfg(test)]
    pub fn checked(&self) -> &BTreeSet<String> {
        &self.checked
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    #[cfg(test)]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    fn running(&self) -> bool {
        self.candidates.is_none() && !matches!(self.steps[0], StepState::Failed(_))
    }

    /// Maps the spec and ranks it afresh.
    pub fn rank(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = Arc::default();
        self.steps = [StepState::Running, StepState::Pending];
        self.map = None;
        self.reply = Reply::default();
        self.output_locked = true;
        self.candidates = None;
        self.checked.clear();
        self.selected = None;
        self.notes.clear();
        cx.notify();

        let (project_dir, agent, cancel) =
            (self.project_dir.clone(), self.agent, self.cancel.clone());
        self._run = cx.spawn(async move |this, cx| {
            let mapped = cx
                .background_spawn({
                    let project_dir = project_dir.clone();
                    async move {
                        let (spec_root, files) = divergence::list_spec_files(&project_dir)?;
                        anyhow::Ok(generate_skills::map(&project_dir, &spec_root, &files))
                    }
                })
                .await;
            let map = match mapped {
                Ok(map) => map,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.steps[0] = StepState::Failed(format!("{err:#}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let prompt = generate_skills::prompt(&map);
            if this
                .update(cx, |this, cx| {
                    this.map = Some(map.clone());
                    this.steps = [StepState::Done, StepState::Running];
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let (lines, mut streamed) = futures::channel::mpsc::unbounded::<String>();
            let ranking = cx.background_spawn(async move {
                let reply = agent(&project_dir, &prompt, &cancel, &|line| {
                    lines.unbounded_send(line).ok();
                })?;
                generate_skills::parse_reply(&reply)
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
            let ranked = ranking.await;
            this.update(cx, |this, cx| this.ranked(map, ranked, cx))
                .ok();
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

    /// Takes the harness's ranking, or, when it failed, ranks by connections
    /// alone.
    fn ranked(
        &mut self,
        map: SpecMap,
        ranked: anyhow::Result<Vec<generate_skills::Ranked>>,
        cx: &mut Context<Self>,
    ) {
        let ranked = match ranked {
            Ok(ranked) => {
                self.steps[1] = StepState::Done;
                ranked
            }
            Err(err) => {
                let why = format!("{err:#}");
                if !self.reply.is_done()
                    && let Some(error) = self.reply.apply(HarnessEvent::Failed(why.clone()))
                {
                    self.reply.push_error(error);
                }
                self.steps[1] = StepState::Failed(why.into());
                self.notes.push(
                    "The harness couldn't rank the scopes, so they're ranked by how connected they are."
                        .into(),
                );
                Vec::new()
            }
        };
        let candidates = generate_skills::candidates(&map, &ranked);
        self.checked = candidates
            .iter()
            .filter(|candidate| candidate.should && !map.scopes[&candidate.name].has_skill)
            .map(|candidate| candidate.name.clone())
            .collect();
        self.selected = candidates.first().map(|candidate| candidate.name.clone());
        self.map = Some(map);
        self.candidates = Some(candidates);
        cx.notify();
    }

    pub fn select(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(ix) = self
            .candidates
            .as_ref()
            .and_then(|candidates| candidates.iter().position(|c| c.name == name))
        {
            self.list_scroll.scroll_to_item(ix);
        }
        self.selected = Some(name);
        self.details_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    pub fn set_checked(&mut self, name: String, checked: bool, cx: &mut Context<Self>) {
        if checked {
            self.checked.insert(name);
        } else {
            self.checked.remove(&name);
        }
        cx.notify();
    }

    /// Writes a skill for each scope checked, noting how it went.
    pub fn create_skills(&mut self, cx: &mut Context<Self>) {
        let Some(map) = &self.map else {
            return;
        };
        let names: Vec<String> = self.checked.iter().cloned().collect();
        if names.is_empty() {
            return;
        }
        let results = generate_skills::write_skills(&self.project_dir, map, &names);
        let map = self.map.as_mut().unwrap();
        let mut created = 0;
        self.notes.clear();
        for (name, result) in results {
            match result {
                Ok(_) => {
                    created += 1;
                    if let Some(scope) = map.scopes.get_mut(&name) {
                        scope.has_skill = true;
                    }
                    self.checked.remove(&name);
                }
                Err(err) => self
                    .notes
                    .push(format!("Couldn't create the skill for {name}: {err:#}")),
            }
        }
        if created > 0 {
            self.notes.insert(
                0,
                format!(
                    "Created {created} skill{}. Build the spec for them to be written for the agents.",
                    if created == 1 { "" } else { "s" }
                ),
            );
        }
        cx.notify();
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
            .id(("generate-skills-step", ix))
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
                Icon::new(IconName::Sparkles)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().font_semibold().child("Generate Skills"))
            .child(div().flex_1())
            .child(
                Button::new("generate-skills-again")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Rank again")
                    .tooltip("Map and rank the spec's scopes afresh")
                    .disabled(self.running())
                    .on_click(cx.listener(|this, _, _, cx| this.rank(cx))),
            )
            .child(
                Button::new("generate-skills-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close, stopping any ranking still running")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel.cancel();
                        cx.emit(CloseGenerateSkills);
                    })),
            )
    }

    fn render_running(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if self.output_locked {
            self.output_scroll.scroll_to_bottom();
        }
        let output = div()
            .id("generate-skills-output")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.output_scroll)
            .px_4()
            .pb_3()
            .child(output_table(OUTPUT_IX, &self.reply, None, None, cx))
            .map(gpui_kit::TestSupportExt::test_support);
        let this = cx.entity().downgrade();
        let toggle: SetLock = Rc::new(move |locked, _, cx| {
            this.update(cx, |this, cx| {
                this.output_locked = locked;
                if locked {
                    this.output_scroll.scroll_to_bottom();
                }
                cx.notify();
            })
            .ok();
        });
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
                    .child(self.step_heading(0, "Mapping the spec", cx))
                    .child(self.step_heading(1, "Ranking scopes for skills", cx)),
            )
            .child(div().flex_1().min_h_0().child(scrollbar::with_scrollbar(
                "generate-skills-output",
                &self.output_scroll,
                output,
                true,
                Some((self.output_locked, toggle)),
                cx,
            )))
            .into_any_element()
    }

    fn render_list(
        &self,
        map: &SpecMap,
        candidates: &[Candidate],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let rows = candidates.iter().enumerate().map(|(ix, candidate)| {
            let scope = &map.scopes[&candidate.name];
            let selected = self.selected.as_deref() == Some(candidate.name.as_str());
            let name = candidate.name.clone();
            let check = {
                let name = name.clone();
                checkbox(("generate-skills-check", ix), "")
                    .checked(self.checked.contains(&candidate.name))
                    .disabled(scope.has_skill)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_checked(name.clone(), *checked, cx)
                    }))
            };
            let worth = match candidate.worth {
                Some(worth) => h_flex()
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
                                    .w(relative(worth)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(36.))
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{}%", percent(worth))),
                    )
                    .into_any_element(),
                None => div().flex_none().w(px(92.)).into_any_element(),
            };
            let verdict = if scope.has_skill {
                "Has a skill"
            } else if candidate.should {
                "Should"
            } else {
                "Could"
            };
            let row = h_flex()
                .id(("generate-skills-row", ix))
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .when(selected, |row| row.bg(theme.list_active))
                .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(name.clone(), cx)))
                .child(check)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(candidate.name.clone()),
                )
                .child(worth)
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} ← · {} →",
                            scope.used_by.len(),
                            scope.uses.len()
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(72.))
                        .text_xs()
                        .when(!scope.has_skill && candidate.should, |label| {
                            label.text_color(theme.success)
                        })
                        .when(scope.has_skill || !candidate.should, |label| {
                            label.text_color(theme.muted_foreground)
                        })
                        .child(verdict),
                );
            gpui_kit::TestSupportExt::test_support(row)
        });
        let list = v_flex()
            .id("generate-skills-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .py_1()
            .children(rows);
        scrollbar::with_scrollbar(
            "generate-skills-list",
            &self.list_scroll,
            list,
            true,
            None,
            cx,
        )
    }

    fn render_details(
        &self,
        map: &SpecMap,
        candidates: &[Candidate],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(candidate) = self
            .selected
            .as_ref()
            .and_then(|name| candidates.iter().find(|c| &c.name == name))
        else {
            return div()
                .p_6()
                .text_color(theme.muted_foreground)
                .child("Select a scope to see how it's connected.")
                .into_any_element();
        };
        let scope = &map.scopes[&candidate.name];
        let connections = |id: &'static str,
                           title: &str,
                           of: &std::collections::BTreeMap<String, usize>,
                           cx: &mut Context<Self>| {
            let theme = cx.theme();
            let mut sorted: Vec<(&String, &usize)> = of.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let most = sorted.first().map_or(1, |(_, count)| **count).max(1);
            let rows = sorted.into_iter().enumerate().map(|(ix, (name, count))| {
                let target = name.clone();
                let share = *count as f32 / most as f32;
                let row = h_flex()
                    .id((id, ix))
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(target.clone(), cx)))
                    .child(div().w(px(220.)).flex_none().truncate().child(name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .h(px(4.))
                            .rounded_full()
                            .bg(theme.muted)
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme.primary)
                                    .w(relative(share)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(32.))
                            .text_right()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    );
                gpui_kit::TestSupportExt::test_support(row)
            });
            let empty = of.is_empty();
            v_flex()
                .gap_1()
                .child(
                    div()
                        .pt_2()
                        .font_medium()
                        .child(format!("{title} ({})", of.len())),
                )
                .when(empty, |list| {
                    list.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("None"),
                    )
                })
                .children(rows)
        };
        let used_by = connections("generate-skills-used-by", "Used by", &scope.used_by, cx);
        let uses = connections("generate-skills-uses", "Uses", &scope.uses, cx);
        let theme = cx.theme();
        let body = v_flex()
            .id("generate-skills-details")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.details_scroll)
            .gap_2()
            .p_4()
            .child(div().text_lg().font_semibold().child(scope.name.clone()))
            .child(
                div()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(scope.file.clone()),
            )
            .child(if candidate.why.is_empty() {
                div()
                    .text_color(theme.muted_foreground)
                    .child("The harness didn't rank this scope.")
            } else {
                div().child(candidate.why.clone())
            })
            .child(used_by)
            .child(uses)
            .map(gpui_kit::TestSupportExt::test_support);
        scrollbar::with_scrollbar(
            "generate-skills-details",
            &self.details_scroll,
            body,
            true,
            None,
            cx,
        )
    }

    fn render_results(
        &self,
        map: &SpecMap,
        candidates: &[Candidate],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let count = self.checked.len();
        let notes = self.notes.clone();
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
                        0 => "No scopes checked".to_string(),
                        1 => "1 scope checked".to_string(),
                        n => format!("{n} scopes checked"),
                    }),
            )
            .child(
                Button::new("generate-skills-create")
                    .primary()
                    .label("Create Skills")
                    .tooltip("Add a skill building each checked scope to the spec")
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.create_skills(cx))),
            );
        v_flex()
            .size_full()
            .when(!notes.is_empty(), |view| {
                view.child(
                    v_flex()
                        .id("generate-skills-notes")
                        .flex_none()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_sm()
                        .text_color(theme.warning)
                        .children(notes)
                        .map(gpui_kit::TestSupportExt::test_support),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(relative(0.5))
                            .h_full()
                            .border_r_1()
                            .border_color(theme.border)
                            .child(self.render_list(map, candidates, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_details(map, candidates, cx)),
                    ),
            )
            .child(footer)
            .into_any_element()
    }
}

impl Render for GenerateSkillsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match (self.map.clone(), self.candidates.clone()) {
            (Some(map), Some(candidates)) => self.render_results(&map, &candidates, cx),
            _ => self.render_running(cx),
        };
        let view = v_flex()
            .id("generate-skills")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body));
        gpui_kit::TestSupportExt::test_support(view)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use anyhow::{Result, bail};
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::GenerateSkillsView;
    use crate::divergence::Cancel;

    /// Ranks B as a skill that should be built, and A as one that could.
    fn agent(_: &Path, prompt: &str, _: &Cancel, on_line: &dyn Fn(String)) -> Result<String> {
        assert!(
            prompt.contains("- AScope (spec/a/index.pi): used by 2, uses 0"),
            "{prompt}"
        );
        on_line(r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#.into());
        on_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Ranking."}}}"#.into());
        Ok(r#"{"skills": [
            {"scope": "AScope", "worth": 0.5, "should": false, "why": "Used, but small."},
            {"scope": "BScope", "worth": 0.9, "should": true, "why": "Joins the others."}
        ]}"#
        .into())
    }

    fn failing(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> Result<String> {
        bail!("the harness stopped")
    }

    fn open(
        cx: &mut TestAppContext,
        dir: &Path,
        run: crate::divergence::RunAgent,
    ) -> (
        gpui_kit::Entity<GenerateSkillsView>,
        gpui_kit::AnyWindowHandle,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::project_directory::ProjectDirectory::set(dir.to_path_buf(), cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let skills = cx.new(|cx| GenerateSkillsView::with_agent(dir.to_path_buf(), run, cx));
            view = Some(skills.clone());
            Root::new(skills, window, cx)
        });
        (view.unwrap(), window.into())
    }

    /// The scopes come back ranked, the ones that should have a skill
    /// checked and the first selected with its connections; a connection
    /// selects its scope; and Create Skills writes a skill for each checked
    /// scope, which then has one.
    #[gpui_kit::test]
    async fn ranks_scopes_and_creates_skills(cx: &mut TestAppContext) {
        let (dir, _) = crate::generate_skills::fixture("view");
        let (view, handle) = open(cx, &dir, agent);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("generate-skills-create").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            let names: Vec<&str> = view
                .candidates()
                .unwrap()
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect();
            assert_eq!(names, ["BScope", "AScope", "CScope"]);
            assert_eq!(view.checked().iter().collect::<Vec<_>>(), ["BScope"]);
            assert_eq!(view.selected(), Some("BScope"));
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // B is used by C, and uses A.
            assert!(
                window
                    .try_find(("generate-skills-used-by", 0usize))
                    .is_some()
            );
            window.click(("generate-skills-uses", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |view, _| view.selected().map(str::to_string)),
            Some("AScope".into())
        );

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            window.click("generate-skills-create", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(dir.join("spec/agent/skills/BuildB.pi").is_file());
        view.read_with(cx, |view, _| {
            assert!(view.checked().is_empty());
            assert!(
                view.notes()[0].starts_with("Created 1 skill."),
                "{:?}",
                view.notes()
            );
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A harness that fails still leaves the scopes offered, by connections.
    #[gpui_kit::test]
    async fn a_failed_harness_still_offers_scopes(cx: &mut TestAppContext) {
        let (dir, _) = crate::generate_skills::fixture("view-failed");
        let (view, handle) = open(cx, &dir, failing);
        cx.wait_for(handle, Duration::from_secs(10), |window, _| {
            window.try_find("generate-skills-create").is_some()
        })
        .await;
        view.read_with(cx, |view, _| {
            let names: Vec<&str> = view
                .candidates()
                .unwrap()
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect();
            assert_eq!(names, ["AScope", "BScope", "CScope"]);
            assert!(view.checked().is_empty());
            assert!(view.notes()[0].contains("couldn't rank"));
        });
        std::fs::remove_dir_all(&dir).ok();
    }
}
