//! A file's changes as rows to show: every line of the file, old and new, in
//! order, with the words that changed within a changed line marked, and runs
//! of unchanged lines far from any change collapsed. The same lines lay out
//! unified, one column with removed lines before added ones, or side by side,
//! old beside new, with a removed line paired with the added line that
//! replaced it and blank filler opposite a line with no counterpart.

use std::collections::HashSet;
use std::ops::Range;

use similar::{ChangeTag, TextDiff};

/// Unchanged lines kept in view either side of a change.
pub const CONTEXT_LINES: usize = 3;

/// A run of unchanged lines shorter than this stays shown, since collapsing
/// it would save hardly any room.
const MIN_COLLAPSED: usize = 4;

/// Spaces a tab is shown as.
const TAB: &str = "    ";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Unchanged,
    Removed,
    Added,
}

/// A line of the old or new file, or both when unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub kind: Kind,
    /// One-based line numbers in the old and new file.
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// The line, tabs expanded, without its line ending.
    pub text: String,
    /// The parts of `text` that changed, for a line that replaced, or was
    /// replaced by, a similar one.
    pub changed: Vec<Range<usize>>,
}

/// What a diff shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileDiff {
    /// Every line, old and new, in order.
    pub lines: Vec<Line>,
    pub insertions: usize,
    pub deletions: usize,
}

/// A row of the unified layout.
#[derive(Clone, Debug, PartialEq)]
pub enum UnifiedRow {
    Line(usize),
    /// Unchanged lines hidden from `start`, `len` of them.
    Collapsed {
        start: usize,
        len: usize,
    },
}

/// A row of the side-by-side layout: the old line on the left and the new on
/// the right, either missing where the other side has no counterpart.
#[derive(Clone, Debug, PartialEq)]
pub enum SideRow {
    Pair {
        old: Option<usize>,
        new: Option<usize>,
    },
    Collapsed {
        start: usize,
        len: usize,
    },
}

impl FileDiff {
    /// The changes from `old` to `new`.
    pub fn compute(old: &str, new: &str) -> Self {
        let diff = TextDiff::from_lines(old, new);
        let mut file = Self::default();
        for op in diff.ops() {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => Kind::Unchanged,
                    ChangeTag::Delete => Kind::Removed,
                    ChangeTag::Insert => Kind::Added,
                };
                match kind {
                    Kind::Removed => file.deletions += 1,
                    Kind::Added => file.insertions += 1,
                    Kind::Unchanged => {}
                }
                let mut text = String::new();
                let mut changed = Vec::new();
                let segments: Vec<_> = change.iter_strings_lossy().collect();
                // A line changed through and through isn't worth marking word
                // by word.
                let marks = kind != Kind::Unchanged
                    && segments
                        .iter()
                        .any(|(emphasized, value)| !emphasized && !value.trim().is_empty())
                    && segments
                        .iter()
                        .any(|(emphasized, value)| *emphasized && !value.trim().is_empty());
                for (emphasized, value) in segments {
                    let value = value.trim_end_matches(['\n', '\r']).replace('\t', TAB);
                    let start = text.len();
                    text.push_str(&value);
                    if marks && emphasized && !value.is_empty() {
                        // Neighbouring changed words read as one change.
                        match changed.last_mut() {
                            Some(Range { end, .. }) if *end == start => *end = text.len(),
                            _ => changed.push(start..text.len()),
                        }
                    }
                }
                file.lines.push(Line {
                    kind,
                    old: change.old_index().map(|ix| ix + 1),
                    new: change.new_index().map(|ix| ix + 1),
                    text,
                    changed,
                });
            }
        }
        file
    }

    /// Whether anything changed.
    pub fn has_changes(&self) -> bool {
        self.insertions + self.deletions > 0
    }

    /// Runs of lines to show, as ranges of `lines`, and the collapsed runs
    /// between them: unchanged lines more than [`CONTEXT_LINES`] from any
    /// change are collapsed, unless the run starting there is in `expanded`.
    fn segments(&self, expanded: &HashSet<usize>) -> Vec<Segment> {
        let count = self.lines.len();
        let mut shown = vec![!self.has_changes(); count];
        for (ix, line) in self.lines.iter().enumerate() {
            if line.kind != Kind::Unchanged {
                let from = ix.saturating_sub(CONTEXT_LINES);
                let to = (ix + CONTEXT_LINES + 1).min(count);
                shown[from..to].iter_mut().for_each(|shown| *shown = true);
            }
        }
        let mut segments = Vec::new();
        let mut ix = 0;
        while ix < count {
            let start = ix;
            let visible = shown[ix];
            while ix < count && shown[ix] == visible {
                ix += 1;
            }
            let len = ix - start;
            if visible || len < MIN_COLLAPSED || expanded.contains(&start) {
                segments.push(Segment::Lines(start..ix));
            } else {
                segments.push(Segment::Collapsed { start, len });
            }
        }
        segments
    }

    /// The rows of the unified layout.
    pub fn unified(&self, expanded: &HashSet<usize>) -> Vec<UnifiedRow> {
        self.segments(expanded)
            .into_iter()
            .flat_map(|segment| match segment {
                Segment::Lines(range) => range.map(UnifiedRow::Line).collect::<Vec<_>>(),
                Segment::Collapsed { start, len } => vec![UnifiedRow::Collapsed { start, len }],
            })
            .collect()
    }

    /// The rows of the side-by-side layout.
    pub fn side_by_side(&self, expanded: &HashSet<usize>) -> Vec<SideRow> {
        let mut rows = Vec::new();
        for segment in self.segments(expanded) {
            let range = match segment {
                Segment::Collapsed { start, len } => {
                    rows.push(SideRow::Collapsed { start, len });
                    continue;
                }
                Segment::Lines(range) => range,
            };
            let mut ix = range.start;
            while ix < range.end {
                if self.lines[ix].kind == Kind::Unchanged {
                    rows.push(SideRow::Pair {
                        old: Some(ix),
                        new: Some(ix),
                    });
                    ix += 1;
                    continue;
                }
                // A block of changes: removed lines, then the added lines that
                // replaced them, paired in order.
                let mut removed = Vec::new();
                let mut added = Vec::new();
                while ix < range.end && self.lines[ix].kind != Kind::Unchanged {
                    match self.lines[ix].kind {
                        Kind::Removed => removed.push(ix),
                        _ => added.push(ix),
                    }
                    ix += 1;
                }
                for pair in 0..removed.len().max(added.len()) {
                    rows.push(SideRow::Pair {
                        old: removed.get(pair).copied(),
                        new: added.get(pair).copied(),
                    });
                }
            }
        }
        rows
    }
}

enum Segment {
    Lines(Range<usize>),
    Collapsed { start: usize, len: usize },
}

/// The rows where each change starts: the first changed row after an
/// unchanged or collapsed one.
pub fn change_starts(is_change: impl Iterator<Item = bool>) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous = false;
    for (ix, change) in is_change.enumerate() {
        if change && !previous {
            starts.push(ix);
        }
        previous = change;
    }
    starts
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{FileDiff, Kind, SideRow, UnifiedRow, change_starts};

    /// Changed, removed, and added lines are found with their line numbers,
    /// and within a changed line only the words that changed are marked.
    #[test]
    fn finds_lines_and_changed_words() {
        let old = "one\nlet total = 1;\nthree\ngone\n";
        let new = "one\nlet total = 2;\nthree\nfour\n";
        let diff = FileDiff::compute(old, new);
        assert_eq!((diff.insertions, diff.deletions), (2, 2));

        let kinds: Vec<_> = diff
            .lines
            .iter()
            .map(|line| (line.kind, line.old, line.new))
            .collect();
        assert_eq!(
            kinds,
            [
                (Kind::Unchanged, Some(1), Some(1)),
                (Kind::Removed, Some(2), None),
                (Kind::Added, None, Some(2)),
                (Kind::Unchanged, Some(3), Some(3)),
                (Kind::Removed, Some(4), None),
                (Kind::Added, None, Some(4)),
            ]
        );
        let removed = &diff.lines[1];
        assert_eq!(removed.text, "let total = 1;");
        let marked: Vec<&str> = removed
            .changed
            .iter()
            .map(|range| &removed.text[range.clone()])
            .collect();
        // Split into words the way `similar` does, punctuation included.
        assert_eq!(marked, ["1;"]);
        // Lines that share nothing aren't marked word by word.
        assert!(diff.lines[4].changed.is_empty());
    }

    /// Unchanged lines far from any change collapse, keeping three lines of
    /// context either side, and expand again when asked.
    #[test]
    fn collapses_far_unchanged_lines() {
        let old: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let new = old.replace("line 10\n", "line ten\n");
        let diff = FileDiff::compute(&old, &new);
        let rows = diff.unified(&HashSet::new());
        assert_eq!(
            rows.first(),
            Some(&UnifiedRow::Collapsed { start: 0, len: 6 })
        );
        assert_eq!(
            rows.last(),
            Some(&UnifiedRow::Collapsed { start: 14, len: 7 })
        );
        // 3 context, removed, added, 3 context.
        assert_eq!(rows.len(), 2 + 8);

        let rows = diff.unified(&HashSet::from([0]));
        assert!(matches!(rows[0], UnifiedRow::Line(0)));
        assert_eq!(
            rows.last(),
            Some(&UnifiedRow::Collapsed { start: 14, len: 7 })
        );
    }

    /// Side by side, a removed line pairs with the line that replaced it, and
    /// lines without a counterpart face blank filler.
    #[test]
    fn pairs_changes_side_by_side() {
        let diff = FileDiff::compute("a\nb\nc\n", "a\nB\nextra\nc\n");
        let rows = diff.side_by_side(&HashSet::new());
        assert_eq!(
            rows,
            [
                SideRow::Pair {
                    old: Some(0),
                    new: Some(0)
                },
                SideRow::Pair {
                    old: Some(1),
                    new: Some(2)
                },
                SideRow::Pair {
                    old: None,
                    new: Some(3)
                },
                SideRow::Pair {
                    old: Some(4),
                    new: Some(4)
                },
            ]
        );
    }

    /// A new file is all additions, and a file without changes shows whole.
    #[test]
    fn new_and_unchanged_files() {
        let new = FileDiff::compute("", "a\nb\n");
        assert_eq!((new.insertions, new.deletions), (2, 0));
        assert!(new.lines.iter().all(|line| line.kind == Kind::Added));

        let text: String = (1..=30).map(|n| format!("{n}\n")).collect();
        let same = FileDiff::compute(&text, &text);
        assert!(!same.has_changes());
        assert_eq!(same.unified(&HashSet::new()).len(), 30);
    }

    #[test]
    fn finds_where_changes_start() {
        let rows = [false, true, true, false, true, false, false, true];
        assert_eq!(change_starts(rows.into_iter()), [1, 4, 7]);
    }
}
