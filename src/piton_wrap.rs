//! Hard wrapping a Piton file at 80 columns, as the EditorScope's wrap says:
//! only lines past the 80th column are split, each at a single space Piton
//! joins back as it was, so the file compiles to exactly what it did before.
//! Prose in a multi-line value breaks onto lines at its own indentation, and
//! a comment onto comments of its own; nothing else is ever broken.

use std::ops::Range;

/// The column long lines are wrapped at, the ruler's.
pub const COLUMN: usize = 80;

/// A wrap: each space broken at, by its byte offset in the text, and what
/// takes its place, a line break and the indentation, and for a comment its
/// `// `, of the line it starts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wrap {
    pub breaks: Vec<(usize, String)>,
}

impl Wrap {
    /// `text` wrapped.
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + self.breaks.len() * 8);
        let mut at = 0;
        for (space, replacement) in &self.breaks {
            out.push_str(&text[at..*space]);
            out.push_str(replacement);
            at = space + 1;
        }
        out.push_str(&text[at..]);
        out
    }

    /// Where `offset` in the text is once wrapped, so the cursor and the
    /// selection stay on the same text.
    pub fn offset(&self, offset: usize) -> usize {
        let grown: usize = self
            .breaks
            .iter()
            .filter(|(space, _)| *space < offset)
            .map(|(_, replacement)| replacement.len() - 1)
            .sum();
        offset + grown
    }

    /// The byte range of the text it changes, from the first space broken to
    /// just after the last; none when it breaks nothing.
    pub fn span(&self) -> Option<Range<usize>> {
        let first = self.breaks.first()?.0;
        let last = self.breaks.last()?.0;
        Some(first..last + 1)
    }
}

/// How `text`, a Piton file, wraps at [`COLUMN`], breaking only the lines in
/// `lines`, by their zero-based numbers.
pub fn wrap(text: &str, lines: Range<usize>) -> Wrap {
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&crate::piton_syntax::language())
        .is_err()
    {
        return Wrap::default();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Wrap::default();
    };
    let root = tree.root_node();
    let mut breaks = Vec::new();
    let mut line_start = 0;
    for (row, line) in text.split('\n').enumerate() {
        let start = line_start;
        line_start += line.len() + 1;
        if !lines.contains(&row) || line.chars().count() <= COLUMN {
            continue;
        }
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let Some(node) = root.descendant_for_byte_range(start + indent_len, start + indent_len + 1)
        else {
            continue;
        };
        // The node the line's first character begins, as Piton reads it.
        let mut top = node;
        while let Some(parent) = top.parent() {
            if parent.id() == root.id() {
                break;
            }
            top = parent;
        }
        if top.start_byte() != start + indent_len {
            continue;
        }
        let found = match top.kind() {
            "comment" => comment_breaks(line, indent),
            // Prose in a multi-line value, alone on its line, never at the
            // top level, where declarations and imports are.
            "prose" if indent_len > 0 && top.end_position().row == row => {
                prose_breaks(line, indent, start, top)
            }
            _ => Vec::new(),
        };
        breaks.extend(
            found
                .into_iter()
                .map(|(space, replacement)| (start + space, replacement)),
        );
    }
    Wrap { breaks }
}

/// Where a comment line breaks, its later lines comments of their own.
fn comment_breaks(line: &str, indent: &str) -> Vec<(usize, String)> {
    let body = indent.len() + 2;
    // Past the comment's own marker, and the space after it.
    let after_marker = body + line[body..].len() - line[body..].trim_start().len();
    let prefix = format!("{indent}// ");
    let allowed: Vec<usize> = single_spaces(line)
        .into_iter()
        .filter(|&space| space > after_marker)
        .collect();
    breaks_at(line, &allowed, prefix.chars().count())
        .into_iter()
        .map(|space| (space, format!("\n{prefix}")))
        .collect()
}

/// Where a prose line breaks: at a single space outside every expression and
/// escape wrapper, and before any comment that ends the line, starting no
/// line Piton would read as something else.
fn prose_breaks(
    line: &str,
    indent: &str,
    line_start: usize,
    node: tree_sitter::Node,
) -> Vec<(usize, String)> {
    let mut kept: Vec<Range<usize>> = escape_wrappers(line);
    let mut end = line.len();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let range = child.start_byte() - line_start..child.end_byte() - line_start;
        match child.kind() {
            "text" => {}
            "comment" => end = end.min(range.start),
            _ => kept.push(range),
        }
    }
    let allowed: Vec<usize> = single_spaces(line)
        .into_iter()
        .filter(|&space| space > indent.len() && space < end)
        .filter(|&space| !kept.iter().any(|range| range.contains(&space)))
        .filter(|&space| starts_plainly(&line[space + 1..]))
        .collect();
    breaks_at(line, &allowed, indent.chars().count())
        .into_iter()
        .map(|space| (space, format!("\n{indent}")))
        .collect()
}

/// Which of `allowed`, spaces in `line`, it breaks at: each the last before
/// [`COLUMN`] on its line, or, with none, the first after it; each line
/// after the first starting `prefix` columns in.
fn breaks_at(line: &str, allowed: &[usize], prefix: usize) -> Vec<usize> {
    let column = |from: usize, to: usize| line[from..to].chars().count();
    let mut breaks = Vec::new();
    let (mut from, mut taken) = (0, 0);
    while taken + column(from, line.len()) > COLUMN {
        let after = allowed.iter().copied().filter(|&space| space > from);
        let fitting = after
            .clone()
            .rfind(|&space| taken + column(from, space) <= COLUMN);
        let Some(space) = fitting.or_else(|| after.clone().next()) else {
            break;
        };
        breaks.push(space);
        from = space + 1;
        taken = prefix;
    }
    breaks
}

/// The byte offsets of the spaces in `line` with no space either side.
fn single_spaces(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    (1..bytes.len().saturating_sub(1))
        .filter(|&at| bytes[at] == b' ' && bytes[at - 1] != b' ' && bytes[at + 1] != b' ')
        .collect()
}

/// Whether a line beginning `rest` reads as prose, carrying on the line
/// before it: not a list item, composition line, comment, escape, or key,
/// nor an inline list.
fn starts_plainly(rest: &str) -> bool {
    const STARTS: [&str; 6] = ["- ", "+ ", "++ ", "//", "\\", "["];
    if STARTS.iter().any(|start| rest.starts_with(start)) {
        return false;
    }
    // A word followed by a colon and a space is a key.
    let word_end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    !(word_end > 0 && rest[word_end..].starts_with(": "))
}

/// The byte ranges of `line`'s escape wrappers: a run of backslashes and a
/// space, to a space and the same run closing it, each kept whole.
fn escape_wrappers(line: &str) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let run = |at: usize| bytes[at..].iter().take_while(|&&b| b == b'\\').count();
    let mut wrappers = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let opens = bytes[at] == b'\\'
            && (at == 0 || bytes[at - 1] != b'\\')
            && run(at) > 0
            && bytes.get(at + run(at)) == Some(&b' ');
        if !opens {
            at += 1;
            continue;
        }
        let count = run(at);
        // The same run, after a space, with no more backslashes beside it.
        let closing = (at + count + 1..bytes.len()).find(|&close| {
            bytes[close - 1] == b' '
                && run(close) == count
                && (close == 0 || bytes[close - 1] != b'\\')
        });
        match closing {
            Some(close) => {
                wrappers.push(at..close + count);
                at = close + count;
            }
            None => at += count,
        }
    }
    wrappers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(text: &str) -> String {
        wrap(text, 0..usize::MAX).apply(text)
    }

    fn long(words: usize) -> String {
        (0..words)
            .map(|n| format!("word{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Prose past the 80th column breaks at the last space before it, the
    /// lines after taking its indentation; short lines are left alone.
    #[test]
    fn long_prose_breaks_at_its_indentation() {
        let text = format!("scope A:\n    pitch:\n        {}\n        short\n", long(30));
        let out = wrapped(&text);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 5, "{out}");
        for line in &lines[2..lines.len() - 1] {
            assert!(line.chars().count() <= COLUMN, "{line:?} runs long");
            assert!(line.starts_with("        word"), "{line:?}");
        }
        assert_eq!(lines.last(), Some(&"        short"));
        // Joined back with single spaces, it is the same text.
        let joined: Vec<&str> = lines[2..lines.len() - 1].iter().map(|l| l.trim()).collect();
        assert_eq!(joined.join(" "), long(30));
    }

    /// Keys with values, list items, imports, and top-level lines are never
    /// broken, however long.
    #[test]
    fn keys_items_and_declarations_are_left_alone() {
        for text in [
            format!("scope A:\n    name: {}\n", long(30)),
            format!("scope A:\n    items:\n        - {}\n", long(30)),
            format!("scope A:\n    items:\n        + {}\n", long(30)),
            format!("use /lib/{}\n", "x".repeat(90)),
            format!("{} B:\n", long(30)),
        ] {
            assert_eq!(wrapped(&text), text);
        }
    }

    /// No break is made inside an expression or an escape wrapper, nor
    /// where the line it starts would read as something else.
    #[test]
    fn breaks_keep_what_piton_reads() {
        let filler = "a".repeat(60);
        for (inside, what) in [
            ("{one two three four five six}", "an expression"),
            ("@{One.two three}", "a reference"),
            ("\\ {one two three four five} \\", "an escape wrapper"),
        ] {
            let text = format!("scope A:\n    pitch:\n        {filler} {inside} tail\n");
            let out = wrapped(&text);
            assert!(out.contains(inside), "{what} was broken: {out}");
        }
        for start in ["- item", "+ more", "// not", "key: value", "[a, b]"] {
            let text = format!("scope A:\n    pitch:\n        {} {start} {}\n", "b".repeat(72), long(4));
            let out = wrapped(&text);
            for line in out.lines().skip(2) {
                assert!(!line.trim_start().starts_with(start), "{start:?} starts a line: {out}");
            }
        }
    }

    /// Escape blocks and code fences are kept exactly.
    #[test]
    fn blocks_and_fences_are_kept() {
        let text = format!(
            "scope A:\n    raw:\n        \\\\\\\n        {}\n        \\\\\\\n    code:\n        ```text\n        {}\n        ```\n",
            long(30),
            long(30)
        );
        assert_eq!(wrapped(&text), text);
    }

    /// A long comment breaks into comments of its own.
    #[test]
    fn comments_break_into_comments() {
        let text = format!("    // {}\n", long(30));
        let out = wrapped(&text);
        assert!(out.lines().count() > 1, "{out}");
        for line in out.lines() {
            assert!(line.starts_with("    // word"), "{line:?}");
            assert!(line.chars().count() <= COLUMN, "{line:?}");
        }
    }

    /// A line with nowhere it may break is left as it is; one with no space
    /// before the 80th column breaks at the first after it.
    #[test]
    fn breaks_at_the_first_space_after_when_none_fits() {
        let unbroken = format!("scope A:\n    pitch:\n        {}\n", "x".repeat(100));
        assert_eq!(wrapped(&unbroken), unbroken);
        let text = format!("scope A:\n    pitch:\n        {} end\n", "y".repeat(90));
        assert_eq!(
            wrapped(&text),
            format!("scope A:\n    pitch:\n        {}\n        end\n", "y".repeat(90))
        );
    }

    /// Wrapping a whole project's spec leaves what it compiles to exactly as
    /// it was, every file of its reference the same.
    #[test]
    fn a_wrapped_spec_compiles_the_same() {
        use std::path::{Path, PathBuf};
        if crate::piton_build::piton_missing() {
            return;
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("suspense-wrap-build-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    copy(&path, &to.join(entry.file_name()));
                } else {
                    std::fs::copy(&path, to.join(entry.file_name())).unwrap();
                }
            }
        }
        copy(&manifest.join("project-templates/scope-concept-shape/spec"), &dir.join("spec"));
        std::fs::copy(manifest.join("piton.config.pi"), dir.join("piton.config.pi")).unwrap();
        std::fs::write(
            dir.join("spec/scope/application/index.pi"),
            "use /lib\n\n\
             // A long comment that goes on and on, well past the eightieth column of the ruler, so it wraps.\n\
             export scope ApplicationScope:\n    concept: {ApplicationConcept}\n\n\
             concept ApplicationConcept:\n    pitch:\n        \
             What the application is for, said at a length that runs well past the ruler, with {1 + 2} and \\ {not an expression} \\ in it - and + then more words.\n        \
             A second line, also long enough to run past the eightieth column of the editor, and on.\n\n        \
             A second paragraph, which a blank line keeps apart, running long past the ruler once again here.\n\n    \
             shape: {ApplicationShape}\n\n\
             shape ApplicationShape:\n    description:\n        \
             What the application is like to use: what it shows, and what can be done with it, at some length.\n",
        )
        .unwrap();
        let build = |dir: &Path| {
            let out = std::process::Command::new("piton")
                .arg("build")
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            fn files(at: &Path, all: &mut Vec<(PathBuf, String)>) {
                for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        files(&path, all);
                    } else {
                        all.push((path.clone(), std::fs::read_to_string(&path).unwrap_or_default()));
                    }
                }
            }
            let mut all = Vec::new();
            files(&dir.join(".claude/reference"), &mut all);
            all.sort();
            all
        };
        let before = build(&dir);
        assert!(!before.is_empty());
        let file = dir.join("spec/scope/application/index.pi");
        let text = std::fs::read_to_string(&file).unwrap();
        let wrapped = wrap(&text, 0..usize::MAX).apply(&text);
        assert_ne!(wrapped, text, "nothing was wrapped");
        for line in wrapped.lines() {
            assert!(
                line.chars().count() <= COLUMN || !line.contains(' '),
                "{line:?} still runs long:\n{wrapped}"
            );
        }
        std::fs::write(&file, &wrapped).unwrap();
        assert_eq!(build(&dir), before, "the wrapped spec compiles otherwise:\n{wrapped}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only the lines asked for are broken; offsets after a break move with
    /// the text.
    #[test]
    fn only_the_lines_asked_for_are_wrapped() {
        let text = format!(
            "scope A:\n    a:\n        {}\n    b:\n        {}\n",
            long(30),
            long(30)
        );
        let wrap = wrap(&text, 4..5);
        let out = wrap.apply(&text);
        assert!(out.contains(&format!("        {}\n", long(30))), "{out}");
        assert_ne!(out, text);
        let end = text.len();
        assert_eq!(wrap.offset(end), out.len());
        assert_eq!(wrap.offset(0), 0);
        let span = wrap.span().unwrap();
        assert!(span.start > text.find("    b:").unwrap());
    }
}
