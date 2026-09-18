//! What every sidebar's panels share: a panel with a title has a header on
//! the theme's raised surface, a small step apart from its body on the base
//! surface, with no line between them, and a single thin line in the theme's
//! line colour lies between one panel and the next.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex};
use gpui_kit::*;

use crate::theme;

/// A panel's header: its title, and, for a counted panel, how many rows it
/// lists, muted, at its right.
pub fn header(title: &'static str, count: Option<usize>, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .justify_between()
        .gap_2()
        .px_3()
        .py_2()
        .bg(theme::color(theme::palette(cx).raised))
        .text_sm()
        .font_semibold()
        .child(title)
        .children(count.map(|count| {
            div()
                .text_xs()
                .font_normal()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string())
        }))
}

/// The line between one panel and the next.
pub fn divider(cx: &App) -> Div {
    div()
        .flex_none()
        .w_full()
        .h(px(1.))
        .bg(cx.theme().sidebar_border)
}
