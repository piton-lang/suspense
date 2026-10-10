//! The agent's usage, as the harness reports it: the plan limits it has
//! used, the tokens of the runs of a conversation and of every run the
//! project has made, those saved with it and those that have just reported,
//! and what those tokens would cost at API prices. Nothing here asks the
//! harness for anything; it only adds up what the runs reported.

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::agent::Agent;
use crate::chat_input::tokens_label;
use crate::harness::HarnessEvent;
use crate::prompt_history::RunRecord;

/// Tokens, each `None` until reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spend {
    /// Input tokens read neither from nor into the cache.
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// Of the cache writes, those at the 1-hour rate, where the harness
    /// said; the rest are at the 5-minute rate.
    pub cache_write_1h: Option<u64>,
}

impl Spend {
    fn figures(&self) -> [Option<u64>; 5] {
        [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.cache_write_1h,
        ]
    }

    fn from_figures(
        [input, output, cache_read, cache_write, cache_write_1h]: [Option<u64>; 5],
    ) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
        }
    }

    /// Whether nothing at all has been reported.
    pub fn is_empty(&self) -> bool {
        self.figures().iter().all(Option::is_none)
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
        let mut figures = self.figures();
        for (a, b) in figures.iter_mut().zip(other.figures()) {
            if let Some(b) = b {
                *a = Some(a.map_or(b, |a| a + b));
            }
        }
        *self = Self::from_figures(figures);
    }

    /// What was spent since `earlier`, figure by figure, a figure `earlier`
    /// didn't report counting whole; `None` when any figure went down, as
    /// when its conversation started over.
    pub fn since(&self, earlier: &Spend) -> Option<Spend> {
        let mut figures = self.figures();
        for (a, b) in figures.iter_mut().zip(earlier.figures()) {
            let b = b.unwrap_or(0);
            match a {
                Some(a) if *a >= b => *a -= b,
                None if b == 0 => {}
                _ => return None,
            }
        }
        Some(Self::from_figures(figures))
    }
}

/// How what a run reports spending counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tally {
    /// More, on top of what the run reported before, as OpenCode reports
    /// each step.
    More,
    /// Running totals of the whole conversation it carries on, every run
    /// before it in that conversation included, as Claude Code's
    /// `modelUsage` and Codex's turns report them.
    Conversation,
}

/// A model's published API prices, in US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

impl Prices {
    /// Cache writes are 1.25 times the input price at the 5-minute rate,
    /// and twice it at the 1-hour rate.
    const fn of(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write_5m: input * 1.25,
            cache_write_1h: input * 2.,
        }
    }
}

/// The API prices of the models the application knows, by id.
const PRICES: &[(&str, Prices)] = &[
    ("claude-fable-5-1", Prices::of(10., 50., 0.25)),
    ("claude-mythos-5-1", Prices::of(10., 50., 0.25)),
    ("claude-fable-5", Prices::of(10., 50., 1.)),
    ("claude-mythos-5", Prices::of(10., 50., 1.)),
    ("claude-opus-5-5", Prices::of(4., 20., 0.2)),
    ("claude-opus-5", Prices::of(5., 25., 0.5)),
    ("claude-opus-4-8", Prices::of(5., 25., 0.5)),
    ("claude-opus-4-7", Prices::of(5., 25., 0.5)),
    ("claude-opus-4-6", Prices::of(5., 25., 0.5)),
    ("claude-sonnet-5", Prices::of(2., 10., 0.2)),
    ("claude-sonnet-4-6", Prices::of(3., 15., 0.3)),
    ("claude-haiku-4-5", Prices::of(1., 5., 0.1)),
];

/// A model's id without the date or context-size suffix it may carry:
/// `claude-haiku-4-5-20251001` and `claude-opus-5-5[1m]` are
/// `claude-haiku-4-5` and `claude-opus-5-5`.
pub fn model_id(model: &str) -> &str {
    let model = match model.rfind('[') {
        Some(at) if model.ends_with(']') => &model[..at],
        _ => model,
    };
    match model.rsplit_once('-') {
        Some((id, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => id,
        _ => model,
    }
}

/// `model`'s API prices, if the application knows them.
pub fn prices(model: &str) -> Option<Prices> {
    let id = model_id(model);
    PRICES
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, prices)| *prices)
}

/// What tokens would cost at API prices, in US dollars, by kind, and the
/// models left out of it, their prices not being known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub unpriced: Vec<String>,
}

impl Cost {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// What `models`' tokens would cost; `None` when none of them has known
    /// prices, so no cost can be worked out.
    pub fn of<'a>(models: impl IntoIterator<Item = (&'a str, &'a Spend)>) -> Option<Cost> {
        let mut cost = Cost::default();
        let mut priced = false;
        // A model none of whose tokens were spent has nothing to price.
        for (model, spend) in models
            .into_iter()
            .filter(|(_, spend)| spend.tokens().unwrap_or(0) > 0)
        {
            let Some(prices) = prices(model) else {
                if !cost.unpriced.iter().any(|known| known == model) {
                    cost.unpriced.push(model.to_string());
                }
                continue;
            };
            priced = true;
            let tokens = |figure: Option<u64>| figure.unwrap_or(0) as f64 / 1e6;
            let written = spend.cache_write.unwrap_or(0);
            let one_hour = spend.cache_write_1h.unwrap_or(0).min(written);
            cost.input += tokens(spend.input) * prices.input;
            cost.output += tokens(spend.output) * prices.output;
            cost.cache_read += tokens(spend.cache_read) * prices.cache_read;
            cost.cache_write += tokens(Some(written - one_hour)) * prices.cache_write_5m
                + tokens(Some(one_hour)) * prices.cache_write_1h;
        }
        priced.then_some(cost)
    }
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

/// Whether a run was a task's, each step of a chain one of its own, or a
/// question's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Task,
    Question,
}

/// What a model is called where the harness didn't say which it was.
const UNKNOWN_MODEL: &str = "Unknown model";

/// A run of a task or a question, and what it has reported spending.
#[derive(Clone, Debug)]
struct Run {
    kind: RunKind,
    /// The conversation it carried on as the chat input shows it, and which
    /// of them; none for a run saved with the project.
    conversation: Option<(Conversation, u64)>,
    /// Its hidden anchor's name, so a run saved while it was followed is
    /// counted once.
    name: Option<String>,
    /// When it was sent, in milliseconds since the Unix epoch, the order
    /// runs are taken in.
    sent: u64,
    /// The conversation it carried on, or carried on a copy of, by the
    /// harness's id, when known.
    resumed_from: Option<String>,
    /// It carried a conversation on, whether or not it is known which.
    resumed: bool,
    /// The conversation the harness said it is.
    session: Option<String>,
    /// The model the harness said it uses.
    model: Option<String>,
    /// Its conversation's running totals, as last reported, by model; the
    /// run's own model where the harness didn't say, as "".
    totals: BTreeMap<String, Spend>,
    /// What it reported on top, by model, as `totals` is.
    added: BTreeMap<String, Spend>,
    /// How its cache writes split, at the 5-minute and the 1-hour rate, as
    /// last reported.
    split: Option<(u64, u64)>,
    /// It reported its usage.
    reported: bool,
    /// It is over.
    over: bool,
}

impl Run {
    fn new(kind: RunKind, sent: u64, resumed_from: Option<String>, resumed: bool) -> Self {
        Self {
            kind,
            conversation: None,
            name: None,
            sent,
            resumed_from,
            resumed,
            session: None,
            model: None,
            totals: BTreeMap::new(),
            added: BTreeMap::new(),
            split: None,
            reported: false,
            over: false,
        }
    }

    /// Follows what it reports. Returns whether it reported usage or the
    /// model it uses.
    fn follow(&mut self, event: &HarnessEvent) -> bool {
        match event {
            HarnessEvent::Session(session) => {
                self.session.get_or_insert_with(|| session.clone());
                false
            }
            HarnessEvent::Model(model) => {
                self.model = Some(model.clone());
                true
            }
            HarnessEvent::Spent {
                model,
                spend,
                tally,
            } => {
                let key = model.clone().unwrap_or_default();
                match tally {
                    Tally::More => self.added.entry(key).or_default().add(spend),
                    Tally::Conversation => {
                        self.totals.insert(key, *spend);
                    }
                }
                self.reported = true;
                true
            }
            HarnessEvent::CacheWrites {
                five_minute,
                one_hour,
            } => {
                self.split = Some((*five_minute, *one_hour));
                false
            }
            // The conversation it was to carry on wasn't the harness's: it
            // went again as a new one, and what it reported before was
            // never.
            HarnessEvent::NewConversation(_) => {
                self.resumed_from = None;
                self.resumed = false;
                self.session = None;
                self.totals.clear();
                self.added.clear();
                self.split = None;
                self.reported = false;
                false
            }
            HarnessEvent::Usage { .. } => true,
            _ => false,
        }
    }

    /// `own`, its own tokens, by the model each is of, its cache writes
    /// split as it reported.
    fn named(&self, own: BTreeMap<String, Spend>) -> BTreeMap<String, Spend> {
        let mut named: BTreeMap<String, Spend> = BTreeMap::new();
        for (model, mut spend) in own {
            let model = if model.is_empty() {
                self.model.clone().unwrap_or_else(|| UNKNOWN_MODEL.into())
            } else {
                model
            };
            if spend.cache_write_1h.is_none()
                && let (Some(written), Some((five_minute, one_hour))) =
                    (spend.cache_write, self.split)
                && five_minute + one_hour > 0
            {
                let share = one_hour as f64 / (five_minute + one_hour) as f64;
                spend.cache_write_1h = Some((written as f64 * share).round() as u64);
            }
            named.entry(model).or_default().add(&spend);
        }
        named
    }
}

/// A run saved with the project, as its record keeps what the harness
/// printed for it.
#[derive(Clone, Debug)]
pub struct SavedRun(Run);

impl SavedRun {
    /// The run `record` keeps, of the task or question named `name`, sent at
    /// `sent_at`, in seconds since the Unix epoch; none for one that never
    /// reached the harness.
    pub fn of(record: &RunRecord, name: &str, sent_at: u64, kind: RunKind) -> Option<Self> {
        if !record.output.iter().any(|line| line.get("sent").is_none()) {
            return None;
        }
        let mut run = Run::new(
            kind,
            sent_at * 1000,
            record.resumed_from.clone(),
            record.resumed,
        );
        run.name = Some(name.to_string());
        run.over = true;
        for line in record.output.iter().filter(|line| tells_usage(line)) {
            for event in crate::harness::parse(line) {
                run.follow(&event);
            }
        }
        Some(Self(run))
    }
}

/// Whether a line a harness printed can say what its run is, or spent:
/// leaving the rest, most of them, unread.
fn tells_usage(line: &Value) -> bool {
    matches!(
        line.get("type").and_then(Value::as_str),
        Some(
            "system"
                | "result"
                | "thread.started"
                | "turn.completed"
                | "step_start"
                | "step_finish"
        )
    )
}

/// Every run's own tokens, by model, a run's own figures being how much its
/// conversation's totals rose since that conversation's previous run.
fn own_figures(runs: &[&Run]) -> Vec<BTreeMap<String, Spend>> {
    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by_key(|&ix| (runs[ix].sent, ix));
    // Each conversation's totals as its latest run left them, and every
    // run's, in order.
    let mut last: HashMap<&str, &BTreeMap<String, Spend>> = HashMap::new();
    let mut earlier: Vec<(RunKind, &BTreeMap<String, Spend>)> = Vec::new();
    let mut own = vec![BTreeMap::new(); runs.len()];
    for ix in order {
        let run = runs[ix];
        let base = match &run.resumed_from {
            Some(from) => last.get(from.as_str()).copied(),
            None => run
                .session
                .as_deref()
                .and_then(|session| last.get(session).copied())
                // A copy of a conversation saved before which was kept is
                // measured from the latest run it can have copied.
                .or_else(|| {
                    run.resumed
                        .then(|| {
                            earlier
                                .iter()
                                .rev()
                                .filter(|(kind, _)| *kind == run.kind)
                                .map(|(_, totals)| *totals)
                                .find(|totals| rose(&run.totals, totals).is_some())
                        })
                        .flatten()
                }),
        };
        let mut figures = base
            .and_then(|base| rose(&run.totals, base))
            .unwrap_or_else(|| run.totals.clone());
        for (model, spend) in &run.added {
            figures.entry(model.clone()).or_default().add(spend);
        }
        own[ix] = run.named(figures);
        if !run.totals.is_empty() {
            if let Some(session) = &run.session {
                last.insert(session, &run.totals);
            }
            earlier.push((run.kind, &run.totals));
        }
    }
    own
}

/// How much `totals` rose since `base`, model by model; `None` when any of
/// them went down, as when the conversation started over.
fn rose(
    totals: &BTreeMap<String, Spend>,
    base: &BTreeMap<String, Spend>,
) -> Option<BTreeMap<String, Spend>> {
    if base.keys().any(|model| !totals.contains_key(model)) {
        return None;
    }
    totals
        .iter()
        .map(|(model, spend)| {
            let before = base.get(model).copied().unwrap_or_default();
            Some((model.clone(), spend.since(&before)?))
        })
        .collect()
}

/// Every run the project has made, saved with it or followed since it was
/// opened, and what each reported.
#[derive(Clone, Debug, Default)]
pub struct ProjectUsage {
    /// Those followed since it was opened, by the number `start_run` gave.
    runs: Vec<Run>,
    /// Those saved with it, of tasks and of questions.
    saved: Vec<Run>,
    /// The harness and model of the latest report, and when it came.
    harness: Option<Agent>,
    model: Option<String>,
    reported: Option<u64>,
}

impl ProjectUsage {
    /// Counts a run of `kind` starting in the `epoch`th of `conversation`,
    /// carrying on, or a copy of, `resumed_from`, the harness's id of it;
    /// its events are then followed by the run number returned.
    pub fn start_run(
        &mut self,
        conversation: Conversation,
        epoch: u64,
        kind: RunKind,
        resumed_from: Option<String>,
    ) -> usize {
        let sent = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or(0);
        let resumed = resumed_from.is_some();
        let mut run = Run::new(kind, sent, resumed_from, resumed);
        run.conversation = Some((conversation, epoch));
        self.runs.push(run);
        self.runs.len() - 1
    }

    /// Run `run` is of the task or question named `name`, as it is saved.
    pub fn name_run(&mut self, run: usize, name: &str) {
        if let Some(run) = self.runs.get_mut(run) {
            run.name = Some(name.to_string());
        }
    }

    /// Run `run` is over, so the project counts it.
    pub fn end_run(&mut self, run: usize) {
        if let Some(run) = self.runs.get_mut(run) {
            run.over = true;
        }
    }

    /// The runs of `kind` saved with the project, as last read.
    pub fn set_saved(&mut self, kind: RunKind, saved: Vec<SavedRun>) {
        self.saved.retain(|run| run.kind != kind);
        self.saved
            .extend(saved.into_iter().map(|SavedRun(run)| run));
    }

    /// Follows what run `run`, of `agent`, reports, at `now`.
    pub fn follow(&mut self, run: usize, agent: Agent, event: &HarnessEvent, now: u64) {
        let Some(run) = self.runs.get_mut(run) else {
            return;
        };
        if !run.follow(event) {
            return;
        }
        if let HarnessEvent::Model(model) = event {
            self.harness = Some(agent);
            self.model = Some(model.clone());
            return;
        }
        // A model another harness reported doesn't say where these came from.
        if self.harness != Some(agent) {
            self.model = None;
        }
        self.harness = Some(agent);
        self.reported = Some(now);
    }

    /// Every run, those followed first, a saved one left out where it is one
    /// followed since; and each one's own tokens.
    fn figures(&self) -> Vec<(&Run, BTreeMap<String, Spend>)> {
        let followed: Vec<&str> = self
            .runs
            .iter()
            .filter_map(|run| run.name.as_deref())
            .collect();
        let runs: Vec<&Run> = self
            .runs
            .iter()
            .chain(self.saved.iter().filter(|run| {
                run.name
                    .as_deref()
                    .is_none_or(|name| !followed.contains(&name))
            }))
            .collect();
        let own = own_figures(&runs);
        runs.into_iter().zip(own).collect()
    }

    /// What the runs of the `epoch`th of `conversation` spent, by model.
    pub fn conversation(&self, conversation: Conversation, epoch: u64) -> BTreeMap<String, Spend> {
        let mut models = BTreeMap::new();
        for (run, own) in self.figures() {
            if run.conversation == Some((conversation, epoch)) {
                merge(&mut models, own);
            }
        }
        models
    }

    /// What the runs of the task or question named `name` spent: their
    /// tokens, and what they cost where every model they used is priced;
    /// none while nothing it ran reported any.
    pub fn spent_by(&self, name: &str) -> Option<(u64, Option<f64>)> {
        let mut models = BTreeMap::new();
        for (run, own) in self.figures() {
            if run.name.as_deref() == Some(name) {
                merge(&mut models, own);
            }
        }
        let tokens: u64 = models.values().filter_map(Spend::tokens).sum();
        (tokens > 0).then(|| {
            let cost = Cost::of(models.iter().map(|(model, spend)| (model.as_str(), spend)));
            (tokens, cost.map(|cost| cost.total()))
        })
    }

    /// Every run the project has made that is over, and what they spent.
    pub fn project(&self) -> ProjectReport {
        let mut report = ProjectReport::default();
        let mut models = BTreeMap::new();
        for (run, own) in self.figures() {
            if !run.over {
                continue;
            }
            let count = match run.kind {
                RunKind::Task => &mut report.tasks,
                RunKind::Question => &mut report.questions,
            };
            count.runs += 1;
            if !run.reported {
                count.without_usage += 1;
            }
            merge(&mut models, own);
        }
        report.cost = Cost::of(models.iter().map(|(model, spend)| (model.as_str(), spend)));
        report.models = models
            .into_iter()
            // A model that spent nothing isn't one the runs used.
            .filter(|(_, spend)| spend.tokens().unwrap_or(0) > 0)
            .map(|(name, spend)| ModelSpend {
                priced: prices(&name).is_some(),
                name,
                spend,
            })
            .collect();
        report
    }

    /// The harness and model of the latest report, and when it came.
    pub fn source(&self) -> (Option<Agent>, Option<String>, Option<u64>) {
        (self.harness, self.model.clone(), self.reported)
    }
}

/// Adds `own` into `models`, each model by its id, whatever suffix it
/// carries.
fn merge(models: &mut BTreeMap<String, Spend>, own: BTreeMap<String, Spend>) {
    for (model, spend) in own {
        models
            .entry(model_id(&model).to_string())
            .or_default()
            .add(&spend);
    }
}

/// How many runs there were, and how many of them reported no usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunCount {
    pub runs: usize,
    pub without_usage: usize,
}

/// A model's tokens, and whether its prices are known.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelSpend {
    pub name: String,
    pub spend: Spend,
    pub priced: bool,
}

/// Every run the project has made: how many there were, of tasks and of
/// questions, their tokens by model, and what they would cost at API prices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectReport {
    pub tasks: RunCount,
    pub questions: RunCount,
    pub models: Vec<ModelSpend>,
    pub cost: Option<Cost>,
}

impl ProjectReport {
    /// Whether it has no runs, nor tokens.
    pub fn is_empty(&self) -> bool {
        self.tasks.runs == 0 && self.questions.runs == 0 && self.models.is_empty()
    }

    /// Every token its runs reported; `None` if none were.
    pub fn tokens(&self) -> Option<u64> {
        self.models
            .iter()
            .filter_map(|model| model.spend.tokens())
            .reduce(|a, b| a + b)
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
    /// What its runs spent since the project was opened, and what that
    /// would cost at API prices, where it can be worked out.
    pub conversation_spend: Spend,
    pub conversation_cost: Option<Cost>,
    /// Every run the project has made.
    pub project: ProjectReport,
    /// The harness and model the figures came from, and when they were last
    /// reported, in seconds since the Unix epoch.
    pub harness: Option<Agent>,
    pub model: Option<String>,
    pub reported: Option<u64>,
}

impl UsageReport {
    /// The conversation's figures, from its tokens by model.
    pub fn set_conversation(&mut self, models: &BTreeMap<String, Spend>) {
        let mut spend = Spend::default();
        for model in models.values() {
            spend.add(model);
        }
        self.conversation_spend = spend;
        self.conversation_cost =
            Cost::of(models.iter().map(|(model, spend)| (model.as_str(), spend)));
    }

    /// Whether nothing has been reported yet.
    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
            && self.context.is_none()
            && self.conversation_spend.is_empty()
            && self.project.is_empty()
    }

    /// The plan limit nearest to running out.
    pub fn nearest_limit(&self) -> Option<&PlanLimit> {
        self.limits.iter().max_by(|a, b| a.used.total_cmp(&b.used))
    }

    /// The summary: the share used of the limit nearest to running out, or
    /// else what the project's runs would cost at API prices, or else their
    /// tokens, or else nothing but "Usage"; and how it is coloured.
    pub fn summary(&self) -> (String, Level) {
        if let Some(limit) = self.nearest_limit() {
            return (format!("Usage {}%", limit.percent()), Level::of(limit.used));
        }
        let text = match (&self.project.cost, self.project.tokens()) {
            (Some(cost), _) => format!("Usage {}", cost_label(cost.total())),
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

/// How many runs there were, as "42 runs".
fn runs_label(runs: usize) -> String {
    if runs == 1 {
        "1 run".into()
    } else {
        format!("{runs} runs")
    }
}

/// What a cost leaves out, its models' prices unknown, as "excluding
/// some-model"; none when it leaves nothing out.
fn excluding(cost: &Cost) -> Option<String> {
    (!cost.unpriced.is_empty()).then(|| format!("excluding {}", cost.unpriced.join(", ")))
}

/// How wide a column of the popover's details is: more than the 240 pixels
/// it is at least, so each figure's label and value fit beside each other.
pub const COLUMN_WIDTH: Pixels = px(260.);

/// The gap between the popover's two columns, the line between them down
/// its middle.
pub const COLUMN_GAP: Pixels = px(16.);

/// The popover's details, as of `now`: each group headed, each figure
/// labelled, and the groups and figures not reported left out. What is going
/// on now is in one column, and the project in another beside it, or, short
/// of `side_by_side`, beneath it.
pub fn details(report: &UsageReport, now: u64, side_by_side: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let heading = |text: &str| {
        div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .child(text.to_string())
    };
    let figure = |id: &str, label: &str, value: AnyElement| {
        gpui_kit::TestSupportExt::test_support(
            h_flex()
                .id(SharedString::from(format!("usage-{id}")))
                .justify_between()
                .gap_4()
                .child(div().text_color(muted).child(label.to_string()))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .text_right()
                        .font_features(crate::subagents::tabular_figures())
                        .child(value),
                ),
        )
        .into_any_element()
    };
    let text = |value: String| div().child(value).into_any_element();
    // A figure, and beside it, muted, what qualifies it.
    let qualified = |value: String, beside: Option<String>| {
        h_flex()
            .gap_1()
            .child(value)
            .when_some(beside, |this, beside| {
                this.child(div().text_color(muted).child(beside))
            })
            .into_any_element()
    };
    let token_figures = |group: &str, spend: &Spend| {
        [
            ("input", "Input", spend.input),
            ("output", "Output", spend.output),
            ("cache-read", "Cache read", spend.cache_read),
            ("cache-written", "Cache written", spend.cache_write),
        ]
        .into_iter()
        .filter_map(|(id, label, value)| {
            Some(figure(
                &format!("{group}-{id}"),
                label,
                text(tokens_label(value?)),
            ))
        })
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
                            .child(
                                div()
                                    .font_features(crate::subagents::tabular_figures())
                                    .child(format!("{}%", limit.percent())),
                            ),
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
            .map(|context| {
                figure(
                    "conversation-context",
                    "Context",
                    text(tokens_label(context)),
                )
            })
            .into_iter()
            .chain(token_figures("conversation", &report.conversation_spend))
            .chain(report.conversation_cost.as_ref().map(|cost| {
                figure(
                    "conversation-cost",
                    "Cost",
                    qualified(cost_label(cost.total()), excluding(cost)),
                )
            }))
            .collect(),
    );

    let project = &report.project;
    let mut project_rows = Vec::new();
    for (id, label, count) in [
        ("tasks", "Tasks", project.tasks),
        ("questions", "Questions", project.questions),
    ] {
        if count.runs > 0 {
            project_rows.push(figure(
                &format!("project-{id}"),
                label,
                qualified(
                    runs_label(count.runs),
                    (count.without_usage > 0)
                        .then(|| format!("· {} without usage", count.without_usage)),
                ),
            ));
        }
    }
    for (ix, model) in project.models.iter().enumerate() {
        let rows = token_figures(&format!("project-model-{ix}"), &model.spend);
        if rows.is_empty() {
            continue;
        }
        project_rows.push(
            gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id(SharedString::from(format!("usage-project-model-{ix}")))
                    .pt_1()
                    .gap_2()
                    .font_weight(FontWeight::MEDIUM)
                    .child(model.name.clone())
                    .when(!model.priced, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(muted)
                                .child("Price unknown"),
                        )
                    }),
            )
            .into_any_element(),
        );
        project_rows.extend(rows);
    }
    if let Some(cost) = &project.cost {
        let total = cost.total();
        project_rows.push(
            div()
                .pt_1()
                .child(figure(
                    "project-cost",
                    "Cost at API prices",
                    qualified(cost_label(total), excluding(cost)),
                ))
                .into_any_element(),
        );
        for (id, label, part) in [
            ("input", "Input", cost.input),
            ("output", "Output", cost.output),
            ("cache-reads", "Cache reads", cost.cache_read),
            ("cache-writes", "Cache writes", cost.cache_write),
        ] {
            let share = if total > 0. { part / total * 100. } else { 0. };
            project_rows.push(figure(
                &format!("project-cost-{id}"),
                label,
                text(format!("{} · {share:.0}%", cost_label(part))),
            ));
        }
        project_rows.push(
            gpui_kit::TestSupportExt::test_support(
                div()
                    .id("usage-project-cost-note")
                    .text_xs()
                    .text_color(muted)
                    .child(
                        "What these tokens would cost at API prices, not what a \
                         subscription charges.",
                    ),
            )
            .into_any_element(),
        );
    }
    let project = group("project", "This project", project_rows);
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
    // What is going on now: the plan limits, the conversation, and at its
    // foot where the figures came from.
    let now_shown = limits.is_some() || conversation.is_some() || !source.is_empty();
    let left = now_shown.then(|| {
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .id("usage-column-now")
                .flex_none()
                .w(COLUMN_WIDTH)
                .gap_3()
                .children(limits)
                .children(conversation)
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
                }),
        )
    });
    // The project, the longest.
    let right = project.map(|project| {
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .id("usage-column-project")
                .flex_none()
                .w(COLUMN_WIDTH)
                .gap_3()
                .child(project),
        )
    });
    match (left, right) {
        (Some(left), Some(right)) if side_by_side => body.child(
            h_flex()
                .items_start()
                .gap(COLUMN_GAP / 2.)
                .child(left)
                .child(gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("usage-column-line")
                        .self_stretch()
                        .flex_none()
                        .w(px(1.))
                        .bg(theme.border),
                ))
                .child(right),
        ),
        (left, right) => body.children(left).children(right),
    }
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
    use serde_json::json;

    use super::{
        Agent, Conversation, Cost, HarnessEvent, Level, PlanLimit, PlanLimits, ProjectReport,
        ProjectUsage, RunKind, SavedRun, Spend, Tally, UsageReport, cost_label, model_id, prices,
        until,
    };
    use crate::prompt_history::RunRecord;

    fn limit(name: &str, used: f64, resets_at: Option<u64>) -> PlanLimit {
        PlanLimit {
            name: name.into(),
            used,
            resets_at,
        }
    }

    /// A Claude Code result's running totals of `model`.
    fn totals(model: &str, input: u64, output: u64) -> HarnessEvent {
        HarnessEvent::Spent {
            model: Some(model.into()),
            spend: Spend {
                input: Some(input),
                output: Some(output),
                ..Spend::default()
            },
            tally: Tally::Conversation,
        }
    }

    /// A run of `kind`, carrying on `from`, in the conversation `session`,
    /// reporting `events`, then over.
    fn run(
        usage: &mut ProjectUsage,
        kind: RunKind,
        from: Option<&str>,
        session: &str,
        events: &[HarnessEvent],
    ) -> usize {
        let conversation = match kind {
            RunKind::Task => Conversation::Tasks,
            RunKind::Question => Conversation::Questions,
        };
        let run = usage.start_run(conversation, 0, kind, from.map(Into::into));
        usage.follow(
            run,
            Agent::Claude,
            &HarnessEvent::Session(session.into()),
            1,
        );
        for event in events {
            usage.follow(run, Agent::Claude, event, 2);
        }
        usage.end_run(run);
        run
    }

    fn output(report: &ProjectReport, model: &str) -> Option<u64> {
        report
            .models
            .iter()
            .find(|spend| spend.name == model)
            .and_then(|spend| spend.spend.output)
    }

    #[test]
    fn summary_is_the_nearest_limit_then_cost_then_tokens_then_bare() {
        let mut report = UsageReport::default();
        assert_eq!(report.summary(), ("Usage".into(), Level::Normal));
        assert!(report.is_empty());

        report.project.models = vec![super::ModelSpend {
            name: "some-model".into(),
            spend: Spend {
                output: Some(42_100),
                ..Spend::default()
            },
            priced: false,
        }];
        assert_eq!(report.summary(), ("Usage 42.1k".into(), Level::Normal));

        report.project.cost = Some(Cost {
            input: 1.2391,
            ..Cost::default()
        });
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

    /// A model is known by its id whatever date or context-size suffix it
    /// carries; one whose prices aren't known gets no cost, and a cost
    /// leaving it out says so.
    #[test]
    fn costs_are_worked_out_from_each_models_prices() {
        assert_eq!(model_id("claude-opus-5-5[1m]"), "claude-opus-5-5");
        assert_eq!(model_id("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(model_id("claude-opus-5"), "claude-opus-5");
        assert_eq!(prices("claude-opus-5[1m]").unwrap().input, 5.);
        assert!(prices("gpt-5-codex").is_none());

        let opus = Spend {
            input: Some(1_000_000),
            output: Some(1_000_000),
            cache_read: Some(10_000_000),
            cache_write: Some(3_000_000),
            cache_write_1h: Some(1_000_000),
        };
        let codex = Spend {
            output: Some(5),
            ..Spend::default()
        };
        let cost = Cost::of([("claude-opus-5-5", &opus), ("gpt-5-codex", &codex)]).unwrap();
        assert!((cost.input - 4.).abs() < 1e-9);
        assert!((cost.output - 20.).abs() < 1e-9);
        assert!((cost.cache_read - 2.).abs() < 1e-9);
        // 2M at the 5-minute rate, $5, and 1M at the 1-hour rate, $8.
        assert!((cost.cache_write - 18.).abs() < 1e-9);
        assert_eq!(cost.unpriced, ["gpt-5-codex"]);
        assert!(Cost::of([("gpt-5-codex", &codex)]).is_none());
    }

    /// A run's own figures are how much its conversation's running totals
    /// rose since that conversation's previous run, each model apart; when
    /// any went down, the conversation started over and the run counts in
    /// full; a copy is measured from the run whose conversation it copied.
    #[test]
    fn a_runs_own_figures_are_what_its_conversations_totals_rose_by() {
        let mut usage = ProjectUsage::default();
        run(
            &mut usage,
            RunKind::Task,
            None,
            "s",
            &[totals("claude-opus-5", 100, 10)],
        );
        run(
            &mut usage,
            RunKind::Task,
            Some("s"),
            "s",
            &[
                totals("claude-opus-5", 250, 30),
                totals("claude-haiku-4-5-20251001", 7, 1),
            ],
        );
        // A question copying the tasks' conversation, its totals carrying
        // the copied ones.
        run(
            &mut usage,
            RunKind::Question,
            Some("s"),
            "copy",
            &[
                totals("claude-opus-5", 300, 40),
                totals("claude-haiku-4-5-20251001", 7, 1),
            ],
        );
        // Started over: lower totals count in full.
        run(
            &mut usage,
            RunKind::Task,
            Some("s"),
            "s",
            &[totals("claude-opus-5", 5, 2)],
        );

        let report = usage.project();
        // 10, then 20, then 10, then 2.
        assert_eq!(output(&report, "claude-opus-5"), Some(42));
        assert_eq!(output(&report, "claude-haiku-4-5"), Some(1));
        assert_eq!(report.tasks.runs, 3);
        assert_eq!(report.questions.runs, 1);
        let tasks = usage.conversation(Conversation::Tasks, 0);
        assert_eq!(tasks["claude-opus-5"].input, Some(105 + 150));
    }

    /// Saved runs are counted with those followed since, in the order they
    /// were sent, one saved while it was followed counted once; a copy saved
    /// before what it copied was kept is measured from the latest run it can
    /// have copied.
    #[test]
    fn saved_runs_count_with_those_followed_once_each() {
        let result = |session: &str, input: u64, output: u64| {
            json!({ "type": "result", "is_error": false, "result": "", "session_id": session,
                "total_cost_usd": 99.0,
                "usage": { "cache_creation": { "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": 10 } },
                "modelUsage": { "claude-opus-5-5[1m]": { "inputTokens": input,
                    "outputTokens": output, "cacheCreationInputTokens": 1_000_000 } } })
        };
        let init = |session: &str| {
            json!({ "type": "system", "subtype": "init", "session_id": session,
                "model": "claude-opus-5-5[1m]" })
        };
        let record = |lines: Vec<serde_json::Value>, resumed: bool| RunRecord {
            output: lines,
            resumed,
            ..RunRecord::default()
        };
        let first = record(vec![init("a"), result("a", 100, 10)], false);
        let copy = record(vec![init("b"), result("b", 130, 15)], true);
        let quiet = record(vec![init("c")], false);
        let never = RunRecord {
            error: Some("did not compile".into()),
            ..RunRecord::default()
        };
        let saved: Vec<SavedRun> = [
            SavedRun::of(&first, "First", 10, RunKind::Question),
            SavedRun::of(&copy, "Copy", 20, RunKind::Question),
            SavedRun::of(&quiet, "Quiet", 30, RunKind::Question),
            SavedRun::of(&never, "Never", 40, RunKind::Question),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(saved.len(), 3);
        let mut usage = ProjectUsage::default();
        usage.set_saved(RunKind::Question, saved);
        // "Quiet" was followed here as well.
        let followed = usage.start_run(Conversation::Questions, 0, RunKind::Question, None);
        usage.name_run(followed, "Quiet");
        usage.end_run(followed);

        let report = usage.project();
        assert_eq!(report.questions.runs, 3);
        assert_eq!(report.questions.without_usage, 1);
        // 10, then the 5 the copy added.
        assert_eq!(output(&report, "claude-opus-5-5"), Some(15));
        let opus = &report.models[0];
        // The first run's million written counts, the copy's carried one
        // doesn't; all of it at the 1-hour rate, as the split says.
        assert_eq!(opus.spend.cache_write, Some(1_000_000));
        assert_eq!(opus.spend.cache_write_1h, Some(1_000_000));
        let cost = report.cost.unwrap();
        assert!((cost.cache_write - 8.).abs() < 1e-9, "{cost:?}");
    }

    /// A figure no run reported stays unreported, rather than counting as
    /// nothing, and what is added on adds up.
    #[test]
    fn added_tokens_add_up_leaving_the_unreported_out() {
        let mut usage = ProjectUsage::default();
        let step = |input: u64| HarnessEvent::Spent {
            model: None,
            spend: Spend {
                input: Some(input),
                ..Spend::default()
            },
            tally: Tally::More,
        };
        let question = usage.start_run(Conversation::Questions, 0, RunKind::Question, None);
        usage.follow(
            question,
            Agent::OpenCode,
            &HarnessEvent::Model("big-pickle".into()),
            3,
        );
        usage.follow(question, Agent::OpenCode, &step(5), 7);
        usage.follow(question, Agent::OpenCode, &step(6), 8);
        let questions = usage.conversation(Conversation::Questions, 0);
        assert_eq!(questions["big-pickle"].input, Some(11));
        assert_eq!(questions["big-pickle"].output, None);
        // Under way, the project doesn't count it yet.
        assert!(usage.project().is_empty());
        usage.end_run(question);
        let report = usage.project();
        assert_eq!(report.questions.runs, 1);
        assert!(!report.models[0].priced);
        assert!(report.cost.is_none());
        assert_eq!(usage.harness, Some(Agent::OpenCode));
        assert_eq!(usage.reported, Some(8));
        // A conversation left for a new one has spent nothing yet.
        assert!(usage.conversation(Conversation::Questions, 1).is_empty());
    }

    #[test]
    fn spend_since_is_none_once_a_figure_went_down() {
        let now = Spend {
            input: Some(10),
            output: Some(5),
            ..Spend::default()
        };
        let before = Spend {
            input: Some(4),
            ..Spend::default()
        };
        assert_eq!(
            now.since(&before),
            Some(Spend {
                input: Some(6),
                output: Some(5),
                ..Spend::default()
            })
        );
        let more = Spend {
            input: Some(11),
            ..Spend::default()
        };
        assert_eq!(now.since(&more), None);
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
