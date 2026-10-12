//! Where a task's time went, as the TasksTab's time breakdown says: its
//! phases and tool calls, as its record keeps them, split into kinds, none
//! counted twice, and the time between tool calls while the harness runs
//! counted as waiting on the model.

use std::sync::{Arc, Mutex, MutexGuard};

/// A phase, or a tool call, and when it began and ended, in milliseconds
/// since the Unix epoch; one under way has no end yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Timed {
    pub name: String,
    /// A tool call's id.
    pub tool: Option<String>,
    pub began: u64,
    pub ended: Option<u64>,
}

impl From<&crate::prompt_history::Phase> for Timed {
    fn from(phase: &crate::prompt_history::Phase) -> Self {
        Self {
            name: phase.name.clone(),
            tool: phase.tool.clone(),
            began: phase.began_at,
            ended: Some(phase.ended_at),
        }
    }
}

/// A task's phases, shared with its run as they begin and end.
pub type SharedTimes = Arc<Mutex<Vec<Timed>>>;

pub fn lock(shared: &SharedTimes) -> MutexGuard<'_, Vec<Timed>> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A kind of time, in the order the bar shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    SpecBuild,
    Preparing,
    Model,
    Commands,
    Files,
    Subagents,
    Web,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::SpecBuild,
        Kind::Preparing,
        Kind::Model,
        Kind::Commands,
        Kind::Files,
        Kind::Subagents,
        Kind::Web,
        Kind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::SpecBuild => "Spec build",
            Kind::Preparing => "Preparing",
            Kind::Model => "Model",
            Kind::Commands => "Commands",
            Kind::Files => "Files",
            Kind::Subagents => "Subagents",
            Kind::Web => "Web",
            Kind::Other => "Other",
        }
    }

    /// Its place among the kinds.
    pub fn index(self) -> usize {
        Kind::ALL.iter().position(|kind| *kind == self).unwrap_or(7)
    }

    /// Its place among the theme's chart colours, which it keeps wherever
    /// it is shown: Other's the neutral grey, the last.
    pub fn color_index(self) -> usize {
        match self {
            Kind::Other => 8,
            kind => kind.index(),
        }
    }

    /// Whether its tooltip counts its calls.
    pub fn counts_calls(self) -> bool {
        matches!(self, Kind::Commands | Kind::Files | Kind::Subagents | Kind::Web)
    }

    /// Which wins where two overlap: what the task waited on most directly.
    fn rank(self) -> u8 {
        match self {
            Kind::SpecBuild => 8,
            Kind::Preparing => 7,
            Kind::Subagents => 6,
            Kind::Commands => 5,
            Kind::Web => 4,
            Kind::Files => 3,
            Kind::Other => 2,
            Kind::Model => 1,
        }
    }
}

/// The kind of a tool, by its name as the harness gives it.
fn tool_kind(tool: &str) -> Kind {
    match tool.to_lowercase().as_str() {
        "bash" | "bashoutput" | "killshell" | "killbash" | "shell" | "command" | "exec"
        | "local_shell" | "command_execution" => Kind::Commands,
        "read" | "write" | "edit" | "multiedit" | "glob" | "grep" | "ls" | "list" | "notebookedit"
        | "notebookread" | "patch" | "apply_patch" | "file_change" => Kind::Files,
        "task" | "agent" => Kind::Subagents,
        "webfetch" | "websearch" | "web_search" | "fetch" => Kind::Web,
        _ => Kind::Other,
    }
}

/// The kind of a phase, by its name as the debug log gives it; none for
/// the harness's own run, in which the time no call takes is the model's.
fn kind_of(name: &str) -> Option<Kind> {
    if name.starts_with("subagent tool ") {
        return Some(Kind::Subagents);
    }
    if let Some(rest) = name.strip_prefix("tool ") {
        return Some(tool_kind(rest.split(' ').next().unwrap_or_default()));
    }
    Some(match name {
        "harness" => return None,
        "spec build" => Kind::SpecBuild,
        "snapshot" | "closing snapshot" | "preparing the sandbox" | "preparing the container" | "first event" => {
            Kind::Preparing
        }
        _ => Kind::Other,
    })
}

/// What a phase is called where it is shown: a tool call by what it acted
/// on, or its tool, and a phase by its name.
pub fn shown_name(name: &str) -> String {
    let rest = name
        .strip_prefix("subagent tool ")
        .or_else(|| name.strip_prefix("tool "));
    match rest {
        Some(rest) => match rest.split_once(' ') {
            Some((_, on)) if !on.trim().is_empty() => on.trim().to_string(),
            _ => rest.to_string(),
        },
        None => {
            let mut chars = name.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A stretch of the timeline, in the kind that took it.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: Kind,
    pub began: u64,
    pub ended: u64,
    /// What it was: a phase's or tool call's name, or the model.
    pub label: String,
    /// The tool call it was, if one.
    pub tool: Option<String>,
}

/// A kind's calls: how many there were, and the slowest, by what it was
/// and how long it took.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calls {
    pub count: usize,
    pub slowest: Option<(String, u64)>,
}

/// Where a task's time went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Breakdown {
    /// Each kind's time, in milliseconds, by its index.
    pub times: [u64; 8],
    pub calls: [Calls; 8],
    /// How long it waited in the queue before it was sent, never part of
    /// the chart.
    pub queued: u64,
    /// The timeline's stretches, in order, and its start and end.
    pub segments: Vec<Segment>,
    pub start: u64,
    pub end: u64,
    /// Where each step after the first began, for a chain.
    pub steps: Vec<u64>,
}

impl Breakdown {
    /// Of `times`, those under way taken as ending at `now`; none with no
    /// phases.
    #[cfg(test)]
    pub fn of(times: &[Timed], now: u64) -> Option<Self> {
        Self::of_steps(&[times.to_vec()], now)
    }

    /// Of a chain's `steps`, each its own phases, taken together.
    pub fn of_steps(steps: &[Vec<Timed>], now: u64) -> Option<Self> {
        // Waiting in the queue is left out: the chart starts once it was sent.
        let is_queued = |timed: &&Timed| timed.name == "queued";
        let queued: u64 = steps
            .iter()
            .flatten()
            .filter(is_queued)
            .map(|timed| timed.ended.unwrap_or(now).saturating_sub(timed.began))
            .sum();
        let all: Vec<(&Timed, u64, Option<Kind>)> = steps
            .iter()
            .flatten()
            .filter(|timed| !is_queued(timed))
            .map(|timed| (timed, timed.ended.unwrap_or(now).max(timed.began), kind_of(&timed.name)))
            .collect();
        if all.is_empty() {
            return None;
        }
        let mut breakdown = Self {
            queued,
            start: all.iter().map(|(t, ..)| t.began).min()?,
            end: all.iter().map(|(_, end, _)| *end).max()?,
            ..Self::default()
        };
        breakdown.steps = steps
            .iter()
            .skip(1)
            .filter_map(|step| step.iter().filter(|t| t.name != "queued").map(|t| t.began).min())
            .collect();
        let mut edges: Vec<u64> = all.iter().flat_map(|(t, end, _)| [t.began, *end]).collect();
        edges.sort_unstable();
        edges.dedup();
        for pair in edges.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            // What took it: the most direct wait covering it; the model's,
            // within the harness's run, where no call does.
            let winner = all
                .iter()
                .enumerate()
                .filter(|(_, (t, end, _))| t.began <= a && *end >= b)
                .max_by_key(|(at, (_, _, kind))| (kind.map_or(Kind::Model, |k| k).rank(), *at));
            let Some((_, (timed, _, kind))) = winner else {
                // Nothing ran: between a chain's steps, or paused.
                continue;
            };
            let kind = kind.unwrap_or(Kind::Model);
            breakdown.times[kind.index()] += b - a;
            let (label, tool) = match kind {
                Kind::Model => ("Model".to_string(), None),
                _ => (shown_name(&timed.name), timed.tool.clone()),
            };
            match breakdown.segments.last_mut() {
                Some(last) if last.kind == kind && last.ended == a && last.label == label && last.tool == tool => {
                    last.ended = b
                }
                _ => breakdown.segments.push(Segment {
                    kind,
                    began: a,
                    ended: b,
                    label,
                    tool,
                }),
            }
        }
        // Each kind's calls, the main thread's only, and the slowest.
        for (timed, end, kind) in &all {
            let Some(kind) = kind.filter(|kind| kind.counts_calls()) else {
                continue;
            };
            if !timed.name.starts_with("tool ") {
                continue;
            }
            let calls = &mut breakdown.calls[kind.index()];
            calls.count += 1;
            let took = end - timed.began;
            if calls.slowest.as_ref().is_none_or(|(_, slowest)| took > *slowest) {
                calls.slowest = Some((shown_name(&timed.name), took));
            }
        }
        Some(breakdown)
    }

    /// All its kinds' time together.
    pub fn total(&self) -> u64 {
        self.times.iter().sum()
    }

    /// The kinds with time, largest first.
    pub fn by_time(&self) -> Vec<(Kind, u64)> {
        let mut kinds: Vec<(Kind, u64)> = Kind::ALL
            .iter()
            .map(|kind| (*kind, self.times[kind.index()]))
            .filter(|(_, time)| *time > 0)
            .collect();
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.index().cmp(&b.0.index())));
        kinds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(name: &str, began: u64, ended: u64) -> Timed {
        Timed {
            name: name.into(),
            tool: name.contains("tool ").then(|| format!("id-{began}")),
            began,
            ended: Some(ended),
        }
    }

    /// Each phase counts as its kind, the harness's time no call takes as
    /// the model's, a subagent's own calls as Subagents, and nothing twice.
    #[test]
    fn time_is_split_into_kinds_without_counting_twice() {
        let times = [
            timed("queued", 0, 1_000),
            timed("spec build", 1_000, 3_000),
            timed("snapshot", 3_000, 3_500),
            timed("harness", 3_500, 20_000),
            timed("first event", 3_500, 4_000),
            timed("tool Bash npm run build", 6_000, 10_000),
            timed("tool Read /a.rs", 11_000, 11_500),
            timed("tool Task Explore", 12_000, 16_000),
            timed("subagent tool Grep foo", 13_000, 14_000),
            timed("tool WebFetch https://x", 17_000, 18_000),
            timed("closing snapshot", 20_000, 20_200),
        ];
        let b = Breakdown::of(&times, 0).unwrap();
        let t = |kind: Kind| b.times[kind.index()];
        // Queued is never part of the chart, which starts once it was sent.
        assert_eq!(b.queued, 1_000);
        assert_eq!(b.start, 1_000);
        assert_eq!(t(Kind::SpecBuild), 2_000);
        assert_eq!(t(Kind::Preparing), 500 + 500 + 200);
        assert_eq!(t(Kind::Commands), 4_000);
        assert_eq!(t(Kind::Files), 500);
        assert_eq!(t(Kind::Subagents), 4_000);
        assert_eq!(t(Kind::Web), 1_000);
        assert_eq!(t(Kind::Model), 16_000 - 4_000 - 500 - 4_000 - 1_000);
        assert_eq!(b.total(), 19_200);
        assert_eq!(b.by_time()[0].0, Kind::Model);
        let commands = &b.calls[Kind::Commands.index()];
        assert_eq!((commands.count, commands.slowest.clone()), (1, Some(("npm run build".into(), 4_000))));
        assert_eq!(b.calls[Kind::Subagents.index()].count, 1, "a subagent's own calls counted as calls");
        let bash = b.segments.iter().find(|s| s.kind == Kind::Commands).unwrap();
        assert_eq!(bash.tool.as_deref(), Some("id-6000"));
    }

    /// A chain's steps together, the gap between them, as time paused,
    /// left out, and where each step began marked.
    #[test]
    fn chains_leave_out_the_gaps_between_steps() {
        let steps = vec![
            vec![timed("harness", 0, 1_000)],
            vec![timed("harness", 5_000, 7_000)],
        ];
        let b = Breakdown::of_steps(&steps, 0).unwrap();
        assert_eq!(b.total(), 3_000);
        assert_eq!(b.steps, [5_000]);
        assert_eq!((b.start, b.end), (0, 7_000));
        assert_eq!(Breakdown::of(&[], 0), None);
        // Under way, up to now.
        let open = [Timed { name: "harness".into(), tool: None, began: 0, ended: None }];
        assert_eq!(Breakdown::of(&open, 2_500).unwrap().times[Kind::Model.index()], 2_500);
    }
}
