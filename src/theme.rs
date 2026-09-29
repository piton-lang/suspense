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

/// The palette of the mode showing, at that mode's brightness.
pub fn palette(cx: &App) -> &'static Palette {
    let dark = cx.theme().is_dark();
    shifted(dark, brightness_of(dark, cx))
}

/// How far the brightness setting runs either way from 0, before what a mode
/// allows narrows it.
pub const BRIGHTNESS_LIMIT: i32 = 10;

/// How far, in each channel, `step` moves every grey: 2% of their lightness a
/// step, rounded half away from 0 to a whole channel value, so every grey
/// moves by exactly the same amount and the steps between them keep their
/// size.
pub fn brightness_shift(step: i32) -> i32 {
    // 2% of 255 is 5.1: in tenths, 51 a step, kept in integers so the
    // rounding is exact and the same either way.
    let tenths = step * 51;
    (tenths + 5 * tenths.signum()) / 10
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

/// `color`'s red, green, and blue, each moved by `shift`, clipped to black
/// and white.
fn shift_channels(color: u32, shift: i32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |out, at| {
        let channel = ((color >> at) & 0xff) as i32;
        out | (((channel + shift).clamp(0, 255) as u32) << at)
    })
}

/// `palette` at brightness `step`: every neutral grey moved by the same
/// amount of lightness, everything else as it is. Within the mode's range no
/// grey clips, so the steps between them and their order stay as at 0.
pub fn brightened(palette: &Palette, step: i32) -> Palette {
    let shift = brightness_shift(step);
    let mut p = *palette;
    for grey in greys_mut(&mut p) {
        *grey = shift_channels(*grey, shift);
    }
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

/// Whether `palette` can be moved to brightness `step`: within the setting's
/// ends, no grey pushed below black or above white, and every text keeping
/// its contrast.
pub fn brightness_allowed(palette: &Palette, step: i32) -> bool {
    let shift = brightness_shift(step);
    let clips = greys(palette).into_iter().any(|(_, grey)| {
        [16, 8, 0].into_iter().any(|at| {
            let channel = ((grey >> at) & 0xff) as i32 + shift;
            !(0..=255).contains(&channel)
        })
    });
    step.abs() <= BRIGHTNESS_LIMIT && !clips && keeps_contrast(&brightened(palette, step))
}

/// How far `palette`'s brightness can go either way: from 0 out to the last
/// step allowed before one isn't.
pub fn brightness_range(palette: &Palette) -> std::ops::RangeInclusive<i32> {
    let reach = |direction: i32| {
        let mut step = 0;
        while brightness_allowed(palette, step + direction) {
            step += direction;
        }
        step
    };
    reach(-1)..=reach(1)
}

/// The brightness range of dark or light mode, worked out once.
pub fn mode_brightness_range(dark: bool) -> std::ops::RangeInclusive<i32> {
    static RANGES: std::sync::LazyLock<[std::ops::RangeInclusive<i32>; 2]> =
        std::sync::LazyLock::new(|| [brightness_range(&LIGHT), brightness_range(&DARK)]);
    RANGES[dark as usize].clone()
}

/// Every appearance the Brightness slider runs through, darkest first: dark
/// mode from its darkest step to its lightest, then light mode from its
/// darkest to its lightest, each as whether it is dark mode and its step.
pub fn appearances() -> &'static [(bool, i32)] {
    static ALL: std::sync::LazyLock<Vec<(bool, i32)>> = std::sync::LazyLock::new(|| {
        let dark = mode_brightness_range(true).map(|step| (true, step));
        let light = mode_brightness_range(false).map(|step| (false, step));
        dark.chain(light).collect()
    });
    &ALL
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

/// Dark or light mode's palette at brightness `step`, from every step worked
/// out once, so the palette showing can still be borrowed for the life of
/// the application.
fn shifted(dark: bool, step: i32) -> &'static Palette {
    static PALETTES: std::sync::LazyLock<[Vec<Palette>; 2]> = std::sync::LazyLock::new(|| {
        let every = |palette: &Palette| {
            (-BRIGHTNESS_LIMIT..=BRIGHTNESS_LIMIT)
                .map(|step| brightened(palette, step))
                .collect()
        };
        [every(&LIGHT), every(&DARK)]
    });
    let step = step.clamp(-BRIGHTNESS_LIMIT, BRIGHTNESS_LIMIT);
    &PALETTES[dark as usize][(step + BRIGHTNESS_LIMIT) as usize]
}

/// Each mode's brightness, and the gpui-kit themes built for the steps taken
/// so far, so dragging back and forth doesn't build them again.
#[derive(Default)]
struct Brightness {
    dark: i32,
    light: i32,
    configs: std::collections::HashMap<(bool, i32), Rc<ThemeConfig>>,
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
    let cached = cx
        .try_global::<Brightness>()
        .and_then(|b| b.configs.get(&(dark, step)).cloned());
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
                .insert((dark, step), config.clone());
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
    set("table.background", hex(p.well));
    set("table.even.background", hex_alpha(p.well, 0));
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

    /// Every step of brightness a mode allows moves every grey by the same
    /// amount: the difference between any two greys, in every channel, is
    /// what it is at 0, so their order and the steps between them never
    /// change, and a lighter box on a darker one stays lighter.
    #[test]
    fn brightening_keeps_every_step_and_order() {
        use super::{brightened, brightness_range, brightness_shift, greys};
        for (name, p) in [("dark", DARK), ("light", LIGHT)] {
            let range = brightness_range(&p);
            assert!(range.contains(&0), "{name}: 0 is out of range");
            let at_zero = greys(&p);
            for step in range {
                let moved = greys(&brightened(&p, step));
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
                for (a, before_a) in &at_zero {
                    for (b, before_b) in &at_zero {
                        let after = |grey: &str| moved.iter().find(|(n, _)| *n == grey).unwrap().1;
                        let lightness = |c: u32| Hsla::from(color(c)).l;
                        assert_eq!(
                            (after(a) & 0xff) as i32 - (after(b) & 0xff) as i32,
                            (*before_a & 0xff) as i32 - (*before_b & 0xff) as i32,
                            "{name} at {step}: the step from {b} to {a} changed"
                        );
                        assert_eq!(
                            lightness(*before_a).total_cmp(&lightness(*before_b)),
                            lightness(after(a)).total_cmp(&lightness(after(b))),
                            "{name} at {step}: {a} and {b} changed order"
                        );
                    }
                }
            }
            // A raised box stays lighter than the base, and a well on the
            // side of it it is at 0; the ribbon's base-coloured buttons stay
            // on their side of its command area.
            for step in brightness_range(&p) {
                let q = brightened(&p, step);
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
            }
        }
        // A step is 2% of the lightness, 5.1 of 255, rounded half away from 0.
        assert_eq!(
            (-10..=10).map(brightness_shift).collect::<Vec<_>>(),
            [
                -51, -46, -41, -36, -31, -26, -20, -15, -10, -5, 0, 5, 10, 15, 20, 26, 31, 36, 41,
                46, 51
            ]
        );
    }

    /// The range each mode allows is worked out, not written down: it runs
    /// from 0 out to the last step at which no grey clips below black or
    /// above white and every text keeps the contrast the theme asks of it on
    /// every surface; the next step either way, within -10 to +10, breaks
    /// one of them.
    #[test]
    fn brightness_stops_before_clipping_or_losing_contrast() {
        use super::{
            BRIGHTNESS_LIMIT, brightened, brightness_allowed, brightness_range, brightness_shift,
            greys, keeps_contrast, mode_brightness_range,
        };
        let clips = |p: &Palette, step: i32| {
            greys(p).into_iter().any(|(_, grey)| {
                let v = (grey & 0xff) as i32 + brightness_shift(step);
                !(0..=255).contains(&v)
            })
        };
        let contrast_holds = |p: &Palette| {
            [p.text, p.text_secondary].into_iter().all(|text| {
                [p.base, p.recessed, p.well, p.overlay]
                    .into_iter()
                    .all(|surface| contrast(text, surface) >= 4.5)
            }) && [p.raised, p.hover, p.pressed, p.selected]
                .into_iter()
                .all(|control| contrast(p.text, control) >= 4.)
                && [
                    p.text_tertiary,
                    p.success,
                    p.warning,
                    p.error,
                    p.accent,
                    p.orange,
                ]
                .into_iter()
                .all(|hue| contrast(hue, p.base) >= 3.)
        };
        for (name, dark, p, expected) in [
            ("dark", true, DARK, -3..=1),
            ("light", false, LIGHT, -4..=2),
        ] {
            let range = brightness_range(&p);
            assert_eq!(range, expected, "{name}'s range");
            assert_eq!(mode_brightness_range(dark), range);
            for step in range.clone() {
                let q = brightened(&p, step);
                assert!(!clips(&p, step), "{name} at {step} clips");
                assert!(
                    contrast_holds(&q) && keeps_contrast(&q),
                    "{name} at {step} loses contrast"
                );
            }
            for beyond in [*range.start() - 1, *range.end() + 1] {
                assert!(!brightness_allowed(&p, beyond), "{name} allows {beyond}");
                if beyond.abs() <= BRIGHTNESS_LIMIT {
                    assert!(
                        clips(&p, beyond) || !contrast_holds(&brightened(&p, beyond)),
                        "{name} stops at {beyond} for no reason"
                    );
                }
            }
        }
        // Dark mode's darkest grey would go below black first; its secondary
        // text would lose its contrast first the other way. Light mode's well
        // would go past white first; its tertiary text the other way.
        assert!(clips(&DARK, -4) && !contrast_holds(&brightened(&DARK, 2)));
        assert!(clips(&LIGHT, 3) && !contrast_holds(&brightened(&LIGHT, -5)));
    }

    /// Brightness moves only the greys: text, the accent, the status and
    /// other hues, and the strengths of what is laid over the surfaces are
    /// the same at every step.
    #[test]
    fn brightness_leaves_text_and_hues_alone() {
        use super::{BRIGHTNESS_LIMIT, brightened};
        for p in [DARK, LIGHT] {
            for step in -BRIGHTNESS_LIMIT..=BRIGHTNESS_LIMIT {
                let q = brightened(&p, step);
                let kept = |p: &Palette| {
                    (
                        [
                            p.text,
                            p.text_secondary,
                            p.text_tertiary,
                            p.text_disabled,
                            p.accent,
                            p.accent_fill,
                            p.success,
                            p.warning,
                            p.error,
                            p.purple,
                            p.cyan,
                            p.orange,
                        ],
                        [p.dim, p.bevel_light, p.bevel_shade],
                    )
                };
                assert_eq!(kept(&q), kept(&p), "at {step}");
            }
            assert_eq!(brightened(&p, 0), p, "0 isn't the palette as given");
        }
    }

    /// Setting a mode's brightness moves the palette and gpui-kit's theme at
    /// once, its text as it was; each mode keeps its own, starting at 0; and
    /// a setting past what the mode allows stops at its end.
    #[gpui_kit::test]
    fn each_mode_keeps_its_own_brightness(cx: &mut TestAppContext) {
        use super::{brightened, brightness, palette, set_brightness};
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(brightness(cx), 0);
            assert_eq!(set_brightness(true, 1, cx), 1);
            assert_eq!(*palette(cx), brightened(&DARK, 1));
            let theme = Theme::global(cx);
            assert_eq!(theme.background, color(0x494949), "the base didn't move");
            assert_eq!(theme.foreground, color(DARK.text), "the text moved");

            Theme::change(ThemeMode::Light, None, cx);
            assert_eq!(brightness(cx), 0, "light mode took dark mode's");
            assert_eq!(*palette(cx), LIGHT);
            assert_eq!(set_brightness(false, -10, cx), -4, "past the end");
            assert_eq!(Theme::global(cx).background, color(LIGHT.base - 0x141414));

            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(brightness(cx), 1, "dark mode lost its own");
            assert_eq!(Theme::global(cx).background, color(0x494949));
            assert_eq!(set_brightness(true, 5, cx), 1, "past the end");
            assert_eq!(set_brightness(true, 0, cx), 0);
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
