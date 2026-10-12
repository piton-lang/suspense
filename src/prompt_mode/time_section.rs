//! An open task's Time section in the Tasks tab, as its time breakdown
//! says: a bar split by the kinds its time went to, a legend of them,
//! largest first, and a timeline of its phases and tool calls.

use super::*;
use super::tasks_tab::section;
use crate::time_breakdown::{Breakdown, Kind, Timed};

/// How tall the bar is, and how rounded.
const BAR_HEIGHT: Pixels = px(12.);
const BAR_RADIUS: Pixels = px(3.);

/// How tall the timeline is, and how far its bars sit in from its edges.
const TIMELINE_HEIGHT: Pixels = px(24.);
const TIMELINE_INSET: Pixels = px(4.);

/// How strongly a segment shows while another kind is hovered.
const UNHOVERED: f32 = 0.35;

fn elapsed(ms: u64) -> String {
    crate::subagents::format_elapsed(Duration::from_millis(ms))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// Hovering a kind's segment or legend line shows it, the others lighter.
fn hover(kind: Kind, cx: &mut Context<PromptMode>) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |this: &mut PromptMode, on: &bool, _, cx| {
        if *on || this.tasks_tab.time_hover == Some(kind) {
            this.tasks_tab.time_hover = on.then_some(kind);
            cx.notify();
        }
    })
}

impl PromptMode {
    /// The Time section of the task open at `ix`: a chain's steps' together,
    /// any other task's its own; none before it has started.
    pub(super) fn render_time_section(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let task = self.tasks.get(ix)?;
        let members: Vec<usize> = if task.sent.mode == Some(SendMode::Both) {
            self.chain_step_ixs(ix).into_iter().map(|(ix, _)| ix).collect()
        } else {
            vec![ix]
        };
        let steps: Vec<Vec<Timed>> = members
            .iter()
            .filter_map(|&ix| self.tasks.get(ix))
            .map(|task| crate::time_breakdown::lock(&task.timing).clone())
            .filter(|times| !times.is_empty())
            .collect();
        let muted = cx.theme().muted_foreground;
        let Some(breakdown) = Breakdown::of_steps(&steps, now_ms()).filter(|b| b.total() > 0) else {
            // From before its phases were kept: its total alone.
            let took = task.took()?;
            return Some(
                section("Time", cx)
                    .child(
                        gpui_kit::TestSupportExt::test_support(div().id("time-not-recorded"))
                            .text_xs()
                            .text_color(muted)
                            .child(format!(
                                "{} · its breakdown wasn't recorded",
                                crate::subagents::format_elapsed(took.time)
                            )),
                    )
                    .into_any_element(),
            );
        };
        let colors = crate::theme::chart_colors(cx.theme().is_dark());
        let hovered = self.tasks_tab.time_hover;
        let total = breakdown.total();
        let share = |time: u64| (time as f64 * 100. / total as f64).round() as u64;
        let shade = |kind: Kind| {
            let color = colors[kind.color_index()];
            match hovered {
                Some(on) if on != kind => color.opacity(UNHOVERED),
                _ => color,
            }
        };
        let tip = |kind: Kind| -> SharedString {
            let time = breakdown.times[kind.index()];
            let mut text = format!("{} {} · {}%", kind.label(), elapsed(time), share(time));
            let calls = &breakdown.calls[kind.index()];
            if kind.counts_calls() && calls.count > 0 {
                let plural = if calls.count == 1 { "call" } else { "calls" };
                text.push_str(&format!("\n{} {plural}", calls.count));
                if let Some((name, took)) = &calls.slowest {
                    text.push_str(&format!(" · slowest {name}, {}", elapsed(*took)));
                }
            }
            text.into()
        };

        // The bar, a segment per kind, in order, as wide as its share.
        let mut bar = gpui_kit::TestSupportExt::test_support(h_flex().id("time-bar"))
            .w_full()
            .h(BAR_HEIGHT)
            .gap(px(1.))
            .rounded(BAR_RADIUS)
            .overflow_hidden();
        for kind in Kind::ALL {
            let time = breakdown.times[kind.index()];
            if time == 0 {
                continue;
            }
            let text = tip(kind);
            bar = bar.child(
                gpui_kit::TestSupportExt::test_support(div().id(("time-segment", kind.index())))
                    .h_full()
                    .min_w(px(1.))
                    .w(relative(time as f32 / total as f32))
                    .bg(shade(kind))
                    .on_hover(hover(kind, cx))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    }),
            );
        }

        // The legend, largest first, the largest in the text colour.
        let text_color = cx.theme().foreground;
        let legend = v_flex().w_full().gap_0p5().children(
            breakdown.by_time().into_iter().enumerate().map(|(at, (kind, time))| {
                let text = tip(kind);
                gpui_kit::TestSupportExt::test_support(h_flex().id(("time-legend", kind.index())))
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .font_features(crate::subagents::tabular_figures())
                    .text_color(if at == 0 { text_color } else { muted })
                    .child(div().flex_none().size(px(8.)).bg(shade(kind)))
                    .child(kind.label())
                    .child(elapsed(time))
                    .child(div().text_color(muted).child(format!("· {}%", share(time))))
                    .on_hover(hover(kind, cx))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    })
            }),
        );

        // The timeline: each stretch at its place, where each step began
        // marked, with its span at either end.
        let span = (breakdown.end - breakdown.start).max(1) as f32;
        let at = |time: u64| relative((time.saturating_sub(breakdown.start)) as f32 / span);
        let mut timeline = gpui_kit::TestSupportExt::test_support(div().id("time-timeline"))
            .relative()
            .w_full()
            .h(TIMELINE_HEIGHT)
            .rounded(BAR_RADIUS)
            .bg(cx.theme().secondary);
        for (n, segment) in breakdown.segments.iter().enumerate() {
            let text: SharedString = format!("{}, {}", segment.label, elapsed(segment.ended - segment.began)).into();
            let tool = segment.tool.clone();
            let owner = tool.as_deref().and_then(|tool| {
                members
                    .iter()
                    .copied()
                    .find(|&ix| self.tasks.get(ix).is_some_and(|task| task.reply.row_of_tool(tool).is_some()))
            });
            timeline = timeline.child(
                gpui_kit::TestSupportExt::test_support(div().id(("time-stretch", n)))
                    .absolute()
                    .top(TIMELINE_INSET)
                    .bottom(TIMELINE_INSET)
                    .left(at(segment.began))
                    .w(relative((segment.ended - segment.began) as f32 / span))
                    .min_w(px(1.))
                    .bg(shade(segment.kind))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    })
                    .when_some(tool.zip(owner), |stretch, (tool, owner)| {
                        stretch.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            this.open_output_at_tool(owner, &tool, cx)
                        }))
                    }),
            );
        }
        for (n, &began) in breakdown.steps.iter().enumerate() {
            timeline = timeline.child(
                gpui_kit::TestSupportExt::test_support(div().id(("time-step-mark", n)))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(at(began))
                    .w(px(1.))
                    .bg(text_color),
            );
        }
        let ends = h_flex()
            .w_full()
            .justify_between()
            .text_xs()
            .text_color(muted)
            .font_features(crate::subagents::tabular_figures())
            .child(elapsed(0))
            .child(elapsed(breakdown.end - breakdown.start));
        // How long it waited in the queue, apart from the chart.
        let queued = (breakdown.queued > 0).then(|| {
            gpui_kit::TestSupportExt::test_support(div().id("time-queued"))
                .text_xs()
                .text_color(muted)
                .child(format!("Queued {} before it started", elapsed(breakdown.queued)))
        });

        Some(
            gpui_kit::TestSupportExt::test_support(section("Time", cx).id("time-section"))
                .child(bar)
                .child(legend)
                .child(div().pt_1().child(timeline))
                .child(ends)
                .children(queued)
                .into_any_element(),
        )
    }

    /// Opens the task at `ix`'s output, scrolled to the row of its tool call
    /// `id`.
    pub(super) fn open_output_at_tool(&mut self, ix: usize, id: &str, cx: &mut Context<Self>) {
        self.open_task(ix, cx);
        let Some(name) = self.tasks.get(ix).map(|task| task.name.clone()) else {
            return;
        };
        let mut in_tab = false;
        for tab in &mut self.files {
            if let BodyTab::Task(tab) = tab
                && tab.name == name
            {
                tab.locked = false;
                tab.table.reveal_tool(id);
                in_tab = true;
            }
        }
        if !in_tab {
            self.output_table.reveal_tool(id);
        }
        cx.notify();
    }
}
