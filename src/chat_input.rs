//! Chat input: a multi-line, Piton-highlighted editor on the left and a send
//! button on the right, in the body of the Code, Chain, Spec, and Ask tabs.
//! Enter adds a line; Ctrl/Cmd+Enter sends, or queues the prompt while the
//! harness works.
//! Ctrl+Tab and Ctrl+Shift+Tab cycle the tabs, Tab and Shift+Tab move focus
//! out of the input, and Esc takes focus out of it.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::Focusable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Enter, Escape, IndentInline, InputEvent, MoveDown,
    MoveUp, OutdentInline, Rope,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{CompletionContext, CompletionResponse};

use crate::completion_menu::CompletionMenu;
use crate::growing_input::GrowToFit;
use crate::harness_mentions;
use crate::main_window::FocusChat;
use crate::piton_lsp::PitonSession;
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::project_lsp::ProjectLsp;

/// Keys the preview's markdown apart from any task's.
const PREVIEW_TABLE: usize = usize::MAX;

/// The input grows with its text up to this many rows, then scrolls.
const MAX_ROWS: usize = 12;

#[cfg(target_os = "macos")]
const SEND_SHORTCUT: &str = "⌘Enter";
#[cfg(not(target_os = "macos"))]
const SEND_SHORTCUT: &str = "Ctrl+Enter";

#[cfg(target_os = "macos")]
const SEND_MENU_SHORTCUT: &str = "⌘⇧Enter";
#[cfg(not(target_os = "macos"))]
const SEND_MENU_SHORTCUT: &str = "Ctrl+Shift+Enter";

actions!(
    chat_input,
    [NextMode, PreviousMode, ToggleSendMenu, SendPreviewed]
);

#[cfg(target_os = "macos")]
const PREVIEW_HINT: &str = "⌘Enter to send · Esc to edit";
#[cfg(not(target_os = "macos"))]
const PREVIEW_HINT: &str = "Ctrl+Enter to send · Esc to edit";

#[cfg(target_os = "macos")]
const EDITING_HINT: &str = "⌘Enter to save · Esc to cancel";
#[cfg(not(target_os = "macos"))]
const EDITING_HINT: &str = "Ctrl+Enter to save · Esc to cancel";

/// The other ways to send a prompt, in the send button's menu, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOption {
    PreviewCompiled,
    Queue,
}

impl SendOption {
    pub const ALL: [SendOption; 2] = [SendOption::PreviewCompiled, SendOption::Queue];

    fn label(self) -> &'static str {
        match self {
            Self::PreviewCompiled => "Preview Compiled Prompt",
            Self::Queue => "Queue",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::PreviewCompiled => IconName::Eye,
            Self::Queue => IconName::ListEnd,
        }
    }

    /// Whether it can be picked in `mode`: a question is never queued.
    fn enabled(self, mode: SendMode) -> bool {
        !(self == Self::Queue && mode == SendMode::Ask)
    }
}

/// What editing a queued prompt set aside, to bring back after.
struct SetAside {
    text: String,
    attachments: Vec<Attachment>,
    tab: usize,
}

/// A queued prompt being edited in the input.
struct Editing {
    /// Its place in the queue, counted from 1.
    position: usize,
    set_aside: SetAside,
}

/// Emitted when editing a queued prompt is over: saved, with what it now
/// holds, or cancelled.
pub enum QueuedEdit {
    Saved {
        text: String,
        mode: SendMode,
        attached_text: Vec<String>,
    },
    Cancelled,
}

/// The prompt as it would be sent, shown in place of the text input.
#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Compiling,
    /// The markdown the harness would receive.
    Compiled(String),
    /// Why it could not be compiled.
    Failed(String),
}

/// Emitted to compile the prompt for a preview; its result comes back to
/// [`ChatInput::set_preview`] with the same `id`.
pub struct PreviewPrompt {
    pub id: usize,
    pub text: String,
    pub mode: SendMode,
    pub attached_text: Vec<String>,
}

const CONTEXT: &str = "ChatInput";

/// Ctrl+Tab and Ctrl+Shift+Tab cycle the tabs: Ctrl on macOS too.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", NextMode, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousMode, Some(CONTEXT)),
        // Ctrl/Cmd+Shift+Enter opens the send button's menu.
        KeyBinding::new("secondary-shift-enter", ToggleSendMenu, Some(CONTEXT)),
        // Sends a previewed prompt, when the text input isn't there to.
        KeyBinding::new("secondary-enter", SendPreviewed, Some(CONTEXT)),
    ]);
}

/// The tab selected to start with: the chain, Code and Spec together.
const DEFAULT_TAB: usize = 1;

/// The tabs the input sits in, in the order Ctrl+Tab cycles them.
const TABS: [SendMode; 4] = [
    SendMode::Code,
    SendMode::Both,
    SendMode::Spec,
    SendMode::Ask,
];

/// Index of the chain tab in `TABS`.
const BOTH_TAB: usize = 1;

/// Index of the Ask tab in `TABS`.
const ASK_TAB: usize = 3;

/// How far the cover over the seam between two locked tabs reaches either
/// side of it: past both 1px borders, with a pixel to spare.
const SEAM_COVER_REACH: Pixels = px(2.);

/// The chain's width until its tab has been laid out: an icon tab is a square
/// a little wider than the icon it centres.
const CHAIN_WIDTH_ESTIMATE: Pixels = px(40.);

/// How the tint and the chain's icon slide between tabs: critically damped,
/// so they settle without bouncing past.
const CHAIN_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);

/// How strongly the selected tabs and their body are tinted: red for Code,
/// blue for Spec, purple for both, and green for Ask.
const TINT_OPACITY: f32 = 0.1;

/// The tint at `position` between the tabs: red at Code (0), blue at Spec
/// (2), purple for both (1), where they meet, and green at Ask (3). The tabs
/// wrap around, so past Ask it blends straight back to Code's red at 4.
fn tint(red: Hsla, blue: Hsla, green: Hsla, position: f32) -> Hsla {
    let position = position.rem_euclid(TABS.len() as f32);
    if position > 3. {
        blend(green, red, position - 3.)
    } else if position > 2. {
        blend(blue, green, position - 2.)
    } else {
        blend(red, blue, position / 2.)
    }
}

/// The tint of the tab `mode` is sent from, as the chat input shows it.
pub fn mode_tint(mode: SendMode, cx: &App) -> Hsla {
    let position = TABS.iter().position(|tab| *tab == mode).unwrap_or(0);
    let theme = cx.theme();
    tint(theme.red, theme.blue, theme.green, position as f32)
}

/// `from` blended `t` of the way to `to`, at the tint's opacity.
fn blend(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0., 1.);
    // The shorter way round the hue circle, which from red to blue passes
    // through purple rather than green.
    let hue_step = (to.h - from.h + 0.5).rem_euclid(1.) - 0.5;
    Hsla {
        h: (from.h + hue_step * t).rem_euclid(1.),
        s: from.s + (to.s - from.s) * t,
        l: from.l + (to.l - from.l) * t,
        a: TINT_OPACITY,
    }
}

/// How much of tab `ix`'s tint shows with the tint at `position`: all of it
/// while the tab is selected, alone or (for Code and Spec) with the other,
/// fading out over one tab either side.
fn tab_tint_shown(ix: usize, position: f32) -> f32 {
    let (first, last) = match TABS[ix] {
        SendMode::Code => (0., 1.),
        SendMode::Spec => (1., 2.),
        SendMode::Both | SendMode::Ask => (ix as f32, ix as f32),
    };
    // The tabs wrap around, so Code is as near Ask as Chain is to Code.
    let count = TABS.len() as f32;
    let position = position.rem_euclid(count);
    let distance = [position - count, position, position + count]
        .into_iter()
        .map(|position| (first - position).max(position - last).max(0.))
        .fold(f32::MAX, f32::min);
    (1. - distance).clamp(0., 1.)
}

/// What a prompt is sent to work on: the selected tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SendMode {
    Code,
    /// The code and the spec together.
    Both,
    Spec,
    /// A question, which edits neither the code nor the spec.
    Ask,
}

impl SendMode {
    /// Every mode, in the order of their tabs.
    pub const ALL: [SendMode; 4] = TABS;

    pub fn label(self) -> &'static str {
        match self {
            SendMode::Code => "Code",
            SendMode::Both => "Code and Spec",
            SendMode::Spec => "Spec",
            SendMode::Ask => "Ask",
        }
    }

    /// The name the mode is saved under.
    pub fn key(self) -> &'static str {
        match self {
            SendMode::Code => "code",
            SendMode::Both => "combined",
            SendMode::Spec => "spec",
            SendMode::Ask => "ask",
        }
    }

    /// The mode saved under `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    /// What sending in the mode is for, shown beside the tabs.
    pub fn help(self) -> &'static str {
        match self {
            SendMode::Code => "Changes the code, leaving the spec as it is.",
            SendMode::Both => "Changes the code and the spec together.",
            SendMode::Spec => "Changes the spec, leaving the code as it is.",
            SendMode::Ask => "Asks a question about the code and the spec, changing neither.",
        }
    }

    fn id(self) -> &'static str {
        match self {
            SendMode::Code => "code-tab",
            SendMode::Both => "both-tab",
            SendMode::Spec => "spec-tab",
            SendMode::Ask => "ask-tab",
        }
    }
}

/// Emitted with the input's text, and the mode it is sent in, when the user
/// sends it.
pub struct Submit {
    pub text: String,
    pub mode: SendMode,
    /// The text attached to the prompt, in the order it was attached.
    pub attached_text: Vec<String>,
    /// Queued on purpose, rather than sent if the harness is free.
    pub queue: bool,
}

/// Something attached to the prompt being written, sent along with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub id: usize,
    pub text: String,
}

pub struct ChatInput {
    editor: Entity<EditorState>,
    completion: Entity<CompletionMenu>,
    /// The tab bar and its body, which Esc moves focus to: out of the input,
    /// but not onto nothing, where the window would hand it straight back.
    focus_handle: FocusHandle,
    /// Index into `TABS`.
    selected_tab: usize,
    /// Where the tint is sliding to: the selected tab's index, counted on
    /// past the last tab when Ctrl+Tab wraps around, so the tint moves straight
    /// from Ask to Code instead of back across Spec.
    tint_target: f32,
    lsp: Option<Arc<PitonSession>>,
    busy: bool,
    /// The context of the conversation the selected tab's next prompt carries
    /// on, in tokens; `None` when the next prompt starts a new one.
    context: Option<u64>,
    /// How the input grows to fit its text.
    fit: GrowToFit,
    /// Width of the chain's tab as last laid out, which is how far Code and
    /// Spec slide together beneath it when it is selected.
    chain_width: Option<Pixels>,
    /// What is attached to the prompt: kept across tab switches, and sent,
    /// then cleared, with the prompt.
    attachments: Vec<Attachment>,
    next_attachment_id: usize,
    /// The send button's menu, while open: the option highlighted.
    send_menu: Option<usize>,
    /// The compiled prompt shown in place of the text input, and which
    /// request for it is the latest.
    preview: Option<Preview>,
    preview_id: usize,
    preview_scroll: ScrollHandle,
    /// The queued prompt being edited, while one is.
    editing: Option<Editing>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<QueuedEdit> for ChatInput {}

impl EventEmitter<Submit> for ChatInput {}
impl EventEmitter<PreviewPrompt> for ChatInput {}

/// Emitted when another tab is selected.
pub struct TabChanged;

impl EventEmitter<TabChanged> for ChatInput {}

/// Emitted to leave the selected tab's conversation, so its next prompt
/// starts a new one with no context.
pub struct NewSession;

impl EventEmitter<NewSession> for ChatInput {}

/// A number of tokens, short: "850", "42.1k", "123k", "1.2M".
pub fn tokens_label(tokens: u64) -> String {
    let short = |value: f64, unit: &str| {
        if value < 100. {
            let text = format!("{value:.1}");
            format!("{}{unit}", text.trim_end_matches(".0"))
        } else {
            format!("{value:.0}{unit}")
        }
    };
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => short(tokens as f64 / 1_000., "k"),
        _ => short(tokens as f64 / 1_000_000., "M"),
    }
}

impl ChatInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(piton_syntax::LANGUAGE_NAME)
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                // No empty rows below the last line: the input is sized to its
                // text, so any extra would be blank space, and it would leave
                // room for the editor to stay scrolled past the first line
                // after Enter grows the input.
                .scroll_beyond_last_line(Some(0))
                .placeholder(format!(
                    "Write a prompt… (/ for skills and commands, @ for agents, {SEND_SHORTCUT} to send)"
                ))
        });
        // Typing goes straight into the input, without clicking it first.
        editor.read(cx).focus_handle(cx).focus(window, cx);
        let completion = cx.new(|cx| CompletionMenu::new(editor.clone(), window, cx));

        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this, editor, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        // An edit may be an accepted completion, whose import the
                        // hidden anchor then takes on.
                        if let Some(lsp) = &this.lsp {
                            lsp.note_edit(&editor.read(cx).value());
                        }
                        this.completion
                            .update(cx, |menu, cx| menu.on_edit(window, cx));
                    }
                    // Re-render on every edit so the input can grow to fit its text.
                    cx.notify();
                },
            ),
            cx.observe_global_in::<ProjectLsp>(window, |this, _, cx| this.connect_lsp(cx)),
        ];

        let mut this = Self {
            editor,
            completion,
            focus_handle: cx.focus_handle(),
            selected_tab: DEFAULT_TAB,
            tint_target: DEFAULT_TAB as f32,
            lsp: None,
            busy: false,
            context: None,
            fit: GrowToFit::new(MAX_ROWS),
            chain_width: None,
            attachments: Vec::new(),
            next_attachment_id: 0,
            send_menu: None,
            preview: None,
            preview_id: 0,
            preview_scroll: ScrollHandle::new(),
            editing: None,
            _subscriptions: subscriptions,
        };
        this.connect_lsp(cx);
        this
    }

    /// Attaches `text` to the prompt, after anything already attached.
    pub fn attach_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.next_attachment_id += 1;
        self.attachments.push(Attachment {
            id: self.next_attachment_id,
            text,
        });
        cx.notify();
    }

    /// Removes the attachment `id`.
    pub fn remove_attachment(&mut self, id: usize, cx: &mut Context<Self>) {
        self.attachments.retain(|attachment| attachment.id != id);
        cx.notify();
    }

    #[cfg(test)]
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// The attachments listed above the input, one row each.
    fn render_attachments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let rows = self.attachments.iter().map(|attachment| {
            let id = attachment.id;
            let lines = attachment.text.lines().count().max(1);
            let first_line = attachment
                .text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_string();
            let row = h_flex()
                .id(("attachment", id))
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_sm()
                .child(
                    Icon::new(IconName::TextQuote)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(first_line))
                .when(lines > 1, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{lines} lines")),
                    )
                })
                .child(
                    Button::new(("remove-attachment", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .tooltip("Remove this attachment")
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx)),
                        ),
                );
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row)
        });
        let list = v_flex().id("attachments").gap_1().children(rows);
        // Lets UI tests find the list; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(list).into_any_element())
    }

    /// The project's `piton lsp` session, once it is running.
    pub fn lsp(&self) -> Option<Arc<PitonSession>> {
        self.lsp.clone()
    }

    /// Moves keyboard focus into the input.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.is_some() {
            self.focus_handle.focus(window, cx);
        } else {
            self.editor.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Opens the send button's menu on its first option, or closes it. Not
    /// while the input is empty, or a preview shows.
    pub fn toggle_send_menu(&mut self, cx: &mut Context<Self>) {
        if self.send_menu.take().is_none()
            && !self.editor.read(cx).value().is_empty()
            && self.preview.is_none()
            && self.editing.is_none()
        {
            self.completion.update(cx, |menu, cx| menu.hide(cx));
            let mode = TABS[self.selected_tab];
            self.send_menu = SendOption::ALL
                .iter()
                .position(|option| option.enabled(mode));
        }
        cx.notify();
    }

    /// Whether the send button's menu is open.
    #[cfg(test)]
    pub fn send_menu_open(&self) -> bool {
        self.send_menu.is_some()
    }

    /// Moves the menu's highlight `step` options along, wrapping around.
    fn step_send_menu(&mut self, step: isize, cx: &mut Context<Self>) {
        let mode = TABS[self.selected_tab];
        if let Some(highlight) = &mut self.send_menu {
            let count = SendOption::ALL.len() as isize;
            // Over any option that can't be picked.
            for _ in 0..count {
                *highlight = (*highlight as isize + step).rem_euclid(count) as usize;
                if SendOption::ALL[*highlight].enabled(mode) {
                    break;
                }
            }
            cx.notify();
        }
    }

    /// Picks the menu's option at `ix`, closing it.
    pub fn pick_send_option(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mode = TABS[self.selected_tab];
        match SendOption::ALL
            .get(ix)
            .filter(|option| option.enabled(mode))
        {
            Some(SendOption::PreviewCompiled) => {
                self.send_menu = None;
                self.start_preview(window, cx)
            }
            Some(SendOption::Queue) => {
                self.send_menu = None;
                self.send(true, window, cx)
            }
            None => cx.notify(),
        }
    }

    /// Asks for the prompt compiled as it would be sent from the selected tab,
    /// and shows it in place of the text input, which keeps its text.
    fn start_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        self.preview_id += 1;
        self.preview = Some(Preview::Compiling);
        self.preview_scroll = ScrollHandle::new();
        self.focus_handle.focus(window, cx);
        cx.emit(PreviewPrompt {
            id: self.preview_id,
            text,
            mode: TABS[self.selected_tab],
            attached_text: self
                .attachments
                .iter()
                .map(|attachment| attachment.text.clone())
                .collect(),
        });
        cx.notify();
    }

    /// Shows the compiled prompt, or why it couldn't compile, for the latest
    /// preview asked for, if it still shows.
    pub fn set_preview(
        &mut self,
        id: usize,
        compiled: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        if id != self.preview_id || self.preview.is_none() {
            return;
        }
        self.preview = Some(match compiled {
            Ok(markdown) => Preview::Compiled(markdown),
            Err(error) => Preview::Failed(error),
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn editor_text_for_test(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn set_text_for_test(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_value(text.to_string(), window, cx)
        });
    }

    /// The preview showing, if one is.
    #[cfg(test)]
    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    /// Goes back from the preview to the text input, as it was.
    fn close_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.take().is_some() {
            self.editor.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
    }

    /// The send button's menu, above the button, its right edge on the
    /// button's.
    fn render_send_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let highlight = self.send_menu?;
        let theme = cx.theme();
        let mode = TABS[self.selected_tab];
        let rows = SendOption::ALL.iter().enumerate().map(|(ix, option)| {
            let enabled = option.enabled(mode);
            let row = h_flex()
                .id(("send-option", ix))
                .gap_2()
                .px_3()
                .py_1p5()
                .when(!enabled, |row| row.opacity(0.5))
                .when(enabled, |row| {
                    row.cursor_pointer().hover(|row| row.bg(theme.list_hover))
                })
                .when(ix == highlight && enabled, |row| row.bg(theme.list_active))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick_send_option(ix, window, cx)),
                )
                .child(
                    Icon::new(option.icon())
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().whitespace_nowrap().child(option.label()));
            // Lets UI tests find the option; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row)
        });
        let menu = v_flex()
            .id("send-menu")
            .absolute()
            .bottom_full()
            .right_0()
            .mb_1()
            .py_1()
            .min_w(px(220.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .shadow_md()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.send_menu = None;
                cx.notify();
            }))
            .children(rows);
        // Lets UI tests find the menu; inert in normal builds.
        Some(
            deferred(gpui_kit::TestSupportExt::test_support(menu))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// The compiled prompt in place of the text input: a row saying what it is
    /// and how to go on, above what the harness would receive.
    fn render_preview(
        &self,
        preview: &Preview,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let heading = h_flex()
            .gap_2()
            .text_xs()
            .map(|row| match preview {
                Preview::Compiling => row
                    .child(Spinner::new().xsmall().color(theme.muted_foreground))
                    .child(div().text_color(theme.muted_foreground).child("Compiling…")),
                _ => row.child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Compiled prompt"),
                ),
            })
            .child(div().flex_1())
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .whitespace_nowrap()
                    .child(PREVIEW_HINT),
            );
        let content = match preview {
            Preview::Compiling => None,
            Preview::Compiled(markdown) => Some(
                crate::task_table::markdown_view(
                    crate::markdown::MarkdownKey {
                        kind: crate::markdown::MarkdownKind::Prompt,
                        table: PREVIEW_TABLE,
                        row: self.preview_id,
                    },
                    markdown,
                    None,
                    cx,
                )
                .into_any_element(),
            ),
            Preview::Failed(error) => Some(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .into_any_element(),
            ),
        };
        let body = div()
            .id("prompt-preview-body")
            .max_h(window.viewport_size().height * 0.4)
            .overflow_y_scroll()
            .track_scroll(&self.preview_scroll)
            .children(content);
        let preview = v_flex()
            .id("prompt-preview")
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(heading)
            .child(body);
        // Lets UI tests find the preview; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(preview).into_any_element()
    }

    /// Inserts a harness mention at the cursor, after a space when the cursor
    /// follows other text, then moves focus into the input.
    pub fn insert_mention(&mut self, mention: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let value = editor.value();
            let follows_text = value
                .get(..editor.cursor())
                .and_then(|before| before.chars().next_back())
                .is_some_and(|c| !c.is_whitespace());
            let text = if follows_text {
                format!(" {mention}")
            } else {
                mention.to_string()
            };
            editor.insert(text, window, cx);
        });
        self.focus(window, cx);
    }

    #[cfg(test)]
    pub fn value(&self, cx: &App) -> SharedString {
        self.editor.read(cx).value()
    }

    #[cfg(test)]
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Marks the harness as working; sending then queues the prompt.
    /// Shows how much context the selected tab's conversation holds, or that
    /// the next prompt starts a new one.
    pub fn set_context(&mut self, context: Option<u64>, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            cx.notify();
        }
    }

    pub fn context(&self) -> Option<u64> {
        self.context
    }

    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        cx.notify();
    }

    /// Points completions at the project's `piton lsp`, once it is running.
    /// Harness mentions complete without it.
    fn connect_lsp(&mut self, cx: &mut Context<Self>) {
        let client = ProjectLsp::get(cx);
        let connected = self.lsp.as_ref().map(|lsp| Arc::as_ptr(lsp.client()));
        if client.as_ref().map(Arc::as_ptr) == connected {
            return;
        }
        self.lsp = client.map(|client| Arc::new(PitonSession::new(client)));
        self.set_completions(self.lsp.clone(), cx);
    }

    fn set_completions(&mut self, lsp: Option<Arc<PitonSession>>, cx: &mut Context<Self>) {
        let completions = Rc::new(PromptCompletions { lsp });
        self.completion
            .update(cx, |menu, cx| menu.set_provider(completions, cx));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send(false, window, cx)
    }

    /// Edits the queued prompt at `position` in the queue, counted from 1, in
    /// the input: what is being written is set aside, and the prompt's text,
    /// attachments, and mode take its place.
    pub fn begin_editing(
        &mut self,
        position: usize,
        text: String,
        mode: SendMode,
        attached_text: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_editing(window, cx);
        self.send_menu = None;
        self.preview = None;
        let set_aside = SetAside {
            text: self.editor.read(cx).value().to_string(),
            attachments: std::mem::take(&mut self.attachments),
            tab: self.selected_tab,
        };
        for text in attached_text {
            self.attach_text(text, cx);
        }
        let tab = TABS
            .iter()
            .position(|tab| *tab == mode)
            .unwrap_or(DEFAULT_TAB);
        self.put_back(text, tab, window, cx);
        self.editing = Some(Editing {
            position,
            set_aside,
        });
        cx.notify();
    }

    /// Cancels editing a queued prompt, if one is being edited, bringing back
    /// what was set aside.
    pub fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.end_editing(window, cx) {
            cx.emit(QueuedEdit::Cancelled);
        }
    }

    /// Brings back what editing set aside; whether a prompt was being edited.
    fn end_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing.take() else {
            return false;
        };
        let SetAside {
            text,
            attachments,
            tab,
        } = editing.set_aside;
        self.attachments = attachments;
        self.put_back(text, tab, window, cx);
        cx.notify();
        true
    }

    /// Puts `text` in the input, the cursor at its end, in the tab at `tab`,
    /// with focus.
    fn put_back(&mut self, text: String, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab != self.selected_tab {
            self.move_tint(tab as isize - self.selected_tab as isize);
            self.selected_tab = tab;
            cx.emit(TabChanged);
        }
        self.editor.update(cx, |editor, cx| {
            use gpui_kit::component::input::RopeExt as _;
            editor.set_value(text, window, cx);
            let end = editor.text().offset_to_position(editor.text().len());
            editor.set_cursor_position(end, window, cx);
        });
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// Where in the queue the prompt being edited is, counted from 1.
    pub fn set_editing_position(&mut self, position: usize, cx: &mut Context<Self>) {
        if let Some(editing) = &mut self.editing
            && editing.position != position
        {
            editing.position = position;
            cx.notify();
        }
    }

    /// Whether a queued prompt is being edited.
    #[cfg(test)]
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Sends the prompt, or queues it on purpose when `queue`; while a queued
    /// prompt is being edited, saves the edit instead.
    fn send(&mut self, queue: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        if self.editing.is_some() {
            let attached_text = std::mem::take(&mut self.attachments)
                .into_iter()
                .map(|attachment| attachment.text)
                .collect();
            let mode = TABS[self.selected_tab];
            self.end_editing(window, cx);
            cx.emit(QueuedEdit::Saved {
                text,
                mode,
                attached_text,
            });
            return;
        }
        // Sent from the preview, the text input comes back, empty.
        self.send_menu = None;
        if self.preview.take().is_some() {
            self.editor.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        // The attachments go with the prompt.
        let attached_text = std::mem::take(&mut self.attachments)
            .into_iter()
            .map(|attachment| attachment.text)
            .collect();
        cx.emit(Submit {
            text,
            mode: TABS[self.selected_tab],
            attached_text,
            queue,
        });
    }

    /// Selects a clicked tab and puts focus back in the input.
    /// The mode of the selected tab.
    pub fn mode(&self) -> SendMode {
        TABS[self.selected_tab]
    }

    pub(crate) fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The prompt would compile differently in another mode.
        if ix != self.selected_tab {
            self.preview = None;
        }
        self.send_menu = None;
        self.move_tint(ix as isize - self.selected_tab as isize);
        if self.selected_tab != ix {
            cx.emit(TabChanged);
        }
        self.selected_tab = ix;
        self.focus(window, cx);
        cx.notify();
    }

    /// Tab and Shift+Tab: move focus to the next or previous control, as
    /// anywhere else. An open completion menu keeps its own Tab.
    fn move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion.read(cx).is_open() {
            return;
        }
        cx.stop_propagation();
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
    }

    /// Moves `step` tabs along, wrapping around.
    fn cycle_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.send_menu = None;
        // The prompt would compile differently in another mode.
        self.close_preview(window, cx);
        let count = TABS.len() as isize;
        self.selected_tab = (self.selected_tab as isize + step).rem_euclid(count) as usize;
        // Along the way Ctrl+Tab went, even when it wraps around.
        self.tint_target += step as f32;
        cx.emit(TabChanged);
        cx.notify();
    }

    /// Slides the tint `step` tabs along the shorter way round, keeping to
    /// the row, rather than wrapping, when the two ways are as long.
    fn move_tint(&mut self, step: isize) {
        let count = TABS.len() as isize;
        let half = count / 2;
        let step = if step > half {
            step - count
        } else if step < -half {
            step + count
        } else {
            step
        };
        self.tint_target += step as f32;
    }

    /// Esc closes an open completion menu, and otherwise takes focus out of
    /// the input.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.send_menu.take().is_some() {
            cx.notify();
        } else if self.preview.is_some() {
            self.close_preview(window, cx);
        } else if self.completion.read(cx).is_open() {
            self.completion.update(cx, |menu, cx| menu.hide(cx));
        } else if self.editing.is_some() {
            self.cancel_editing(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }
}

impl Render for ChatInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.editor.read(cx).value();
        let (height, one_row) = self.fit.heights(&self.editor, window, cx);
        let empty = text.is_empty();
        // A question is asked straight away, beside whatever the harness is
        // working on, so it never queues.
        let queues = self.busy && TABS[self.selected_tab] != SendMode::Ask;

        // Follows how the editor is laid out, to fit it anew.
        let track_layout =
            GrowToFit::tracker(&self.editor, cx.entity().downgrade(), |this: &mut Self| {
                &mut this.fit
            });

        // Code, the chain, Spec, and Ask are tabs side by side, each its own
        // bar so that selecting the chain can show Code and Spec selected
        // along with it, as one locked tab. The chain shows an icon: broken
        // and dimmed until selected, then joined. Selecting it pulls in half
        // its width from either side, as though by negative margins, so Code
        // and Spec slide together until they meet beneath it and it sits on
        // the seam between them.
        let selected = self.selected_tab;
        let unified = selected == BOTH_TAB;
        let chain_width = self.chain_width.unwrap_or(CHAIN_WIDTH_ESTIMATE);
        let join = SpringAnimation::new(CHAIN_SPRING).to(if unified { 1. } else { 0. });
        // The tint slides between the tabs, so going from Code to Spec passes
        // through purple. Its position is the selected tab's index: 0 for
        // Code, 1 for the chain, 2 for Spec, 3 for Ask.
        let tint_slide = SpringAnimation::new(CHAIN_SPRING).to(self.tint_target);
        let (red, blue, green) = (cx.theme().red, cx.theme().blue, cx.theme().green);
        let tint = move |position| tint(red, blue, green, position);
        // A joined chain once selected, and a broken one dimmed to half
        // opacity until then.
        let chain_icon = Icon::new(if unified {
            IconName::Link
        } else {
            IconName::Unlink
        })
        .text_color(if unified {
            cx.theme().tab_active_foreground
        } else {
            cx.theme().tab_foreground.opacity(0.5)
        });
        // A tab's contents: its label, or the chain's icon. An icon tab is a
        // small square with none of a label's padding, so no room has to be
        // made for the chain: it sits between Code and Spec the way any two
        // tabs sit side by side.
        let tab = |ix: usize| match TABS[ix] {
            SendMode::Both => Tab::new()
                .icon(chain_icon.clone())
                .tooltip(|window, cx| Tooltip::new(SendMode::Both.label()).build(window, cx)),
            mode => Tab::new().label(mode.label()),
        };
        // A tint laid over a tab, as wide as the tab itself (an invisible copy
        // of it) rather than its bar, fading out as the tab is deselected.
        let tab_tint = |ix: usize| {
            let tint_slide = tint_slide.clone();
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .flex()
                .child(
                    TabBar::new(("tint-width", ix))
                        .selected_index(0)
                        .child(Tab::new().label(TABS[ix].label()))
                        .invisible(),
                )
                .with_spring(("tab-tint", ix), tint_slide, move |this, position| {
                    let shown = tab_tint_shown(ix, position);
                    let color = tint(position);
                    this.bg(Hsla {
                        a: color.a * shown,
                        ..color
                    })
                })
        };
        let chat_input = cx.entity().downgrade();
        let full_tab = |ix: usize| {
            let chat_input = chat_input.clone();
            let bar = TabBar::new(TABS[ix].id())
                .on_click(move |_, window, cx| {
                    chat_input
                        .update(cx, |this, cx| this.select_tab(ix, window, cx))
                        .ok();
                })
                .child(tab(ix));
            // The chain itself never shows as selected: once joined it sits
            // over Code and Spec, whose selection shows through around its icon.
            if ix != BOTH_TAB && (selected == ix || (unified && ix != ASK_TAB)) {
                bar.selected_index(0)
            } else {
                bar
            }
        };
        let measure_chain = {
            let chat_input = chat_input.clone();
            canvas(
                |_, _, _| {},
                move |bounds, _, _, cx| {
                    let width = bounds.size.width;
                    chat_input
                        .update(cx, |this, cx| {
                            if this
                                .chain_width
                                .is_none_or(|old| (old - width).abs() > px(0.01))
                            {
                                this.chain_width = Some(width);
                                cx.notify();
                            }
                        })
                        .ok();
                },
            )
            .absolute()
            .size_full()
        };
        let code = div().relative().child(full_tab(0)).child(tab_tint(0));
        // The chain is laid over the start of Spec, so that it paints above
        // both Code and Spec once they slide beneath it. It is a tab on its
        // own, with no bar, since a bar draws a line along its bottom that
        // would show beneath the joined tabs: hiding it with a clip a pixel
        // above the bottom fails wherever the clip's edge rounds down onto
        // the line. With nothing of its own drawn there, Code and Spec show
        // through beneath it; in the gap between them, the gap's own line
        // shows instead.
        let both = div()
            .absolute()
            .top_0()
            .bottom_0()
            .child({
                let chat_input = chat_input.clone();
                gpui_kit::TestSupportExt::test_support(div().id(TABS[BOTH_TAB].id())).child(
                    tab(BOTH_TAB).on_click(move |_, window, cx| {
                        chat_input
                            .update(cx, |this, cx| this.select_tab(BOTH_TAB, window, cx))
                            .ok();
                    }),
                )
            })
            .child(measure_chain);
        // Once Code and Spec meet, Code's facing border is covered, in their
        // purple, so the joined tab shows no outline beneath the chain. The
        // cover reaches past the border so that, at fractional display scales,
        // no sliver of it is left showing at its edges.
        let seam_cover = cx.theme().tab_active.blend(tint(BOTH_TAB as f32));
        let border = cx.theme().border;
        let spec = div()
            .relative()
            .child(full_tab(2))
            .child(tab_tint(2))
            .with_spring("chain-join", join, move |this, joined| {
                let joined = joined.clamp(0., 1.);
                // What is left of the gap the chain holds open between Code
                // and Spec, as it gives up half its width either side.
                let gap = chain_width * (1. - joined);
                let seam = gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("seam")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(-gap - SEAM_COVER_REACH)
                        .w(SEAM_COVER_REACH * 2.)
                        .when(unified && gap <= SEAM_COVER_REACH, |this| {
                            this.bg(seam_cover)
                        }),
                );
                // The line along the bottom of the gap, beneath the chain while
                // it sits apart, as wide as what is left of the gap, so there is
                // none once Code and Spec meet. Drawn as every tab bar draws its
                // line, the bottom border of a box the bar's full height: a box
                // no taller than its own border has no inside, and the renderer
                // draws nothing of it.
                let gap_line = gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("chain-gap-line")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(-gap)
                        .w(gap)
                        .border_b_1()
                        .border_color(border),
                );
                this.ml(gap)
                    .child(gap_line)
                    .child(seam)
                    .child(both.left(-gap - chain_width * 0.5 * joined))
            });
        let ask = div()
            .relative()
            .child(full_tab(ASK_TAB))
            .child(tab_tint(ASK_TAB));
        // What the selected tab is for, in the rest of the bar: smaller and
        // fainter than the tab labels, so it reads as a note about the tab
        // rather than another tab.
        // Each tab bar draws the line along its own bottom, and the gap
        // between Code and Spec its own; the rest of the bar, beside the help,
        // has its line here. Nothing draws a line beneath the joined tabs.
        let help = gpui_kit::TestSupportExt::test_support(
            div()
                .id("tab-help")
                .flex_1()
                .min_w_0()
                .self_stretch()
                .flex()
                .items_center()
                .px_4()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground.opacity(0.7))
                .child(div().min_w_0().truncate().child(TABS[selected].help())),
        );
        // At the far right, how much context the selected tab's conversation
        // holds, and a button to leave it for a new one.
        let context = self.context;
        let session = h_flex()
            .flex_none()
            .self_stretch()
            .items_center()
            .gap_2()
            .pr_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("chat-context")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Context {}",
                        tokens_label(context.unwrap_or_default())
                    ))
                    .tooltip(move |window, cx| {
                        let text = match context {
                            Some(tokens) => format!(
                                "The conversation this tab's next prompt carries on holds {tokens} tokens"
                            ),
                            None => "This tab's next prompt starts a new conversation".into(),
                        };
                        Tooltip::new(text).build(window, cx)
                    }),
            ))
            .child(
                Button::new("new-session")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Sparkles)
                    .label("New Session")
                    .disabled(context.is_none())
                    .tooltip(if context.is_some() {
                        "Leave this conversation: the next prompt starts a new one, with no context"
                    } else {
                        "The next prompt already starts a new conversation"
                    })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NewSession))),
            );
        let tabs = gpui_kit::TestSupportExt::test_support(
            div()
                .id("chat-tabs")
                .relative()
                .flex()
                .items_center()
                .bg(cx.theme().tab_bar)
                .child(code)
                .child(spec)
                .child(ask)
                .child(help)
                .child(session),
        );

        // Anything attached is listed above the input.
        let attachments = self.render_attachments(cx);
        // While a queued prompt is edited, a row above says which, and how to
        // save or cancel it.
        let editing_row = self.editing.as_ref().map(|editing| {
            let theme = cx.theme();
            let row = h_flex()
                .id("editing-queued")
                .gap_2()
                .text_xs()
                .child(
                    Icon::new(IconName::Pencil)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!("Editing queued prompt {}", editing.position)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .whitespace_nowrap()
                        .child(EDITING_HINT),
                );
            // Lets UI tests find the row; inert in normal builds.
            gpui_kit::TestSupportExt::test_support(row)
        });
        let body = div().flex().flex_col().gap_2().p_3().with_spring(
            "body-tint",
            tint_slide,
            move |this, position| this.bg(tint(position)),
        );
        let input_row = div().flex().flex_row().items_end().gap_2();
        let previewing = self.preview.is_some();
        let editing = self.editing.is_some();
        let send_tooltip = if queues {
            format!("Queue until the harness is free ({SEND_SHORTCUT})")
        } else {
            format!("Send ({SEND_SHORTCUT})")
        };
        // The send button, and joined to its right, the chevron opening its
        // menu of other ways to send; the menu hangs above them.
        let send = h_flex()
            .id("send-split")
            .relative()
            .flex_none()
            .child(
                Button::new("send")
                    .primary()
                    // While the harness works, sending queues the prompt; while a
                    // queued prompt is edited, it saves the edit.
                    .label(if editing {
                        "Save"
                    } else if queues {
                        "Queue"
                    } else {
                        "Send"
                    })
                    // As tall as the input's single line, so the two line up.
                    .h(one_row)
                    .rounded_r_none()
                    .tooltip(if editing {
                        format!("Save the queued prompt ({SEND_SHORTCUT})")
                    } else {
                        send_tooltip
                    })
                    .disabled(empty)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
            .child(
                Button::new("send-options")
                    .primary()
                    .icon(IconName::ChevronDown)
                    .h(one_row)
                    .px_1p5()
                    .rounded_l_none()
                    .border_l_1()
                    .border_color(cx.theme().primary_hover)
                    .tooltip(format!("More ways to send ({SEND_MENU_SHORTCUT})"))
                    .disabled(empty || previewing || editing)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_send_menu(cx))),
            )
            .children(self.render_send_menu(cx));
        let send = gpui_kit::TestSupportExt::test_support(send);
        let input_area = match &self.preview {
            Some(preview) => self.render_preview(&preview.clone(), window, cx),
            None => gpui_kit::TestSupportExt::test_support(
                div()
                    .id("prompt-editor")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .child(Editor::new(&self.editor).h(height))
                    .child(track_layout)
                    .child(self.completion.clone()),
            )
            .into_any_element(),
        };

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(cx.theme().border)
            .key_context(CONTEXT)
            // Ctrl+Tab and Ctrl+Shift+Tab cycle the tabs, and the input keeps
            // focus.
            .on_action(cx.listener(|this, _: &NextMode, window, cx| this.cycle_tab(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousMode, window, cx| this.cycle_tab(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSendMenu, _, cx| this.toggle_send_menu(cx)))
            .on_action(cx.listener(|this, _: &SendPreviewed, window, cx| {
                if this.preview.is_some() {
                    this.submit(window, cx);
                }
            }))
            // Tab and Shift+Tab move focus on, rather than indenting.
            .capture_action(
                cx.listener(|this, _: &IndentInline, window, cx| this.move_focus(true, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &OutdentInline, window, cx| {
                    this.move_focus(false, window, cx)
                }),
            )
            // Esc arrives as the window's FocusChat, which matches ahead of the
            // editor's own binding, or as the editor's Escape once it has
            // nothing to cancel. Handled here, neither reaches the window,
            // which would focus the input again.
            .on_action(cx.listener(|this, _: &FocusChat, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                // The send button's menu takes Enter while it is open.
                if let Some(highlight) = this.send_menu.filter(|_| !action.secondary) {
                    cx.stop_propagation();
                    this.pick_send_option(highlight, window, cx);
                    return;
                }
                // Catch Ctrl/Cmd+Enter before the editor turns it into a newline.
                if action.secondary {
                    cx.stop_propagation();
                    this.submit(window, cx);
                } else if this.completion.read(cx).is_open() {
                    // Enter accepts the completion instead of adding a line.
                    cx.stop_propagation();
                    this.completion
                        .update(cx, |menu, cx| menu.accept_selected(window, cx));
                }
            }))
            // Up and Down move through an open completion menu instead of
            // the text.
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if this.send_menu.is_some() {
                    cx.stop_propagation();
                    this.step_send_menu(-1, cx);
                } else if this.completion.read(cx).is_open() {
                    cx.stop_propagation();
                    this.completion
                        .update(cx, |menu, cx| menu.select_next(-1, window, cx));
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if this.send_menu.is_some() {
                    cx.stop_propagation();
                    this.step_send_menu(1, cx);
                } else if this.completion.read(cx).is_open() {
                    cx.stop_propagation();
                    this.completion
                        .update(cx, |menu, cx| menu.select_next(1, window, cx));
                }
            }))
            .child(tabs)
            .child(
                body.children(editing_row)
                    .children(attachments)
                    .child(input_row.child(input_area).child(send)),
            )
    }
}

/// Completions for the chat input: harness mentions after `/` or `@`, and
/// Piton from `piton lsp`, when it is running, everywhere else.
struct PromptCompletions {
    lsp: Option<Arc<PitonSession>>,
}

impl CompletionProvider for PromptCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let lsp = self.lsp.clone();
        let prompt = text.to_string();
        let project_dir = ProjectDirectory::get(cx);
        cx.background_spawn(async move {
            if harness_mentions::mention_at(&prompt, offset).is_some() {
                let invocables = harness_mentions::discover(project_dir.as_deref());
                let items = harness_mentions::complete(&prompt, offset, &invocables);
                return Ok(CompletionResponse::Array(items.unwrap_or_default()));
            }
            // A slash or hyphen only triggers completion inside a mention.
            let typed = prompt
                .get(..offset)
                .and_then(|before| before.chars().next_back());
            match lsp {
                Some(lsp) if !matches!(typed, Some('/' | '-')) => lsp.complete(&prompt, offset),
                _ => Ok(CompletionResponse::Array(Vec::new())),
            }
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().last().is_some_and(|c| {
            c.is_alphanumeric() || matches!(c, '{' | '.' | '$' | '@' | ':' | '_' | '/' | '-')
        })
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;

    /// Token counts read short, to a decimal place below a hundred of a unit.
    #[test]
    fn tokens_read_short() {
        use super::tokens_label;
        assert_eq!(tokens_label(0), "0");
        assert_eq!(tokens_label(850), "850");
        assert_eq!(tokens_label(1_000), "1k");
        assert_eq!(tokens_label(42_150), "42.1k");
        assert_eq!(tokens_label(123_456), "123k");
        assert_eq!(tokens_label(1_200_000), "1.2M");
    }

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{
        AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
        Styled as _, TestAppContext, Window, div,
    };

    use super::{ChatInput, MAX_ROWS};
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// The chat input along the bottom of the window, where prompt mode puts it.
    struct AtBottom(Entity<ChatInput>);

    impl Render for AtBottom {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .justify_end()
                .child(self.0.clone())
        }
    }

    /// Types `Update @{Appl` into the real chat input key by key, with
    /// `piton lsp` running over this repository's spec, and accepts the
    /// completion menu's selection with Enter: the reference completes to
    /// `ApplicationScope` and the hidden anchor takes on its import. The
    /// input sits at the bottom of the window, and the menu stays inside it.
    #[gpui_kit::test]
    async fn typing_a_reference_autocompletes_and_imports(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx);
            crate::project_lsp::ProjectLsp::init(cx);
        });

        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            let view = cx.new(|_| AtBottom(input));
            Root::new(view, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();

        cx.wait_for(handle, TIMEOUT, |_, cx| chat_input.read(cx).lsp.is_some())
            .await;

        let editor = chat_input.read_with(cx, |input, _| input.editor.clone());
        cx.update_window(handle, |_, window, cx| {
            editor.read(cx).focus_handle(cx).focus(window, cx);
        })
        .unwrap();
        for key in "Update @{Appl".chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }

        let completion = chat_input.read_with(cx, |input, _| input.completion.clone());
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            let menu = completion.read(cx);
            menu.is_open()
                && menu
                    .items(cx)
                    .first()
                    .is_some_and(|item| item.label == "ApplicationScope")
        })
        .await;
        let labels: Vec<String> = cx.update(|cx| {
            completion
                .read(cx)
                .items(cx)
                .iter()
                .map(|item| item.label.clone())
                .collect()
        });
        assert!(
            labels
                .iter()
                .all(|label| label.to_lowercase().contains("appl")),
            "{labels:?}"
        );

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let viewport = window.viewport_size();
            let menu = window.find("completion-menu").bounds();
            assert!(
                menu.size.height > gpui_kit::px(0.)
                    && menu.top() >= gpui_kit::px(0.)
                    && menu.left() >= gpui_kit::px(0.)
                    && menu.bottom() <= viewport.height
                    && menu.right() <= viewport.width,
                "completion menu {menu:?} is not inside the window {viewport:?}"
            );
            // No room below the input at the bottom of the window: the menu
            // opens above the cursor's line.
            let editor = editor.read(cx);
            let (cursor, _) = editor.cursor_layout().unwrap();
            let cursor_top = cursor.top() + editor.scroll_offset().y;
            assert!(
                menu.bottom() <= cursor_top,
                "completion menu {menu:?} does not open above the cursor at {cursor_top:?}"
            );
        })
        .unwrap();

        // Accept the selection the way a user does.
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            editor.read(cx).value().contains("ApplicationScope")
        })
        .await;

        let text = editor.read_with(cx, |editor, _| editor.value().to_string());
        assert_eq!(text, "Update @{ApplicationScope}");
        let accepted = chat_input.read_with(cx, |input, _| {
            input.lsp.as_ref().unwrap().accepted_imports()
        });
        assert!(
            accepted.contains("\"/scope/application\": {\"ApplicationScope\"}"),
            "{accepted}"
        );
    }

    /// Types `/build-app` and then `@Expl` into the real chat input key by
    /// key, with this repository as the project, and accepts each completion
    /// with Enter: the project's skill and the built-in agent are inserted
    /// the way the harness understands them.
    #[gpui_kit::test]
    async fn typing_a_mention_completes_skills_and_agents(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(PathBuf::from(env!("CARGO_MANIFEST_DIR")), cx);
            crate::project_lsp::ProjectLsp::init(cx);
        });

        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            let view = cx.new(|_| AtBottom(input));
            Root::new(view, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        let editor = chat_input.read_with(cx, |input, _| input.editor.clone());
        let completion = chat_input.read_with(cx, |input, _| input.completion.clone());
        cx.update_window(handle, |_, window, cx| {
            editor.read(cx).focus_handle(cx).focus(window, cx);
        })
        .unwrap();

        for (keys, first, expected) in [
            ("/build-app", "build-application", "/build-application "),
            ("@Expl", "Explore", "/build-application @agent-Explore "),
        ] {
            for key in keys.chars() {
                cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                    .unwrap();
                cx.run_until_parked();
            }
            cx.wait_for(handle, TIMEOUT, |_, cx| {
                let menu = completion.read(cx);
                menu.is_open()
                    && menu
                        .items(cx)
                        .first()
                        .is_some_and(|item| item.label == first)
            })
            .await;
            cx.update_window(handle, |_, window, cx| window.press("enter", cx))
                .unwrap();
            cx.wait_for(handle, TIMEOUT, |_, cx| editor.read(cx).value() == expected)
                .await;
        }
    }

    /// To the right of the tabs, filling the bar up to the context and New
    /// Session at its far right, a line of help says what the selected tab is
    /// for, and changes with it.
    #[gpui_kit::test]
    async fn help_beside_the_tabs_says_what_the_tab_is_for(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(input, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("tab-help").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let (ask, help, bar) = (
                window.within("ask-tab").find(0usize).bounds(),
                window.find("tab-help").bounds(),
                window.find("chat-tabs").bounds(),
            );
            let (context, new_session) = (
                window.find("chat-context").bounds(),
                window.find("new-session").bounds(),
            );
            assert!(
                help.left() >= ask.right() - gpui_kit::px(0.5),
                "{help:?} is not right of {ask:?}"
            );
            // The help fills the bar up to the context, and New Session ends
            // it, a little in from its right edge.
            let between = context.left() - help.right();
            assert!(
                between >= gpui_kit::px(0.) && between <= gpui_kit::px(8.),
                "{help:?} does not fill the bar up to the context {context:?}"
            );
            assert!(
                new_session.left() > context.right(),
                "New Session {new_session:?} isn't after the context {context:?}"
            );
            assert!(
                new_session.right() <= bar.right()
                    && bar.right() - new_session.right() <= gpui_kit::px(8.),
                "New Session {new_session:?} isn't at the far right of {bar:?}"
            );
        })
        .unwrap();

        // Each tab has its own help.
        let helps: Vec<&str> = super::TABS.iter().map(|mode| mode.help()).collect();
        for (ix, help) in helps.iter().enumerate() {
            assert!(!help.is_empty());
            assert!(!helps[ix + 1..].contains(help), "{help} is repeated");
        }
        for ix in 0..super::TABS.len() {
            cx.update_window(handle, |_, window, cx| {
                chat_input.update(cx, |input, cx| input.select_tab(ix, window, cx));
            })
            .unwrap();
            chat_input.read_with(cx, |input, _| assert_eq!(input.mode().help(), helps[ix]));
        }
    }

    /// The chain has no line along its bottom while it is joined over Code
    /// and Spec, in combined mode, and has one, like any other tab, while it
    /// sits between them unselected, in separate mode, at any display scale.
    #[gpui_kit::test]
    async fn the_chain_has_a_bottom_line_only_apart(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(input, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("tab-help").is_some()
        })
        .await;
        for scale in [1., 1.25, 1.5, 1.75, 2.] {
            gpui_kit::VisualTestContext::from_window(handle, cx)
                .simulate_scale_factor_change(scale);
            for (tab, joined) in [(0, false), (1, true), (2, false), (3, false)] {
                cx.update_window(handle, |_, window, cx| {
                    chat_input.update(cx, |input, cx| input.select_tab(tab, window, cx));
                })
                .unwrap();
                let [_, chain, _, _] = settle_tabs(handle, joined, cx);
                cx.update_window(handle, |_, window, cx| {
                    window.refresh();
                    window.render_frame(cx);
                    let scale = window.scale_factor();
                    let help = window.find("tab-help").bounds();
                    let bar = window.find("chat-tabs").bounds();
                    // Whether a visible horizontal line lies along `bottom`, over
                    // the middle of the span from `left` to `right`.
                    let line_at = |left: f32, right: f32, bottom: f32| {
                        let (x, y) = ((left + right) / 2. * scale, bottom * scale - 0.5);
                        let quads = window.painted_quads();
                        // Whether an opaque quad drawn after `order` covers the point.
                        let covered = |order| {
                            quads.iter().any(|q| {
                                let (b, m) = (&q.bounds, &q.content_mask.bounds);
                                q.order > order
                                    && q.background.as_solid().is_some_and(|c| c.a >= 0.99)
                                    && x >= b.origin.x.0.max(m.origin.x.0)
                                    && x < (b.origin.x.0 + b.size.width.0)
                                        .min(m.origin.x.0 + m.size.width.0)
                                    && y >= b.origin.y.0.max(m.origin.y.0)
                                    && y < (b.origin.y.0 + b.size.height.0)
                                        .min(m.origin.y.0 + m.size.height.0)
                            })
                        };
                        quads.iter().any(|q| {
                            let b = &q.bounds;
                            let m = &q.content_mask.bounds;
                            let visible = |x: f32, y: f32| {
                                x >= m.origin.x.0
                                    && x < m.origin.x.0 + m.size.width.0
                                    && y >= m.origin.y.0
                                    && y < m.origin.y.0 + m.size.height.0
                            };
                            let border = q.border_widths.bottom.0 > 0.
                                && q.border_color.a > 0.
                                && x >= b.origin.x.0
                                && x < b.origin.x.0 + b.size.width.0
                                && y >= b.origin.y.0 + b.size.height.0 - q.border_widths.bottom.0
                                && y < b.origin.y.0 + b.size.height.0;
                            border && visible(x, y) && !covered(q.order)
                        })
                    };
                    // Clear of Code's and Spec's own edges.
                    let (left, right) = (chain.left().as_f32() + 8., chain.right().as_f32() - 8.);
                    assert_eq!(
                        line_at(left, right, chain.bottom().as_f32()),
                        !joined,
                        "tab {tab} at {scale}x: the line under the chain {chain:?}"
                    );
                    assert!(
                        line_at(
                            help.left().as_f32(),
                            help.right().as_f32(),
                            bar.bottom().as_f32()
                        ),
                        "tab {tab} at {scale}x: no line under the help text"
                    );
                })
                .unwrap();
            }
        }
    }

    /// Ctrl+Tab in the input moves to the next tab, chain then Spec then Ask
    /// then back to Code, and Ctrl+Shift+Tab back the other way, without
    /// changing the text or taking focus out of the input. Plain Tab moves
    /// focus out instead.
    #[gpui_kit::test]
    async fn ctrl_tab_cycles_the_tabs_and_tab_moves_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(input, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        // It starts on the chain, Code and Spec together.
        chat_input.read_with(cx, |input, _| {
            assert_eq!(input.mode(), super::SendMode::Both);
            assert_eq!(input.tint_target, 1.);
        });
        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| input.select_tab(0, window, cx));
            window.input("hi", cx);
        })
        .unwrap();
        cx.run_until_parked();
        // From Code, with where the tint slides to: on past Ask to Code (4) and back
        // before Code to Ask (-1), rather than across the tabs between.
        let presses = [
            ("ctrl-tab", 1, 1.),
            ("ctrl-tab", 2, 2.),
            ("ctrl-tab", 3, 3.),
            ("ctrl-tab", 0, 4.),
            ("ctrl-tab", 1, 5.),
            ("ctrl-shift-tab", 0, 4.),
            ("ctrl-shift-tab", 3, 3.),
            ("ctrl-shift-tab", 2, 2.),
            ("ctrl-shift-tab", 1, 1.),
            ("ctrl-shift-tab", 0, 0.),
            ("ctrl-shift-tab", 3, -1.),
        ];
        for (key, expected, tint_target) in presses {
            cx.update_window(handle, |_, window, cx| window.press(key, cx))
                .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                let input = chat_input.read(cx);
                assert_eq!(input.selected_tab, expected, "after {key}");
                assert_eq!(input.tint_target, tint_target, "tint after {key}");
                assert_eq!(input.value(cx).as_ref(), "hi");
                assert!(
                    input.is_focused(window, cx),
                    "the input lost focus on {key}"
                );
            })
            .unwrap();
        }

        // Plain Tab neither switches tabs nor indents: it moves focus on.
        cx.update_window(handle, |_, window, cx| window.press("tab", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            let input = chat_input.read(cx);
            assert_eq!(input.selected_tab, 3, "Tab switched the tab");
            assert_eq!(input.value(cx).as_ref(), "hi", "Tab changed the text");
            assert!(!input.is_focused(window, cx), "Tab left focus in the input");
        })
        .unwrap();
    }

    /// The send button is split: Ctrl+Shift+Enter opens its menu over the
    /// button, arrows move through it and Esc closes it, keeping the text;
    /// Enter picks Preview Compiled Prompt, which asks for the prompt
    /// compiled and shows it in place of the input. Esc goes back to the
    /// input as it was, and Ctrl+Enter from the preview sends it.
    #[gpui_kit::test]
    async fn the_send_menu_previews_the_compiled_prompt(cx: &mut TestAppContext) {
        use super::{Preview, PreviewPrompt, SendMode, Submit};
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            crate::main_window::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(cx.new(|_| AtBottom(input)), window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        let previews = Rc::new(std::cell::RefCell::new(Vec::new()));
        let submitted = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscriptions = cx.update(|cx| {
            let (previews, submitted) = (previews.clone(), submitted.clone());
            (
                cx.subscribe(&chat_input, move |_, preview: &PreviewPrompt, _| {
                    previews
                        .borrow_mut()
                        .push((preview.id, preview.text.clone(), preview.mode))
                }),
                cx.subscribe(&chat_input, move |_, submit: &Submit, _| {
                    submitted.borrow_mut().push(submit.text.clone())
                }),
            )
        });
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        let editor_focused = |window: &Window, cx: &gpui_kit::App| {
            chat_input
                .read(cx)
                .editor
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        };

        // Nothing to send, nothing to open.
        cx.update_window(handle, |_, window, cx| {
            window.press("ctrl-shift-enter", cx);
            assert!(!chat_input.read(cx).send_menu_open());
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input
                    .editor
                    .update(cx, |editor, cx| editor.set_value("Fix it", window, cx))
            });
            window.render_frame(cx);
            window.press("ctrl-shift-enter", cx);
            window.render_frame(cx);
            assert!(chat_input.read(cx).send_menu_open());
            let menu = window.find("send-menu").bounds();
            let split = window.find("send-split").bounds();
            assert!(
                menu.bottom() <= split.top(),
                "{menu:?} isn't above {split:?}"
            );
            assert!((menu.right() - split.right()).abs() <= gpui_kit::px(0.5));
            window.find(("send-option", 0usize));
            window.press("down", cx);
            window.press("up", cx);
            assert_eq!(chat_input.read(cx).send_menu, Some(0));
            window.press("escape", cx);
            assert!(!chat_input.read(cx).send_menu_open());
            assert_eq!(
                chat_input.read(cx).editor.read(cx).value().as_ref(),
                "Fix it"
            );
            assert!(editor_focused(window, cx), "Esc took the input's focus");

            window.press("ctrl-shift-enter", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(previews.borrow().len(), 1);
        let (id, text, mode) = previews.borrow()[0].clone();
        assert_eq!((text.as_str(), mode), ("Fix it", SendMode::Both));
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(chat_input.read(cx).preview(), Some(&Preview::Compiling));
            assert!(!chat_input.read(cx).send_menu_open());
            chat_input.update(cx, |input, cx| {
                input.set_preview(id, Ok("Fix **it**, compiled.".into()), cx)
            });
            window.render_frame(cx);
            assert!(window.try_find("prompt-editor").is_none());
            window.find("prompt-preview");
            assert!(chat_input.read(cx).focus_handle.is_focused(window));

            // Esc goes back to the input, as it was.
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(chat_input.read(cx).preview().is_none());
            window.find("prompt-editor");
            assert_eq!(
                chat_input.read(cx).editor.read(cx).value().as_ref(),
                "Fix it"
            );
            assert!(editor_focused(window, cx));

            // A result for a preview no longer showing is ignored.
            chat_input.update(cx, |input, cx| input.set_preview(id, Ok("late".into()), cx));
            assert!(chat_input.read(cx).preview().is_none());

            window.press("ctrl-shift-enter", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (id, _, _) = previews.borrow()[1].clone();
        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input.set_preview(id, Err("could not compile".into()), cx)
            });
            window.render_frame(cx);
            assert_eq!(
                chat_input.read(cx).preview(),
                Some(&Preview::Failed("could not compile".into()))
            );
            // Ctrl+Enter sends from the preview.
            window.press("ctrl-enter", cx);
            window.render_frame(cx);
            assert!(chat_input.read(cx).preview().is_none());
            assert_eq!(chat_input.read(cx).editor.read(cx).value().as_ref(), "");
            assert!(editor_focused(window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(submitted.borrow().as_slice(), ["Fix it"]);
    }

    /// The send menu's Queue option queues the prompt on purpose, and can't be
    /// picked on the Ask tab. Editing a queued prompt sets the prompt being
    /// written aside for the queued one; Esc cancels and Ctrl+Enter saves,
    /// and either way what was set aside comes back.
    #[gpui_kit::test]
    async fn queue_option_and_editing_a_queued_prompt(cx: &mut TestAppContext) {
        use super::{QueuedEdit, SendMode, Submit};
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            crate::main_window::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(cx.new(|_| AtBottom(input)), window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        let submitted = Rc::new(std::cell::RefCell::new(Vec::new()));
        let edits = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscriptions = cx.update(|cx| {
            let (submitted, edits) = (submitted.clone(), edits.clone());
            (
                cx.subscribe(&chat_input, move |_, submit: &Submit, _| {
                    submitted
                        .borrow_mut()
                        .push((submit.text.clone(), submit.mode, submit.queue))
                }),
                cx.subscribe(&chat_input, move |_, edit: &QueuedEdit, _| {
                    edits.borrow_mut().push(match edit {
                        QueuedEdit::Saved {
                            text,
                            mode,
                            attached_text,
                        } => Some((text.clone(), *mode, attached_text.clone())),
                        QueuedEdit::Cancelled => None,
                    })
                }),
            )
        });
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        let text = |cx: &gpui_kit::App| chat_input.read(cx).editor.read(cx).value().to_string();

        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| input.set_text_for_test("Later", window, cx));
            window.press("ctrl-shift-enter", cx);
            window.press("down", cx);
            window.render_frame(cx);
            window.find(("send-option", 1usize));
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            submitted.borrow().as_slice(),
            [("Later".to_string(), SendMode::Both, true)]
        );

        // On the Ask tab, Queue is passed over.
        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input.select_tab(super::ASK_TAB, window, cx);
                input.set_text_for_test("Why?", window, cx);
            });
            window.press("ctrl-shift-enter", cx);
            window.press("down", cx);
            assert_eq!(chat_input.read(cx).send_menu, Some(0));
            chat_input.update(cx, |input, cx| input.pick_send_option(1, window, cx));
            assert!(
                chat_input.read(cx).send_menu_open(),
                "Queue was picked on Ask"
            );
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(submitted.borrow().len(), 1);

        // Editing a queued prompt sets what is written aside.
        chat_input.update(cx, |input, cx| input.attach_text("mine".into(), cx));
        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input.begin_editing(
                    2,
                    "Queued".into(),
                    SendMode::Code,
                    vec!["theirs".into()],
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            window.find("editing-queued");
            assert_eq!(text(cx), "Queued");
            assert_eq!(chat_input.read(cx).mode(), SendMode::Code);
            assert_eq!(chat_input.read(cx).attachments()[0].text, "theirs");
            window.press("ctrl-shift-enter", cx);
            assert!(
                !chat_input.read(cx).send_menu_open(),
                "the menu opened while editing"
            );
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(!chat_input.read(cx).is_editing());
            assert!(window.try_find("editing-queued").is_none());
            assert_eq!(text(cx), "Why?");
            assert_eq!(chat_input.read(cx).mode(), SendMode::Ask);
            assert_eq!(chat_input.read(cx).attachments()[0].text, "mine");
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(edits.borrow().as_slice(), [None]);

        cx.update_window(handle, |_, window, cx| {
            chat_input.update(cx, |input, cx| {
                input.begin_editing(1, "Queued".into(), SendMode::Spec, Vec::new(), window, cx);
                input.set_text_for_test("Queued, edited", window, cx);
            });
            window.press("ctrl-enter", cx);
            assert!(!chat_input.read(cx).is_editing());
            assert_eq!(text(cx), "Why?");
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            edits.borrow()[1],
            Some(("Queued, edited".to_string(), SendMode::Spec, Vec::new()))
        );
        assert_eq!(submitted.borrow().len(), 1, "saving an edit sent a prompt");
    }

    /// Attachments are listed above the input, survive switching tabs, can be
    /// removed one at a time, and go with the prompt when it is sent, which
    /// empties the list.
    #[gpui_kit::test]
    async fn attachments_are_listed_kept_across_tabs_and_sent(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(input, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let handle = window.into();
        let submitted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let submitted = submitted.clone();
            cx.subscribe(&chat_input, move |_, submit: &super::Submit, _| {
                submitted.borrow_mut().push((
                    submit.text.clone(),
                    submit.mode,
                    submit.attached_text.clone(),
                ))
            })
        });
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, _| {
            assert!(
                window.try_find("attachments").is_none(),
                "an empty list shows"
            );
        })
        .unwrap();

        chat_input.update(cx, |input, cx| {
            input.attach_text("first line\nsecond line".into(), cx);
            input.attach_text("drop me".into(), cx);
            input.attach_text("third".into(), cx);
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let list = window.find("attachments").bounds();
            let editor = window.find("prompt-editor").bounds();
            let tabs = window.find("chat-tabs").bounds();
            assert!(
                list.top() >= tabs.bottom() && list.bottom() <= editor.top(),
                "the attachments {list:?} aren't between the tabs {tabs:?} and the input {editor:?}"
            );
            // From the chain, on to Spec.
            window.press("ctrl-tab", cx);
            window.render_frame(cx);
            crate::double_borders::assert_none(window);
            window.click(("remove-attachment", 2usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
        let texts = |cx: &mut TestAppContext| {
            chat_input.read_with(cx, |input, _| {
                input
                    .attachments()
                    .iter()
                    .map(|attachment| attachment.text.clone())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(texts(cx), ["first line\nsecond line", "third"]);

        cx.update_window(handle, |_, window, cx| {
            window.input("go", cx);
            #[cfg(target_os = "macos")]
            window.press("cmd-enter", cx);
            #[cfg(not(target_os = "macos"))]
            window.press("ctrl-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            *submitted.borrow(),
            [(
                "go".to_string(),
                super::SendMode::Spec,
                vec!["first line\nsecond line".to_string(), "third".to_string()]
            )]
        );
        assert!(texts(cx).is_empty(), "sending left the attachments behind");
    }

    type Bounds = gpui_kit::Bounds<gpui_kit::Pixels>;

    /// The Code, chain, Spec, and Ask tabs, as last laid out.
    fn tab_layout(window: &mut Window, cx: &mut gpui_kit::App) -> [Bounds; 4] {
        window.render_frame(cx);
        [
            window.within("code-tab").find(0usize).bounds(),
            window.within("both-tab").find(0usize).bounds(),
            window.within("spec-tab").find(0usize).bounds(),
            window.within("ask-tab").find(0usize).bounds(),
        ]
    }

    /// Draws frames until Spec has slid into place: against Code once the
    /// chain is selected and the three lock together, and against the chain
    /// itself, sitting between them as an ordinary tab, until then.
    fn settle_tabs(
        handle: gpui_kit::AnyWindowHandle,
        joined: bool,
        cx: &mut TestAppContext,
    ) -> [Bounds; 4] {
        let start = std::time::Instant::now();
        loop {
            let layout = cx
                .update_window(handle, |_, window, cx| tab_layout(window, cx))
                .unwrap();
            let [code, chain, spec, _] = layout;
            let spec_offset = spec.left() - if joined { code.right() } else { chain.right() };
            if spec_offset.abs() <= super::px(0.5) {
                return layout;
            }
            assert!(
                start.elapsed() < TIMEOUT,
                "the tabs did not settle: Spec is {spec_offset:?} out of place"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// The chain is a tab like the others: a small square, with no gap either
    /// side of it, between Code and Spec. Selected, it gives up its width so
    /// Code and Spec meet beneath it, with the chain centred on the seam.
    #[gpui_kit::test]
    async fn chain_is_an_evenly_spaced_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            Root::new(input, window, cx)
        });
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        // From Code selected, once the chain has slid back all the way.
        cx.update_window(handle, |_, window, cx| window.press("ctrl-shift-tab", cx))
            .unwrap();
        settle_tabs(handle, false, cx);
        std::thread::sleep(Duration::from_millis(600));
        let [code, chain, spec, ask] = settle_tabs(handle, false, cx);
        for (left, right, between) in [
            (code, chain, "Code and the chain"),
            (chain, spec, "the chain and Spec"),
            (spec, ask, "Spec and Ask"),
        ] {
            assert!(
                (right.left() - left.right()).abs() <= super::px(0.5),
                "there is a gap between {between}: {left:?}, {right:?}"
            );
            assert_eq!(left.size.height, right.size.height, "{between}");
        }
        // The chain is only an icon, so it is the narrowest of the tabs.
        assert!(
            chain.size.width < code.size.width && chain.size.width < spec.size.width,
            "the chain {chain:?} is not narrower than Code {code:?} and Spec {spec:?}"
        );

        // Ctrl+Tab moves to the chain, which selects Code and Spec with it.
        cx.update_window(handle, |_, window, cx| window.press("ctrl-tab", cx))
            .unwrap();
        let [code, chain, _, _] = settle_tabs(handle, true, cx);
        let edge = code.right();
        assert!(
            (chain.center().x - edge).abs() <= super::px(0.5),
            "the chain {chain:?} is not centred on the seam at {edge:?}"
        );
        // The border where Code meets Spec is covered, with room to spare, so
        // no outline shows beneath the chain.
        let seam = cx
            .update_window(handle, |_, window, _| window.find("seam").bounds())
            .unwrap();
        assert!(
            seam.left() <= edge - super::px(2.) && seam.right() >= edge + super::px(2.),
            "the border between Code and Spec, at {edge:?}, is not covered: {seam:?}"
        );
        assert_eq!(
            (seam.top(), seam.bottom()),
            (chain.top(), chain.bottom()),
            "the cover is not as tall as the tabs"
        );

        // Tab again moves to Spec alone, and the chain opens up between them.
        cx.update_window(handle, |_, window, cx| window.press("ctrl-tab", cx))
            .unwrap();
        settle_tabs(handle, false, cx);
    }

    /// Code is tinted red and Spec blue, faintly, both together purple, a hue
    /// between the two, and Ask green. Each tab's own tint shows only while
    /// it is selected.
    #[test]
    fn tints_are_red_blue_and_purple_between() {
        let (red, blue, green) = (gpui_kit::red(), gpui_kit::blue(), gpui_kit::green());
        let (code, both, spec, ask) = (
            super::tint(red, blue, green, 0.),
            super::tint(red, blue, green, 1.),
            super::tint(red, blue, green, 2.),
            super::tint(red, blue, green, 3.),
        );
        let same = |a: gpui_kit::Hsla, b: gpui_kit::Hsla| {
            [(a.h, b.h), (a.s, b.s), (a.l, b.l)]
                .iter()
                .all(|(x, y)| (x - y).abs() < 1e-4)
        };
        assert!(same(code, red), "Code {code:?} is not {red:?}");
        assert!(same(spec, blue), "Spec {spec:?} is not {blue:?}");
        assert!(same(ask, green), "Ask {ask:?} is not {green:?}");
        for color in [code, both, spec, ask] {
            assert_eq!(color.a, super::TINT_OPACITY);
        }
        // Purple sits past blue, on the way round the hue circle to red.
        assert!(
            both.h > blue.h,
            "{both:?} is not between {blue:?} and {red:?}"
        );

        // Rows: Code, chain, Spec, Ask. Columns: the tint at each tab.
        let shown = [
            [1., 1., 0., 0.],
            [0., 1., 0., 0.],
            [0., 1., 1., 0.],
            [0., 0., 0., 1.],
        ];
        for (ix, row) in shown.iter().enumerate() {
            for (position, expected) in row.iter().enumerate() {
                assert_eq!(
                    super::tab_tint_shown(ix, position as f32),
                    *expected,
                    "tab {ix} with the tint at {position}"
                );
            }
        }

        // Wrapping from Ask on to Code blends green straight into red, and
        // never lights up Spec or the chain on the way.
        assert!(same(super::tint(red, blue, green, 4.), red));
        for step in 1..10 {
            let position = 3. + step as f32 / 10.;
            let color = super::tint(red, blue, green, position);
            let between = |a: f32, b: f32, x: f32| {
                let (span, off) = ((b - a).rem_euclid(1.), (x - a).rem_euclid(1.));
                off <= span + 1e-4
            };
            assert!(
                between(green.h, red.h, color.h) || between(red.h, green.h, color.h),
                "{color:?} at {position} is not between green and red"
            );
            for ix in [1, 2] {
                assert_eq!(
                    super::tab_tint_shown(ix, position),
                    0.,
                    "tab {ix} at {position}"
                );
            }
        }
        assert_eq!(super::tab_tint_shown(0, 4.), 1.);
        assert_eq!(super::tab_tint_shown(3, 4.), 0.);
    }

    /// With one line of text, the input is as tall as the Send button and
    /// lines up with it.
    #[gpui_kit::test]
    async fn single_line_input_lines_up_with_send_button(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            Root::new(input, window, cx)
        });
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            // A second frame sizes the input from the editor's laid-out line height.
            window.render_frame(cx);
            let editor = window.find("prompt-editor").bounds();
            let send = window.find("send").bounds();
            assert!(
                (editor.size.height - send.size.height).abs() <= gpui_kit::px(1.),
                "input {editor:?} and Send {send:?} differ in height"
            );
            assert!(
                (editor.bottom() - send.bottom()).abs() <= gpui_kit::px(1.),
                "input {editor:?} and Send {send:?} are not aligned"
            );
        })
        .unwrap();
    }

    /// The chat input at the bottom of the window, with a probe laid out
    /// after it that notes the editor's scroll offset between prepaint and
    /// paint: the offset its text is actually painted at, before paint
    /// clamps it for the next frame.
    struct Probed {
        input: Entity<ChatInput>,
        painted_scroll_y: Rc<Cell<gpui_kit::Pixels>>,
    }

    impl Render for Probed {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let editor = self.input.read(cx).editor.clone();
            let painted_scroll_y = self.painted_scroll_y.clone();
            div()
                .size_full()
                .flex()
                .flex_col()
                .justify_end()
                .child(self.input.clone())
                .child(
                    gpui_kit::canvas(
                        move |_, _, cx| painted_scroll_y.set(editor.read(cx).scroll_offset().y),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_0(),
                )
        }
    }

    /// What one painted frame of the input looked like.
    #[derive(Debug, Clone, Copy)]
    struct Frame {
        rows: usize,
        top: gpui_kit::Pixels,
        bottom: gpui_kit::Pixels,
        height: gpui_kit::Pixels,
        /// The offset the text was painted at in this frame.
        painted_scroll_y: gpui_kit::Pixels,
        /// The offset the editor kept after painting.
        scroll_y: gpui_kit::Pixels,
        cursor_top: gpui_kit::Pixels,
        cursor_bottom: gpui_kit::Pixels,
    }

    /// Presses Enter in the input at the bottom of the window, `presses`
    /// times, and records every frame drawn after each press, the way the
    /// window draws them: only what the press invalidated, with no forced
    /// refresh in between.
    async fn frames_after_enter(
        presses: usize,
        scale_factor: f32,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::Pixels, Vec<Vec<Frame>>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let painted = Rc::new(Cell::new(gpui_kit::px(0.)));
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            let painted_scroll_y = painted.clone();
            let view = cx.new(|_| Probed {
                input,
                painted_scroll_y,
            });
            Root::new(view, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let editor = chat_input.read_with(cx, |input, _| input.editor.clone());
        let handle = window.into();
        // Most displays are not 2x: at other scales the rows are a fraction
        // of a pixel off the device grid.
        cx.simulate_window_scale_factor_change(handle, scale_factor);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            editor.read(cx).line_height().is_some()
        })
        .await;

        let record = |window: &mut Window, cx: &mut gpui_kit::App| {
            window.draw(cx).clear(cx);
            let bounds = window.find("prompt-editor").bounds();
            let editor = editor.read(cx);
            let scroll_y = editor.scroll_offset().y;
            // The cursor is laid out in window coordinates, before scrolling.
            let (cursor, _) = editor.cursor_layout().unwrap();
            let painted_scroll_y = painted.get();
            Frame {
                rows: editor.value().split('\n').count(),
                top: bounds.top(),
                bottom: bounds.bottom(),
                height: bounds.size.height,
                painted_scroll_y,
                scroll_y,
                cursor_top: cursor.top() + painted_scroll_y,
                cursor_bottom: cursor.bottom() + painted_scroll_y,
            }
        };

        cx.update_window(handle, |_, window, cx| {
            editor.read(cx).focus_handle(cx).focus(window, cx);
            window.render_frame(cx);
            window.render_frame(cx);
            window.input("x", cx);
        })
        .unwrap();
        let line_height = editor.read_with(cx, |editor, _| editor.line_height().unwrap());

        let mut presses_frames = vec![];
        for _ in 0..presses {
            let frames = cx
                .update_window(handle, |_, window, cx| {
                    let before = record(window, cx);
                    window.dispatch_keystroke(gpui_kit::Keystroke::parse("enter").unwrap(), cx);
                    let mut frames = vec![before];
                    for _ in 0..6 {
                        frames.push(record(window, cx));
                    }
                    frames
                })
                .unwrap();
            cx.run_until_parked();
            presses_frames.push(frames);
        }
        (line_height, presses_frames)
    }

    /// Enter grows the input by one row in the very next frame, and it stays
    /// put from then on: the bottom edge never moves, the first line never
    /// scrolls out of view, and the cursor is always inside the input.
    async fn assert_enter_grows_without_flicker(scale_factor: f32, cx: &mut TestAppContext) {
        let (line_height, presses) = frames_after_enter(MAX_ROWS + 3, scale_factor, cx).await;
        let bottom = presses[0][0].bottom;
        let one_row = presses[0][0].height;
        for (press, frames) in presses.iter().enumerate() {
            let rows = frames.last().unwrap().rows;
            let expected = one_row + line_height * (rows.min(MAX_ROWS) - 1) as f32;
            for (ix, frame) in frames.iter().enumerate().skip(1) {
                let context = format!(
                    "at {scale_factor}x, line height {line_height:?}, press {press}, frame {ix} of {frames:#?}"
                );
                assert!(
                    (frame.height - expected).abs() <= gpui_kit::px(0.5),
                    "height is not {expected:?} rows: {context}"
                );
                assert!(
                    (frame.bottom - bottom).abs() <= gpui_kit::px(0.5),
                    "the bottom edge moved from {bottom:?}: {context}"
                );
                if rows <= MAX_ROWS {
                    assert!(
                        frame.painted_scroll_y.abs() <= gpui_kit::px(0.5),
                        "painted scrolled although everything fits: {context}"
                    );
                } else {
                    assert!(
                        (frame.painted_scroll_y - frames.last().unwrap().painted_scroll_y).abs()
                            <= gpui_kit::px(0.5),
                        "scroll did not settle in the first frame: {context}"
                    );
                }
                assert!(
                    frame.cursor_top >= frame.top - gpui_kit::px(0.5)
                        && frame.cursor_bottom <= frame.bottom + gpui_kit::px(0.5),
                    "the cursor is outside the input: {context}"
                );
            }
        }
    }

    #[gpui_kit::test]
    async fn enter_grows_the_input_without_flicker_at_1x(cx: &mut TestAppContext) {
        assert_enter_grows_without_flicker(1.0, cx).await;
    }

    #[gpui_kit::test]
    async fn enter_grows_the_input_without_flicker_at_1_25x(cx: &mut TestAppContext) {
        assert_enter_grows_without_flicker(1.25, cx).await;
    }

    #[gpui_kit::test]
    async fn enter_grows_the_input_without_flicker_at_1_5x(cx: &mut TestAppContext) {
        assert_enter_grows_without_flicker(1.5, cx).await;
    }

    #[gpui_kit::test]
    async fn enter_grows_the_input_without_flicker_at_1_75x(cx: &mut TestAppContext) {
        assert_enter_grows_without_flicker(1.75, cx).await;
    }

    #[gpui_kit::test]
    async fn enter_grows_the_input_without_flicker_at_2x(cx: &mut TestAppContext) {
        assert_enter_grows_without_flicker(2.0, cx).await;
    }

    /// The input grows a row for each line and for each soft wrap of a long
    /// line, and shrinks back when the text goes.
    #[gpui_kit::test]
    async fn input_grows_and_shrinks_with_its_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::bind_keys(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
        });
        let mut chat_input = None;
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| ChatInput::new(window, cx));
            chat_input = Some(input.clone());
            Root::new(input, window, cx)
        });
        let chat_input = chat_input.unwrap();
        let editor = chat_input.read_with(cx, |input, _| input.editor.clone());
        let handle = window.into();
        cx.wait_for(handle, TIMEOUT, |window, _| {
            window.try_find("prompt-editor").is_some()
        })
        .await;

        let set_text = |text: String, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
                // Lay out the text, then fit the input to it.
                for _ in 0..3 {
                    window.render_frame(cx);
                }
                let line_height = editor.read(cx).line_height().unwrap();
                let height = window.find("prompt-editor").bounds().size.height;
                (height, line_height)
            })
            .unwrap()
        };

        let (one_row, line_height) = set_text(String::new(), cx);

        let (three_lines, _) = set_text("one\ntwo\nthree".into(), cx);
        assert!(
            (three_lines - one_row - line_height * 2.).abs() <= gpui_kit::px(1.),
            "three lines: {three_lines:?}, one row: {one_row:?}, line height: {line_height:?}"
        );

        let (wrapped, _) = set_text("a long line that has to wrap ".repeat(40), cx);
        assert!(
            wrapped >= one_row + line_height * 2.,
            "one long line did not grow the input: {wrapped:?} vs one row {one_row:?}"
        );
        let editor_width = cx
            .update_window(handle, |_, window, _| {
                window.find("prompt-editor").bounds().size.width
            })
            .unwrap();
        assert!(
            editor_width
                <= cx
                    .update_window(handle, |_, window, _| window.viewport_size().width)
                    .unwrap(),
            "the input got wider than the window instead of wrapping"
        );

        let (cleared, _) = set_text(String::new(), cx);
        assert!(
            (cleared - one_row).abs() <= gpui_kit::px(1.),
            "cleared input did not shrink back: {cleared:?} vs {one_row:?}"
        );
    }
}
