//! The raw prompt modal: exactly what the harness was given for a task,
//! nothing rendered and nothing hidden. Opened from the latest task's header,
//! it shows the system prompt as the harness received it and the compiled
//! user prompt, each with a Copy button; for a harness that takes no system
//! prompt of its own, the one text it was given, the system prompt ahead of
//! the prompt. The text is plain, wrapped, selectable monospace, laid out only
//! where it is in view, however long it is.

use gpui_kit::assets::IconName;
use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

use crate::agent::Agent;
use crate::chat_input::SendMode;
use crate::harness;
use crate::measured_list::{MeasuredList, RenderRow};
use crate::scrollbar;
use crate::task_table;

/// What stands in for the system prompt when none was sent, as for Freeform.
pub const NO_SYSTEM_PROMPT: &str = "No system prompt";
/// What stands in for the system prompt of a task saved before it was kept.
pub const NOT_RECORDED: &str = "Not recorded";

/// How many lines of text each row of the list holds.
const LINES_PER_ROW: usize = 16;
/// The share of the window's height the modal takes.
const HEIGHT_SHARE: f32 = 0.85;
/// The widest the modal gets, and its share of a narrower window's width.
const MAX_WIDTH: Pixels = px(1000.);
const WIDTH_SHARE: f32 = 0.9;

/// What a task's harness was given besides its compiled prompt: which harness
/// it was, the system prompt as it received it, if any, the instructions that
/// headed its message, if any, and whether it carried on a conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Given {
    pub harness: Agent,
    pub system_prompt: Option<String>,
    pub instructions: Option<String>,
    pub resumed: bool,
}

/// A headed section of the modal, with its text, or, where there is none,
/// what is shown in its place.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub text: Result<SharedString, &'static str>,
}

impl Section {
    /// What its Copy button copies, if it has any text.
    pub fn copied(&self) -> Option<&SharedString> {
        self.text.as_ref().ok()
    }
}

/// What the modal shows for a task.
#[derive(Clone, Debug, PartialEq)]
pub struct RawPrompt {
    pub mode: Option<SendMode>,
    pub anchor: SharedString,
    /// The harness it was sent to, unless that wasn't recorded.
    pub harness: Option<Agent>,
    pub sections: Vec<Section>,
}

impl RawPrompt {
    /// What a task sent in `mode`, from the hidden anchor `anchor`, was given:
    /// its compiled `user_prompt`, and what else was `given` the harness, if
    /// that was recorded. A harness that takes no system prompt of its own
    /// was given one text, as [`harness::prompt_as_given`] makes it for the
    /// run itself.
    pub fn new(
        mode: Option<SendMode>,
        anchor: SharedString,
        user_prompt: &str,
        given: Option<&Given>,
    ) -> Self {
        // The message the prompt was sent as: its instructions, if it was
        // given any, then the prompt.
        let message = crate::system_prompts::with_instructions(
            given.and_then(|given| given.instructions.as_deref()),
            user_prompt,
        );
        let user_prompt = message.as_str();
        let sections = match given {
            Some(given) if !given.harness.takes_system_prompt() => vec![Section {
                title: "Prompt",
                text: Ok(harness::prompt_as_given(
                    given.harness,
                    user_prompt,
                    given.system_prompt.as_deref(),
                    given.resumed,
                )
                .into()),
            }],
            _ => vec![
                Section {
                    title: "System prompt",
                    text: match given {
                        Some(Given {
                            system_prompt: Some(system_prompt),
                            ..
                        }) => Ok(system_prompt.clone().into()),
                        Some(_) => Err(NO_SYSTEM_PROMPT),
                        None => Err(NOT_RECORDED),
                    },
                },
                Section {
                    title: "User prompt",
                    text: Ok(user_prompt.to_string().into()),
                },
            ],
        };
        Self {
            mode,
            anchor,
            harness: given.map(|given| given.harness),
            sections,
        }
    }

    /// With a section listing the images the harness was given alongside the
    /// prompt, by their paths from the project directory, one a line, in
    /// order; none without any.
    pub fn with_images(mut self, images: &[String]) -> Self {
        if !images.is_empty() {
            self.sections.push(Section {
                title: "Attached images",
                text: Ok(images.join("\n").into()),
            });
        }
        self
    }

    /// The line beneath the title: the task's mode, its hidden anchor's name,
    /// and the harness it was sent to, as far as each is known.
    pub fn subtitle(&self) -> String {
        [
            self.mode.map(|mode| mode.label().to_string()),
            (!self.anchor.is_empty()).then(|| self.anchor.to_string()),
            Some(match self.harness {
                Some(harness) => harness.label().to_string(),
                None => "Harness not recorded".to_string(),
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

/// A row of the modal's list.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// A section's heading and Copy button.
    Heading(usize),
    /// Some lines of a section's text: the section, and which of its rows.
    Text(usize, usize, SharedString),
    /// What stands in for a section with no text.
    Placeholder(usize, &'static str),
}

/// A section's text, cut into rows of a few lines each, so only the rows in
/// view are laid out.
fn text_rows(section: usize, text: &str) -> Vec<Row> {
    let lines: Vec<&str> = text.split('\n').collect();
    lines
        .chunks(LINES_PER_ROW)
        .enumerate()
        .map(|(ix, lines)| Row::Text(section, ix, lines.join("\n").into()))
        .collect()
}

/// The modal's content.
pub struct RawPromptView {
    prompt: RawPrompt,
    rows: Rc<[Row]>,
    list: MeasuredList,
}

#[cfg(test)]
thread_local! {
    /// The modal last opened on this thread, for tests to read.
    static LAST_OPENED: std::cell::RefCell<Option<WeakEntity<RawPromptView>>> =
        const { std::cell::RefCell::new(None) };
}

/// The modal last opened, while it still exists.
#[cfg(test)]
pub fn last_opened() -> Option<Entity<RawPromptView>> {
    LAST_OPENED.with_borrow(|view| view.as_ref().and_then(WeakEntity::upgrade))
}

impl RawPromptView {
    fn new(prompt: RawPrompt) -> Self {
        let rows: Rc<[Row]> = prompt
            .sections
            .iter()
            .enumerate()
            .flat_map(|(ix, section)| {
                let body: Vec<Row> = match &section.text {
                    Ok(text) => text_rows(ix, text),
                    Err(placeholder) => vec![Row::Placeholder(ix, placeholder)],
                };
                std::iter::once(Row::Heading(ix)).chain(body)
            })
            .collect();
        let list = MeasuredList::new(task_table::OVERDRAW);
        list.reset(rows.len());
        Self { prompt, rows, list }
    }

    #[cfg(test)]
    pub fn prompt(&self) -> &RawPrompt {
        &self.prompt
    }

    /// Opens the modal over `window`, showing `prompt`.
    pub fn open(prompt: RawPrompt, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let view = cx.new(|_| Self::new(prompt));
        #[cfg(test)]
        LAST_OPENED.with_borrow_mut(|last| *last = Some(view.downgrade()));
        let content = view.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let content = content.clone();
            let viewport = window.viewport_size();
            dialog
                .w((viewport.width * WIDTH_SHARE).min(MAX_WIDTH))
                .margin_top(viewport.height * (1. - HEIGHT_SHARE) / 2.)
                .p_0()
                .content(move |body, _, _| body.child(content.clone()))
        });
        view
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(row) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        match row {
            Row::Heading(section) => {
                let section_ix = *section;
                let section = &self.prompt.sections[section_ix];
                let copied = section.copied().cloned();
                let copy = Button::new(("raw-prompt-copy", section_ix))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .label("Copy")
                    .tooltip(format!("Copy the {}", section.title.to_lowercase()))
                    .disabled(copied.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(text) = &copied {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                        }
                    });
                let heading = h_flex()
                    .id(("raw-prompt-section", section_ix))
                    .justify_between()
                    .gap_2()
                    .when(section_ix > 0, |row| row.pt_4())
                    .pb_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(section.title),
                    )
                    // Lets UI tests find and click the button; inert in normal
                    // builds.
                    .child(gpui_kit::TestSupportExt::test_support(
                        div()
                            .id(("raw-prompt-copy-button", section_ix))
                            .flex_none()
                            .child(copy),
                    ));
                gpui_kit::TestSupportExt::test_support(heading).into_any_element()
            }
            Row::Text(section, row, text) => {
                let text = div()
                    .id(("raw-prompt-text", ix))
                    .w_full()
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(SelectableText::new(
                        SharedString::from(format!("raw-prompt-{section}-{row}")),
                        text.clone(),
                    ));
                // Lets UI tests find the row; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(text).into_any_element()
            }
            Row::Placeholder(section, placeholder) => {
                let placeholder = div()
                    .id(("raw-prompt-placeholder", *section))
                    .text_sm()
                    .italic()
                    .text_color(theme.muted_foreground)
                    .child(*placeholder);
                gpui_kit::TestSupportExt::test_support(placeholder).into_any_element()
            }
        }
    }
}

impl Render for RawPromptView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = window.viewport_size().height * HEIGHT_SHARE;
        let theme = cx.theme();
        let this = cx.entity().downgrade();
        let render: RenderRow = Rc::new(move |ix, _, cx| {
            this.update(cx, |this, cx| this.render_row(ix, cx))
                .unwrap_or_else(|_| div().into_any_element())
        });
        let title = v_flex()
            .flex_none()
            .gap_0p5()
            .px_4()
            // Room for the dialog's close button.
            .pr_12()
            .pt_4()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            // Lets UI tests find the title and what is beneath it; inert in
            // normal builds.
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("raw-prompt-title")
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Raw prompt"),
            ))
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("raw-prompt-subtitle")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(self.prompt.subtitle()),
            ));
        let list = div()
            .id("raw-prompt-list")
            .size_full()
            .px_4()
            .py_3()
            .child(self.list.element(render));
        let root = v_flex()
            .id("raw-prompt")
            .h(height)
            .w_full()
            .child(title)
            .child(div().flex_1().min_h_0().child(scrollbar::with_scrollbar(
                "raw-prompt-list",
                &self.list,
                // Lets UI tests find the list; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(list),
                true,
                None,
                cx,
            )));
        // Lets UI tests find the modal; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(root)
    }
}

#[cfg(test)]
mod tests {
    use super::{Given, NO_SYSTEM_PROMPT, NOT_RECORDED, RawPrompt, Row, text_rows};
    use crate::agent::Agent;
    use crate::chat_input::SendMode;

    #[test]
    fn claude_code_gets_the_system_prompt_apart() {
        let given = Given {
            harness: Agent::Claude,
            system_prompt: Some("Be brief.".into()),
            instructions: None,
            resumed: false,
        };
        let prompt = RawPrompt::new(
            Some(SendMode::Code),
            "Prompt_a".into(),
            "Fix it.",
            Some(&given),
        );
        let sections: Vec<_> = prompt
            .sections
            .iter()
            .map(|section| (section.title, section.copied().map(|text| text.to_string())))
            .collect();
        assert_eq!(
            sections,
            [
                ("System prompt", Some("Be brief.".to_string())),
                ("User prompt", Some("Fix it.".to_string())),
            ]
        );
        assert_eq!(prompt.subtitle(), "Code · Prompt_a · Claude Code");
    }

    #[test]
    fn codex_and_opencode_get_one_prompt() {
        for harness in [Agent::Codex, Agent::OpenCode] {
            let given = Given {
                harness,
                system_prompt: Some("Be brief.".into()),
                instructions: None,
                resumed: false,
            };
            let prompt = RawPrompt::new(None, "Prompt_a".into(), "Fix it.", Some(&given));
            assert_eq!(prompt.sections.len(), 1);
            assert_eq!(prompt.sections[0].title, "Prompt");
            assert_eq!(
                prompt.sections[0].copied().unwrap().as_ref(),
                crate::harness::prompt_as_given(harness, "Fix it.", Some("Be brief."), false)
            );
        }
    }

    /// The user prompt shown is the message as sent: its instructions in
    /// their block ahead of the prompt. A harness that takes no system prompt
    /// of its own is given it only on a conversation's first prompt.
    #[test]
    fn the_message_shows_its_instructions() {
        let given = Given {
            harness: Agent::Claude,
            system_prompt: Some("Be brief.".into()),
            instructions: Some("Only the code.".into()),
            resumed: true,
        };
        let prompt = RawPrompt::new(None, "Prompt_a".into(), "Fix it.", Some(&given));
        assert_eq!(prompt.sections[0].copied().unwrap().as_ref(), "Be brief.");
        assert_eq!(
            prompt.sections[1].copied().unwrap().as_ref(),
            "<task-instructions>\nOnly the code.\n</task-instructions>\n\nFix it."
        );
        let codex = |resumed| {
            let given = Given {
                harness: Agent::Codex,
                system_prompt: Some("Be brief.".into()),
                instructions: Some("Only the code.".into()),
                resumed,
            };
            RawPrompt::new(None, "Prompt_a".into(), "Fix it.", Some(&given)).sections[0]
                .copied()
                .unwrap()
                .to_string()
        };
        assert!(codex(false).starts_with("<system-prompt>\nBe brief."));
        assert!(codex(false).ends_with("</task-instructions>\n\nFix it."));
        assert!(
            codex(true).starts_with("<task-instructions>"),
            "resumed, it was given again"
        );
    }

    /// The images given alongside the prompt are listed by their paths, in
    /// a section of their own after the prompt, for any harness; a prompt
    /// with none has no such section.
    #[test]
    fn attached_images_are_listed() {
        let images = vec![
            ".suspense/images/1-a.png".to_string(),
            ".suspense/images/2-b.jpg".to_string(),
        ];
        for harness in [Agent::Claude, Agent::Codex] {
            let given = Given {
                harness,
                system_prompt: Some("Be brief.".into()),
                instructions: None,
                resumed: false,
            };
            let prompt =
                RawPrompt::new(None, "Prompt_a".into(), "Look.", Some(&given)).with_images(&images);
            let last = prompt.sections.last().unwrap();
            assert_eq!(last.title, "Attached images");
            assert_eq!(
                last.copied().unwrap().as_ref(),
                ".suspense/images/1-a.png\n.suspense/images/2-b.jpg"
            );
            let plain =
                RawPrompt::new(None, "Prompt_a".into(), "Look.", Some(&given)).with_images(&[]);
            assert!(
                plain
                    .sections
                    .iter()
                    .all(|section| section.title != "Attached images")
            );
        }
    }

    #[test]
    fn a_missing_system_prompt_says_why() {
        let none = Given {
            harness: Agent::Claude,
            system_prompt: None,
            instructions: None,
            resumed: false,
        };
        let sent = RawPrompt::new(None, "".into(), "Hi", Some(&none));
        assert_eq!(sent.sections[0].text, Err(NO_SYSTEM_PROMPT));
        let old = RawPrompt::new(None, "Prompt_a".into(), "Hi", None);
        assert_eq!(old.sections[0].text, Err(NOT_RECORDED));
        assert_eq!(old.subtitle(), "Prompt_a · Harness not recorded");
    }

    #[test]
    fn text_is_cut_into_rows_that_join_back_up() {
        let text: String = (0..40).map(|n| format!("line {n}\n")).collect();
        let rows = text_rows(1, &text);
        assert_eq!(rows.len(), 3);
        let joined = rows
            .iter()
            .map(|row| match row {
                Row::Text(1, _, text) => text.to_string(),
                row => panic!("{row:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(joined, text);
    }
}
