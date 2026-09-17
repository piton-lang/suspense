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
    /// How much black dims the window behind a modal surface.
    pub dim: f32,
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
    dim: 0.4,
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
    dim: 0.25,
};

/// How much of the accent fill selected text is laid over with.
const SELECTION_ALPHA: u8 = 0x59;

pub const DARK_NAME: &str = "Suspense Dark";
pub const LIGHT_NAME: &str = "Suspense Light";

/// The palette of the mode showing.
pub fn palette(cx: &App) -> &'static Palette {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
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

    use super::{DARK, LIGHT, Palette, color};

    /// WCAG's contrast ratio between two opaque colours.
    fn contrast(a: u32, b: u32) -> f32 {
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
            for hue in [p.text_tertiary, p.success, p.warning, p.error, p.accent] {
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
