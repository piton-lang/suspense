//! A floating panel over the whole main window, inset from its edges, with the
//! window dimmed behind it. It is modal: clicking the dimmed window closes it,
//! nothing beneath takes the mouse, and <Tab> cycles focus within it. Keeping
//! focus from going beneath otherwise is up to whatever shows it.

use gpui_kit::component::{ActiveTheme as _, FocusTrapElement as _};
use gpui_kit::*;

/// How far the panel is inset from each edge of the window.
pub const INSET: Pixels = px(32.);

/// `content` in a panel whose elements are named `{name}-panel` and
/// `{name}-backdrop`, tracking `focus` and trapping <Tab> within it, and
/// calling `close` when the dimmed window around it is clicked.
pub fn inset_panel(
    name: &str,
    focus: &FocusHandle,
    content: impl IntoElement,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let panel = div()
        .id(SharedString::from(format!("{name}-panel")))
        .absolute()
        .inset(INSET)
        // Clicks inside stay inside, rather than reaching the backdrop.
        .occlude()
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .shadow_2xl()
        // Focus is tracked and trapped within what the panel holds.
        .child(
            div()
                .size_full()
                .track_focus(focus)
                .child(content)
                .focus_trap(SharedString::from(format!("{name}-trap")), focus),
        );
    // Lets UI tests find the panel; inert in normal builds.
    let panel = gpui_kit::TestSupportExt::test_support(panel);
    let backdrop = div()
        .id(SharedString::from(format!("{name}-backdrop")))
        .absolute()
        .inset_0()
        // Nothing beneath takes the mouse while the panel is open.
        .occlude()
        .bg(black().opacity(0.4))
        .on_click(close)
        .child(panel);
    // Lets UI tests find the panel and backdrop; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(backdrop).into_any_element()
}
