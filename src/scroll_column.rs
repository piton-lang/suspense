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

/// Locks the scroll to the bottom (`true`) or unlocks it (`false`).
pub type SetLock = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// `content`, which scrolls with `handle`, beside its scroll column. With
/// `fill`, it fills the space it is given; otherwise it is as tall as
/// `content`, which is expected to cap its own height. With `lock`, whether
/// the scroll is locked to the bottom and how to switch it, the column ends in
/// a button for that; scrolling the list by hand, with the wheel or the
/// column, locks it when that leaves the list at the bottom and breaks the lock
/// when it doesn't.
pub fn with_scroll_column(
    id: impl Into<SharedString>,
    handle: &ScrollHandle,
    content: impl IntoElement,
    fill: bool,
    lock: Option<(bool, SetLock)>,
    cx: &App,
) -> AnyElement {
    let follow = follower(handle, &lock);
    let row = h_flex()
        .items_stretch()
        .w_full()
        .when(fill, |row| row.flex_1().min_h_0().h_full())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(fill, |it| it.h_full())
                // Scrolling by hand locks or unlocks the scroll. This bubbles
                // up from the list, which has already taken the scroll.
                .when_some(follow, |content, follow| {
                    content.on_scroll_wheel(move |_, window, cx| follow(window, cx))
                })
                .child(content),
        )
        .child(scroll_column(id.into(), handle, lock, cx));
    row.into_any_element()
}

/// After the list is scrolled by hand: locks the scroll when the list is left
/// at the bottom, and unlocks it when it isn't. A list with nothing to scroll
/// keeps its lock as it is.
type Follow = Rc<dyn Fn(&mut Window, &mut App)>;

fn follower(handle: &ScrollHandle, lock: &Option<(bool, SetLock)>) -> Option<Follow> {
    let (_, set_lock) = lock.as_ref()?;
    let (handle, set_lock) = (handle.clone(), set_lock.clone());
    Some(Rc::new(move |window, cx| {
        let max = handle.max_offset().y;
        if max > px(0.) {
            set_lock(at_bottom(&handle), window, cx);
        }
    }))
}

/// Whether the list is scrolled to its bottom, or past it before it is
/// clamped.
fn at_bottom(handle: &ScrollHandle) -> bool {
    handle.offset().y <= -handle.max_offset().y + px(1.)
}

/// The column itself.
fn scroll_column(
    id: SharedString,
    handle: &ScrollHandle,
    lock: Option<(bool, SetLock)>,
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
    let follow = follower(handle, &lock);
    let scroll_by = |delta: Pixels| {
        let handle = handle.clone();
        let follow = follow.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let offset = handle.offset();
            let max = handle.max_offset().y;
            let y = (offset.y + delta).min(px(0.)).max(-max);
            handle.set_offset(point(offset.x, y));
            if let Some(follow) = &follow {
                follow(window, cx);
            }
            window.refresh();
        }
    };

    let track = canvas(
        // Where on the thumb it was grabbed, while it is being dragged. Kept
        // with the track from frame to frame, since each mouse move redraws
        // the column.
        |_, window, cx| {
            window.use_keyed_state("scroll-grab", cx, |_, _| Rc::new(Cell::new(None::<Pixels>)))
        },
        {
            let handle = handle.clone();
            let follow = follow.clone();
            move |bounds, grab: Entity<Rc<Cell<Option<Pixels>>>>, window, cx| {
                let grab = grab.read(cx).clone();
                let geometry = Geometry::of(&handle, bounds);
                window.paint_quad(fill(bounds, track_color));
                // The thumb fills the track's width, square.
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x, geometry.thumb_top),
                        size(bounds.size.width, geometry.thumb_height),
                    ),
                    thumb_color.opacity(0.5),
                ));

                window.on_mouse_event({
                    let (handle, grab, follow) = (handle.clone(), grab.clone(), follow.clone());
                    move |event: &MouseDownEvent, phase, window, cx| {
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
                        if let Some(follow) = &follow {
                            follow(window, cx);
                        }
                        window.refresh();
                    }
                });
                window.on_mouse_event({
                    let (handle, grab, follow) = (handle.clone(), grab.clone(), follow.clone());
                    move |event: &MouseMoveEvent, _, window, cx| {
                        if let Some(at) = grab.get() {
                            if !event.dragging() {
                                grab.set(None);
                                return;
                            }
                            Geometry::of(&handle, bounds)
                                .scroll_thumb_to(&handle, event.position.y - at);
                            if let Some(follow) = &follow {
                                follow(window, cx);
                            }
                            window.refresh();
                        }
                    }
                });
                window.on_mouse_event({
                    let grab = grab.clone();
                    move |_: &MouseUpEvent, _, _, _| grab.set(None)
                });
            }
        },
    )
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
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id(SharedString::from(format!("{id}-scroll-track")))
                .flex_1()
                .min_h(px(8.))
                .child(track),
        ))
        .child(
            square("scroll-down", IconName::ChevronDown)
                .tooltip("Scroll down")
                .on_click(scroll_by(-STEP)),
        )
        .when_some(lock, |column, (locked, set_lock)| {
            column.child(
                square("scroll-lock", IconName::ArrowDownToLine)
                    .selected(locked)
                    .tooltip(if locked {
                        "Unlock the scroll from the bottom"
                    } else {
                        "Lock the scroll to the bottom"
                    })
                    .on_click(move |_, window, cx| set_lock(!locked, window, cx))
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
    use gpui_kit::component::{Root, v_flex};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement, ParentElement as _,
        Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext, Window,
        div, point, px, size,
    };

    use super::{Geometry, MIN_THUMB, with_scroll_column};

    struct TallList {
        scroll: ScrollHandle,
    }

    impl Render for TallList {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let list = div()
                .id("tall")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(
                    v_flex()
                        .children((0..200).map(|ix| div().h(px(20.)).child(format!("row {ix}")))),
                );
            div().size_full().child(with_scroll_column(
                "tall",
                &self.scroll,
                list,
                true,
                None,
                cx,
            ))
        }
    }

    /// Dragging the thumb down the track scrolls the list with it, all the way
    /// to the bottom when dragged there.
    #[gpui_kit::test]
    async fn dragging_the_thumb_scrolls_the_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let scroll = ScrollHandle::new();
        let window = cx.add_window({
            let scroll = scroll.clone();
            |window, cx| {
                let view = cx.new(|_| TallList { scroll });
                Root::new(view, window, cx)
            }
        });
        let handle = window.into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let track = window.find("tall-scroll-track").bounds();
            let from = point(track.center().x, track.top() + px(4.));
            let to = point(track.center().x, track.bottom() + px(40.));
            window.drag(from, to, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let (offset, max) = (scroll.offset().y, scroll.max_offset().y);
        assert!(max > px(0.), "nothing to scroll");
        assert!(
            (offset + max).abs() <= px(1.),
            "dragging the thumb to the bottom left the list at {offset:?} of {max:?}"
        );
    }

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
