//! The popover by selected text: a small row of buttons floating just below
//! and to the right of where the drag that selected the text ended, above
//! everything. What the buttons are is up to whatever shows it, such as the
//! editor or an answer. Pressing one is taken by the popover alone, never by
//! the text beneath, so the selection is still there when it acts; pressing
//! the mouse anywhere else closes it.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::*;

/// How far from where the drag ended the popover shows.
const OFFSET: Point<Pixels> = point(px(6.), px(10.));

/// Something a popover's button does with the selected text.
pub struct SelectionAction {
    /// Its button is named `{name}-{key}` in the popover `name`.
    pub key: &'static str,
    pub icon: IconName,
    pub label: &'static str,
    pub disabled: bool,
    pub on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

impl SelectionAction {
    pub fn new(
        key: &'static str,
        icon: IconName,
        label: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            key,
            icon,
            label,
            disabled: false,
            on_click: Box::new(on_click),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// The popover `name`, named `{name}-popover`, at `position`, where the drag
/// that selected the text ended, with a button for each of `actions`, in
/// order. `dismiss` is called when the mouse is pressed outside it.
pub fn selection_popover(
    name: &str,
    position: Point<Pixels>,
    actions: Vec<SelectionAction>,
    dismiss: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let popover = h_flex()
        .id(SharedString::from(format!("{name}-popover")))
        // What's beneath doesn't take the press, which would clear or start
        // a selection before a button acts on it.
        .occlude()
        .gap_1()
        .p_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .shadow_md()
        .on_mouse_down_out(move |_, window, cx| dismiss(window, cx))
        .children(actions.into_iter().map(|action| {
            Button::new(SharedString::from(format!("{name}-{}", action.key)))
                .ghost()
                .small()
                .icon(action.icon)
                .label(action.label)
                .disabled(action.disabled)
                .on_click(action.on_click)
        }));
    // Lets UI tests find the popover; inert in normal builds.
    let popover = gpui_kit::TestSupportExt::test_support(popover);
    deferred(
        anchored()
            .position(position + OFFSET)
            .snap_to_window()
            .child(popover),
    )
    .with_priority(1)
    .into_any_element()
}
