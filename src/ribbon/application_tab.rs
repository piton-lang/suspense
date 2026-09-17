//! The ribbon's Application tab, for the application rather than the project:
//! the Dark mode switch, and Settings, which opens the settings window.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize, Ribbon};
use crate::settings_window::OpenSettings;
use crate::theme_preference;

pub(super) const COMMANDS: &[CommandPlace] = &[
    CommandPlace {
        command: Command::DarkMode,
        group: "Appearance",
        size: CommandSize::Slim,
        primary: true,
    },
    CommandPlace {
        command: Command::Settings,
        group: "Preferences",
        size: CommandSize::Full,
        primary: true,
    },
];

/// Dark mode: a switch rather than a button, since it is on or off, sitting in
/// a block the size and colour of a button of `size`, square.
pub(super) fn dark_mode(
    size: CommandSize,
    background: Hsla,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let switch = Switch::new("dark-mode")
        .label("Dark mode")
        .checked(cx.theme().is_dark())
        .on_click(|dark, window, cx| set_dark_mode(*dark, window, cx));
    let block = h_flex().items_center().px_2().bg(background);
    match size {
        CommandSize::Slim => block.h(super::SLIM_HEIGHT).w_full().child(switch.small()),
        CommandSize::Full => block.h_full().child(switch),
    }
    .into_any_element()
}

/// Settings: opens the settings in the inset panel.
pub(super) fn settings(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button("settings", IconName::Settings, "Settings".into())
        .tooltip_with_action(
            "Edit the system prompt each chat tab sends",
            &OpenSettings,
            None,
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx))
        .into_any_element()
}

/// Switches between light and dark mode, saving the choice.
pub fn set_dark_mode(dark: bool, window: &mut Window, cx: &mut App) {
    let mode = if dark {
        gpui_kit::component::ThemeMode::Dark
    } else {
        gpui_kit::component::ThemeMode::Light
    };
    if let Err(err) = theme_preference::set(mode, window, cx) {
        let note = Notification::error(format!("{err:#}")).title("Could not save dark mode");
        window.push_notification(note, cx);
    }
}
