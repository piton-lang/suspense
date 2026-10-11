//! What every sidebar's panels share: a panel with a title has a header, 32
//! pixels tall on the darkest surface, with no line between it and its body.

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
