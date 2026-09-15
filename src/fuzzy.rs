//! Fuzzy matching: a candidate matches when the query's characters all appear
//! in it in order, ignoring case, though not necessarily next to each other.
//! Of the ways the query can line up with a candidate, the best scoring one is
//! kept: runs of consecutive characters, and characters at the start of a word
//! or of a file's name, score higher, and gaps between characters lower.

/// A candidate the query matched, and how well.
#[derive(Debug, PartialEq)]
pub struct FuzzyMatch {
    pub score: i32,
    /// Byte offsets of the matched characters in the candidate.
    pub positions: Vec<usize>,
}

const MATCH: i32 = 16;
const CONSECUTIVE: i32 = 12;
const WORD_START: i32 = 10;
/// On top of a word start, for the first character of a file's name.
const NAME_START: i32 = 8;
const IN_NAME: i32 = 2;
/// Per character skipped between two matched characters.
const GAP: i32 = 1;

const NONE: i32 = i32::MIN / 2;

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | ' ' | '.' | ':')
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Matches `query` against `candidate`, whose name (a file's name, after its
/// folders) starts at byte `name_start`; pass 0 when all of it is the name.
/// Whitespace in the query is ignored, and an empty query matches everything.
pub fn fuzzy_match(query: &str, candidate: &str, name_start: usize) -> Option<FuzzyMatch> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold)
        .collect();
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    let chars: Vec<(usize, char)> = candidate.char_indices().collect();
    let folded: Vec<char> = chars.iter().map(|&(_, c)| fold(c)).collect();

    // Most candidates do not match at all; turn them away before scoring.
    let mut next = 0;
    for &c in &folded {
        if next < query.len() && c == query[next] {
            next += 1;
        }
    }
    if next < query.len() {
        return None;
    }

    let bonus: Vec<i32> = chars
        .iter()
        .enumerate()
        .map(|(j, &(offset, c))| {
            let prev = j.checked_sub(1).map(|p| chars[p].1);
            let word_start = prev.is_none_or(is_separator)
                || prev.is_some_and(|p| p.is_lowercase() && c.is_uppercase());
            let mut bonus = 0;
            if word_start {
                bonus += WORD_START;
            }
            if offset == name_start {
                bonus += NAME_START;
            }
            if offset >= name_start {
                bonus += IN_NAME;
            }
            bonus
        })
        .collect();

    // `score[i * m + j]`: the best score with the query's `i`th character
    // matched at the candidate's `j`th, and `from` the previous one's `j`.
    let (n, m) = (query.len(), chars.len());
    let mut score = vec![NONE; n * m];
    let mut from = vec![usize::MAX; n * m];
    for j in 0..m {
        if folded[j] == query[0] {
            score[j] = MATCH + bonus[j];
        }
    }
    for i in 1..n {
        let (prev, row) = score.split_at_mut(i * m);
        let prev = &prev[(i - 1) * m..];
        let row = &mut row[..m];
        // The best previous match at least one character back, less the gap.
        // Separators skipped cost nothing, so `main_window` and `MainWindow`
        // are the same distance apart.
        let (mut gap_best, mut gap_from) = (NONE, usize::MAX);
        for j in 1..m {
            let skipped = if is_separator(chars[j - 1].1) { 0 } else { GAP };
            if gap_best > NONE {
                gap_best -= skipped;
            }
            if j >= 2 && prev[j - 2] > NONE && prev[j - 2] - skipped > gap_best {
                (gap_best, gap_from) = (prev[j - 2] - skipped, j - 2);
            }
            if folded[j] != query[i] {
                continue;
            }
            let consecutive = if prev[j - 1] > NONE {
                prev[j - 1] + CONSECUTIVE
            } else {
                NONE
            };
            let (best, best_from) = if consecutive >= gap_best {
                (consecutive, j - 1)
            } else {
                (gap_best, gap_from)
            };
            if best > NONE {
                row[j] = best + MATCH + bonus[j];
                from[i * m + j] = best_from;
            }
        }
    }

    let last = &score[(n - 1) * m..];
    let (mut j, best) = last.iter().copied().enumerate().max_by_key(|&(_, s)| s)?;
    if best <= NONE {
        return None;
    }
    let mut positions = vec![0; n];
    for i in (0..n).rev() {
        positions[i] = chars[j].0;
        j = from[i * m + j];
    }
    Some(FuzzyMatch {
        score: best,
        positions,
    })
}

#[cfg(test)]
mod tests {
    use super::fuzzy_match;

    fn score(query: &str, candidate: &str) -> Option<i32> {
        let name_start = candidate.rfind('/').map_or(0, |ix| ix + 1);
        fuzzy_match(query, candidate, name_start).map(|m| m.score)
    }

    #[test]
    fn matches_characters_in_order_ignoring_case() {
        assert!(score("mwin", "src/main_window.rs").is_some());
        assert!(score("MAIN", "src/main.rs").is_some());
        assert!(score("niam", "src/main.rs").is_none());
        assert!(score("mainx", "src/main.rs").is_none());
        assert_eq!(
            fuzzy_match("", "anything", 0).unwrap().positions,
            Vec::<usize>::new()
        );
    }

    #[test]
    fn keeps_the_best_alignment() {
        // `a` also appears earlier, but matching it in `app` keeps the run.
        let m = fuzzy_match("app", "a/src/app.rs", 6).unwrap();
        assert_eq!(m.positions, [6, 7, 8]);
        let m = fuzzy_match("mw", "src/main_window.rs", 4).unwrap();
        assert_eq!(m.positions, [4, 9]);
    }

    #[test]
    fn ranks_word_starts_names_and_runs_higher() {
        assert!(score("main", "src/main.rs") > score("main", "src/domain_main_helpers.rs/x"));
        assert!(score("tree", "src/project_tree.rs") > score("tree", "tree/src/other.rs"));
        assert!(score("pt", "src/project_tree.rs") > score("pt", "src/prompt.rs"));
    }
}
