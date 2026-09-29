//! The theme editor, shown in the main window's inset panel: the base colour
//! the mode showing lays its greys out from, and the colours of Code, Chain,
//! Spec, and Ask, each chosen with a colour picker in place of the theme's
//! own, or put back to it. Dark and light mode each keep their own, and a
//! choice shows everywhere at once and is saved for the user straight away
//! (see [`crate::theme_preference`]).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_input::{self, SendMode};
use crate::{ribbon, theme, theme_preference};

actions!(suspense, [OpenThemeEditor]);

/// Emitted when the theme editor is closed.
pub struct CloseThemeEditor;

/// A colour the editor chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Base,
    Code,
    Chain,
    Spec,
    Ask,
}

impl Role {
    const ALL: [Role; 5] = [Role::Base, Role::Code, Role::Chain, Role::Spec, Role::Ask];

    fn label(self) -> &'static str {
        match self {
            Role::Base => "Base",
            Role::Code => "Code",
            Role::Chain => "Chain",
            Role::Spec => "Spec",
            Role::Ask => "Ask",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Role::Base => "base",
            Role::Code => "code",
            Role::Chain => "chain",
            Role::Spec => "spec",
            Role::Ask => "ask",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Role::Base => {
                "The window and its panels. Every other grey keeps its step from it, \
                 and brightness moves them all from here."
            }
            Role::Code => "Code's tab, and whatever shows Code or was sent in it.",
            Role::Chain => {
                "The chain's tab, Code and Spec together. Unless chosen, it is midway \
                 between Code and Spec."
            }
            Role::Spec => "Spec's tab, and whatever shows Spec or was sent in it.",
            Role::Ask => "Ask's tab, and the questions asked in it.",
        }
    }

    /// The mode it colours, for all but the base.
    fn mode(self) -> Option<SendMode> {
        match self {
            Role::Base => None,
            Role::Code => Some(SendMode::Code),
            Role::Chain => Some(SendMode::Both),
            Role::Spec => Some(SendMode::Spec),
            Role::Ask => Some(SendMode::Ask),
        }
    }

    fn slot(self, colors: &mut theme::ModeColors) -> &mut Option<u32> {
        match self {
            Role::Base => &mut colors.base,
            Role::Code => &mut colors.code,
            Role::Chain => &mut colors.chain,
            Role::Spec => &mut colors.spec,
            Role::Ask => &mut colors.ask,
        }
    }

    /// Whether it has been chosen for the mode showing, rather than the
    /// theme's own.
    fn chosen(self, cx: &App) -> bool {
        self.slot(&mut theme::mode_colors(cx.theme().is_dark()))
            .is_some()
    }

    /// Its colour in the mode showing, chosen or the theme's own: the base
    /// as it is at brightness 0.
    fn color(self, cx: &App) -> u32 {
        match self.mode() {
            None => theme::reference(cx.theme().is_dark()).base,
            Some(mode) => to_u32(chat_input::mode_color(mode, cx)),
        }
    }
}

/// `color` as 0xRRGGBB, opaque.
fn to_u32(color: Hsla) -> u32 {
    let rgb = color.to_rgb();
    [rgb.r, rgb.g, rgb.b].into_iter().fold(0, |out, c| {
        (out << 8) | (c.clamp(0., 1.) * 255.).round() as u32
    })
}

/// One colour, as edited.
struct Row {
    role: Role,
    picker: Entity<ColorPickerState>,
    /// The colour the picker was last given or picked, so it is only given
    /// one when the colour has changed some other way.
    shown: u32,
}

pub struct ThemeEditor {
    rows: Vec<Row>,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseThemeEditor> for ThemeEditor {}

impl Focusable for ThemeEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ThemeEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The pickers follow the mode showing and its colours, however they
        // were changed.
        let mut subscriptions = vec![
            cx.observe_global_in::<gpui_kit::component::Theme>(window, |this, window, cx| {
                this.sync(window, cx)
            }),
        ];
        let rows = Role::ALL
            .into_iter()
            .map(|role| {
                let shown = role.color(cx);
                let picker = cx
                    .new(|cx| ColorPickerState::new(window, cx).default_value(theme::color(shown)));
                subscriptions.push(cx.subscribe_in(
                    &picker,
                    window,
                    move |this, _, event: &ColorPickerEvent, window, cx| {
                        let ColorPickerEvent::Change(Some(color)) = event else {
                            return;
                        };
                        this.choose(role, Some(to_u32(*color)), window, cx);
                    },
                ));
                Row {
                    role,
                    picker,
                    shown,
                }
            })
            .collect();
        Self {
            rows,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Chooses `color` for `role` in the mode showing, or, if none, puts the
    /// theme's own back; shows it everywhere at once, and saves it.
    fn choose(
        &mut self,
        role: Role,
        color: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dark = cx.theme().is_dark();
        let mut colors = theme::custom_colors();
        *role.slot(colors.of_mut(dark)) = color;
        if let Some(row) = self.rows.iter_mut().find(|row| row.role == role)
            && let Some(color) = color
        {
            // The picker already shows it, having picked it.
            row.shown = color;
        }
        self.set(colors, window, cx);
    }

    /// Puts every colour of the mode showing back to the theme's own.
    fn reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dark = cx.theme().is_dark();
        let mut colors = theme::custom_colors();
        *colors.of_mut(dark) = theme::ModeColors::default();
        self.set(colors, window, cx);
    }

    fn set(&mut self, colors: theme::CustomColors, window: &mut Window, cx: &mut Context<Self>) {
        theme::set_custom_colors(colors, cx);
        theme_preference::save_colors();
        self.sync(window, cx);
        cx.notify();
    }

    /// Gives each picker its colour in the mode showing, where that has
    /// changed since it was last given or picked.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for row in &mut self.rows {
            let color = row.role.color(cx);
            if row.shown != color {
                row.shown = color;
                row.picker.update(cx, |picker, cx| {
                    picker.set_value(theme::color(color), window, cx)
                });
            }
        }
        cx.notify();
    }

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let role = row.role;
        let chosen = role.chosen(cx);
        // Lets UI tests find the row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(
            h_flex().id(SharedString::from(format!("theme-{}", role.key()))),
        )
        .gap_4()
        .items_center()
        .child(
            ColorPicker::new(&row.picker).accessibility_label(format!("{} colour", role.label())),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().font_semibold().child(role.label()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(format!("#{:06x}", row.shown)),
                        )
                        .when(!chosen, |this| {
                            this.child(div().text_sm().text_color(muted).child("Theme's own"))
                        }),
                )
                .child(div().text_sm().text_color(muted).child(role.description())),
        )
        .child(
            Button::new(SharedString::from(format!("theme-{}-reset", role.key())))
                .small()
                .outline()
                .label("Reset")
                .disabled(!chosen)
                .tooltip(if chosen {
                    "Put back the theme's own colour"
                } else {
                    "Already the theme's own colour"
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.choose(role, None, window, cx)),
                ),
        )
    }

    /// Dark mode and Light mode, one picked: the mode showing, whose colours
    /// are edited. Picking the other shows it, as the ribbon does.
    fn render_modes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        h_flex().gap_1().children(
            [
                (true, "Dark mode", IconName::Moon),
                (false, "Light mode", IconName::Sun),
            ]
            .map(|(mode, label, icon)| {
                Button::new(if mode { "theme-dark" } else { "theme-light" })
                    .small()
                    .ghost()
                    .icon(icon)
                    .label(label)
                    .selected(dark == mode)
                    .tooltip(if mode {
                        "Show dark mode, and edit its colours"
                    } else {
                        "Show light mode, and edit its colours"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if cx.theme().is_dark() != mode {
                            ribbon::set_dark_mode(mode, window, cx);
                            this.sync(window, cx);
                        }
                    }))
            }),
        )
    }
}

impl Render for ThemeEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, muted, border) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
        );
        let any_chosen = theme::mode_colors(theme.is_dark()) != theme::ModeColors::default();
        let heading = h_flex()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(div().flex_1().font_semibold().child("Theme"))
            .child(
                Button::new("theme-editor-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close the theme editor")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseThemeEditor))),
            );
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|row| self.render_row(row, cx).into_any_element())
            .collect();
        let page = div()
            .id("theme-editor")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                v_flex()
                    .gap_4()
                    .p_6()
                    .child(
                        h_flex()
                            .gap_4()
                            .child(div().flex_1().text_lg().font_semibold().child("Colours"))
                            .child(self.render_modes(cx)),
                    )
                    .child(div().text_sm().text_color(muted).child(
                        "Dark and light mode each keep their own colours; these are the \
                         mode showing. A colour shows everywhere as soon as it is chosen, \
                         and is kept for the next launch.",
                    ))
                    .child(v_flex().gap_5().children(rows))
                    .child(
                        h_flex().child(
                            Button::new("theme-reset-all")
                                .small()
                                .outline()
                                .label("Reset all")
                                .disabled(!any_chosen)
                                .tooltip(if any_chosen {
                                    "Put back the theme's own colours for the mode showing"
                                } else {
                                    "The mode showing already has the theme's own colours"
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.reset_all(window, cx)),
                                ),
                        ),
                    ),
            );
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(background)
            .text_color(foreground)
            .child(heading)
            .child(div().flex_1().min_h_0().child(page))
    }
}
