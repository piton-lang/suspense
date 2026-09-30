//! The application's theme (see the ThemeScope): a neutral grey interface
//! defined by its dark mode, around a base of #444444, with light mode read
//! from it. The palette here is the one source of every colour; gpui-kit's
//! theme is built from it, so components with default styling follow it, and
//! what draws its own colours asks for a role here.

use std::rc::Rc;

use gpui_kit::component::tag::{Tag, TagVariant};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use serde_json::{Map, Value, json};

/// Every role of the theme, for one mode, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Where content is read or written: the editor, code, answers.
    pub well: u32,
    /// Text inputs, lists, trees.
    pub recessed: u32,
    /// The window and its panels.
    pub base: u32,
    /// Buttons, tab bars, headers, cards.
    pub raised: u32,
    pub hover: u32,
    pub pressed: u32,
    /// A selected row or tab without focus.
    pub selected: u32,
    /// Menus, popovers, tooltips, inset panels.
    pub overlay: u32,
    /// The ribbon's command area, and its open tabs, which read as one piece
    /// with it.
    pub ribbon: u32,
    /// The ribbon's row of tabs, a step from its command area: lighter in dark
    /// mode, darker in light mode. Also the dividers between its groups.
    pub ribbon_tabs: u32,
    /// The darkest surface in dark mode, a step past the ribbon's command area
    /// away from its tab row: the project tree, and the commit message on the
    /// git panel. In light mode, a step lighter than the command area instead.
    pub darkest: u32,
    pub text: u32,
    /// What the spec calls muted.
    pub text_secondary: u32,
    /// Dimmer than muted: help text, placeholders.
    pub text_tertiary: u32,
    pub text_disabled: u32,
    /// Dividers between areas and rows.
    pub line: u32,
    /// The border of controls, inputs, and floating surfaces.
    pub edge: u32,
    /// Focus, selected tabs' underlines, links, accent text.
    pub accent: u32,
    /// Primary buttons, a selected item in a focused list.
    pub accent_fill: u32,
    pub success: u32,
    pub warning: u32,
    pub error: u32,
    pub purple: u32,
    pub cyan: u32,
    pub orange: u32,
    /// How much black dims the window behind a modal surface.
    pub dim: f32,
    /// How much white lights, and black shades, a bevel's edges.
    pub bevel_light: f32,
    pub bevel_shade: f32,
}

pub const DARK: Palette = Palette {
    well: 0x2e2e2e,
    recessed: 0x383838,
    base: 0x444444,
    raised: 0x4e4e4e,
    hover: 0x585858,
    pressed: 0x3c3c3c,
    selected: 0x5e5e5e,
    overlay: 0x4a4a4a,
    // Two and one steps of 0x11 below the base, so the ribbon's buttons, at
    // the base, stand as far from its command area as its tab row, twice over.
    ribbon: 0x222222,
    ribbon_tabs: 0x333333,
    darkest: 0x111111,
    text: 0xebebeb,
    text_secondary: 0xc4c4c4,
    text_tertiary: 0x9c9c9c,
    text_disabled: 0x7a7a7a,
    line: 0x3a3a3a,
    edge: 0x2a2a2a,
    accent: 0x7fb4ea,
    accent_fill: 0x3d6a98,
    success: 0x8fd095,
    warning: 0xe6bb68,
    error: 0xf0928a,
    purple: 0xbba0e6,
    cyan: 0x78c8ce,
    orange: 0xf2a168,
    dim: 0.4,
    bevel_light: 0.08,
    bevel_shade: 0.06,
};

pub const LIGHT: Palette = Palette {
    well: 0xf5f5f5,
    recessed: 0xe8e8e8,
    base: 0xd0d0d0,
    raised: 0xdadada,
    hover: 0xe2e2e2,
    pressed: 0xc6c6c6,
    selected: 0xbcbcbc,
    overlay: 0xdcdcdc,
    // The same step the other way: the command area on the base, where white
    // laid over it lightens its buttons, and the tab row a step darker.
    ribbon: 0xd0d0d0,
    ribbon_tabs: 0xbfbfbf,
    darkest: 0xe1e1e1,
    text: 0x1e1e1e,
    text_secondary: 0x454545,
    text_tertiary: 0x636363,
    text_disabled: 0x8c8c8c,
    line: 0xbdbdbd,
    edge: 0xa6a6a6,
    accent: 0x1f5fa6,
    accent_fill: 0x2b68ad,
    success: 0x2c6e31,
    warning: 0x8a5a00,
    error: 0xb3261e,
    purple: 0x6b4aa6,
    cyan: 0x1f6f78,
    orange: 0x9a3f00,
    dim: 0.25,
    bevel_light: 0.55,
    bevel_shade: 0.03,
};

/// How much of the accent fill selected text is laid over with.
const SELECTION_ALPHA: u8 = 0x59;

pub const DARK_NAME: &str = "Suspense Dark";
pub const LIGHT_NAME: &str = "Suspense Light";

/// The colours of one mode the user has chosen in place of the theme's own,
/// from the theme editor; each is the theme's own while it is none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModeColors {
    /// The base grey the mode's whole ladder of greys is laid out from, at
    /// brightness 0.
    pub base: Option<u32>,
    pub code: Option<u32>,
    /// The chain's colour; the theme's own is midway between Code's and
    /// Spec's.
    pub chain: Option<u32>,
    pub spec: Option<u32>,
    pub ask: Option<u32>,
}

/// The colours the user has chosen, for dark and light mode apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CustomColors {
    pub dark: ModeColors,
    pub light: ModeColors,
}

impl CustomColors {
    pub fn of(&self, dark: bool) -> &ModeColors {
        if dark { &self.dark } else { &self.light }
    }

    pub fn of_mut(&mut self, dark: bool) -> &mut ModeColors {
        if dark {
            &mut self.dark
        } else {
            &mut self.light
        }
    }
}

thread_local! {
    /// The colours chosen: kept apart from the application's globals, since
    /// the brightness ranges they change are asked for with no `App` at hand.
    /// Everything that reads them runs on the main thread.
    static CUSTOM: std::cell::Cell<CustomColors> = std::cell::Cell::new(CustomColors::default());
}

/// The colours the user has chosen in place of the theme's own.
pub fn custom_colors() -> CustomColors {
    CUSTOM.get()
}

/// The chosen colours of dark or light mode.
pub fn mode_colors(dark: bool) -> ModeColors {
    *custom_colors().of(dark)
}

/// Makes `colors` the ones chosen, and has every window take them at once:
/// each mode's greys laid out from its base, its brightness kept within what
/// that base allows.
pub fn set_custom_colors(colors: CustomColors, cx: &mut App) {
    if custom_colors() == colors {
        return;
    }
    CUSTOM.set(colors);
    for dark in [true, false] {
        let range = mode_brightness_range(dark);
        let brightness = cx.default_global::<Brightness>();
        let step = if dark {
            &mut brightness.dark
        } else {
            &mut brightness.light
        };
        *step = (*step).clamp(*range.start(), *range.end());
        install_brightness(dark, cx);
    }
    if cx.has_global::<Theme>() {
        let mode = cx.theme().mode;
        Theme::change(mode, None, cx);
    }
    cx.refresh_windows();
}

/// Dark or light mode's palette at brightness 0: the theme's own, its greys
/// moved with the base chosen for it, if one has been.
pub fn reference(dark: bool) -> Palette {
    let own = if dark { DARK } else { LIGHT };
    match mode_colors(dark).base {
        Some(base) => rebased(&own, base),
        None => own,
    }
}

/// `own` with its base at `base`, and every other grey as far from it, in
/// each channel, as it was from its own base.
fn rebased(own: &Palette, base: u32) -> Palette {
    let mut p = *own;
    for grey in greys_mut(&mut p) {
        *grey = from_channels(|at| {
            (channel(*grey, at) + channel(base, at) - channel(own.base, at)) as f32
        });
    }
    p
}

/// A colour's channels, averaged: its lightness, as far as brightness goes.
fn level(color: u32) -> i32 {
    (channel(color, 16) + channel(color, 8) + channel(color, 0)) / 3
}

/// The palette of the mode showing, at that mode's brightness.
pub fn palette(cx: &App) -> &'static Palette {
    let dark = cx.theme().is_dark();
    shifted(dark, brightness_of(dark, cx))
}

/// The grey the Brightness slider crosses from dark mode to light mode on:
/// #777777, lightness 50 in CIELAB, halfway between black and white to the
/// eye. Dark mode's base runs up to just below it, light mode's down to it,
/// so the base moves through it with no jump.
pub const MIDDLE_GREY: u32 = 0x77;

/// How far, in channel values, the base can be from the middle grey and still
/// feel it: within this, the steps between the greys narrow, so crossing
/// between the modes, where the steps turn round, changes as little as it
/// can.
const MIDDLE_BAND: f32 = 48.;

/// How much the steps between the greys narrow on the middle grey itself:
/// to half, so the change of mode moves every surface half as far, while
/// every step keeps its direction and the greys their order.
const MIDDLE_NARROWING: f32 = 0.5;

/// How far, in each channel, `step` moves the base: one channel value a step,
/// so the slider moves the interface as finely as a colour can.
pub fn brightness_shift(step: i32) -> i32 {
    step
}

/// The palette's neutral greys, which brightness moves: every surface, the
/// ribbon's, the darkest, and the lines. Text, the accent, the status hues,
/// and the overlays' strengths aren't among them.
fn greys_mut(p: &mut Palette) -> [&mut u32; 13] {
    [
        &mut p.well,
        &mut p.recessed,
        &mut p.base,
        &mut p.raised,
        &mut p.hover,
        &mut p.pressed,
        &mut p.selected,
        &mut p.overlay,
        &mut p.ribbon,
        &mut p.ribbon_tabs,
        &mut p.darkest,
        &mut p.line,
        &mut p.edge,
    ]
}

/// The palette's neutral greys, named, as brightness moves them.
pub fn greys(palette: &Palette) -> [(&'static str, u32); 13] {
    let p = palette;
    [
        ("well", p.well),
        ("recessed", p.recessed),
        ("base", p.base),
        ("raised", p.raised),
        ("hover", p.hover),
        ("pressed", p.pressed),
        ("selected", p.selected),
        ("overlay", p.overlay),
        ("ribbon", p.ribbon),
        ("ribbon_tabs", p.ribbon_tabs),
        ("darkest", p.darkest),
        ("line", p.line),
        ("edge", p.edge),
    ]
}

/// A channel of `color`, `at` bits up.
fn channel(color: u32, at: u32) -> i32 {
    ((color >> at) & 0xff) as i32
}

/// A colour from its red, green, and blue, each clipped to black and white.
fn from_channels(f: impl Fn(u32) -> f32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |out, at| {
        out | ((f(at).round().clamp(0., 255.) as u32) << at)
    })
}

/// `a` laid `amount` of the way toward `b`.
fn mix(a: u32, b: u32, amount: f32) -> u32 {
    from_channels(|at| {
        let (a, b) = (channel(a, at) as f32, channel(b, at) as f32);
        a + (b - a) * amount
    })
}

/// How near the middle grey dark or light mode's base is at `step`, from 0,
/// out of the middle band, to 1, on the middle grey, easing in and out.
fn nearness_to_middle(dark: bool, step: i32) -> f32 {
    let base = level(reference(dark).base) + brightness_shift(step);
    let t = (1. - (base - MIDDLE_GREY as i32).abs() as f32 / MIDDLE_BAND).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// `color`, or as little of the way toward `toward` as it takes for `ok`,
/// or `toward` itself if nothing short of it will do.
fn legible(color: u32, toward: u32, ok: impl Fn(u32) -> bool) -> u32 {
    if ok(color) {
        return color;
    }
    if !ok(toward) {
        return toward;
    }
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..16 {
        let middle = (low + high) / 2.;
        if ok(mix(color, toward, middle)) {
            high = middle;
        } else {
            low = middle;
        }
    }
    mix(color, toward, high)
}

/// Dark or light mode's palette at brightness `step`. Every neutral grey
/// moves with the base by the same amount; near the middle grey the steps
/// between them narrow, all alike, so the two modes meet there with as
/// little change as turning the text round allows. Text and hues
/// stay as given unless the base has come too near them to keep the
/// contrast the theme asks, and then move only as far toward white, in dark
/// mode, or black, in light mode, as it takes. The bevel keeps the lift it
/// has at 0.
pub fn brightened(dark: bool, step: i32) -> Palette {
    let own = reference(dark);
    let shift = brightness_shift(step);
    let width = 1. - MIDDLE_NARROWING * nearness_to_middle(dark, step);
    let mut p = own;
    for grey in greys_mut(&mut p) {
        *grey = from_channels(|at| {
            let step = (channel(*grey, at) - channel(own.base, at)) as f32;
            (channel(own.base, at) + shift) as f32 + step * width
        });
    }

    // Text and hues, only as far from what they were as the contrast needs:
    // what the theme asks, or what they had at 0 if that was less.
    let toward = if dark { 0xffffff } else { 0x000000 };
    let on = |surfaces: &[(u32, u32)], color: u32, asked: f32, moved: u32| {
        surfaces
            .iter()
            .all(|&(now, then)| contrast(moved, now) >= asked.min(contrast(color, then)))
    };
    let read = [
        (p.base, own.base),
        (p.recessed, own.recessed),
        (p.well, own.well),
        (p.overlay, own.overlay),
    ];
    let controls = [
        (p.raised, own.raised),
        (p.hover, own.hover),
        (p.pressed, own.pressed),
        (p.selected, own.selected),
    ];
    let base = [(p.base, own.base)];
    p.text = legible(own.text, toward, |c| {
        on(&read, own.text, 4.5, c) && on(&controls, own.text, 4., c)
    });
    p.text_secondary = legible(own.text_secondary, toward, |c| {
        on(&read, own.text_secondary, 4.5, c)
    });
    p.text_disabled = legible(own.text_disabled, toward, |c| {
        on(&base, own.text_disabled, f32::INFINITY, c)
    });
    for (moved, given) in [
        (&mut p.text_tertiary, own.text_tertiary),
        (&mut p.accent, own.accent),
        (&mut p.success, own.success),
        (&mut p.warning, own.warning),
        (&mut p.error, own.error),
        (&mut p.purple, own.purple),
        (&mut p.cyan, own.cyan),
        (&mut p.orange, own.orange),
    ] {
        *moved = legible(given, toward, |c| on(&base, given, 3., c));
    }

    // The bevel's light and shade, as much lighter and darker as at 0.
    let (then, now) = (channel(own.base, 0) as f32, channel(p.base, 0) as f32);
    p.bevel_light = (own.bevel_light * (255. - then) / (255. - now).max(1.)).min(1.);
    p.bevel_shade = (own.bevel_shade * then / now.max(1.)).min(1.);
    p
}

/// WCAG's contrast ratio between two opaque colours.
pub fn contrast(a: u32, b: u32) -> f32 {
    let luminance = |c: u32| {
        let channel = |shift: u32| {
            let v = ((c >> shift) & 0xff) as f32 / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Whether the text and hues of `p` keep the contrast the theme asks of them
/// on its surfaces: primary and secondary text 4.5 to 1 on the base,
/// recessed, well, and overlay surfaces; primary text 4 to 1 on controls in
/// any state; tertiary text, the status colours, and the accent 3 to 1 on the
/// base.
#[cfg(test)]
pub fn keeps_contrast(p: &Palette) -> bool {
    let read = [p.base, p.recessed, p.well, p.overlay];
    let controls = [p.raised, p.hover, p.pressed, p.selected];
    let hues = [
        p.text_tertiary,
        p.success,
        p.warning,
        p.error,
        p.accent,
        p.orange,
    ];
    [p.text, p.text_secondary].into_iter().all(|text| {
        read.into_iter()
            .all(|surface| contrast(text, surface) >= 4.5)
    }) && controls
        .into_iter()
        .all(|control| contrast(p.text, control) >= 4.)
        && hues.into_iter().all(|hue| contrast(hue, p.base) >= 3.)
}

/// Whether dark or light mode can be at brightness `step`: no grey pushed
/// below black or above white, and its base on its own side of the middle
/// grey, dark mode's below it and light mode's on it or above. 0 is always
/// allowed, as the base was chosen, wherever that is.
pub fn brightness_allowed(dark: bool, step: i32) -> bool {
    if step == 0 {
        return true;
    }
    let reference = &reference(dark);
    let base = level(reference.base) + brightness_shift(step);
    let side = if dark {
        base < MIDDLE_GREY as i32
    } else {
        base >= MIDDLE_GREY as i32
    };
    let clips = greys(reference).into_iter().any(|(_, grey)| {
        [16, 8, 0].into_iter().any(|at| {
            let moved = channel(grey, at) + brightness_shift(step);
            !(0..=255).contains(&moved)
        })
    });
    side && !clips
}

/// How far dark or light mode's brightness can go either way: from 0 out to
/// the last step allowed before one isn't.
pub fn brightness_range(dark: bool) -> std::ops::RangeInclusive<i32> {
    let reach = |direction: i32| {
        let mut step = 0;
        while brightness_allowed(dark, step + direction) {
            step += direction;
        }
        step
    };
    reach(-1)..=reach(1)
}

thread_local! {
    /// Each mode's brightness range, for each base it has had.
    static RANGES: std::cell::RefCell<
        std::collections::HashMap<(bool, Option<u32>), std::ops::RangeInclusive<i32>>,
    > = Default::default();
    /// The appearances, for each pair of bases the modes have had.
    static APPEARANCES: std::cell::RefCell<
        std::collections::HashMap<(Option<u32>, Option<u32>), Rc<[(bool, i32)]>>,
    > = Default::default();
    /// Each palette asked for, by its mode, base, and brightness.
    static PALETTES: std::cell::RefCell<
        std::collections::HashMap<(bool, Option<u32>, i32), &'static Palette>,
    > = Default::default();
}

/// The brightness range of dark or light mode, from its base, worked out
/// once for each base.
pub fn mode_brightness_range(dark: bool) -> std::ops::RangeInclusive<i32> {
    let key = (dark, mode_colors(dark).base);
    if let Some(range) = RANGES.with_borrow(|ranges| ranges.get(&key).cloned()) {
        return range;
    }
    let range = brightness_range(dark);
    RANGES.with_borrow_mut(|ranges| ranges.insert(key, range.clone()));
    range
}

/// Every appearance the Brightness slider runs through, darkest first: dark
/// mode from its darkest step to its lightest, then light mode from its
/// darkest to its lightest, each as whether it is dark mode and its step.
pub fn appearances() -> Rc<[(bool, i32)]> {
    let key = (mode_colors(true).base, mode_colors(false).base);
    if let Some(all) = APPEARANCES.with_borrow(|all| all.get(&key).cloned()) {
        return all;
    }
    let dark = mode_brightness_range(true).map(|step| (true, step));
    let light = mode_brightness_range(false).map(|step| (false, step));
    let all: Rc<[(bool, i32)]> = dark.chain(light).collect();
    APPEARANCES.with_borrow_mut(|appearances| appearances.insert(key, all.clone()));
    all
}

/// Where dark or light mode at brightness `step` falls among the
/// appearances, the step kept within what the mode allows.
pub fn appearance_index(dark: bool, step: i32) -> usize {
    let range = mode_brightness_range(dark);
    let step = step.clamp(*range.start(), *range.end());
    appearances()
        .iter()
        .position(|&appearance| appearance == (dark, step))
        .expect("every step a mode allows is an appearance")
}

/// Dark or light mode's palette at brightness `step`, each worked out once,
/// when first asked for, and kept for the life of the application, so the
/// palette showing can still be borrowed for as long.
fn shifted(dark: bool, step: i32) -> &'static Palette {
    let range = mode_brightness_range(dark);
    let step = step.clamp(*range.start(), *range.end());
    let key = (dark, mode_colors(dark).base, step);
    if let Some(palette) = PALETTES.with_borrow(|palettes| palettes.get(&key).copied()) {
        return palette;
    }
    let palette: &'static Palette = Box::leak(Box::new(brightened(dark, step)));
    PALETTES.with_borrow_mut(|palettes| palettes.insert(key, palette));
    palette
}

/// Each mode's brightness, and the gpui-kit themes built for the steps taken
/// so far, so dragging back and forth doesn't build them again.
#[derive(Default)]
struct Brightness {
    dark: i32,
    light: i32,
    /// By mode, base, and step.
    configs: std::collections::HashMap<(bool, Option<u32>, i32), Rc<ThemeConfig>>,
}

impl Global for Brightness {}

/// The brightness of the mode showing.
pub fn brightness(cx: &App) -> i32 {
    brightness_of(cx.theme().is_dark(), cx)
}

/// The brightness of dark or light mode: 0 until set.
pub fn brightness_of(dark: bool, cx: &App) -> i32 {
    cx.try_global::<Brightness>()
        .map_or(0, |b| if dark { b.dark } else { b.light })
}

/// Sets dark or light mode's brightness to `step`, kept within what the mode
/// allows, and, if that mode is showing, has every window take it at once:
/// only colours change, so nothing is laid out anew. Returns the step taken.
pub fn set_brightness(dark: bool, step: i32, cx: &mut App) -> i32 {
    let range = mode_brightness_range(dark);
    let step = step.clamp(*range.start(), *range.end());
    if cx.has_global::<Brightness>() && brightness_of(dark, cx) == step {
        return step;
    }
    let brightness = cx.default_global::<Brightness>();
    if dark {
        brightness.dark = step;
    } else {
        brightness.light = step;
    }
    install_brightness(dark, cx);
    if cx.has_global::<Theme>() && cx.theme().is_dark() == dark {
        let mode = cx.theme().mode;
        Theme::change(mode, None, cx);
        cx.refresh_windows();
    }
    step
}

/// Makes gpui-kit's theme for dark or light mode the one for its brightness.
fn install_brightness(dark: bool, cx: &mut App) {
    if !cx.has_global::<Theme>() || !cx.has_global::<ThemeRegistry>() {
        return;
    }
    let step = brightness_of(dark, cx);
    let key = (dark, mode_colors(dark).base, step);
    let cached = cx
        .try_global::<Brightness>()
        .and_then(|b| b.configs.get(&key).cloned());
    let config = match cached {
        Some(config) => config,
        None => {
            let (mode, registry) = (
                if dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                },
                ThemeRegistry::global(cx),
            );
            let default = if dark {
                registry.default_dark_theme()
            } else {
                registry.default_light_theme()
            };
            let value = self::config(shifted(dark, step), mode, default);
            let config = Rc::new(
                serde_json::from_value::<ThemeConfig>(value)
                    .expect("the theme's own configuration is valid"),
            );
            cx.default_global::<Brightness>()
                .configs
                .insert(key, config.clone());
            config
        }
    };
    let theme = Theme::global_mut(cx);
    if dark {
        theme.dark_theme = config;
    } else {
        theme.light_theme = config;
    }
}

/// `color` as an opaque colour.
pub fn color(color: u32) -> Hsla {
    rgb(color).into()
}

/// A hue the spec names for what is shown, as the theme gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hue {
    Blue,
    Cyan,
    Purple,
    Green,
    Amber,
    Orange,
    Red,
    Grey,
}

impl Hue {
    pub fn of(self, palette: &Palette) -> Hsla {
        color(match self {
            Self::Blue => palette.accent,
            Self::Cyan => palette.cyan,
            Self::Purple => palette.purple,
            Self::Green => palette.success,
            Self::Amber => palette.warning,
            Self::Orange => palette.orange,
            Self::Red => palette.error,
            Self::Grey => palette.text_tertiary,
        })
    }
}

/// A tag in `hue`: the hue's text over a faint tint of it, with a border of it.
pub fn tag(hue: Hue, cx: &App) -> Tag {
    let color = hue.of(palette(cx));
    Tag::new().with_variant(TagVariant::Custom {
        color: color.opacity(0.16),
        foreground: color,
        border: color.opacity(0.45),
    })
}

/// How wide a bevel's edges are.
pub const BEVEL: Pixels = px(1.);

/// Which way a bevel faces: raised, lit along its top and left, or pressed in,
/// the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bevel {
    Raised,
    #[cfg_attr(not(test), allow(dead_code))]
    Pressed,
}

/// A bevel's lit and shaded edges' colours, laid over whatever they sit on.
pub fn bevel_colors(palette: &Palette) -> (Hsla, Hsla) {
    (
        white().opacity(palette.bevel_light),
        black().opacity(palette.bevel_shade),
    )
}

/// The bevel of a surface at `bounds`, each edge with its colour. Raised, it is
/// lit along the top, then down the left beneath that, and shaded along the
/// bottom, then up the right above that, so no two overlap; pressed, the lit
/// and shaded edges swap. None for a surface too small.
pub fn bevel_edges(
    bounds: Bounds<Pixels>,
    bevel: Bevel,
    palette: &Palette,
) -> Vec<(Bounds<Pixels>, Hsla)> {
    let (width, height) = (bounds.size.width, bounds.size.height);
    if width < BEVEL * 2. || height < BEVEL * 2. {
        return Vec::new();
    }
    let (light, shade) = match (bevel, bevel_colors(palette)) {
        (Bevel::Raised, (light, shade)) => (light, shade),
        (Bevel::Pressed, (light, shade)) => (shade, light),
    };
    let side = height - BEVEL * 2.;
    vec![
        (Bounds::new(bounds.origin, size(width, BEVEL)), light),
        (
            Bounds::new(
                point(bounds.left(), bounds.top() + BEVEL),
                size(BEVEL, side),
            ),
            light,
        ),
        (
            Bounds::new(
                point(bounds.left(), bounds.bottom() - BEVEL),
                size(width, BEVEL),
            ),
            shade,
        ),
        (
            Bounds::new(
                point(bounds.right() - BEVEL, bounds.top() + BEVEL),
                size(BEVEL, side),
            ),
            shade,
        ),
    ]
}

/// The bevel, laid over whatever positioned element it is put in, filling it
/// and taking no room or mouse.
pub fn bevel(bevel: Bevel, cx: &App) -> impl IntoElement {
    let palette = *palette(cx);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            for (edge, color) in bevel_edges(bounds, bevel, &palette) {
                window.paint_quad(fill(edge, color));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The black laid over the window behind a modal surface.
pub fn dimming(cx: &App) -> Hsla {
    black().opacity(palette(cx).dim)
}

/// Registers the theme's two modes with gpui-kit and makes them the ones
/// light and dark mode show. Call after `gpui_kit::init`, before the theme
/// mode is chosen.
pub fn init(cx: &mut App) {
    let registry = ThemeRegistry::global(cx);
    let set = json!({
        "name": "Suspense",
        "themes": [
            config(&DARK, ThemeMode::Dark, registry.default_dark_theme()),
            config(&LIGHT, ThemeMode::Light, registry.default_light_theme()),
        ],
    });
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(&set.to_string())
        .expect("the theme's own configuration is valid");
    let themes = ThemeRegistry::global(cx).themes();
    let (dark, light) = (themes[DARK_NAME].clone(), themes[LIGHT_NAME].clone());
    let theme = Theme::global_mut(cx);
    theme.dark_theme = dark;
    theme.light_theme = light;
    // Each mode at the brightness already set, if any has been.
    if cx.has_global::<Brightness>() {
        install_brightness(true, cx);
        install_brightness(false, cx);
    }
}

fn hex(color: u32) -> String {
    format!("#{color:06x}")
}

fn hex_alpha(color: u32, alpha: u8) -> String {
    format!("#{color:06x}{alpha:02x}")
}

/// A gpui-kit theme for `palette`, keeping the syntax colours of `default`.
fn config(palette: &Palette, mode: ThemeMode, default: &Rc<ThemeConfig>) -> Value {
    let p = palette;
    let dark = mode.is_dark();
    // Text on a solid status or accent colour: dark over dark mode's light
    // hues, white over light mode's dark ones.
    let on_hue = if dark { hex(0x2a2a2a) } else { hex(0xffffff) };
    let (fill_hover, fill_active) = if dark {
        (0x4877a6, 0x335a82)
    } else {
        (0x3674ba, 0x245a96)
    };
    let dim = hex_alpha(0x000000, (p.dim * 255.).round() as u8);

    let mut colors = Map::new();
    let mut set = |key: &str, value: String| {
        colors.insert(key.to_string(), Value::String(value));
    };
    set("background", hex(p.base));
    set("foreground", hex(p.text));
    set("border", hex(p.edge));
    set("input.border", hex(p.edge));
    set("window.border", hex(p.edge));
    set("caret", hex(p.text));
    set("ring", hex(p.accent));
    set("overlay", dim);
    set(
        "selection.background",
        hex_alpha(p.accent_fill, SELECTION_ALPHA),
    );

    set("muted.background", hex(p.recessed));
    set("muted.foreground", hex(p.text_secondary));
    set("accent.background", hex(p.hover));
    set("accent.foreground", hex(p.text));
    set("accordion.background", hex(p.base));
    set("popover.background", hex(p.overlay));
    set("popover.foreground", hex(p.text));
    set("tiles.background", hex(p.base));
    set("group_box.background", hex(p.raised));
    set("group_box.foreground", hex(p.text));
    set("group_box.title.foreground", hex(p.text_secondary));
    set("description_list.label.background", hex(p.raised));
    set("description_list.label.foreground", hex(p.text_secondary));
    set("skeleton.background", hex(p.raised));

    set("primary.background", hex(p.accent_fill));
    set("primary.hover.background", hex(fill_hover));
    set("primary.active.background", hex(fill_active));
    set("primary.foreground", hex(0xffffff));
    set("secondary.background", hex(p.raised));
    set("secondary.hover.background", hex(p.hover));
    set("secondary.active.background", hex(p.pressed));
    set("secondary.foreground", hex(p.text));
    set("button.background", hex(p.raised));
    set("button.hover.background", hex(p.hover));
    set("button.active.background", hex(p.pressed));
    set("button.foreground", hex(p.text));
    set("button.primary.background", hex(p.accent_fill));
    set("button.primary.hover.background", hex(fill_hover));
    set("button.primary.active.background", hex(fill_active));
    set("button.primary.foreground", hex(0xffffff));

    for (name, hue) in [
        ("danger", p.error),
        ("success", p.success),
        ("warning", p.warning),
        ("info", p.accent),
    ] {
        set(&format!("{name}.background"), hex(hue));
        set(&format!("{name}.hover.background"), hex(hue));
        set(&format!("{name}.active.background"), hex(hue));
        set(&format!("{name}.foreground"), on_hue.clone());
    }
    set("link", hex(p.accent));
    set("link.hover", hex(p.accent));
    set("link.active", hex(p.accent));

    set("list.background", hex(p.recessed));
    set("list.even.background", hex_alpha(p.recessed, 0));
    set("list.head.background", hex(p.raised));
    set("list.hover.background", hex(p.hover));
    set("list.active.background", hex(p.selected));
    set("list.active.border", hex(p.accent));
    // A table's rows sit on the darkest surface, as the file tree does.
    set("table.background", hex(p.darkest));
    set("table.even.background", hex_alpha(p.darkest, 0));
    set("table.head.background", hex(p.raised));
    set("table.head.foreground", hex(p.text_secondary));
    set("table.foot.background", hex(p.raised));
    set("table.foot.foreground", hex(p.text_secondary));
    set("table.hover.background", hex(p.hover));
    set("table.active.background", hex(p.selected));
    set("table.active.border", hex(p.accent));
    set("table.row.border", hex(p.line));

    set("tab.background", hex_alpha(p.raised, 0));
    set("tab.foreground", hex(p.text_secondary));
    set("tab.active.background", hex(p.base));
    set("tab.active.foreground", hex(p.text));
    set("tab_bar.background", hex(p.raised));
    set("tab_bar.segmented.background", hex(p.recessed));
    set("title_bar.background", hex(p.raised));
    set("title_bar.border", hex(p.line));
    set("status_bar.background", hex(p.raised));
    set("status_bar.border", hex(p.line));
    set("sidebar.background", hex(p.base));
    set("sidebar.foreground", hex(p.text));
    set("sidebar.border", hex(p.line));
    set("sidebar.accent.background", hex(p.hover));
    set("sidebar.accent.foreground", hex(p.text));
    set("sidebar.primary.background", hex(p.accent_fill));
    set("sidebar.primary.foreground", hex(0xffffff));

    set("scrollbar.background", hex_alpha(p.recessed, 0));
    set("scrollbar.thumb.background", hex(p.pressed));
    set("scrollbar.thumb.hover.background", hex(p.hover));
    set("switch.background", hex(p.pressed));
    set("switch.thumb.background", hex(p.text));
    set("slider.background", hex(p.accent));
    set("slider.thumb.background", hex(p.text));
    set("progress.bar.background", hex(p.accent));
    set("drag.border", hex(p.accent));
    set("drop_target.background", hex_alpha(p.accent_fill, 0x40));

    for (ix, hue) in [p.accent, p.cyan, p.purple, p.success, p.warning]
        .into_iter()
        .enumerate()
    {
        set(&format!("chart.{}", ix + 1), hex(hue));
    }
    set("chart_bullish", hex(p.success));
    set("chart_bearish", hex(p.error));
    for (name, hue) in [
        ("red", p.error),
        ("green", p.success),
        ("blue", p.accent),
        ("yellow", p.warning),
        ("magenta", p.purple),
        ("cyan", p.cyan),
    ] {
        set(&format!("base.{name}"), hex(hue));
        set(&format!("base.{name}.light"), hex(hue));
    }

    // The editor sits in the well, keeping gpui-kit's syntax colours.
    let mut highlight = serde_json::to_value(&default.highlight).unwrap_or(Value::Null);
    if let Value::Object(style) = &mut highlight {
        for (key, value) in [
            ("editor.background", hex(p.well)),
            ("editor.foreground", hex(p.text)),
            ("editor.active_line.background", hex(p.recessed)),
            ("editor.line_number", hex(p.text_tertiary)),
            ("editor.active_line_number", hex(p.text)),
        ] {
            style.insert(key.to_string(), Value::String(value));
        }
    }

    json!({
        "name": if dark { DARK_NAME } else { LIGHT_NAME },
        "mode": if dark { "dark" } else { "light" },
        "colors": colors,
        "highlight": highlight,
    })
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::{Hsla, TestAppContext};

    use super::{Bevel, DARK, LIGHT, Palette, bevel_colors, bevel_edges, color, contrast};

    /// The bevel is a pixel wide, lit along the top and left, shaded along
    /// the bottom and right, within the surface, with no edge over another;
    /// and faint on dark and light surfaces alike, its shade fainter still.
    #[test]
    fn the_bevel_is_faint_and_a_pixel_wide() {
        use gpui_kit::{Bounds, point, px, size};
        let surface = Bounds::new(point(px(10.), px(100.)), size(px(17.), px(60.)));
        for (palette, lightness) in [(&DARK, [0.18, 0.27]), (&LIGHT, [0.82, 0.91])] {
            let (light, shade) = bevel_colors(palette);
            let edges = bevel_edges(surface, Bevel::Raised, palette);
            // Pressed, the same edges, lit and shaded the other way round.
            let pressed = bevel_edges(surface, Bevel::Pressed, palette);
            for ((edge, color), (pressed_edge, pressed_color)) in edges.iter().zip(&pressed) {
                assert_eq!(edge, pressed_edge);
                assert_eq!(*pressed_color, if *color == light { shade } else { light });
            }
            assert_eq!(edges.len(), 4);
            let area: f32 = edges
                .iter()
                .map(|(edge, _)| edge.size.width.as_f32() * edge.size.height.as_f32())
                .sum();
            // Its outline, a pixel wide, counted once.
            assert_eq!(area, 2. * 17. + 2. * 58.);
            for (edge, color) in &edges {
                assert!(
                    edge.left() >= surface.left()
                        && edge.right() <= surface.right()
                        && edge.top() >= surface.top()
                        && edge.bottom() <= surface.bottom()
                );
                assert!(edge.size.width == px(1.) || edge.size.height == px(1.));
                if *color == light {
                    assert!(edge.top() == surface.top() || edge.left() == surface.left());
                } else {
                    assert!(edge.bottom() == surface.bottom() || edge.right() == surface.right());
                }
            }
            for l in lightness {
                let under = Hsla {
                    h: 0.,
                    s: 0.,
                    l,
                    a: 1.,
                };
                let lit = under.blend(light).l - l;
                let shaded = l - under.blend(shade).l;
                assert!(lit > 0. && lit < 0.1, "light {lit}");
                assert!(shaded > 0. && shaded < 0.03, "shade {shaded}");
                assert!(shaded < lit, "shade {shaded} over light {lit}");
            }
        }
        let tiny = Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.)));
        assert!(bevel_edges(tiny, Bevel::Raised, &DARK).is_empty());
    }

    /// The contrast the theme promises, in both modes.
    #[test]
    fn both_modes_keep_their_contrast() {
        for (name, p) in [("dark", DARK), ("light", LIGHT)] {
            let Palette {
                base,
                recessed,
                well,
                overlay,
                ..
            } = p;
            for text in [p.text, p.text_secondary] {
                for surface in [base, recessed, well, overlay] {
                    assert!(
                        contrast(text, surface) >= 4.5,
                        "{name}: {text:06x} on {surface:06x}"
                    );
                }
            }
            for control in [p.raised, p.hover, p.pressed, p.selected] {
                assert!(
                    contrast(p.text, control) >= 4.,
                    "{name}: text on {control:06x}"
                );
            }
            for hue in [
                p.text_tertiary,
                p.success,
                p.warning,
                p.error,
                p.accent,
                p.orange,
            ] {
                assert!(contrast(hue, base) >= 3., "{name}: {hue:06x} on the base");
            }
            assert!(
                contrast(0xffffff, p.accent_fill) >= 4.5,
                "{name}: primary buttons"
            );
            let edge = contrast(p.edge, base);
            assert!((1.4..=1.7).contains(&edge), "{name}: edge {edge}");
            assert!(
                contrast(p.line, base) < edge,
                "{name}: lines are quieter than edges"
            );
        }
    }

    /// The order of the surfaces holds in both modes: the base is the
    /// reference, pressed and selected are furthest from it that way in dark
    /// mode, hover furthest the raised way.
    #[test]
    fn surfaces_keep_their_order() {
        let lightness = |c: u32| Hsla::from(color(c)).l;
        let d = DARK;
        assert_eq!(d.base, 0x444444);
        assert!(lightness(d.well) < lightness(d.recessed));
        assert!(lightness(d.recessed) < lightness(d.base));
        assert!(lightness(d.base) < lightness(d.raised));
        assert!(lightness(d.raised) < lightness(d.hover));
        let l = LIGHT;
        assert!(lightness(l.well) > lightness(l.recessed));
        assert!(lightness(l.recessed) > lightness(l.base));
        assert!(lightness(l.selected) < lightness(l.pressed));
        assert!(lightness(l.pressed) < lightness(l.base));
    }

    /// The darkest surface is a step past the ribbon's command area, away
    /// from its tab row, as far as the tab row is from it: #111111 in dark
    /// mode, a step lighter than the command area in light mode.
    #[test]
    fn the_darkest_surface_steps_past_the_ribbon() {
        assert_eq!(DARK.darkest, 0x111111);
        for (name, p) in [("dark", DARK), ("light", LIGHT)] {
            let channel = |c: u32| (c & 0xff) as i32;
            assert_eq!(
                channel(p.ribbon) - channel(p.darkest),
                channel(p.ribbon_tabs) - channel(p.ribbon),
                "{name}: the darkest surface isn't a step from the command area"
            );
            assert!(
                contrast(p.text, p.darkest) >= 4.5,
                "{name}: text on the darkest surface"
            );
        }
    }

    /// Away from the middle grey, every step of brightness moves every grey
    /// by the same amount, so the steps between them stay as at 0; nearer,
    /// they narrow, but no grey ever passes another it was lighter or darker
    /// than, and a raised box stays lighter than the base.
    #[test]
    fn brightening_keeps_every_step_and_order() {
        use super::{
            brightened, brightness_shift, greys, mode_brightness_range, nearness_to_middle,
        };
        for (name, dark, p) in [("dark", true, DARK), ("light", false, LIGHT)] {
            let at_zero = greys(&p);
            for step in mode_brightness_range(dark) {
                let q = brightened(dark, step);
                let moved = greys(&q);
                if nearness_to_middle(dark, step) == 0. {
                    for ((grey, before), (_, after)) in at_zero.iter().zip(&moved) {
                        for at in [16, 8, 0] {
                            let channel = |c: u32| ((c >> at) & 0xff) as i32;
                            assert_eq!(
                                channel(*after) - channel(*before),
                                brightness_shift(step),
                                "{name} at {step}: {grey} didn't move with the rest"
                            );
                        }
                    }
                }
                for (a, before_a) in &at_zero {
                    for (b, before_b) in &at_zero {
                        let after = |grey: &str| moved.iter().find(|(n, _)| *n == grey).unwrap().1;
                        if before_a > before_b {
                            assert!(after(a) >= after(b), "{name} at {step}: {a} went past {b}");
                        }
                    }
                }
                assert!(q.raised > q.base, "{name} at {step}: raised isn't lighter");
                assert_eq!(
                    q.well < q.base,
                    p.well < p.base,
                    "{name} at {step}: the well"
                );
                assert_eq!(
                    q.base > q.ribbon,
                    p.base > p.ribbon,
                    "{name} at {step}: the ribbon's buttons turned"
                );
                assert_eq!(
                    q.ribbon_tabs > q.ribbon,
                    p.ribbon_tabs > p.ribbon,
                    "{name} at {step}: the ribbon's tab row turned"
                );
                assert_ne!(q.selected, q.base, "{name} at {step}: selection vanished");
            }
        }
    }

    /// The slider runs from dark mode as dark as it goes before a grey
    /// clips, up through the middle grey, #777777, into light mode, up to
    /// where a grey would clip above white; the base never jumps on the way,
    /// and no neighbouring appearances are far apart.
    #[test]
    fn brightness_runs_through_the_middle_grey() {
        use super::{
            MIDDLE_GREY, appearances, brightened, brightness_allowed, mode_brightness_range,
        };
        assert_eq!(mode_brightness_range(true), -0x11..=0x76 - 0x44);
        assert_eq!(mode_brightness_range(false), 0x77 - 0xd0..=0xff - 0xf5);
        for dark in [true, false] {
            let range = mode_brightness_range(dark);
            assert!(range.contains(&0));
            for beyond in [*range.start() - 1, *range.end() + 1] {
                assert!(!brightness_allowed(dark, beyond), "{dark} allows {beyond}");
            }
        }
        let bases: Vec<u32> = appearances()
            .iter()
            .map(|&(dark, step)| brightened(dark, step).base & 0xff)
            .collect();
        assert_eq!(bases.first(), Some(&0x33));
        assert_eq!(bases.last(), Some(&0xda));
        assert!(
            bases.contains(&MIDDLE_GREY),
            "it never reaches the middle grey"
        );
        for pair in bases.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "the base jumped");
        }
        // Every grey changes gently from one position to the next, even
        // across the change of mode.
        for pair in appearances().windows(2) {
            let (a, b) = (
                brightened(pair[0].0, pair[0].1),
                brightened(pair[1].0, pair[1].1),
            );
            for ((grey, x), (_, y)) in super::greys(&a).iter().zip(&super::greys(&b)) {
                let step = ((x & 0xff) as i32 - (y & 0xff) as i32).abs();
                assert!(step <= 36, "{grey} jumps {step} at {pair:?}");
            }
        }
    }

    /// Text keeps its contrast on the base all the way along, in both modes,
    /// through the middle grey: primary text at least 4.5 to 1, the rest as
    /// much as a colour can get. Away from the middle, it and the hues are
    /// exactly as given; nearer, they only ever move further from the base.
    #[test]
    fn text_stays_readable_through_the_middle() {
        use super::{appearances, brightened};
        for &(dark, step) in appearances().iter() {
            let p = brightened(dark, step);
            let given = if dark { DARK } else { LIGHT };
            assert!(
                contrast(p.text, p.base) >= 4.5,
                "{dark} at {step}: text {:06x} on {:06x}",
                p.text,
                p.base
            );
            let pairs = [
                (p.text, given.text),
                (p.text_secondary, given.text_secondary),
                (p.text_tertiary, given.text_tertiary),
                (p.text_disabled, given.text_disabled),
                (p.accent, given.accent),
                (p.success, given.success),
                (p.warning, given.warning),
                (p.error, given.error),
                (p.purple, given.purple),
                (p.cyan, given.cyan),
                (p.orange, given.orange),
            ];
            for (moved, was) in pairs {
                assert!(
                    contrast(moved, p.base) >= contrast(was, p.base) - 1e-3,
                    "{dark} at {step}: {was:06x} moved closer to the base"
                );
            }
            let brighter_than_given = if dark { step > 0 } else { step < 0 };
            if !brighter_than_given {
                assert_eq!(
                    pairs.map(|(a, _)| a),
                    pairs.map(|(_, b)| b),
                    "{dark} at {step}"
                );
            }
            assert_eq!(p.accent_fill, given.accent_fill);
            assert_eq!(p.dim, given.dim);
        }
        for dark in [true, false] {
            let given = if dark { DARK } else { LIGHT };
            assert_eq!(brightened(dark, 0), given, "0 isn't the palette as given");
            assert!(super::keeps_contrast(&brightened(dark, 0)));
        }
    }

    /// Setting a mode's brightness moves the palette and gpui-kit's theme at
    /// once; each mode keeps its own, starting at 0; and a setting past what
    /// the mode allows stops at its end.
    #[gpui_kit::test]
    fn each_mode_keeps_its_own_brightness(cx: &mut TestAppContext) {
        use super::{brightened, brightness, palette, set_brightness};
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(brightness(cx), 0);
            assert_eq!(set_brightness(true, 5, cx), 5);
            assert_eq!(*palette(cx), brightened(true, 5));
            let theme = Theme::global(cx);
            assert_eq!(theme.background, color(0x494949), "the base didn't move");
            assert_eq!(theme.foreground, color(DARK.text), "the text moved");

            Theme::change(ThemeMode::Light, None, cx);
            assert_eq!(brightness(cx), 0, "light mode took dark mode's");
            assert_eq!(*palette(cx), LIGHT);
            assert_eq!(set_brightness(false, -200, cx), -0x59, "past the end");
            assert_eq!(Theme::global(cx).background, color(0x777777));

            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(brightness(cx), 5, "dark mode lost its own");
            assert_eq!(Theme::global(cx).background, color(0x494949));
            assert_eq!(set_brightness(true, 500, cx), 0x32, "past the end");
            assert_eq!(set_brightness(true, 0, cx), 0);
            assert_eq!(Theme::global(cx).background, color(DARK.base));
        });
    }

    /// A base chosen for a mode lays its greys out from it, each as far from
    /// it in every channel as from the theme's own; the brightness range
    /// follows the new base, and the mode's brightness is kept within it;
    /// the other mode is left as it was, and putting the theme's own base
    /// back puts everything back.
    #[gpui_kit::test]
    fn a_chosen_base_moves_the_greys(cx: &mut TestAppContext) {
        use super::{
            CustomColors, ModeColors, appearances, brightness, custom_colors,
            mode_brightness_range, palette, reference, set_brightness, set_custom_colors,
        };
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let (range, count) = (mode_brightness_range(true), appearances().len());
            assert_eq!(set_brightness(true, *range.start(), cx), -0x11);

            let base = 0x383c44;
            let chosen = CustomColors {
                dark: ModeColors {
                    base: Some(base),
                    ..Default::default()
                },
                ..Default::default()
            };
            set_custom_colors(chosen, cx);
            assert_eq!(custom_colors(), chosen);
            let moved = reference(true);
            assert_eq!(moved.base, base);
            assert_eq!(moved.well, 0x22262e, "the well didn't keep its step");
            assert_eq!(moved.text, DARK.text, "the text moved");
            assert_eq!(reference(false), LIGHT, "light mode moved too");
            // Its red is 0x0c darker, so the darkest grey's red, 0x05 now,
            // can only go that far before it clips.
            let range = mode_brightness_range(true);
            assert_eq!(*range.start(), -0x05);
            assert_eq!(brightness(cx), *range.start(), "kept past the range");
            assert_ne!(appearances().len(), count);
            let base = super::brightened(true, brightness(cx)).base;
            assert_eq!(*palette(cx), super::brightened(true, brightness(cx)));
            assert_eq!(Theme::global(cx).background, color(base));

            set_custom_colors(CustomColors::default(), cx);
            assert_eq!(reference(true), DARK);
            assert_eq!(appearances().len(), count);
            set_brightness(true, 0, cx);
            assert_eq!(Theme::global(cx).background, color(DARK.base));
        });
    }

    /// gpui-kit's theme takes its colours from the palette in each mode.
    #[gpui_kit::test]
    fn gpui_kit_follows_the_palette(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            for (mode, p) in [(ThemeMode::Dark, DARK), (ThemeMode::Light, LIGHT)] {
                Theme::change(mode, None, cx);
                let theme = Theme::global(cx);
                // gpui-kit lays some colours on at its own opacity.
                let same = |actual: Hsla, expected: u32, name: &str| {
                    let expected = color(expected);
                    assert!(
                        (actual.h - expected.h).abs() < 1e-3
                            && (actual.s - expected.s).abs() < 1e-3
                            && (actual.l - expected.l).abs() < 1e-3,
                        "{mode:?} {name}: {actual:?} isn't {expected:?}"
                    );
                };
                same(theme.background, p.base, "background");
                same(theme.foreground, p.text, "foreground");
                same(theme.muted_foreground, p.text_secondary, "muted_foreground");
                same(theme.border, p.edge, "border");
                same(theme.popover, p.overlay, "popover");
                same(theme.list_hover, p.hover, "list_hover");
                same(theme.list_active, p.selected, "list_active");
                // A table's rows are on the file tree's darkest surface.
                same(*theme.tokens.table, p.darkest, "table");
                same(theme.primary, p.accent_fill, "primary");
                same(theme.ring, p.accent, "ring");
                same(theme.danger, p.error, "danger");
                same(theme.success, p.success, "success");
                same(theme.warning, p.warning, "warning");
                same(theme.tab_bar, p.raised, "tab_bar");
                same(theme.red, p.error, "red");
                same(theme.blue, p.accent, "blue");
            }
        });
    }
}
