//! How fast the harness is writing, as the MessageListScope's output rate
//! says: while a run writes a reply, the reply's output tokens over the time
//! it has taken so far, smoothed over the last few seconds and left out
//! while the harness waits on a tool call; and, followed through the run,
//! the time it spent writing, as measured, for a harness that reports no
//! time of its own waiting on the model.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::harness::HarnessEvent;

/// About how many characters of text make a token, where the harness
/// reports none mid-turn.
const CHARS_PER_TOKEN: f64 = 4.;

/// How far back the live rate is smoothed over.
const WINDOW: Duration = Duration::from_secs(3);

/// The least time a reply must have taken for its rate to be told: short,
/// as the replies between a task's tool calls are.
const LEAST: Duration = Duration::from_millis(200);

/// A run's writing, as it streams in.
#[derive(Debug, Default)]
pub struct RateMeter {
    /// Tool calls under way.
    tools: usize,
    /// When the current reply's first text streamed in.
    reply_from: Option<Instant>,
    /// The current reply's tokens so far, as of each time text came.
    samples: VecDeque<(Instant, f64)>,
    tokens: f64,
    /// When the current turn began, and when usage was last reported.
    turn_from: Option<Instant>,
    usage_at: Option<Instant>,
    /// The time spent writing, turn by turn, tool calls left out.
    writing: Duration,
    /// The harness has begun: its first turn under way.
    begun: bool,
}

impl RateMeter {
    /// The harness began its first turn at `now`.
    pub fn begin(&mut self, now: Instant) {
        self.begun = true;
        self.turn_from.get_or_insert(now);
    }

    /// Follows what the run reported at `now`. Its first turn begins once
    /// the harness says it has started, never while its sandbox is
    /// prepared.
    pub fn follow(&mut self, event: &HarnessEvent, now: Instant) {
        if !self.begun
            && matches!(
                event,
                HarnessEvent::Session(_)
                    | HarnessEvent::TextStarted
                    | HarnessEvent::TextDelta(_)
                    | HarnessEvent::ThinkingDelta(_)
                    | HarnessEvent::ToolStarted { .. }
            )
        {
            self.begin(now);
        }
        match event {
            HarnessEvent::TextStarted => self.text("", now),
            HarnessEvent::TextDelta(text) | HarnessEvent::ThinkingDelta(text) => self.text(text, now),
            HarnessEvent::ToolStarted { .. } => {
                if self.tools == 0 {
                    self.end_turn(now);
                }
                self.tools += 1;
                self.reply_from = None;
            }
            HarnessEvent::ToolFinished { .. } => {
                self.tools = self.tools.saturating_sub(1);
                if self.tools == 0 {
                    self.turn_from = Some(now);
                }
            }
            HarnessEvent::Usage { .. } | HarnessEvent::Spent { .. } => self.usage_at = Some(now),
            // A message sent to it begins another turn.
            HarnessEvent::Sent { .. } => {
                self.tools = 0;
                self.turn_from = Some(now);
            }
            HarnessEvent::Finished { .. } | HarnessEvent::Answered { .. } | HarnessEvent::Failed(_) => {
                self.end_turn(now);
                self.tools = 0;
                self.reply_from = None;
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str, now: Instant) {
        // Writing, it waits on no tool call, whatever was left unanswered.
        if self.tools > 0 {
            self.tools = 0;
            self.turn_from = None;
        }
        self.turn_from.get_or_insert(now);
        if self.reply_from.is_none() {
            self.reply_from = Some(now);
            self.tokens = 0.;
            self.samples.clear();
            self.samples.push_back((now, 0.));
        }
        self.tokens += text.chars().count() as f64 / CHARS_PER_TOKEN;
        self.samples.push_back((now, self.tokens));
        while self.samples.len() > 2
            && self.samples.get(1).is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// The turn under way ends at `now`: its time, from its start to the
    /// usage reported at its end, counts as writing.
    fn end_turn(&mut self, now: Instant) {
        if let Some(from) = self.turn_from.take() {
            let end = self.usage_at.filter(|at| *at >= from).unwrap_or(now);
            self.writing += end.duration_since(from);
        }
    }

    /// The run is over at `now`: its time spent writing, as measured.
    pub fn finish(&mut self, now: Instant) -> Duration {
        self.end_turn(now);
        self.reply_from = None;
        self.writing
    }

    /// How fast the reply under way is being written at `now`, in tokens a
    /// second; none while no reply is being written, waiting on a tool call
    /// or not yet begun, or too little of it to tell.
    pub fn live(&self, now: Instant) -> Option<f64> {
        let from = self.reply_from.filter(|_| self.tools == 0)?;
        let window_from = from.max(now.checked_sub(WINDOW).unwrap_or(from));
        let base = self
            .samples
            .iter()
            .rev()
            .find(|(at, _)| *at <= window_from)
            .map_or(0., |(_, tokens)| *tokens);
        let elapsed = now.duration_since(window_from);
        let written = self.tokens - base;
        (now.duration_since(from) >= LEAST && elapsed > Duration::ZERO && written > 0.)
            .then(|| written / elapsed.as_secs_f64())
    }
}

/// A run's, or runs', average output rate: their output tokens over the
/// time they took writing them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OutputRate {
    pub tokens: u64,
    pub ms: u64,
    /// Some of the time was measured by the application, the harness having
    /// reported none of its own waiting on the model.
    pub measured: bool,
}

impl OutputRate {
    /// The rate of `tokens` written over `ms`; none where either is nothing.
    pub fn of(tokens: u64, ms: u64, measured: bool) -> Option<Self> {
        (tokens > 0 && ms > 0).then_some(Self { tokens, ms, measured })
    }

    /// Runs' rates taken together; none where none can be told.
    pub fn sum(rates: impl IntoIterator<Item = OutputRate>) -> Option<Self> {
        rates.into_iter().reduce(|a, b| Self {
            tokens: a.tokens + b.tokens,
            ms: a.ms + b.ms,
            measured: a.measured || b.measured,
        })
    }

    pub fn per_second(self) -> f64 {
        self.tokens as f64 * 1000. / self.ms as f64
    }

    /// How it was worked out, as its tooltip says.
    pub fn how(self) -> &'static str {
        if self.measured {
            "Output tokens over time writing, as measured"
        } else {
            "Output tokens over time waiting on the model"
        }
    }
}

/// A rate as shown, "42 tok/s".
pub fn label(per_second: f64) -> String {
    if per_second < 10. {
        format!("{per_second:.1} tok/s")
    } else {
        format!("{per_second:.0} tok/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live rate shows only while text streams in, never while a tool
    /// call is under way or before the reply begins; the time writing
    /// leaves out the tool calls.
    #[test]
    fn rates_follow_the_reply_and_leave_out_tool_calls() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut meter = RateMeter::default();
        meter.begin(at(0));
        assert_eq!(meter.live(at(500)), None, "a rate before any reply");
        meter.follow(&HarnessEvent::TextStarted, at(1000));
        for step in 1..=10 {
            meter.follow(&HarnessEvent::TextDelta("x".repeat(40)), at(1000 + step * 100));
        }
        // 100 tokens in a second.
        let rate = meter.live(at(2000)).unwrap();
        assert!((rate - 100.).abs() < 1., "{rate}");
        meter.follow(&HarnessEvent::Usage { context: 1 }, at(2000));
        meter.follow(&HarnessEvent::ToolStarted { id: "t".into(), name: "Read".into() }, at(2100));
        assert_eq!(meter.live(at(2500)), None, "a rate while a tool call runs");
        meter.follow(&HarnessEvent::ToolFinished { id: "t".into(), is_error: false }, at(5000));
        meter.follow(&HarnessEvent::Usage { context: 2 }, at(6000));
        meter.follow(&HarnessEvent::Finished { is_error: false, result: String::new() }, at(6000));
        // 2s to the first usage, and 1s after the tool call.
        assert_eq!(meter.finish(at(6000)), Duration::from_secs(3));
    }

    /// Thinking streamed counts as writing, and text after a tool call
    /// whose result never came is writing all the same; a short reply
    /// between tool calls is told too.
    #[test]
    fn thinking_and_short_replies_are_told() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut meter = RateMeter::default();
        meter.follow(&HarnessEvent::ToolStarted { id: "t".into(), name: "Bash".into() }, at(0));
        meter.follow(&HarnessEvent::ThinkingDelta("x".repeat(40)), at(1_000));
        meter.follow(&HarnessEvent::ThinkingDelta("x".repeat(40)), at(1_250));
        let rate = meter.live(at(1_250)).expect("no rate while thinking");
        assert!((rate - 40.).abs() < 1., "{rate}");
    }

    #[test]
    fn rates_too_little_to_tell_are_left_out() {
        assert_eq!(OutputRate::of(0, 100, false), None);
        assert_eq!(OutputRate::of(10, 0, true), None);
        assert_eq!(OutputRate::sum([]), None);
        let rate = OutputRate::sum([
            OutputRate::of(100, 1000, false).unwrap(),
            OutputRate::of(300, 1000, true).unwrap(),
        ])
        .unwrap();
        assert_eq!(label(rate.per_second()), "200 tok/s");
        assert_eq!(rate.how(), "Output tokens over time writing, as measured");
        assert_eq!(label(4.25), "4.2 tok/s");
    }
}
