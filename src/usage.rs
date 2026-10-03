//! The agent's usage, as the harness reports it while its runs go: the plan
//! limits it has used, and the tokens and cost of the runs of a conversation
//! and of the whole project since it was opened. Nothing here asks the
//! harness for anything; it only adds up what the runs already report.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::chat_input::tokens_label;
use crate::harness::HarnessEvent;

/// Tokens and cost, each `None` until reported.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    /// Input tokens read neither from nor into the cache.
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// In US dollars.
    pub cost: Option<f64>,
}

impl Spend {
    /// Whether nothing at all has been reported.
    pub fn is_empty(&self) -> bool {
        self.tokens().is_none() && self.cost.is_none()
    }

    /// Every token reported, of whatever kind; `None` if none were.
    pub fn tokens(&self) -> Option<u64> {
        [self.input, self.output, self.cache_read, self.cache_write]
            .into_iter()
            .flatten()
            .reduce(|a, b| a + b)
    }

    /// Adds `other` on, figure by figure; a figure reported by neither stays
    /// unreported.
    pub fn add(&mut self, other: &Spend) {
        fn add<T: std::ops::Add<Output = T> + Copy>(a: &mut Option<T>, b: Option<T>) {
            if let Some(b) = b {
                *a = Some(a.map_or(b, |a| a + b));
            }
        }
        add(&mut self.input, other.input);
        add(&mut self.output, other.output);
        add(&mut self.cache_read, other.cache_read);
        add(&mut self.cache_write, other.cache_write);
        add(&mut self.cost, other.cost);
    }

    /// What was spent since `earlier`, figure by figure: a figure `earlier`
    /// didn't report counts whole, and one this doesn't stays unreported.
    pub fn since(&self, earlier: &Spend) -> Spend {
        fn less(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            a.map(|a| a.saturating_sub(b.unwrap_or(0)))
        }
        Spend {
            input: less(self.input, earlier.input),
            output: less(self.output, earlier.output),
            cache_read: less(self.cache_read, earlier.cache_read),
            cache_write: less(self.cache_write, earlier.cache_write),
            cost: self
                .cost
                .map(|cost| (cost - earlier.cost.unwrap_or(0.)).max(0.)),
        }
    }

    /// Takes each figure `other` reports as the total so far.
    pub fn replace(&mut self, other: &Spend) {
        fn replace<T: Copy>(a: &mut Option<T>, b: Option<T>) {
            if b.is_some() {
                *a = b;
            }
        }
        replace(&mut self.input, other.input);
        replace(&mut self.output, other.output);
        replace(&mut self.cache_read, other.cache_read);
        replace(&mut self.cache_write, other.cache_write);
        replace(&mut self.cost, other.cost);
    }
}

/// How what a run reports spending counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tally {
    /// More, on top of what the run reported before, as OpenCode reports
    /// each step.
    More,
    /// The run's totals so far, as Claude Code reports each result.
    Run,
    /// The totals so far of the conversation it carries on, earlier runs of
    /// it included, as Codex reports each turn.
    Conversation,
}

/// One of the plan's usage limits, as the harness reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanLimit {
    /// The harness's name for it, such as Claude Code's `five_hour`.
    pub name: String,
    /// The share of it used, from 0 to 1.
    pub used: f64,
    /// When it resets, in seconds since the Unix epoch.
    pub resets_at: Option<u64>,
}

impl PlanLimit {
    /// What it is called where it is shown, as Claude Code's /usage calls it.
    pub fn label(&self) -> String {
        match self.name.as_str() {
            "five_hour" => "Current session".into(),
            "seven_day" => "Current week".into(),
            "seven_day_opus" => "Current week (Opus)".into(),
            "seven_day_sonnet" => "Current week (Sonnet)".into(),
            name => {
                let words = name.replace(['_', '-'], " ");
                let mut chars = words.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
        }
    }

    /// The share used, as a whole percentage.
    pub fn percent(&self) -> u32 {
        (self.used * 100.).round().clamp(0., u32::MAX as f64) as u32
    }

    /// Whether it had reset by `now`, so what was used of it no longer holds.
    fn reset_by(&self, now: u64) -> bool {
        self.resets_at.is_some_and(|resets| resets <= now)
    }
}

/// Which conversation a run carried on: the tasks', or the questions'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversation {
    /// The code lane's tasks'.
    Tasks,
    /// The spec lane's tasks', as the HarnessIntegrationScope keeps apart.
    SpecTasks,
    Questions,
}

/// A run of a task or a question, and what it has reported spending.
#[derive(Clone, Debug)]
struct Run {
    conversation: Conversation,
    /// Which of its conversations, counted up each time it is left for a new
    /// one.
    epoch: u64,
    /// The conversation the harness said it is, once it has.
    session: Option<String>,
    /// What its conversation had spent as it began, for a harness that
    /// reports the conversation's totals, once it has.
    before: Option<Spend>,
    spend: Spend,
}

/// What a project's runs have reported since it was opened.
#[derive(Clone, Debug, Default)]
pub struct ProjectUsage {
    runs: Vec<Run>,
    /// The totals a harness last reported of each conversation, by the
    /// conversation it said it is.
    conversations: HashMap<String, Spend>,
    /// The harness and model of the latest report, and when it came.
    harness: Option<Agent>,
    model: Option<String>,
    reported: Option<u64>,
}

impl ProjectUsage {
    /// Counts a run starting in the `epoch`th of `conversation`; its events
    /// are then followed by the run number returned.
    pub fn start_run(&mut self, conversation: Conversation, epoch: u64) -> usize {
        self.runs.push(Run {
            conversation,
            epoch,
            session: None,
            before: None,
            spend: Spend::default(),
        });
        self.runs.len() - 1
    }

    /// Follows what run `run`, of `agent`, reports, at `now`.
    pub fn follow(&mut self, run: usize, agent: Agent, event: &HarnessEvent, now: u64) {
        match event {
            HarnessEvent::Session(session) => {
                if let Some(run) = self.runs.get_mut(run) {
                    run.session = Some(session.clone());
                }
                return;
            }
            HarnessEvent::Spent { spend, tally } => {
                if let Some(run) = self.runs.get_mut(run) {
                    match tally {
                        Tally::More => run.spend.add(spend),
                        Tally::Run => run.spend.replace(spend),
                        // What the conversation spent before this run, as
                        // last reported, isn't this run's. A conversation
                        // carried on from before the project was opened
                        // counts whole, not being known.
                        Tally::Conversation => {
                            let before = match &run.session {
                                Some(session) => self.conversations.insert(session.clone(), *spend),
                                None => None,
                            };
                            let before = run.before.get_or_insert(before.unwrap_or_default());
                            run.spend = spend.since(before);
                        }
                    }
                }
            }
            // The model the figures that follow come from, which isn't a
            // figure itself.
            HarnessEvent::Model(model) => {
                self.harness = Some(agent);
                self.model = Some(model.clone());
                return;
            }
            HarnessEvent::Usage { .. } => {}
            _ => return,
        }
        // A model another harness reported doesn't say where these came from.
        if self.harness != Some(agent) {
            self.model = None;
        }
        self.harness = Some(agent);
        self.reported = Some(now);
    }

    /// What the runs of the `epoch`th of `conversation` spent.
    pub fn conversation(&self, conversation: Conversation, epoch: u64) -> Spend {
        let mut spend = Spend::default();
        for run in &self.runs {
            if run.conversation == conversation && run.epoch == epoch {
                spend.add(&run.spend);
            }
        }
        spend
    }

    /// What every run spent.
    pub fn project(&self) -> Spend {
        let mut spend = Spend::default();
        for run in &self.runs {
            spend.add(&run.spend);
        }
        spend
    }

    pub fn runs(&self) -> usize {
        self.runs.len()
    }

    /// The harness and model of the latest report, and when it came.
    pub fn source(&self) -> (Option<Agent>, Option<String>, Option<u64>) {
        (self.harness, self.model.clone(), self.reported)
    }
}

/// The plan limits each harness last reported, which are the user's, not a
/// project's.
#[derive(Clone, Debug, Default)]
pub struct PlanLimits {
    reported: Vec<(Agent, Vec<PlanLimit>, u64)>,
}

impl PlanLimits {
    /// Follows what a run of `agent` reports, at `now`: a limit it reports
    /// replaces what it last said of that limit.
    pub fn follow(&mut self, agent: Agent, event: &HarnessEvent, now: u64) {
        let HarnessEvent::Limits(limits) = event else {
            return;
        };
        let ix = match self.reported.iter().position(|(of, ..)| *of == agent) {
            Some(ix) => ix,
            None => {
                self.reported.push((agent, Vec::new(), now));
                self.reported.len() - 1
            }
        };
        let (_, known, reported) = &mut self.reported[ix];
        for limit in limits {
            match known.iter_mut().find(|known| known.name == limit.name) {
                Some(known) => *known = limit.clone(),
                None => known.push(limit.clone()),
            }
        }
        *reported = now;
    }

    /// `agent`'s limits that still hold at `now`, and when it last reported
    /// them.
    pub fn of(&self, agent: Agent, now: u64) -> (Vec<PlanLimit>, Option<u64>) {
        self.reported
            .iter()
            .find(|(of, ..)| *of == agent)
            .map(|(_, limits, reported)| {
                (
                    limits
                        .iter()
                        .filter(|limit| !limit.reset_by(now))
                        .cloned()
                        .collect(),
                    Some(*reported),
                )
            })
            .unwrap_or_default()
    }
}

/// How the usage summary is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    /// A plan limit is 80% used or more.
    Warning,
    /// A plan limit is used up.
    Error,
}

/// From 80% of a limit used, the summary warns.
const WARNING_SHARE: f64 = 0.8;

/// Everything the chat input shows of the agent's usage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageReport {
    /// The plan limits of the harness picked.
    pub limits: Vec<PlanLimit>,
    /// Which conversation the selected tab carries on.
    pub conversation: Option<Conversation>,
    /// How many tokens its context holds, once reported.
    pub context: Option<u64>,
    /// What its runs spent since the project was opened.
    pub conversation_spend: Spend,
    /// What every run of the project spent since it was opened, and how many
    /// runs there were.
    pub project_spend: Spend,
    pub runs: usize,
    /// The harness and model the figures came from, and when they were last
    /// reported, in seconds since the Unix epoch.
    pub harness: Option<Agent>,
    pub model: Option<String>,
    pub reported: Option<u64>,
}

impl UsageReport {
    /// Whether nothing has been reported yet.
    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
            && self.context.is_none()
            && self.conversation_spend.is_empty()
            && self.project_spend.is_empty()
    }

    /// The plan limit nearest to running out.
    pub fn nearest_limit(&self) -> Option<&PlanLimit> {
        self.limits.iter().max_by(|a, b| a.used.total_cmp(&b.used))
    }

    /// The summary: the share used of the limit nearest to running out, or
    /// else the project's cost, or else its tokens, or else nothing but
    /// "Usage"; and how it is coloured.
    pub fn summary(&self) -> (String, Level) {
        if let Some(limit) = self.nearest_limit() {
            return (format!("Usage {}%", limit.percent()), Level::of(limit.used));
        }
        let text = match (self.project_spend.cost, self.project_spend.tokens()) {
            (Some(cost), _) => format!("Usage {}", cost_label(cost)),
            (None, Some(tokens)) => format!("Usage {}", tokens_label(tokens)),
            (None, None) => "Usage".into(),
        };
        (text, Level::Normal)
    }
}

impl Level {
    /// Its colour, or `None` for the summary's usual muted text.
    pub fn color(self, cx: &App) -> Option<Hsla> {
        match self {
            Level::Normal => None,
            Level::Warning => Some(cx.theme().warning),
            Level::Error => Some(cx.theme().danger),
        }
    }

    fn of(used: f64) -> Self {
        if used >= 1. {
            Level::Error
        } else if used >= WARNING_SHARE {
            Level::Warning
        } else {
            Level::Normal
        }
    }
}

/// The popover's details, as of `now`: each group headed, each figure
/// labelled, and the groups and figures not reported left out.
pub fn details(report: &UsageReport, now: u64, cx: &App) -> Div {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let heading = |text: &str| {
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .child(text.to_string())
    };
    let figure = |id: &str, label: &str, value: String| {
        gpui_kit::TestSupportExt::test_support(
            h_flex()
                .id(SharedString::from(format!("usage-{id}")))
                .justify_between()
                .gap_4()
                .child(div().text_color(muted).child(label.to_string()))
                .child(div().child(value)),
        )
    };
    let spend_figures = |group: &str, spend: &Spend| {
        [
            ("input", "Input", spend.input.map(tokens_label)),
            ("output", "Output", spend.output.map(tokens_label)),
            (
                "cache-read",
                "Cache read",
                spend.cache_read.map(tokens_label),
            ),
            (
                "cache-written",
                "Cache written",
                spend.cache_write.map(tokens_label),
            ),
            ("cost", "Cost", spend.cost.map(cost_label)),
        ]
        .into_iter()
        .filter_map(|(id, label, value)| Some(figure(&format!("{group}-{id}"), label, value?)))
        .collect::<Vec<_>>()
    };
    let group = |id: &str, title: &str, rows: Vec<AnyElement>| {
        (!rows.is_empty()).then(|| {
            gpui_kit::TestSupportExt::test_support(
                v_flex()
                    .id(SharedString::from(format!("usage-group-{id}")))
                    .gap_1()
                    .child(heading(title))
                    .children(rows),
            )
        })
    };

    let limits = group(
        "limits",
        "Plan limits",
        report
            .limits
            .iter()
            .map(|limit| {
                let fill = Level::of(limit.used).color(cx).unwrap_or(theme.primary);
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_4()
                            .child(div().text_color(muted).child(limit.label()))
                            .child(format!("{}%", limit.percent())),
                    )
                    .child(
                        div()
                            .h(px(4.))
                            .w_full()
                            .rounded_full()
                            .bg(theme.border)
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(fill)
                                    .w(relative(limit.used.clamp(0., 1.) as f32)),
                            ),
                    )
                    .when_some(limit.resets_at, |this, resets| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("Resets {}", until(resets, now))),
                        )
                    })
                    .into_any_element()
            })
            .collect(),
    );
    let conversation_title = match report.conversation {
        Some(Conversation::Questions) => "Conversation (questions)",
        Some(Conversation::Tasks) => "Conversation (code tasks)",
        Some(Conversation::SpecTasks) => "Conversation (spec tasks)",
        None => "Conversation",
    };
    let conversation = group(
        "conversation",
        conversation_title,
        report
            .context
            .map(|context| figure("conversation-context", "Context", tokens_label(context)))
            .into_iter()
            .chain(spend_figures("conversation", &report.conversation_spend))
            .map(IntoElement::into_any_element)
            .collect(),
    );
    let project = group(
        "project",
        "This project",
        spend_figures("project", &report.project_spend)
            .into_iter()
            .chain(
                (!report.project_spend.is_empty())
                    .then(|| figure("project-runs", "Runs", report.runs.to_string())),
            )
            .map(IntoElement::into_any_element)
            .collect(),
    );
    let source = [
        report.harness.map(|harness| harness.label().to_string()),
        report.model.clone(),
        report
            .reported
            .map(|reported| format!("reported {}", crate::divergence::ago(reported, now))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let body = v_flex().gap_3().text_sm();
    if report.is_empty() {
        return body.child(gpui_kit::TestSupportExt::test_support(
            div()
                .id("usage-nothing")
                .text_color(muted)
                .child("Nothing reported yet. Usage shows once the harness reports it."),
        ));
    }
    body.children(limits)
        .children(conversation)
        .children(project)
        .when(!source.is_empty(), |this| {
            this.child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("usage-source")
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(muted)
                    .child(source.join(" · ")),
            ))
        })
}

/// A cost in US dollars, to the cent: "$1.24". Less than a cent, but more
/// than nothing, reads "<$0.01".
pub fn cost_label(cost: f64) -> String {
    if cost > 0. && cost < 0.005 {
        "<$0.01".into()
    } else {
        format!("${cost:.2}")
    }
}

/// How long until `then`, at `now`: "in 2h 14m", "in 3d 4h", "in 12m", or
/// "now" once it has come.
pub fn until(then: u64, now: u64) -> String {
    let seconds = then.saturating_sub(now);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) if seconds == 0 => "now".into(),
        (0, 0, 0) => "in under a minute".into(),
        (0, 0, minutes) => format!("in {minutes}m"),
        (0, hours, minutes) => format!("in {hours}h {minutes}m"),
        (days, hours, _) => format!("in {days}d {hours}h"),
    }
}

/// Seconds since the Unix epoch, now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        Agent, Conversation, HarnessEvent, Level, PlanLimit, PlanLimits, ProjectUsage, Spend,
        Tally, UsageReport, cost_label, until,
    };

    fn limit(name: &str, used: f64, resets_at: Option<u64>) -> PlanLimit {
        PlanLimit {
            name: name.into(),
            used,
            resets_at,
        }
    }

    fn spent(spend: Spend, tally: Tally) -> HarnessEvent {
        HarnessEvent::Spent { spend, tally }
    }

    #[test]
    fn summary_is_the_nearest_limit_then_cost_then_tokens_then_bare() {
        let mut report = UsageReport::default();
        assert_eq!(report.summary(), ("Usage".into(), Level::Normal));
        assert!(report.is_empty());

        report.project_spend.output = Some(42_100);
        assert_eq!(report.summary(), ("Usage 42.1k".into(), Level::Normal));

        report.project_spend.cost = Some(1.2391);
        assert_eq!(report.summary(), ("Usage $1.24".into(), Level::Normal));

        report.limits = vec![
            limit("seven_day", 0.42, None),
            limit("five_hour", 0.1, None),
        ];
        assert_eq!(report.summary(), ("Usage 42%".into(), Level::Normal));

        report.limits[1].used = 0.8;
        assert_eq!(report.summary(), ("Usage 80%".into(), Level::Warning));

        report.limits[0].used = 1.;
        assert_eq!(report.summary(), ("Usage 100%".into(), Level::Error));
    }

    #[test]
    fn cost_and_times_read_shortly() {
        assert_eq!(cost_label(1.2391), "$1.24");
        assert_eq!(cost_label(0.), "$0.00");
        assert_eq!(cost_label(0.001), "<$0.01");
        assert_eq!(until(100, 100), "now");
        assert_eq!(until(130, 100), "in under a minute");
        assert_eq!(until(100 + 12 * 60, 100), "in 12m");
        assert_eq!(until(2 * 3_600 + 14 * 60, 0), "in 2h 14m");
        assert_eq!(until(3 * 86_400 + 4 * 3_600 + 59, 0), "in 3d 4h");
        assert_eq!(limit("five_hour", 0.1, None).label(), "Current session");
        assert_eq!(limit("seven_day", 0.1, None).label(), "Current week");
        assert_eq!(limit("monthly_cap", 0.1, None).label(), "Monthly cap");
    }

    /// A figure no run reported stays unreported, rather than counting as
    /// nothing; figures reported as totals replace what the run reported
    /// before, and others add on.
    #[test]
    fn runs_add_up_by_conversation_and_project_leaving_out_the_unreported() {
        let mut usage = ProjectUsage::default();
        let first = usage.start_run(Conversation::Tasks, 0);
        let question = usage.start_run(Conversation::Questions, 0);
        usage.follow(
            first,
            Agent::Claude,
            &spent(
                Spend {
                    input: Some(10),
                    output: Some(64),
                    cost: Some(0.01),
                    ..Spend::default()
                },
                Tally::Run,
            ),
            5,
        );
        // Its second result's totals include the first's.
        usage.follow(
            first,
            Agent::Claude,
            &spent(
                Spend {
                    input: Some(20),
                    output: Some(125),
                    cost: Some(0.015),
                    ..Spend::default()
                },
                Tally::Run,
            ),
            6,
        );
        usage.follow(
            question,
            Agent::Codex,
            &spent(
                Spend {
                    input: Some(5),
                    cache_read: Some(100),
                    ..Spend::default()
                },
                Tally::More,
            ),
            7,
        );
        usage.follow(
            question,
            Agent::Codex,
            &spent(
                Spend {
                    input: Some(5),
                    ..Spend::default()
                },
                Tally::More,
            ),
            8,
        );

        let tasks = usage.conversation(Conversation::Tasks, 0);
        assert_eq!(tasks.input, Some(20));
        assert_eq!(tasks.output, Some(125));
        assert_eq!(tasks.cost, Some(0.015));
        assert_eq!(tasks.cache_read, None);
        assert_eq!(tasks.cache_write, None);

        let questions = usage.conversation(Conversation::Questions, 0);
        assert_eq!(questions.input, Some(10));
        assert_eq!(questions.cache_read, Some(100));
        assert_eq!(questions.output, None);
        assert_eq!(questions.cost, None);

        // A conversation left for a new one has spent nothing yet.
        assert!(usage.conversation(Conversation::Tasks, 1).is_empty());

        let project = usage.project();
        assert_eq!(project.input, Some(30));
        assert_eq!(project.cache_read, Some(100));
        assert_eq!(project.cost, Some(0.015));
        assert_eq!(project.cache_write, None);
        assert_eq!(usage.runs(), 2);
        assert_eq!(usage.harness, Some(Agent::Codex));
        assert_eq!(usage.reported, Some(8));
    }

    /// Codex reports its thread's totals, earlier runs of it included, so a
    /// run of it counts only what was spent since the thread's totals were
    /// last reported; a thread not reported since the project was opened
    /// counts whole.
    #[test]
    fn a_conversations_totals_count_only_what_each_run_added() {
        let thread = |input: u64, cache_read: u64| {
            spent(
                Spend {
                    input: Some(input),
                    output: Some(input / 10),
                    cache_read: Some(cache_read),
                    ..Spend::default()
                },
                Tally::Conversation,
            )
        };
        let mut usage = ProjectUsage::default();
        let first = usage.start_run(Conversation::Tasks, 0);
        usage.follow(first, Agent::Codex, &HarnessEvent::Session("t".into()), 1);
        usage.follow(first, Agent::Codex, &thread(300, 1_000), 2);
        let second = usage.start_run(Conversation::Tasks, 0);
        usage.follow(second, Agent::Codex, &HarnessEvent::Session("t".into()), 3);
        usage.follow(second, Agent::Codex, &thread(500, 4_000), 4);
        // Another thread, begun apart.
        let other = usage.start_run(Conversation::Questions, 0);
        usage.follow(other, Agent::Codex, &HarnessEvent::Session("u".into()), 5);
        usage.follow(other, Agent::Codex, &thread(70, 0), 6);

        assert_eq!(usage.runs[first].spend.input, Some(300));
        assert_eq!(usage.runs[second].spend.input, Some(200));
        assert_eq!(usage.runs[second].spend.output, Some(20));
        assert_eq!(usage.runs[second].spend.cache_read, Some(3_000));
        let tasks = usage.conversation(Conversation::Tasks, 0);
        assert_eq!(tasks.input, Some(500));
        assert_eq!(tasks.cache_read, Some(4_000));
        assert_eq!(tasks.cost, None);
        assert_eq!(usage.project().input, Some(570));
    }

    #[test]
    fn spend_since_leaves_the_unreported_out() {
        let now = Spend {
            input: Some(10),
            output: Some(5),
            cost: Some(0.5),
            ..Spend::default()
        };
        let before = Spend {
            input: Some(4),
            cost: Some(0.75),
            cache_read: Some(9),
            ..Spend::default()
        };
        assert_eq!(
            now.since(&before),
            Spend {
                input: Some(6),
                output: Some(5),
                cost: Some(0.),
                ..Spend::default()
            }
        );
    }

    #[test]
    fn limits_are_kept_per_harness_merged_by_name_and_dropped_once_reset() {
        let mut limits = PlanLimits::default();
        limits.follow(
            Agent::Claude,
            &HarnessEvent::Limits(vec![
                limit("five_hour", 0.1, Some(1_000)),
                limit("seven_day", 0.42, Some(9_000)),
            ]),
            10,
        );
        limits.follow(
            Agent::Claude,
            &HarnessEvent::Limits(vec![limit("five_hour", 0.3, Some(1_000))]),
            20,
        );
        let (claude, reported) = limits.of(Agent::Claude, 500);
        assert_eq!(
            claude,
            [
                limit("five_hour", 0.3, Some(1_000)),
                limit("seven_day", 0.42, Some(9_000))
            ]
        );
        assert_eq!(reported, Some(20));
        // Once the session's limit resets, what was used of it is gone.
        assert_eq!(
            limits.of(Agent::Claude, 1_000).0,
            [limit("seven_day", 0.42, Some(9_000))]
        );
        assert_eq!(limits.of(Agent::Codex, 500), (Vec::new(), None));
    }
}
