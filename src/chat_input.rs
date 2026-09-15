//! Chat input: a multi-line, Piton-highlighted editor on the left and a send
//! button on the right, in the body of the Code, chain, Spec, and Ask tabs. Enter adds a
//! line; Ctrl/Cmd+Enter sends, or queues the prompt while the harness works.
//! Tab and Shift+Tab cycle the tabs and Esc takes focus out of the input.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::Focusable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Enter, Escape, IndentInline, InputEvent, MoveDown,
    MoveUp, OutdentInline, Rope,
};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Disableable, FocusableExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{CompletionContext, CompletionResponse};

use crate::completion_menu::CompletionMenu;
use crate::harness_mentions;
use crate::main_window::FocusChat;
use crate::piton_lsp::PitonSession;
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::project_lsp::ProjectLsp;

/// The input grows with its text up to this many rows, then scrolls.
const MAX_ROWS: usize = 12;

/// Room the editor keeps free at the right of each line when soft wrapping.
const WRAP_RIGHT_MARGIN: Pixels = px(10.);

#[cfg(target_os = "macos")]
const SEND_SHORTCUT: &str = "⌘Enter";
#[cfg(not(target_os = "macos"))]
const SEND_SHORTCUT: &str = "Ctrl+Enter";

/// The tabs the input sits in, in the order Tab cycles them.
const TABS: [SendMode; 4] = [SendMode::Code, SendMode::Both, SendMode::Spec, SendMode::Ask];

/// The side of the square chain tab, half the height of the tabs.
const BOTH_TAB_SIZE: Pixels = px(16.);

/// Index of the chain tab in `TABS`.
const BOTH_TAB: usize = 1;

/// Index of the Ask tab in `TABS`.
const ASK_TAB: usize = 3;

/// The gap between Code and Spec while the chain is inactive: the chain, with
/// room either side so it overlaps neither tab.
const CHAIN_GAP: Pixels = px(16. + 6. * 2.);

/// How the tabs slide toward and away from the chain: critically damped, so
/// they meet without bouncing past the seam.
const CHAIN_SPRING: SpringConfig = SpringConfig::new(400., 40., 1.);

/// How strongly the selected tabs and their body are tinted: red for Code,
/// blue for Spec, purple for both, and green for Ask.
const TINT_OPACITY: f32 = 0.1;

/// The tint at `position` between the tabs: red at Code (0), blue at Spec
/// (2), purple for both (1), where they meet, and green at Ask (3).
fn tint(red: Hsla, blue: Hsla, green: Hsla, position: f32) -> Hsla {
    if position > 2. {
        blend(blue, green, position - 2.)
    } else {
        blend(red, blue, position / 2.)
    }
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
    let distance = (first - position).max(position - last).max(0.);
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
    fn label(self) -> &'static str {
        match self {
            SendMode::Code => "Code",
            SendMode::Both => "Code and Spec",
            SendMode::Spec => "Spec",
            SendMode::Ask => "Ask",
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
}

pub struct ChatInput {
    editor: Entity<EditorState>,
    completion: Entity<CompletionMenu>,
    /// The tab bar and its body, which Esc moves focus to: out of the input,
    /// but not onto nothing, where the window would hand it straight back.
    focus_handle: FocusHandle,
    /// Index into `TABS`.
    selected_tab: usize,
    lsp: Option<Arc<PitonSession>>,
    busy: bool,
    /// Width the editor's text was last laid out in, which decides where long
    /// lines soft wrap.
    text_width: Option<Pixels>,
    /// What the editor adds around its rows (padding and border) as last
    /// laid out, rounded to device pixels.
    chrome: Option<Pixels>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Submit> for ChatInput {}

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
            cx.subscribe_in(&editor, window, |this, editor, event: &InputEvent, window, cx| {
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
            }),
            cx.observe_global_in::<ProjectLsp>(window, |this, _, cx| this.connect_lsp(cx)),
        ];

        let mut this = Self {
            editor,
            completion,
            focus_handle: cx.focus_handle(),
            selected_tab: 0,
            lsp: None,
            busy: false,
            text_width: None,
            chrome: None,
            _subscriptions: subscriptions,
        };
        this.connect_lsp(cx);
        this
    }

    /// The project's `piton lsp` session, once it is running.
    pub fn lsp(&self) -> Option<Arc<PitonSession>> {
        self.lsp.clone()
    }

    /// Moves keyboard focus into the input.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
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
        let text = self.editor.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        self.editor
            .update(cx, |editor, cx| editor.set_value("", window, cx));
        cx.emit(Submit {
            text,
            mode: TABS[self.selected_tab],
        });
    }

    /// Selects a clicked tab and puts focus back in the input.
    fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_tab = ix;
        self.focus(window, cx);
        cx.notify();
    }

    /// Moves `step` tabs along, wrapping around. An open completion menu
    /// keeps its own Tab and Shift+Tab.
    fn cycle_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.completion.read(cx).is_open() {
            return;
        }
        cx.stop_propagation();
        let count = TABS.len() as isize;
        self.selected_tab = (self.selected_tab as isize + step).rem_euclid(count) as usize;
        cx.notify();
    }

    /// Esc closes an open completion menu, and otherwise takes focus out of
    /// the input.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion.read(cx).is_open() {
            self.completion.update(cx, |menu, cx| menu.hide(cx));
        } else {
            self.focus_handle.focus(window, cx);
        }
    }

    /// The rows `text` takes in the editor: one per line, plus one for every
    /// soft wrap, wrapped the way the editor wraps it.
    fn visual_rows(&self, text: &str, window: &Window, cx: &App) -> usize {
        let Some(wrap_width) = self.text_width.map(|width| width - WRAP_RIGHT_MARGIN) else {
            return text.split('\n').count();
        };
        let theme = cx.theme();
        let mut wrapper = window
            .text_system()
            .line_wrapper(font(theme.mono_font_family.clone()), theme.mono_font_size);
        text.split('\n')
            .map(|line| {
                1 + wrapper
                    .wrap_line(&[LineFragment::text(line)], wrap_width)
                    .count()
            })
            .sum()
    }
}

impl Render for ChatInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.editor.read(cx).value();
        let rows = self.visual_rows(&text, window, cx).clamp(1, MAX_ROWS);
        // Fit the text with the editor's own metrics: its laid-out line height
        // plus the padding and border around its rows. Until the editor has
        // painted, the border is estimated at a device pixel or more per side.
        let line_height = self
            .editor
            .read(cx)
            .line_height()
            .unwrap_or_else(|| window.line_height());
        let chrome = self.chrome.unwrap_or_else(|| {
            gpui_kit::component::Size::Medium.input_py() * 2.
                + ceil_to_device_pixel(px(1.), window) * 2.
        });
        let height = ceil_to_device_pixel(line_height * rows as f32 + chrome, window);
        let one_row = ceil_to_device_pixel(line_height + chrome, window);
        let empty = text.is_empty();

        // Once the editor has painted, check how it was laid out: the width
        // its text wrapped in (a resized window wraps long lines differently)
        // and the padding and border around its rows. When either changed,
        // the input re-fits.
        let this = cx.entity().downgrade();
        let editor = self.editor.clone();
        let track_layout = canvas(
            |_, _, _| {},
            move |bounds, _, _, cx| {
                let Some(text_bounds) = editor.read(cx).text_bounds() else {
                    return;
                };
                let width = text_bounds.size.width;
                let chrome = bounds.size.height - text_bounds.size.height;
                this.update(cx, |this, cx| {
                    let chrome_changed = this
                        .chrome
                        .is_none_or(|old| (old - chrome).abs() > px(0.01));
                    if this.text_width != Some(width) || chrome_changed {
                        this.text_width = Some(width);
                        if chrome_changed {
                            this.chrome = Some(chrome);
                        }
                        cx.notify();
                    }
                })
                .ok();
            },
        )
        .absolute()
        .size_full();

        // Code and Spec are full tabs, each its own bar, with a gap between
        // them where the chain sits: broken and dimmed until selected, then
        // joined. Selecting it selects both tabs, which slide toward the chain
        // until they meet beneath it, so they read as one locked tab. The chain
        // itself never moves; only the tabs slide.
        let selected = self.selected_tab;
        let unified = selected == BOTH_TAB;
        // How far each tab has slid toward the chain: none while apart, half
        // the gap once they meet.
        let slide = SpringAnimation::new(CHAIN_SPRING).to(if unified {
            CHAIN_GAP * 0.5
        } else {
            px(0.)
        });
        // The tint slides between the tabs the same way, so going from Code
        // to Spec passes through purple. Its position is the selected tab's
        // index: 0 for Code, 1 for both, 2 for Spec, 3 for Ask.
        let tint_slide = SpringAnimation::new(CHAIN_SPRING).to(selected as f32);
        let (red, blue, green) = (cx.theme().red, cx.theme().blue, cx.theme().green);
        let tint = move |position| tint(red, blue, green, position);
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
                        .child(Tab::new().label(TABS[ix].label()).px_2())
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
                .child(Tab::new().label(TABS[ix].label()).px_2());
            if selected == ix || unified {
                bar.selected_index(0)
            } else {
                bar
            }
        };
        let code = div()
            .relative()
            .child(full_tab(0))
            .child(tab_tint(0))
            .with_spring("code-slide", slide.clone(), |this, slide| this.left(slide));
        // Spec slides by pulling its left edge in, which carries Ask along.
        let spec = div()
            .relative()
            .ml(CHAIN_GAP)
            .child(full_tab(2))
            .child(tab_tint(2))
            .with_spring("spec-slide", slide.clone(), |this, slide| {
                this.ml(CHAIN_GAP - slide)
            });
        // Ask stands apart from the tabs that edit, and stretches to the
        // right edge.
        let ask = div()
            .relative()
            .flex_1()
            .ml(CHAIN_GAP)
            .child(full_tab(ASK_TAB).w_full())
            .child(tab_tint(ASK_TAB));
        let both = Button::new("both")
            .outline()
            .icon(if unified {
                IconName::Link
            } else {
                IconName::Unlink
            })
            .tooltip(SendMode::Both.label())
            .size(BOTH_TAB_SIZE)
            .focus_ring(false)
            .tab_stop(false)
            .on_click(cx.listener(|this, _, window, cx| this.select_tab(BOTH_TAB, window, cx)));
        let both = if unified { both.primary() } else { both };
        // The selected tabs' facing borders, covered once the tabs meet, in
        // their purple.
        let meet = CHAIN_GAP * 0.5 - px(0.5);
        let seam_cover = cx.theme().tab_active.blend(tint(BOTH_TAB as f32));
        let cover = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-1.))
            .w(px(2.))
            .with_spring("seam-cover", slide, move |this, slide| {
                if slide >= meet {
                    this.bg(seam_cover)
                } else {
                    this
                }
            });
        // An invisible copy of the Code tab bar and half the gap put the
        // chain's place, whatever width Code's label takes.
        let seam = div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .flex()
            .child(
                TabBar::new("code-tab-width")
                    .child(Tab::new().label(SendMode::Code.label()).px_2())
                    .invisible(),
            )
            .child(div().w(CHAIN_GAP * 0.5))
            .child(
                div()
                    .relative()
                    .w_0()
                    .flex()
                    .items_center()
                    .child(cover)
                    .child(
                        div().relative().w_0().h(BOTH_TAB_SIZE).child(
                            gpui_kit::TestSupportExt::test_support(
                                div()
                                    .id(SendMode::Both.id())
                                    .absolute()
                                    .top_0()
                                    .left(BOTH_TAB_SIZE * -0.5)
                                    .rounded(cx.theme().radius)
                                    .when(unified, |this| this.bg(cx.theme().background))
                                    .opacity(if unified { 1. } else { 0.5 })
                                    .child(both),
                            ),
                        ),
                    ),
            );
        let tabs = gpui_kit::TestSupportExt::test_support(
            div()
                .id("chat-tabs")
                .relative()
                .flex()
                .items_center()
                .bg(cx.theme().tab_bar)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .size_full()
                        .border_b_1()
                        .border_color(cx.theme().border),
                )
                .child(code)
                .child(spec)
                .child(ask)
                .child(seam),
        );

        let body = div()
            .flex()
            .flex_row()
            .items_end()
            .gap_2()
            .p_3()
            .with_spring("body-tint", tint_slide, move |this, position| {
                this.bg(tint(position))
            });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(cx.theme().border)
            // Tab and Shift+Tab cycle the tabs rather than indenting, and the
            // input keeps focus.
            .capture_action(cx.listener(|this, _: &IndentInline, _, cx| this.cycle_tab(1, cx)))
            .capture_action(cx.listener(|this, _: &OutdentInline, _, cx| this.cycle_tab(-1, cx)))
            // Esc arrives as the window's FocusChat, which matches ahead of the
            // editor's own binding, or as the editor's Escape once it has
            // nothing to cancel. Handled here, neither reaches the window,
            // which would focus the input again.
            .on_action(cx.listener(|this, _: &FocusChat, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.escape(window, cx)))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
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
                if this.completion.read(cx).is_open() {
                    cx.stop_propagation();
                    this.completion
                        .update(cx, |menu, cx| menu.select_next(-1, window, cx));
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if this.completion.read(cx).is_open() {
                    cx.stop_propagation();
                    this.completion
                        .update(cx, |menu, cx| menu.select_next(1, window, cx));
                }
            }))
            .child(tabs)
            .child(
                body.child(gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("prompt-editor")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .child(Editor::new(&self.editor).h(height))
                        .child(track_layout)
                        .child(self.completion.clone()),
                ))
                .child(
                    Button::new("send")
                        .primary()
                        // While the harness works, sending queues the prompt.
                        .label(if self.busy { "Queue" } else { "Send" })
                        // As tall as the input's single line, so the two line up.
                        .h(one_row)
                        .tooltip(if self.busy {
                            format!("Queue until the harness is free ({SEND_SHORTCUT})")
                        } else {
                            format!("Send ({SEND_SHORTCUT})")
                        })
                        .disabled(empty)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
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

/// Rounds `length` up to a whole device pixel. Layout rounds lengths to
/// device pixels half toward zero, so at fractional scales an input sized in
/// logical pixels can come out a fraction of a pixel short of its rows; the
/// editor then takes its last row for out of view and scrolls it in for a
/// frame, which flickers the text a line up and back.
fn ceil_to_device_pixel(length: Pixels, window: &Window) -> Pixels {
    let scale_factor = window.scale_factor();
    // Float noise just past a whole device pixel must not round up to the next.
    px((f32::from(length) * scale_factor - 1e-3).ceil() / scale_factor)
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `gpui_kit::*` would bring in GPUI's `test`
    // macro and shadow Rust's `#[test]`.
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;

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
        cx.update(|cx| {
            gpui_kit::init(cx);
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
        cx.update(|cx| {
            gpui_kit::init(cx);
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
                menu.is_open() && menu.items(cx).first().is_some_and(|item| item.label == first)
            })
            .await;
            cx.update_window(handle, |_, window, cx| window.press("enter", cx))
                .unwrap();
            cx.wait_for(handle, TIMEOUT, |_, cx| editor.read(cx).value() == expected)
                .await;
        }
    }

    /// Tab in the input moves to the next tab, chain then Spec then Ask then
    /// back to Code, and Shift+Tab back the other way, without indenting the
    /// text or taking focus out of the input.
    #[gpui_kit::test]
    async fn tab_cycles_the_tabs_and_keeps_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
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

        cx.update_window(handle, |_, window, cx| window.input("hi", cx))
            .unwrap();
        cx.run_until_parked();
        let presses = [
            ("tab", 1),
            ("tab", 2),
            ("tab", 3),
            ("tab", 0),
            ("tab", 1),
            ("shift-tab", 0),
            ("shift-tab", 3),
            ("shift-tab", 2),
            ("shift-tab", 1),
        ];
        for (key, expected) in presses {
            cx.update_window(handle, |_, window, cx| window.press(key, cx))
                .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                let input = chat_input.read(cx);
                assert_eq!(input.selected_tab, expected, "after {key}");
                assert_eq!(input.value(cx).as_ref(), "hi");
                assert!(input.is_focused(window, cx), "the input lost focus on {key}");
            })
            .unwrap();
        }
    }

    type Bounds = gpui_kit::Bounds<gpui_kit::Pixels>;

    /// The Code tab, the chain, and the Spec tab, as last laid out.
    fn chain_layout(window: &mut Window, cx: &mut gpui_kit::App) -> (Bounds, Bounds, Bounds) {
        window.render_frame(cx);
        (
            window.within("code-tab").find(0usize).bounds(),
            window.find("both-tab").bounds(),
            window.within("spec-tab").find(0usize).bounds(),
        )
    }

    /// Draws frames until the gap between Code and Spec is `gap`.
    fn settle_chain(
        handle: gpui_kit::AnyWindowHandle,
        gap: gpui_kit::Pixels,
        cx: &mut TestAppContext,
    ) -> (Bounds, Bounds, Bounds) {
        let start = std::time::Instant::now();
        loop {
            let layout = cx
                .update_window(handle, |_, window, cx| chain_layout(window, cx))
                .unwrap();
            let (code, _, spec) = layout;
            if (spec.left() - code.right() - gap).abs() <= super::px(0.5) {
                return layout;
            }
            assert!(
                start.elapsed() < TIMEOUT,
                "the tabs did not settle {gap:?} apart: Code {code:?}, Spec {spec:?}"
            );
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    /// The chain is a square half the height of the tabs, centred on them.
    /// Inactive, it sits in the gap between Code and Spec, overlapping neither.
    /// Active, the tabs slide in until they meet beneath it, and it overlaps
    /// both, centred on the seam. The chain stays put throughout.
    #[gpui_kit::test]
    async fn chain_joins_the_tabs_beneath_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
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

        let (code, chain, spec) = settle_chain(handle, super::CHAIN_GAP, cx);
        assert_eq!(
            chain.size.width, chain.size.height,
            "chain {chain:?} is not square"
        );
        assert!(
            (chain.size.height - code.size.height * 0.5).abs() <= super::px(0.5),
            "chain {chain:?} is not half the height of Code {code:?}"
        );
        assert!(
            (chain.center().y - code.center().y).abs() <= super::px(0.5),
            "chain {chain:?} is not centred on Code {code:?}"
        );
        assert!(
            code.right() <= chain.left() && chain.right() <= spec.left(),
            "inactive chain {chain:?} overlaps Code {code:?} or Spec {spec:?}"
        );
        assert!(
            (chain.center().x - (code.right() + spec.left()) * 0.5).abs() <= super::px(0.5),
            "inactive chain {chain:?} is not centred in the gap"
        );
        let resting = chain;

        // Tab moves to the chain; the tabs slide rather than jump together.
        cx.update_window(handle, |_, window, cx| window.press("tab", cx))
            .unwrap();
        let (code, _, spec) = cx
            .update_window(handle, |_, window, cx| chain_layout(window, cx))
            .unwrap();
        assert!(
            spec.left() - code.right() > super::px(1.),
            "the tabs met without sliding: Code {code:?}, Spec {spec:?}"
        );

        let (code, chain, spec) = settle_chain(handle, super::px(0.), cx);
        assert_eq!(chain, resting, "the chain moved when activated");
        assert!(
            chain.left() < code.right() && spec.left() < chain.right(),
            "active chain {chain:?} does not overlap Code {code:?} and Spec {spec:?}"
        );
        assert!(
            (chain.center().x - code.right()).abs() <= super::px(0.5),
            "active chain {chain:?} is not centred on the seam at {:?}",
            code.right()
        );

        // Back to Code: the tabs slide apart again, around the same chain.
        cx.update_window(handle, |_, window, cx| window.press("shift-tab", cx))
            .unwrap();
        let (_, chain, _) = settle_chain(handle, super::CHAIN_GAP, cx);
        assert_eq!(chain, resting, "the chain moved when deactivated");
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
        assert!(both.h > blue.h, "{both:?} is not between {blue:?} and {red:?}");

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
    }

    /// With one line of text, the input is as tall as the Send button and
    /// lines up with it.
    #[gpui_kit::test]
    async fn single_line_input_lines_up_with_send_button(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
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
