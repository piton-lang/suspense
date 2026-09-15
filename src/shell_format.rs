//! Shell commands laid out for reading: a command too long to follow on one
//! line is split at its top-level `&&`, `||`, `|` and `;`, each chained command
//! on a line of its own. Quoted text, substitutions, subshells, comments and
//! heredocs are left as they are.

/// Commands this short, on one line, read fine as they are.
const ONE_LINE_CHARS: usize = 60;

/// Commands chained with `&&`, `||` or `|` are indented under the first.
const INDENT: &str = "  ";

pub fn format_command(command: &str) -> String {
    let command = command.trim();
    if !command.contains('\n') && command.chars().count() <= ONE_LINE_CHARS {
        return command.to_string();
    }

    let chars: Vec<char> = command.chars().collect();
    let mut out = String::new();
    // Nesting of `(`, `{` and `$(`: only operators outside them are split.
    let mut depth = 0usize;
    // Heredoc delimiters whose bodies start after the current line.
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\\' => {
                out.push(c);
                out.extend(next);
                i += 2;
            }
            '\'' | '"' | '`' => i = copy_quoted(&chars, i, &mut out),
            '#' if out.chars().last().is_none_or(|prev| {
                prev.is_whitespace() || matches!(prev, ';' | '&' | '|' | '(')
            }) =>
            {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '(' | '{' => {
                depth += 1;
                out.push(c);
                i += 1;
            }
            ')' | '}' => {
                depth = depth.saturating_sub(1);
                out.push(c);
                i += 1;
            }
            '\n' => {
                out.push('\n');
                i += 1;
                for (delimiter, strip_tabs) in heredocs.drain(..) {
                    i = copy_heredoc_body(&chars, i, &delimiter, strip_tabs, &mut out);
                }
            }
            '<' if next == Some('<') && chars.get(i + 2) != Some(&'<') => {
                out.push_str("<<");
                i += 2;
                let strip_tabs = chars.get(i) == Some(&'-');
                if strip_tabs {
                    out.push('-');
                    i += 1;
                }
                let mut delimiter = String::new();
                while let Some(&c) = chars.get(i) {
                    if c.is_whitespace() && !delimiter.is_empty() {
                        break;
                    }
                    if matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')') {
                        break;
                    }
                    if !matches!(c, '\'' | '"' | '\\') && !c.is_whitespace() {
                        delimiter.push(c);
                    }
                    out.push(c);
                    i += 1;
                }
                if !delimiter.is_empty() {
                    heredocs.push((delimiter, strip_tabs));
                }
            }
            // Splitting before a heredoc's body would read as if the body
            // belonged to the last chained command, so its line stays whole.
            _ if depth > 0 || !heredocs.is_empty() => {
                out.push(c);
                i += 1;
            }
            '&' if next == Some('&') => i = break_line(&chars, i + 2, "&&", &mut out),
            '|' if next == Some('|') => i = break_line(&chars, i + 2, "||", &mut out),
            '|' if next == Some('&') => i = break_line(&chars, i + 2, "|&", &mut out),
            '|' => i = break_line(&chars, i + 1, "|", &mut out),
            ';' if next == Some(';') => {
                out.push_str(";;");
                i += 2;
            }
            ';' => {
                trim_line_end(&mut out);
                out.push_str(";\n");
                i = skip_blanks(&chars, i + 1);
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim_end().to_string()
}

/// Copies the quoted text starting at `start`, returning the index after it.
fn copy_quoted(chars: &[char], start: usize, out: &mut String) -> usize {
    let quote = chars[start];
    out.push(quote);
    let mut i = start + 1;
    while let Some(&c) = chars.get(i) {
        out.push(c);
        i += 1;
        if c == '\\' && quote != '\'' {
            out.extend(chars.get(i));
            i += 1;
        } else if c == quote {
            break;
        }
    }
    i
}

/// Copies a heredoc's body, up to and including its closing delimiter line.
fn copy_heredoc_body(
    chars: &[char],
    mut i: usize,
    delimiter: &str,
    strip_tabs: bool,
    out: &mut String,
) -> usize {
    while i < chars.len() {
        let end = chars[i..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |len| i + len);
        let line: String = chars[i..end].iter().collect();
        out.push_str(&line);
        i = end;
        if i < chars.len() {
            out.push('\n');
            i += 1;
        }
        let line = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        if line == delimiter {
            break;
        }
    }
    i
}

/// Starts a new, indented line with `operator`, returning the index of what
/// follows it.
fn break_line(chars: &[char], after: usize, operator: &str, out: &mut String) -> usize {
    trim_line_end(out);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(INDENT);
    out.push_str(operator);
    out.push(' ');
    skip_blanks(chars, after)
}

/// Skips spaces, and line breaks that only continued the command.
fn skip_blanks(chars: &[char], mut i: usize) -> usize {
    loop {
        match chars.get(i) {
            Some(' ' | '\t' | '\n') => i += 1,
            Some('\\') if chars.get(i + 1) == Some(&'\n') => i += 2,
            _ => return i,
        }
    }
}

/// Trims the blanks, and any line continuations, ending the current line.
fn trim_line_end(out: &mut String) {
    loop {
        let len = out.trim_end_matches([' ', '\t']).len();
        out.truncate(len);
        if !out.ends_with("\\\n") {
            return;
        }
        out.truncate(out.len() - 2);
    }
}

#[cfg(test)]
mod tests {
    use super::format_command;

    #[test]
    fn short_commands_stay_on_one_line() {
        assert_eq!(format_command("  ls -la | head  "), "ls -la | head");
    }

    #[test]
    fn chained_commands_get_a_line_each() {
        assert_eq!(
            format_command(
                "cd crates/app && cargo build --release 2>&1 | tail -20 || echo \"failed && more\"; git status"
            ),
            "cd crates/app\n  && cargo build --release 2>&1\n  | tail -20\n  || echo \"failed && more\";\ngit status"
        );
    }

    #[test]
    fn subshells_substitutions_and_comments_are_kept_whole() {
        assert_eq!(
            format_command(
                "(cd vendor && make) && echo \"$(git rev-parse HEAD | cut -c1-8)\" # note: a && b"
            ),
            "(cd vendor && make)\n  && echo \"$(git rev-parse HEAD | cut -c1-8)\" # note: a && b"
        );
    }

    #[test]
    fn heredoc_bodies_are_left_as_written() {
        let command = "cat <<'EOF' > notes.txt && echo written\nkeep && this | as is\nEOF\ngit add notes.txt && git commit -m notes";
        assert_eq!(
            format_command(command),
            "cat <<'EOF' > notes.txt && echo written\nkeep && this | as is\nEOF\ngit add notes.txt\n  && git commit -m notes"
        );
    }

    #[test]
    fn existing_continuations_are_folded_into_the_layout() {
        assert_eq!(
            format_command("cargo test --workspace --all-features \\\n  && cargo clippy --workspace -- -D warnings"),
            "cargo test --workspace --all-features\n  && cargo clippy --workspace -- -D warnings"
        );
    }
}
