//! What every sidebar's panels share: a panel with a title has a header, 32
//! pixels tall on the darkest surface, with no line between it and its body,
//! and a single thin line in the theme's line colour lies between one panel
//! and the next, unless the next panel's own change of colour is the edge.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex};
use gpui_kit::*;

use crate::theme;

/// How tall a panel's header is.
pub const HEADER_HEIGHT: Pixels = px(32.);

/// How far a panel's title, and its rows, sit in from its edges.
pub const PADDING: Pixels = px(8.);

/// A panel's header: its title in semibold regular text, 8 pixels from the
/// left, on the darkest surface, as the file tree is, with no count. Along
/// its bottom, inside its height, a 1 pixel border in `body`, the colour of
/// the body of the panel it heads, so it reads as no line.
pub fn header(title: &'static str, body: Hsla, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .w_full()
        .h(HEADER_HEIGHT)
        .px(PADDING)
        .bg(theme::color(theme::palette(cx).darkest))
        .border_b_1()
        .border_color(body)
        .text_color(cx.theme().foreground)
        .text_sm()
        .font_semibold()
        .child(div().min_w_0().truncate().child(title))
}

/// The line between one panel and the next.
pub fn divider(cx: &App) -> Div {
    div()
        .flex_none()
        .w_full()
        .h(px(1.))
        .bg(cx.theme().sidebar_border)
}

/// What lies between one panel and the `next`, when the next is shown: the
/// line, unless the next panel's own change of colour is the edge.
pub fn between(next_shown: bool, next_colour_is_edge: bool, cx: &App) -> Option<Div> {
    (next_shown && !next_colour_is_edge).then(|| divider(cx))
}

#[cfg(test)]
mod tests {
    /// A line lies between two panels, unless the next says its own change of
    /// colour is the edge; a panel not shown leaves none.
    #[gpui_kit::test]
    fn a_line_lies_between_panels_unless_colour_is_the_edge(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            assert!(super::between(true, false, cx).is_some());
            assert!(super::between(true, true, cx).is_none());
            assert!(super::between(false, false, cx).is_none());
            assert!(
                super::between(true, crate::git_panel::COLOUR_IS_EDGE, cx).is_none(),
                "a line lies above the git panel"
            );
        });
    }
}
