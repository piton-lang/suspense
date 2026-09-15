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
