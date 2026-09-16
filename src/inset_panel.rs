//! A floating panel over the whole main window, inset from its edges, with the
//! window dimmed behind it. It is modal: clicking the dimmed window closes it,
//! nothing beneath takes the mouse, and <Tab> cycles focus within it. Keeping
//! focus from going beneath otherwise is up to whatever shows it.
//!
//! It comes in and goes away with the rise-in animation (see
//! [`crate::animations::rise_in`]): the panel is its surface, and the dimmed
//! window its dimming.

use gpui_kit::component::{ActiveTheme as _, FocusTrapElement as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::animations::rise_in::{self, Direction};

/// How far the panel is inset from each edge of the window.
pub const INSET: Pixels = px(32.);

/// The panel's surface, holding `content`, filling its place.
fn surface(content: impl IntoElement, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .shadow_2xl()
        .child(content)
}

/// `content` in a panel whose elements are named `{name}-panel` and
/// `{name}-backdrop`, tracking `focus` and trapping <Tab> within it, and
/// calling `close` when the dimmed window around it is clicked. `opened`
/// counts the times a panel has opened, so each comes in afresh.
pub fn inset_panel(
    name: &str,
    focus: &FocusHandle,
    content: impl IntoElement,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    opened: usize,
    cx: &App,
) -> AnyElement {
    // Focus is tracked and trapped within what the panel holds.
    let content = div()
        .size_full()
        .track_focus(focus)
        .child(content)
        .focus_trap(SharedString::from(format!("{name}-trap")), focus);
    let moving = surface(content, cx)
        .id(SharedString::from(format!("{name}-panel-surface")))
        // Lets UI tests find the surface as it moves; inert in normal builds.
        .map(gpui_kit::TestSupportExt::test_support);
    // Where the panel settles, which it moves within as it comes in.
    let panel = div()
        .id(SharedString::from(format!("{name}-panel")))
        .absolute()
        .inset(INSET)
        // Clicks inside stay inside, rather than reaching the backdrop.
        .occlude()
        .child(rise_in::surface(
            moving,
            "inset-panel",
            opened,
            Direction::In,
        ));
    // Lets UI tests find the panel; inert in normal builds.
    let panel = gpui_kit::TestSupportExt::test_support(panel);
    let backdrop = div()
        .id(SharedString::from(format!("{name}-backdrop")))
        .absolute()
        .inset_0()
        // Nothing beneath takes the mouse while the panel is open.
        .occlude()
        .on_click(close)
        .child(rise_in::dimming("inset-panel", opened, Direction::In))
        .child(panel);
    // Lets UI tests find the panel and backdrop; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(backdrop).into_any_element()
}

/// A panel closing: `content` going away, taking neither the mouse nor focus.
/// `closed` counts the times a panel has closed, so each goes away afresh.
pub fn closing_panel(content: AnyView, closed: usize, cx: &App) -> AnyElement {
    let panel = rise_in::surface(
        surface(div().size_full().child(content), cx),
        "inset-panel",
        closed,
        Direction::Out,
    );
    div()
        .absolute()
        .inset_0()
        .child(rise_in::dimming("inset-panel", closed, Direction::Out))
        .child(div().absolute().inset(INSET).child(panel))
        .into_any_element()
}
