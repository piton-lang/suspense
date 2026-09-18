//! A task's understanding file: the short list of constraints the harness
//! takes from the spec for a Code, Chain, or Spec task and keeps current as
//! it works, saved beside the task's history record. The referenced spec
//! sidebar shows it as it changes (see [`crate::referenced_spec`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::file_link;

/// How often a running task's understanding file is read again.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a row that was added or changed stays highlighted, fading out.
pub const HIGHLIGHT_TIME: Duration = Duration::from_millis(1500);

/// The understanding file of the task whose history record is `prompt_file`.
pub fn path(prompt_file: &Path) -> PathBuf {
    prompt_file.with_extension("understanding.md")
}

/// A constraint as the file lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    /// Its item's text, with any link shown as its text alone.
    pub text: String,
    /// Where its first link points, as written.
    pub link: Option<String>,
}

/// The items of the Markdown list in `markdown`, in order. Anything that
/// isn't a list item, fenced code included, is left out.
pub fn parse(markdown: &str) -> Vec<Constraint> {
    let mut constraints = Vec::new();
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let line = line.trim();
        if let Some(open) = fence {
            if line.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(&line[..3]);
            continue;
        }
        let Some(item) = list_item(line) else {
            continue;
        };
        let (text, link) = without_links(item.trim());
        if !text.is_empty() {
            constraints.push(Constraint { text, link });
        }
    }
    constraints
}

/// A list item's text: after `- `, `* `, `+ `, or a number and `. ` or `) `.
fn list_item(line: &str) -> Option<&str> {
    if let Some(rest) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| line.strip_prefix(marker))
    {
        return Some(rest);
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &line[digits..];
    (digits > 0)
        .then(|| rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")))
        .flatten()
}

/// `text` with each `[label](target)` shown as its label, and the first
/// target.
fn without_links(text: &str) -> (String, Option<String>) {
    let mut shown = String::with_capacity(text.len());
    let mut first = None;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let link = rest[open + 1..].find("](").and_then(|close| {
            let label = &rest[open + 1..open + 1 + close];
            let after = &rest[open + 1 + close + 2..];
            let end = after.find(')')?;
            Some((label, &after[..end], &after[end + 1..]))
        });
        match link {
            Some((label, target, after)) => {
                shown.push_str(&rest[..open]);
                shown.push_str(label);
                first.get_or_insert_with(|| target.trim().to_string());
                rest = after;
            }
            None => {
                shown.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    shown.push_str(rest);
    (shown, first)
}

/// A constraint as read from disk, with the file its link points to if that
/// file exists.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub constraint: Constraint,
    pub target: Option<PathBuf>,
}

/// Reads the understanding `file` and works out where its links point, read
/// from `project_dir`; `None` while there is no file.
pub fn read(file: &Path, project_dir: &Path) -> Option<Vec<Read>> {
    let markdown = std::fs::read_to_string(file).ok()?;
    Some(
        parse(&markdown)
            .into_iter()
            .map(|constraint| Read {
                target: constraint
                    .link
                    .as_deref()
                    .and_then(|link| file_link::resolve(link, Some(project_dir))),
                constraint,
            })
            .collect(),
    )
}

/// A row the sidebar shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub text: String,
    /// Whether its item links anywhere.
    pub linked: bool,
    /// The file it opens, when its link points to one that exists.
    pub target: Option<PathBuf>,
    /// When it was added or last changed, while that is recent enough to
    /// highlight.
    pub changed: Option<Instant>,
}

/// A task's understanding as last read.
#[derive(Clone, Debug, Default)]
pub struct Understanding {
    /// Whether the file exists yet.
    pub exists: bool,
    pub rows: Vec<Row>,
}

impl Understanding {
    /// Takes in the file as read, highlighting rows that are new or whose
    /// text changed. Returns whether anything shown changed.
    pub fn update(&mut self, read: Option<Vec<Read>>) -> bool {
        let Some(read) = read else {
            let changed = self.exists;
            *self = Self::default();
            return changed;
        };
        let now = Instant::now();
        let rows: Vec<Row> = read
            .into_iter()
            .map(|Read { constraint, target }| {
                let changed = match self.rows.iter().find(|row| row.text == constraint.text) {
                    Some(old) => old.changed,
                    None => Some(now),
                };
                Row {
                    text: constraint.text,
                    linked: constraint.link.is_some(),
                    target,
                    changed,
                }
            })
            .collect();
        let changed = !self.exists
            || rows.len() != self.rows.len()
            || rows
                .iter()
                .zip(&self.rows)
                .any(|(new, old)| new.text != old.text || new.target != old.target);
        self.exists = true;
        self.rows = rows;
        changed
    }

    /// How strongly row `row` is highlighted, from 1 as it changes down to 0.
    pub fn highlight(row: &Row) -> f32 {
        row.changed.map_or(0., |at| {
            1. - (at.elapsed().as_secs_f32() / HIGHLIGHT_TIME.as_secs_f32()).min(1.)
        })
    }

    /// Whether any row is still fading.
    pub fn fading(&self) -> bool {
        self.rows.iter().any(|row| Self::highlight(row) > 0.)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Constraint, Read, Understanding, parse, path};

    #[test]
    fn saved_beside_the_history_record() {
        assert_eq!(
            path(Path::new("/p/.suspense/history/1-Prompt_a.pi")),
            Path::new("/p/.suspense/history/1-Prompt_a.understanding.md")
        );
    }

    /// Only list items are kept, each link shown as its text and the first
    /// one kept as where it points.
    #[test]
    fn keeps_list_items_only() {
        let markdown = "# Constraints\n\nSome prose.\n\
            - [The chain tab has no bottom border](spec/chat-input/index.pi)\n\
            * Plain, with no link\n\
            1. See [a](a.pi) and [b](b.pi)\n\
            ```\n- not an item\n```\n\
            -not an item either\n";
        assert_eq!(
            parse(markdown),
            [
                Constraint {
                    text: "The chain tab has no bottom border".into(),
                    link: Some("spec/chat-input/index.pi".into()),
                },
                Constraint {
                    text: "Plain, with no link".into(),
                    link: None,
                },
                Constraint {
                    text: "See a and b".into(),
                    link: Some("a.pi".into()),
                },
            ]
        );
    }

    /// Rows that are new or changed are highlighted; a row kept as it was
    /// keeps its highlight, and a vanished file empties it.
    #[test]
    fn highlights_new_and_changed_rows() {
        let read = |texts: &[&str]| {
            Some(
                texts
                    .iter()
                    .map(|text| Read {
                        constraint: Constraint {
                            text: text.to_string(),
                            link: None,
                        },
                        target: None,
                    })
                    .collect(),
            )
        };
        let mut understanding = Understanding::default();
        assert!(!understanding.update(None));
        assert!(understanding.update(read(&["a", "b"])));
        assert!(understanding.rows.iter().all(|row| row.changed.is_some()));
        for row in &mut understanding.rows {
            row.changed = None;
        }
        assert!(!understanding.update(read(&["a", "b"])));
        assert!(understanding.update(read(&["a", "c", "d"])));
        let changed: Vec<bool> = understanding
            .rows
            .iter()
            .map(|row| row.changed.is_some())
            .collect();
        assert_eq!(changed, [false, true, true]);
        assert!(understanding.fading());
        assert!(understanding.update(None));
        assert!(!understanding.exists && understanding.rows.is_empty());
    }
}
