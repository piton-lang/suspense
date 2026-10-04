//! The ribbon's Application tab, for the application rather than the project:
//! the Brightness slider, which runs the interface from its darkest to its
//! lightest, Reset Brightness beneath it, Theme, which opens the theme editor, Settings, which opens
//! the settings window, Welcome, which opens the welcome page, and
//! Walkthrough, which starts the walkthrough, Check for Updates, which
//! checks for a newer Suspense, and About, which shows which Suspense is
//! running.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, Size, Theme, ThemeMode, WindowExt as _,
    h_flex,
};
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize, Ribbon};
use crate::about::OpenAbout;
use crate::settings_window::OpenSettings;
use crate::theme_editor::OpenThemeEditor;
use crate::self_update::Updates;
use crate::walkthrough::OpenWalkthrough;
use crate::welcome::OpenWelcome;
use crate::{theme, theme_preference};

pub(super) const COMMANDS: &[CommandPlace] = &[
    // The one appearance control, so the collapsed ribbon keeps it.
    CommandPlace {
        command: Command::Brightness,
        group: "Appearance",
        size: CommandSize::Slim,
        primary: true,
    },
    // Beneath the slider, in its column; the collapsed ribbon resets it by
    // double-clicking the slider instead.
    CommandPlace {
        command: Command::ResetBrightness,
        group: "Appearance",
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::Theme,
        group: "Colours",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::Settings,
        group: "Preferences",
        size: CommandSize::Full,
        primary: true,
    },
    // Last; the collapsed ribbon leaves it out.
    CommandPlace {
        command: Command::Welcome,
        group: "Help",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::Walkthrough,
        group: "Help",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::CheckForUpdates,
        group: "Help",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::About,
        group: "Help",
        size: CommandSize::Full,
        primary: false,
    },
];

/// How wide the Brightness slider's track is.
const BRIGHTNESS_SLIDER_WIDTH: Pixels = px(120.);

/// The Brightness slider's state: one track from the darkest appearance to
/// the lightest, dark mode's steps then light mode's, with no switch between
/// them.
pub(super) struct BrightnessSlider {
    /// The appearance the thumb was last put on or dragged to.
    shown: usize,
    /// How many appearances the track runs through, which a mode's base
    /// colour changes.
    count: usize,
    state: Entity<SliderState>,
    _subscription: Subscription,
}

#[cfg(test)]
impl BrightnessSlider {
    /// Where the thumb is, as an index into the appearances.
    pub(super) fn shown(&self, cx: &App) -> f32 {
        self.state.read(cx).value().end()
    }
}

/// Where the appearance showing falls on the slider.
fn current_position(cx: &App) -> usize {
    theme::appearance_index(cx.theme().is_dark(), theme::brightness(cx))
}

/// Makes the Brightness slider's state once, and afterwards moves its thumb
/// to the appearance showing when that was changed some other way, leaving
/// a drag under way alone.
pub(super) fn sync_brightness(ribbon: &mut Ribbon, window: &mut Window, cx: &mut Context<Ribbon>) {
    let position = current_position(cx);
    let count = theme::appearances().len();
    // A new base for a mode gives the track new steps: made afresh for them.
    if ribbon
        .brightness
        .as_ref()
        .is_some_and(|slider| slider.count != count)
    {
        ribbon.brightness = None;
    }
    if let Some(slider) = ribbon.brightness.as_mut() {
        if slider.shown != position {
            slider.shown = position;
            slider
                .state
                .update(cx, |state, cx| state.set_value(position as f32, window, cx));
        }
        return;
    }
    let last = (count - 1) as f32;
    let state = cx.new(|_| {
        SliderState::new()
            .min(0.)
            .max(last)
            .step(1.)
            .default_value(position as f32)
    });
    let subscription = cx.subscribe_in(
        &state,
        window,
        move |ribbon, state, event: &SliderEvent, window, cx| {
            if let SliderEvent::Change(_) = event {
                // Straight away as it moves. Where the thumb is now, rather
                // than where the event says: a double-click may have reset it
                // since.
                let value = state.read(cx).value().end();
                let index = (value.round().max(0.) as usize).min(theme::appearances().len() - 1);
                show_appearance(index, window, cx);
                if let Some(slider) = ribbon.brightness.as_mut() {
                    slider.shown = index;
                }
                cx.notify();
            } else {
                // Saved once let go.
                save_appearance(window, cx);
            }
        },
    );
    ribbon.brightness = Some(BrightnessSlider {
        shown: position,
        count,
        state,
        _subscription: subscription,
    });
}

/// Shows the appearance at `index`: its mode, at its brightness.
fn show_appearance(index: usize, window: &mut Window, cx: &mut App) {
    let (dark, step) = theme::appearances()[index];
    theme::set_brightness(dark, step, cx);
    if cx.theme().is_dark() != dark {
        let mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        Theme::change(mode, Some(window), cx);
        cx.refresh_windows();
    }
}

/// Saves the appearance showing, with a notification if it can't be.
fn save_appearance(window: &mut Window, cx: &mut App) {
    if let Err(err) = theme_preference::save_appearance(cx) {
        let note = Notification::error(format!("{err:#}")).title("Could not save brightness");
        window.push_notification(note, cx);
    }
}

/// Puts the mode showing back to its brightness at 0, and saves it.
pub(super) fn reset_brightness(ribbon: &mut Ribbon, window: &mut Window, cx: &mut Context<Ribbon>) {
    let dark = cx.theme().is_dark();
    theme::set_brightness(dark, 0, cx);
    let position = current_position(cx);
    let count = theme::appearances().len();
    // A new base for a mode gives the track new steps: made afresh for them.
    if ribbon
        .brightness
        .as_ref()
        .is_some_and(|slider| slider.count != count)
    {
        ribbon.brightness = None;
    }
    if let Some(slider) = ribbon.brightness.as_mut() {
        slider.shown = position;
        slider
            .state
            .update(cx, |state, cx| state.set_value(position as f32, window, cx));
    }
    save_appearance(window, cx);
    cx.notify();
}

/// Brightness: a moon, the slider, and a sun, with no background of its own,
/// as tall as a slim button. Double-clicking anywhere on it puts the mode
/// showing back to its brightness at 0. Lets UI tests find it and its parts;
/// inert in normal builds.
pub(super) fn brightness(
    ribbon: &Ribbon,
    size: CommandSize,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let Some(slider) = ribbon.brightness.as_ref() else {
        return Empty.into_any_element();
    };
    let block = gpui_kit::TestSupportExt::test_support(h_flex().id("brightness"))
        .items_center()
        .gap_2()
        .px_2()
        .tooltip(|window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(
                "Makes the whole interface darker or lighter, from its darkest to its \
                 lightest, keeping its contrast. Double-click to reset it.",
            )
            .build(window, cx)
        })
        // Before the slider sees it, so the second click doesn't move it.
        .capture_any_mouse_down(cx.listener(|ribbon, event: &MouseDownEvent, window, cx| {
            if event.button == MouseButton::Left && event.click_count >= 2 {
                cx.stop_propagation();
                reset_brightness(ribbon, window, cx);
            }
        }))
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id("brightness-darkest")
                .flex_none()
                .child(Icon::new(IconName::Moon).with_size(Size::Small)),
        ))
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id("brightness-slider")
                .flex_none()
                .w(BRIGHTNESS_SLIDER_WIDTH)
                .child(Slider::new(&slider.state).horizontal()),
        ))
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id("brightness-lightest")
                .flex_none()
                .child(Icon::new(IconName::Sun).with_size(Size::Small)),
        ));
    match size {
        CommandSize::Slim => block.h(super::SLIM_HEIGHT).w_full(),
        CommandSize::Small => block.h(super::SMALL_HEIGHT).flex_none(),
        CommandSize::Full => block.py_1(),
    }
    .into_any_element()
}

/// Reset Brightness: puts the mode showing back to its brightness at 0, as
/// double-clicking the slider does; disabled while it already is.
pub(super) fn reset_brightness_button(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let at_default = theme::brightness(cx) == 0;
    button(
        "reset-brightness",
        IconName::RotateCcw,
        "Reset Brightness".into(),
    )
    .disabled(at_default)
    .tooltip(if at_default {
        "The brightness is already where this mode starts"
    } else {
        "Put this mode's brightness back where it starts, keeping the mode"
    })
    .on_click(cx.listener(|ribbon, _, window, cx| reset_brightness(ribbon, window, cx)))
    .into_any_element()
}

/// Theme: opens the theme editor in the inset panel.
pub(super) fn theme(button: impl Fn(&'static str, IconName, SharedString) -> Button) -> AnyElement {
    button("theme", IconName::Palette, "Theme".into())
        .tooltip("Choose the base colour, and the colours of Code, Chain, Spec, and Ask")
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenThemeEditor), cx))
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

/// Welcome: opens the welcome page in the inset panel, running its checks
/// anew.
pub(super) fn welcome(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button("welcome", IconName::ListChecks, "Welcome".into())
        .tooltip("Check that Suspense is ready to run")
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenWelcome), cx))
        .into_any_element()
}

/// Walkthrough: starts the walkthrough from its first step.
pub(super) fn walkthrough(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button("walkthrough", IconName::Signpost, "Walkthrough".into())
        .tooltip("Walk through creating a project and the prompt modes")
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenWalkthrough), cx))
        .into_any_element()
}

/// Check for Updates: checks for a newer Suspense straight away, saying how
/// it went in a notification; a spinner and disabled while a check or
/// download runs, and disabled, saying why, in a build that can't update.
pub(super) fn check_for_updates(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &App,
) -> AnyElement {
    let (enabled, tooltip) = Updates::ribbon_state(cx);
    button("check-for-updates", IconName::RefreshCw, "Check for Updates".into())
        .tooltip(tooltip)
        .loading(Updates::busy(cx))
        .disabled(!enabled)
        .on_click(|_, _, cx| Updates::check_from_ribbon(cx))
        .into_any_element()
}

/// About: shows which version of Suspense is running, and its build.
pub(super) fn about(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
) -> AnyElement {
    button("about", IconName::Info, "About".into())
        .tooltip("Show the version and build of Suspense")
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenAbout), cx))
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
