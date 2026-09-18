//! Markdown adjustments for rendering harness replies.

/// `markdown` with inline code spans turned into their plain, escaped text.
///
/// gpui-kit 0.6.1's `TextView` lays out any paragraph containing inline code
/// with a flow that re-wraps each measured piece inside its own box, so words
/// overlap. Paragraphs without inline code use its normal text layout, which
/// wraps correctly. Fenced code blocks are left alone. Remove this once
/// gpui-kit fixes the flow.
pub fn without_inline_code(markdown: &str) -> String {
    let mut result = String::with_capacity(markdown.len());
    let mut fence: Option<(char, usize)> = None;

    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |marker| {
            trimmed.chars().take_while(|c| *c == marker).count()
        });

        if let Some(marker) = marker
            && indent <= 3
            && run >= 3
        {
            match fence {
                None => fence = Some((marker, run)),
                Some((open, len))
                    if marker == open && run >= len && trimmed[run..].trim().is_empty() =>
                {
                    fence = None
                }
                Some(_) => {}
            }
            result.push_str(line);
        } else if fence.is_some() {
            result.push_str(line);
        } else {
            result.push_str(&strip_code_spans(line));
        }
    }
    result
}

/// Replaces each code span in `line` with its content, escaping Markdown
/// punctuation so the text stays literal. Unclosed backticks are kept.
fn strip_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let ticks = rest[start..].bytes().take_while(|b| *b == b'`').count();
        let after = &rest[start + ticks..];
        match closing_run(after, ticks) {
            Some(end) => {
                out.push_str(&rest[..start]);
                let code = &after[..end];
                // One space padding each side is not part of the code.
                let code = if code.len() >= 2
                    && code.starts_with(' ')
                    && code.ends_with(' ')
                    && !code.trim().is_empty()
                {
                    &code[1..code.len() - 1]
                } else {
                    code
                };
                for c in code.chars() {
                    if c.is_ascii_punctuation() {
                        out.push('\\');
                    }
                    out.push(c);
                }
                rest = &after[end + ticks..];
            }
            None => {
                out.push_str(&rest[..start + ticks]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The byte offset of the next run of exactly `ticks` backticks.
fn closing_run(text: &str, ticks: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'`' {
            let len = bytes[ix..].iter().take_while(|b| **b == b'`').count();
            if len == ticks {
                return Some(ix);
            }
            ix += len;
        } else {
            ix += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::without_inline_code;

    #[test]
    fn inline_code_becomes_plain_text() {
        assert_eq!(
            without_inline_code(
                "Covered in [`MainWindowScope.md`](MainWindowScope.md), run `piton build`."
            ),
            "Covered in [MainWindowScope\\.md](MainWindowScope.md), run piton build."
        );
        assert_eq!(
            without_inline_code("Use ``a `nested` tick`` here."),
            "Use a \\`nested\\` tick here."
        );
    }

    #[test]
    fn code_blocks_and_unclosed_backticks_are_kept() {
        let markdown = "```rust\nlet `x` = 1;\n```\nA stray ` backtick.";
        assert_eq!(without_inline_code(markdown), markdown);
    }
}

/// Where a piece of markdown is shown: which kind of thing it is, the table
/// it's in, such as a task's output, and its row there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MarkdownKey {
    pub kind: MarkdownKind,
    pub table: usize,
    pub row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkdownKind {
    /// Text in a task's output.
    Output,
    /// A command in a task's output, laid out as a code block.
    Command,
    /// The prompt a task was sent as.
    Prompt,
}

/// The parsed state of every piece of markdown shown, kept by where it's
/// shown for as long as the app runs, rather than by the element showing it.
/// A virtualized list lays out rows it doesn't draw, and drops them when they
/// scroll away; with the state kept here, a row is parsed once, not each time
/// it comes back, and is as tall when measured out of view as when drawn.
///
/// Markdown over a few kilobytes is parsed in the background, so a row may
/// first measure short. While any is still parsing, its key is noted as
/// changed every [`POLL_INTERVAL`], for whatever lists it to measure it
/// again, until it has been laid out with the blocks it parsed into, or
/// [`POLL_LIMIT`] polls have passed. Each list keeps its own place in the
/// changes (see [`MarkdownStates::changed_since`]), so no list takes a change
/// another needed.
#[derive(Default)]
pub struct MarkdownStates {
    states: std::collections::HashMap<MarkdownKey, MarkdownState>,
    /// Each change, numbered, oldest first.
    changed: std::collections::VecDeque<(u64, MarkdownKey)>,
    /// The number of the latest change.
    latest: u64,
    polling: bool,
}

struct MarkdownState {
    state: gpui_kit::Entity<gpui_kit::component::text::TextViewState>,
    text: String,
    /// Polls left while its text is being parsed in the background; zero
    /// once it has been laid out parsed, or given up on.
    parsing: usize,
}

impl gpui_kit::Global for MarkdownStates {}

/// Markdown longer than this is parsed in the background: gpui-kit's
/// `TextViewState` parses anything up to 4 KiB as it's given.
const BACKGROUND_PARSE_BYTES: usize = 4 * 1024;

/// How often rows still parsing are measured again, and for how many polls.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const POLL_LIMIT: usize = 100;

/// The most keys noted as changed before the oldest are forgotten.
const MAX_CHANGED: usize = 4096;

/// Polls left for `text`: some while it's parsed in the background.
fn polls_for(text: &str) -> usize {
    if text.len() > BACKGROUND_PARSE_BYTES {
        POLL_LIMIT
    } else {
        0
    }
}

impl MarkdownStates {
    /// The state kept for `key`, if one is, and it holds `text`.
    pub fn cached(
        key: MarkdownKey,
        text: &str,
        cx: &gpui_kit::App,
    ) -> Option<gpui_kit::Entity<gpui_kit::component::text::TextViewState>> {
        cx.try_global::<MarkdownStates>()?
            .states
            .get(&key)
            .filter(|entry| entry.text == text)
            .map(|entry| entry.state.clone())
    }

    /// Keeps a state for `key` holding `text`: made the first time, and given
    /// the text again when it has changed.
    pub fn prepare(key: MarkdownKey, text: &str, cx: &mut gpui_kit::App) {
        use gpui_kit::AppContext as _;
        use gpui_kit::component::text::TextViewState;
        // Only read when nothing changes: touching the global mutably tells
        // those observing it it changed, and they draw again.
        let unchanged = cx
            .try_global::<MarkdownStates>()
            .and_then(|states| states.states.get(&key))
            .is_some_and(|entry| entry.text == text);
        if unchanged {
            return;
        }
        cx.default_global::<MarkdownStates>();
        let parsing = polls_for(text);
        if let Some(entry) = cx.global_mut::<MarkdownStates>().states.get_mut(&key) {
            if entry.text != text {
                entry.text = text.to_string();
                entry.parsing = parsing;
                let state = entry.state.clone();
                let text = text.to_string();
                state.update(cx, |state, cx| state.set_text(&text, cx));
                Self::poll(cx);
            }
            return;
        }
        let state = cx.new(|cx| TextViewState::markdown(text, cx));
        cx.global_mut::<MarkdownStates>().states.insert(
            key,
            MarkdownState {
                state,
                text: text.to_string(),
                parsing,
            },
        );
        Self::poll(cx);
    }

    /// Starts polling the states still parsing, unless it already is or none
    /// are.
    fn poll(cx: &mut gpui_kit::App) {
        let states = cx.global::<MarkdownStates>();
        if states.polling || states.states.values().all(|entry| entry.parsing == 0) {
            return;
        }
        cx.global_mut::<MarkdownStates>().polling = true;
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let more = cx.update(|cx| {
                    use gpui_kit::BorrowAppContext as _;
                    let laid_out: Vec<(MarkdownKey, bool)> = cx
                        .global::<MarkdownStates>()
                        .states
                        .iter()
                        .filter(|(_, entry)| entry.parsing > 0)
                        .map(|(key, entry)| {
                            (*key, entry.state.read(cx).list_state().item_count() > 0)
                        })
                        .collect();
                    if laid_out.is_empty() {
                        cx.global_mut::<MarkdownStates>().polling = false;
                        return false;
                    }
                    let noted = cx.update_global::<MarkdownStates, _>(|states, _| {
                        let mut noted = false;
                        for (key, laid_out) in laid_out {
                            let Some(entry) = states.states.get_mut(&key) else {
                                continue;
                            };
                            if laid_out {
                                entry.parsing = 0;
                            } else {
                                entry.parsing -= 1;
                                if states.changed.len() >= MAX_CHANGED {
                                    states.changed.pop_front();
                                }
                                states.latest += 1;
                                states.changed.push_back((states.latest, key));
                                noted = true;
                            }
                        }
                        noted
                    });
                    // Whatever lists the rows draws again, to measure them.
                    if noted {
                        cx.refresh_windows();
                    }
                    true
                });
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    /// The keys whose parsed markdown may have changed since `seen`, which
    /// is moved on past them.
    pub fn changed_since(seen: &mut u64, cx: &gpui_kit::App) -> Vec<MarkdownKey> {
        let Some(states) = cx.try_global::<MarkdownStates>() else {
            return Vec::new();
        };
        let start = states.changed.partition_point(|(n, _)| *n <= *seen);
        *seen = states.latest;
        let mut changed: Vec<MarkdownKey> =
            states.changed.range(start..).map(|(_, key)| *key).collect();
        changed.sort_by_key(|key| (key.kind as u8, key.table, key.row));
        changed.dedup();
        changed
    }

    /// The number of the latest change, for a list starting out.
    pub fn latest(cx: &gpui_kit::App) -> u64 {
        cx.try_global::<MarkdownStates>()
            .map_or(0, |states| states.latest)
    }
}
