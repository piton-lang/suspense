//! A permanent scrollbar for a scrolling list, as a narrow column beside it:
//! a square button at the top that scrolls up, a track whose thumb shows how
//! much of the list is in view and can be dragged or clicked to, and a square
//! button at the bottom that scrolls down. Lists that follow new output, like
//! a task's, add a button below that locks the scroll to the bottom.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// How wide the column is, which is also each button's width and height.
pub const COLUMN_WIDTH: Pixels = px(18.);

/// How far a button scrolls the list.
const STEP: Pixels = px(48.);

/// The shortest the thumb gets, so it can always be grabbed.
const MIN_THUMB: Pixels = px(16.);

/// Called when the lock to the bottom is switched.
pub type ToggleLock = Rc<dyn Fn(&mut Window, &mut App)>;

/// `content`, which scrolls with `handle`, beside its scroll column. With
/// `fill`, it fills the space it is given; otherwise it is as tall as
/// `content`, which is expected to cap its own height. With `lock`, whether
/// the scroll is locked to the bottom and how to switch it, the column ends in
/// a button for that.
pub fn with_scroll_column(
    id: impl Into<SharedString>,
    handle: &ScrollHandle,
    content: impl IntoElement,
    fill: bool,
    lock: Option<(bool, ToggleLock)>,
    cx: &App,
) -> AnyElement {
    let row = h_flex()
        .items_stretch()
        .w_full()
        .when(fill, |row| row.flex_1().min_h_0().h_full())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(fill, |it| it.h_full())
                .child(content),
        )
        .child(scroll_column(id.into(), handle, lock, cx));
    row.into_any_element()
}

/// The column itself.
fn scroll_column(
    id: SharedString,
    handle: &ScrollHandle,
    lock: Option<(bool, ToggleLock)>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (border, thumb_color, track_color) = (theme.border, theme.muted_foreground, theme.tab_bar);
    let square = |name: &str, icon: IconName| {
        Button::new(SharedString::from(format!("{id}-{name}")))
            .ghost()
            .xsmall()
            .icon(icon)
            .w(COLUMN_WIDTH)
            .h(COLUMN_WIDTH)
            .rounded_none()
    };
    let scroll_by = |delta: Pixels| {
        let handle = handle.clone();
        move |_: &ClickEvent, window: &mut Window, _: &mut App| {
            let offset = handle.offset();
            let max = handle.max_offset().y;
            let y = (offset.y + delta).min(px(0.)).max(-max);
            handle.set_offset(point(offset.x, y));
            window.refresh();
        }
    };

    // Where on the thumb it was grabbed, while it is being dragged.
    let grab: Rc<Cell<Option<Pixels>>> = Rc::default();
    let track = canvas(|_, _, _| {}, {
        let handle = handle.clone();
        move |bounds, _, window, _| {
            let geometry = Geometry::of(&handle, bounds);
            window.paint_quad(fill(bounds, track_color));
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(bounds.origin.x + px(4.), geometry.thumb_top),
                        size(bounds.size.width - px(8.), geometry.thumb_height),
                    ),
                    thumb_color.opacity(0.5),
                )
                .corner_radii(px(3.)),
            );

            window.on_mouse_event({
                let (handle, grab) = (handle.clone(), grab.clone());
                move |event: &MouseDownEvent, phase, window, _| {
                    if phase != DispatchPhase::Bubble || !bounds.contains(&event.position) {
                        return;
                    }
                    let geometry = Geometry::of(&handle, bounds);
                    let y = event.position.y;
                    // Grabbed where it was pressed; pressed off the thumb, the
                    // thumb first jumps to centre on the press.
                    let at = if y >= geometry.thumb_top
                        && y <= geometry.thumb_top + geometry.thumb_height
                    {
                        y - geometry.thumb_top
                    } else {
                        geometry.thumb_height / 2.
                    };
                    grab.set(Some(at));
                    geometry.scroll_thumb_to(&handle, y - at);
                    window.refresh();
                }
            });
            window.on_mouse_event({
                let (handle, grab) = (handle.clone(), grab.clone());
                move |event: &MouseMoveEvent, _, window, _| {
                    if let Some(at) = grab.get() {
                        if !event.dragging() {
                            grab.set(None);
                            return;
                        }
                        Geometry::of(&handle, bounds)
                            .scroll_thumb_to(&handle, event.position.y - at);
                        window.refresh();
                    }
                }
            });
            window.on_mouse_event({
                let grab = grab.clone();
                move |_: &MouseUpEvent, _, _, _| grab.set(None)
            });
        }
    })
    .size_full();

    let column = v_flex()
        .id(SharedString::from(format!("{id}-scroll-column")))
        .flex_none()
        .w(COLUMN_WIDTH)
        .border_l_1()
        .border_color(border)
        .child(
            square("scroll-up", IconName::ChevronUp)
                .tooltip("Scroll up")
                .on_click(scroll_by(STEP)),
        )
        .child(div().flex_1().min_h(px(8.)).child(track))
        .child(
            square("scroll-down", IconName::ChevronDown)
                .tooltip("Scroll down")
                .on_click(scroll_by(-STEP)),
        )
        .when_some(lock, |column, (locked, toggle)| {
            column.child(
                square("scroll-lock", IconName::ArrowDownToLine)
                    .selected(locked)
                    .tooltip(if locked {
                        "Unlock the scroll from the bottom"
                    } else {
                        "Lock the scroll to the bottom"
                    })
                    .on_click(move |_, window, cx| toggle(window, cx))
                    .border_t_1()
                    .border_color(border),
            )
        });
    // Lets UI tests find the column; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(column).into_any_element()
}

/// Where the thumb sits in a track, for a list scrolled as `handle` is.
struct Geometry {
    track: Bounds<Pixels>,
    thumb_top: Pixels,
    thumb_height: Pixels,
    /// How far the list scrolls.
    max: Pixels,
}

impl Geometry {
    fn of(handle: &ScrollHandle, track: Bounds<Pixels>) -> Self {
        let viewport = handle.bounds().size.height;
        let max = handle.max_offset().y.max(px(0.));
        let content = viewport + max;
        let height = track.size.height;
        // All of the list in view fills the track.
        let thumb_height = if content <= px(0.) {
            height
        } else {
            (height * (viewport / content)).max(MIN_THUMB).min(height)
        };
        let scrolled = (-handle.offset().y).max(px(0.)).min(max);
        let room = height - thumb_height;
        let thumb_top = if max > px(0.) {
            track.origin.y + room * (scrolled / max)
        } else {
            track.origin.y
        };
        Self {
            track,
            thumb_top,
            thumb_height,
            max,
        }
    }

    /// Scrolls the list so the thumb's top is at `top`.
    fn scroll_thumb_to(&self, handle: &ScrollHandle, top: Pixels) {
        let room = self.track.size.height - self.thumb_height;
        if room <= px(0.) || self.max <= px(0.) {
            return;
        }
        let fraction = ((top - self.track.origin.y) / room).clamp(0., 1.);
        let offset = handle.offset();
        handle.set_offset(point(offset.x, -(self.max * fraction)));
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, ScrollHandle, point, px, size};

    use super::{Geometry, MIN_THUMB};

    /// With nothing to scroll, the thumb fills the track; otherwise it is as
    /// tall as the share of the list in view, and never too short to grab.
    #[test]
    fn thumb_fills_the_track_until_there_is_something_to_scroll() {
        let track = Bounds::new(point(px(0.), px(100.)), size(px(18.), px(200.)));
        let geometry = Geometry::of(&ScrollHandle::new(), track);
        assert_eq!(geometry.thumb_top, px(100.));
        assert_eq!(geometry.thumb_height, px(200.));
        assert!(MIN_THUMB < px(200.));
    }
}
