//! What an Ask answer hands back for the user, as the AskConversationScope
//! says: prompts to send, as fenced code blocks whose info string is
//! `suspense-prompt` and the mode they are for, and questions asked back, as
//! `suspense-question` blocks, the question on their first lines and then
//! the answers to pick from, one `- ` line each. Each is shown as a card in
//! the answer where it stands, never as a code block.

use crate::chat_input::SendMode;

/// A part of an answer's text, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum AnswerPart {
    /// Markdown, shown as ever.
    Markdown(String),
    /// A prompt for the user to send: the mode it is for, if one of the
    /// five, its text as written, and whether its block has closed.
    Prompt {
        mode: Option<SendMode>,
        text: String,
        closed: bool,
    },
    /// A question asked back: the question, as markdown, the answers offered
    /// in order, and whether its block has closed.
    Question {
        question: String,
        answers: Vec<String>,
        closed: bool,
    },
}

/// The mode a `suspense-prompt` block names, if it is one of the five.
pub fn mode_named(name: &str) -> Option<SendMode> {
    match name.trim().to_ascii_lowercase().as_str() {
        "code" => Some(SendMode::Code),
        "chain" => Some(SendMode::Both),
        "spec" => Some(SendMode::Spec),
        "ask" => Some(SendMode::Ask),
        "freeform" => Some(SendMode::Freeform),
        _ => None,
    }
}

/// Whether `text` might hold a block for a card, so text without any is
/// shown as ever, untouched.
pub fn has_blocks(text: &str) -> bool {
    text.contains("suspense-prompt") || text.contains("suspense-question")
}

/// A fence opening a code block: its character, how many of them, and its
/// info string.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start();
    let c = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = line.chars().take_while(|ch| *ch == c).count();
    (count >= 3).then(|| (c, count, line[count..].trim()))
}

/// Whether `line` closes a block opened with `count` of `c`.
fn closes(line: &str, c: char, count: usize) -> bool {
    let line = line.trim();
    line.chars().count() >= count && line.chars().all(|ch| ch == c)
}

/// `text` split into its parts, in order: markdown, and the blocks shown as
/// cards. A block still open at the end, as one streaming in, is a card all
/// the same, not closed. Blocks inside other code blocks are left as code.
pub fn split(text: &str) -> Vec<AnswerPart> {
    let mut parts = Vec::new();
    let mut markdown = String::new();
    let mut lines = text.split_inclusive('\n').peekable();
    // An ordinary code block being passed over, by its fence.
    let mut in_code: Option<(char, usize)> = None;
    while let Some(line) = lines.next() {
        if let Some((c, count)) = in_code {
            if closes(line, c, count) {
                in_code = None;
            }
            markdown.push_str(line);
            continue;
        }
        let Some((c, count, info)) = fence(line) else {
            markdown.push_str(line);
            continue;
        };
        let mut words = info.split_whitespace();
        let kind = words.next().unwrap_or_default();
        if kind != "suspense-prompt" && kind != "suspense-question" {
            in_code = Some((c, count));
            markdown.push_str(line);
            continue;
        }
        let mode = words.next().and_then(mode_named);
        let mut body = String::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if closes(line, c, count) {
                closed = true;
                break;
            }
            body.push_str(line);
        }
        if !markdown.trim().is_empty() {
            parts.push(AnswerPart::Markdown(std::mem::take(&mut markdown)));
        }
        markdown.clear();
        let body = body.trim_end_matches(['\n', '\r']).to_string();
        parts.push(if kind == "suspense-prompt" {
            AnswerPart::Prompt {
                mode,
                text: body,
                closed,
            }
        } else {
            let (question, answers) = question_and_answers(&body);
            AnswerPart::Question {
                question,
                answers,
                closed,
            }
        });
    }
    if !markdown.trim().is_empty() {
        parts.push(AnswerPart::Markdown(markdown));
    }
    parts
}

/// A question block's question, its lines before the answers, and the
/// answers, each a line begun with a dash and a space, ending the block.
fn question_and_answers(body: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = body.lines().collect();
    let first_answer = lines
        .iter()
        .rposition(|line| !line.trim_start().starts_with("- ") && !line.trim().is_empty())
        .map_or(0, |last| last + 1);
    let answers = lines[first_answer..]
        .iter()
        .filter_map(|line| line.trim_start().strip_prefix("- "))
        .map(|answer| answer.trim().to_string())
        .filter(|answer| !answer.is_empty())
        .collect();
    let question = lines[..first_answer].join("\n").trim().to_string();
    (question, answers)
}

/// The question asked when `answer` is picked on a card asking `question`:
/// the question quoted, then the answer on a paragraph of its own.
pub fn picked_question(question: &str, answer: &str) -> String {
    let quoted = question
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{quoted}\n\n{answer}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_and_questions_are_parted_from_the_markdown() {
        let text = "Try this:\n\n```suspense-prompt chain\nAdd a Save button.\n```\n\nThen:\n\n```suspense-question\nWhich colour?\n- Red\n- Blue\n```\n";
        assert_eq!(
            split(text),
            vec![
                AnswerPart::Markdown("Try this:\n\n".into()),
                AnswerPart::Prompt {
                    mode: Some(SendMode::Both),
                    text: "Add a Save button.".into(),
                    closed: true,
                },
                AnswerPart::Markdown("\nThen:\n\n".into()),
                AnswerPart::Question {
                    question: "Which colour?".into(),
                    answers: vec!["Red".into(), "Blue".into()],
                    closed: true,
                },
            ]
        );
    }

    #[test]
    fn unknown_modes_open_blocks_and_code_are_handled() {
        assert_eq!(
            split("```suspense-prompt sideways\nGo.\n```"),
            vec![AnswerPart::Prompt {
                mode: None,
                text: "Go.".into(),
                closed: true
            }]
        );
        // Still streaming, or never closed: a card all the same.
        assert_eq!(
            split("```suspense-question\nWhy?\n- Because"),
            vec![AnswerPart::Question {
                question: "Why?".into(),
                answers: vec!["Because".into()],
                closed: false
            }]
        );
        // Inside another code block, it is code.
        let code = "````markdown\n```suspense-prompt code\nx\n```\n````\n";
        assert_eq!(split(code), vec![AnswerPart::Markdown(code.into())]);
        // A question with a list in it keeps it, and offers nothing.
        assert_eq!(
            split("```suspense-question\nPick one:\n- a\n- b\nOr say.\n```"),
            vec![AnswerPart::Question {
                question: "Pick one:\n- a\n- b\nOr say.".into(),
                answers: Vec::new(),
                closed: true
            }]
        );
    }

    #[test]
    fn a_pick_is_asked_with_the_question_quoted() {
        assert_eq!(
            picked_question("Which?\n\nReally?", "Blue"),
            "> Which?\n>\n> Really?\n\nBlue"
        );
    }
}
