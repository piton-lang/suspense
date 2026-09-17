//! A permanent scrollbar for a scrolling list, as a narrow column beside it:
//! a square button at the top that scrolls up, a track whose thumb shows how
//! much of the list is in view and can be dragged or clicked to, and a square
//! button at the bottom that scrolls down. Lists that follow new output, like
//! a task's, add a button below that locks the scroll to the bottom.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

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

/// How long the pulse lasts when the scroll locks.
const PULSE_TIME: Duration = Duration::from_millis(900);

/// What a scroll column scrolls: an element tracking a [`ScrollHandle`], or a
/// virtualized [`list`] with its [`ListState`].
#[derive(Clone)]
pub enum Scroll {
    Handle(ScrollHandle),
    List(ListState),
}

impl From<&ScrollHandle> for Scroll {
    fn from(handle: &ScrollHandle) -> Self {
        Scroll::Handle(handle.clone())
    }
}

impl From<&ListState> for Scroll {
    fn from(state: &ListState) -> Self {
        Scroll::List(state.clone())
    }
}

impl Scroll {
    /// How far it is scrolled, negative going down.
    pub fn offset(&self) -> Point<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.offset(),
            Scroll::List(state) => state.scroll_px_offset_for_scrollbar(),
        }
    }

    /// How far it can scroll.
    pub fn max_offset(&self) -> Point<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.max_offset(),
            Scroll::List(state) => state.max_offset_for_scrollbar(),
        }
    }

    pub fn set_offset(&self, offset: Point<Pixels>) {
        match self {
            Scroll::Handle(handle) => handle.set_offset(offset),
            Scroll::List(state) => state.set_offset_from_scrollbar(offset),
        }
    }

    /// The part of it in view.
    fn viewport(&self) -> Bounds<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.bounds(),
            Scroll::List(state) => state.viewport_bounds(),
        }
    }
}

/// A virtualized list's state for a list with a scroll column, measuring every
/// row. Left to itself, a list counts a row it hasn't laid out as no height at
/// all, so how far it scrolls, and the thumb with it, would jump as rows come
/// into view. This way, its next layout measures every row not yet measured,
/// once each: rows already measured keep their height, so it costs only the
/// rows that are new or changed, or every row once after the width changes.
/// The list does so again by itself after a reset, a remeasure, or a change of
/// width, but not after rows are spliced in: call [`measure_new_rows`] then.
pub fn measured_list(alignment: ListAlignment, overdraw: Pixels) -> ListState {
    ListState::new(0, alignment, overdraw).measure_all()
}

/// Has a [`measured_list`] measure the rows just spliced into it.
pub fn measure_new_rows(state: &ListState) {
    state.clone().measure_all();
}

/// The track's fill, and the thumb's under the pointer and while dragged. The
/// track is black laid over whatever the column sits on, so it is darker than
/// what is beneath it on any surface. The thumb at rest has no fill at all, so
/// it is the colour of that surface, as the buttons are, and white is laid over
/// it as it is hovered and grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollColors {
    pub track: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
}

pub fn scroll_colors(dark: bool) -> ScrollColors {
    let (black, white) = (hsla(0., 0., 0., 1.), hsla(0., 0., 1., 1.));
    if dark {
        ScrollColors {
            track: black.opacity(0.4),
            hover: white.opacity(0.035),
            pressed: white.opacity(0.07),
        }
    } else {
        ScrollColors {
            track: black.opacity(0.16),
            hover: white.opacity(0.18),
            pressed: white.opacity(0.35),
        }
    }
}

/// Locks the scroll to the bottom (`true`) or unlocks it (`false`).
pub type SetLock = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// `content`, which scrolls with `handle`, beside its scroll column. With
/// `fill`, it fills the space it is given; otherwise it is as tall as
/// `content`, which is expected to cap its own height. With `lock`, whether
/// the scroll is locked to the bottom and how to switch it, the column ends in
/// a button for that; scrolling the list by hand, with the wheel or the
/// column, locks it when that leaves the list at the bottom and breaks the lock
/// when it doesn't.
pub fn with_scrollbar(
    id: impl Into<SharedString>,
    handle: impl Into<Scroll>,
    content: impl IntoElement,
    fill: bool,
    lock: Option<(bool, SetLock)>,
    cx: &App,
) -> AnyElement {
    let handle: Scroll = handle.into();
    let follow = follower(&handle, &lock);
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
        .child(scrollbar(id.into(), &handle, lock, cx));
    row.into_any_element()
}

/// After the list is scrolled by hand: locks the scroll when the list is left
/// at the bottom, and unlocks it when it isn't. A list with nothing to scroll
/// keeps its lock as it is.
type Follow = Rc<dyn Fn(&mut Window, &mut App)>;

fn follower(handle: &Scroll, lock: &Option<(bool, SetLock)>) -> Option<Follow> {
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
fn at_bottom(handle: &Scroll) -> bool {
    handle.offset().y <= -handle.max_offset().y + px(1.)
}

/// The column itself.
fn scrollbar(
    id: SharedString,
    handle: &Scroll,
    lock: Option<(bool, SetLock)>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let border = theme.border;
    let colors = scroll_colors(theme.is_dark());
    let palette = *crate::theme::palette(cx);
    // A square button filling the track inside its line, with no lines of its
    // own; see [`in_track`].
    let square = |name: &str, icon: IconName| {
        Button::new(SharedString::from(format!("{id}-{name}")))
            .ghost()
            .xsmall()
            .icon(icon)
            .w(COLUMN_WIDTH - TRACK_LINE)
            .h(COLUMN_WIDTH - TRACK_LINE)
            .rounded_none()
    };
    // `button` sitting just inside the track, as the thumb does: the track's
    // line runs down beside it, and a pixel of the dark track lies between it
    // and the part of the track toward `gap_above` or below it.
    let in_track = |button: AnyElement, gap_above: bool| {
        let gap = || div().flex_none().h(TRACK_LINE).w_full().bg(colors.track);
        h_flex()
            .w(COLUMN_WIDTH)
            .h(COLUMN_WIDTH)
            .child(div().w(TRACK_LINE).h_full().flex_none().bg(border))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .when(gap_above, |slot| slot.child(gap()))
                    .child(button)
                    .when(!gap_above, |slot| slot.child(gap())),
            )
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
            let grab = window
                .use_keyed_state("scroll-grab", cx, |_, _| Rc::new(Cell::new(None::<Pixels>)));
            // Whether the pointer was over the thumb when last drawn.
            let hovered =
                window.use_keyed_state("scroll-thumb-hover", cx, |_, _| Rc::new(Cell::new(false)));
            (grab, hovered)
        },
        {
            let handle = handle.clone();
            let follow = follow.clone();
            move |bounds,
                  (grab, hovered): (Entity<Rc<Cell<Option<Pixels>>>>, Entity<Rc<Cell<bool>>>),
                  window,
                  cx| {
                let grab = grab.read(cx).clone();
                let hovered = hovered.read(cx).clone();
                let geometry = Geometry::of(&handle, bounds);
                let thumb = thumb_bounds(&geometry, bounds);
                // The track is dark above and below the thumb, inside its line,
                // while the thumb is left the colour of what the column sits on.
                let inside = bounds.origin.x + TRACK_LINE;
                for (top, bottom) in [
                    (bounds.top(), thumb.top()),
                    (thumb.bottom(), bounds.bottom()),
                ] {
                    if bottom > top {
                        window.paint_quad(fill(
                            Bounds::new(point(inside, top), size(thumb.size.width, bottom - top)),
                            colors.track,
                        ));
                    }
                }
                // The line beside the list runs the track's full height, the
                // thumb never covering it.
                window.paint_quad(fill(
                    Bounds::new(bounds.origin, size(TRACK_LINE, bounds.size.height)),
                    border,
                ));
                // The thumb fills the track inside its line, square, with no
                // lines of its own, lightening while hovered or dragged.
                let is_hovered = thumb.contains(&window.mouse_position());
                hovered.set(is_hovered);
                if grab.get().is_some() {
                    window.paint_quad(fill(thumb, colors.pressed));
                } else if is_hovered {
                    window.paint_quad(fill(thumb, colors.hover));
                }
                // The theme's faint bevel.
                for (edge, color) in
                    crate::theme::bevel_edges(thumb, crate::theme::Bevel::Raised, &palette)
                {
                    window.paint_quad(fill(edge, color));
                }
                // Moving on or off the thumb redraws it.
                window.on_mouse_event({
                    let (handle, hovered) = (handle.clone(), hovered.clone());
                    move |event: &MouseMoveEvent, _, window, _| {
                        let geometry = Geometry::of(&handle, bounds);
                        let thumb = thumb_bounds(&geometry, bounds);
                        if thumb.contains(&event.position) != hovered.get() {
                            window.refresh();
                        }
                    }
                });

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
        .child(in_track(
            bevelled(
                format!("{id}-scroll-up"),
                square("scroll-up", IconName::ChevronUp)
                    .tooltip("Scroll up")
                    .on_click(scroll_by(STEP)),
            )
            .into_any_element(),
            false,
        ))
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id(SharedString::from(format!("{id}-scroll-track")))
                .flex_1()
                .min_h(px(8.))
                .child(track),
        ))
        .child(in_track(
            bevelled(
                format!("{id}-scroll-down"),
                square("scroll-down", IconName::ChevronDown)
                    .tooltip("Scroll down")
                    .on_click(scroll_by(-STEP)),
            )
            .into_any_element(),
            true,
        ))
        .when_some(lock, |column, (locked, set_lock)| {
            let button = square("scroll-lock", IconName::ArrowDownToLine)
                .selected(locked)
                .tooltip(if locked {
                    "Unlock the scroll from the bottom"
                } else {
                    "Lock the scroll to the bottom"
                })
                .on_click(move |_, window, cx| set_lock(!locked, window, cx));
            column.child(in_track(
                div()
                    .id(SharedString::from(format!("{id}-scroll-lock-pulse")))
                    .relative()
                    .child(pulse(locked, cx))
                    .child(button)
                    .into_any_element(),
                true,
            ))
        });
    // Lets UI tests find the column; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(column).into_any_element()
}

/// `button`, with the theme's bevel while hovered, and its pressed bevel while
/// pressed with the pointer still over it. The bevel takes no mouse: it
/// watches the mouse before the button does.
fn bevelled(id: String, button: impl IntoElement) -> impl IntoElement {
    use crate::theme::{Bevel, bevel_edges, palette};
    let key = SharedString::from(format!("{id}-bevel"));
    let overlay = canvas(
        move |_, window, cx| {
            // Whether the button was pressed, and not yet let go.
            window.use_keyed_state(key, cx, |_, _| Rc::new(Cell::new(false)))
        },
        move |bounds, pressed: Entity<Rc<Cell<bool>>>, window, cx| {
            let pressed = pressed.read(cx).clone();
            let hovered = bounds.contains(&window.mouse_position());
            let bevel = match (hovered, pressed.get()) {
                (true, true) => Some(Bevel::Pressed),
                (true, false) => Some(Bevel::Raised),
                (false, _) => None,
            };
            if let Some(bevel) = bevel {
                for (edge, color) in bevel_edges(bounds, bevel, palette(cx)) {
                    window.paint_quad(fill(edge, color));
                }
            }
            window.on_mouse_event({
                let pressed = pressed.clone();
                move |event: &MouseDownEvent, phase, window, _| {
                    if phase == DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && bounds.contains(&event.position)
                    {
                        pressed.set(true);
                        window.refresh();
                    }
                }
            });
            window.on_mouse_event({
                let pressed = pressed.clone();
                move |_: &MouseUpEvent, phase, window, _| {
                    if phase == DispatchPhase::Capture && pressed.replace(false) {
                        window.refresh();
                    }
                }
            });
            // Moving on or off the button redraws it.
            window.on_mouse_event(move |event: &MouseMoveEvent, _, window, _| {
                if bounds.contains(&event.position) != hovered {
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    div().relative().child(button).child(overlay)
}

/// The glowing pulse that emanates from the lock button when the scroll
/// locks: a soft glow that blooms and fades, and two thin rings that ripple
/// out, the second just after the first. Drawn behind the button, over
/// whatever is beside the column, and never taking the mouse.
fn pulse(locked: bool, cx: &App) -> impl IntoElement {
    let color = cx.theme().ring;
    canvas(
        move |_, window, cx| {
            let state =
                window.use_keyed_state("pulse", cx, |_, _| Rc::new(Cell::new(Pulse::default())));
            let mut pulse = state.read(cx).get();
            let elapsed = pulse.observe(locked, Instant::now());
            state.read(cx).set(pulse);
            if elapsed.is_some() {
                window.request_animation_frame();
            }
            elapsed
        },
        move |bounds, elapsed, window, _| {
            let Some(elapsed) = elapsed else {
                return;
            };
            let frame = PulseFrame::at(elapsed);
            let center = bounds.center();
            let side = bounds.size.width.min(bounds.size.height);
            if frame.glow > 0. {
                window.paint_drop_shadows(
                    bounds,
                    (side / 2.).into(),
                    &[BoxShadow {
                        color: color.opacity(0.6 * frame.glow),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(10.),
                        spread_radius: px(2.) * frame.glow,
                        inset: false,
                    }],
                );
            }
            for ring in frame.rings {
                if ring.opacity <= 0. {
                    continue;
                }
                let diameter = side * ring.scale;
                let bounds = Bounds::new(
                    point(center.x - diameter / 2., center.y - diameter / 2.),
                    size(diameter, diameter),
                );
                window.paint_quad(quad(
                    bounds,
                    diameter / 2.,
                    transparent_black(),
                    px(ring.width),
                    color.opacity(ring.opacity),
                    BorderStyle::Solid,
                ));
            }
        },
    )
    .absolute()
    .inset_0()
}

/// Whether the lock button is pulsing, from how the scroll was locked in the
/// frames before.
#[derive(Clone, Copy, Debug, Default)]
struct Pulse {
    /// Whether the scroll was locked last frame; `None` before the first.
    locked: Option<bool>,
    started: Option<Instant>,
}

impl Pulse {
    /// Notes whether the scroll is locked `now`, returning how far into the
    /// pulse it is while one is under way. Locking starts one; a list first
    /// shown locked doesn't, and unlocking stops one.
    fn observe(&mut self, locked: bool, now: Instant) -> Option<Duration> {
        if locked && self.locked == Some(false) {
            self.started = Some(now);
        }
        if !locked {
            self.started = None;
        }
        self.locked = Some(locked);
        let elapsed = self
            .started
            .map(|started| now.saturating_duration_since(started));
        let elapsed = elapsed.filter(|elapsed| *elapsed < PULSE_TIME);
        if elapsed.is_none() {
            self.started = None;
        }
        elapsed
    }
}

/// One frame of the pulse.
#[derive(Debug, PartialEq)]
struct PulseFrame {
    /// How strong the glow is, from 0 to 1.
    glow: f32,
    rings: [Ring; 2],
}

/// One of the pulse's rings, sized against the button.
#[derive(Debug, PartialEq)]
struct Ring {
    scale: f32,
    opacity: f32,
    width: f32,
}

impl PulseFrame {
    /// How long the glow takes to bloom, and then to fade.
    const BLOOM: f32 = 0.12;
    const FADE: f32 = 0.5;
    /// When each ring sets out, and how long it takes to spread.
    const RING_DELAYS: [f32; 2] = [0., 0.16];
    const RING_TIME: f32 = 0.7;
    /// How large a ring grows, against the button.
    const RING_SCALE: f32 = 2.8;

    fn at(elapsed: Duration) -> Self {
        let t = elapsed.as_secs_f32();
        let glow = if t < Self::BLOOM {
            ease_out_cubic(t / Self::BLOOM)
        } else {
            1. - ease_in_out(((t - Self::BLOOM) / Self::FADE).min(1.))
        };
        let rings = Self::RING_DELAYS.map(|delay| {
            let progress = ((t - delay) / Self::RING_TIME).clamp(0., 1.);
            if t < delay {
                return Ring {
                    scale: 1.,
                    opacity: 0.,
                    width: 0.,
                };
            }
            let spread = ease_out_quint()(progress);
            let left = 1. - progress;
            Ring {
                scale: 1. + (Self::RING_SCALE - 1.) * spread,
                opacity: 0.7 * left * left,
                width: 0.75 + 1.25 * left,
            }
        });
        Self {
            glow: glow.clamp(0., 1.),
            rings,
        }
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// The width of the line down the track's side against the list.
const TRACK_LINE: Pixels = px(1.);

/// The thumb in a track at `track`: inside the track's line, never over it.
fn thumb_bounds(geometry: &Geometry, track: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(track.origin.x + TRACK_LINE, geometry.thumb_top),
        size(
            (track.size.width - TRACK_LINE).max(px(0.)),
            geometry.thumb_height,
        ),
    )
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
    fn of(handle: &Scroll, track: Bounds<Pixels>) -> Self {
        let viewport = handle.viewport().size.height;
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
    fn scroll_thumb_to(&self, handle: &Scroll, top: Pixels) {
        let room = self.track.size.height - self.thumb_height;
        if room <= px(0.) || self.max <= px(0.) {
            return;
        }
        let fraction = ((top - self.track.origin.y) / room).clamp(0., 1.);
        let offset = handle.offset();
        handle.set_offset(point(offset.x, -(self.max * fraction)));
    }
}

/// Where the thumb is drawn in a track at `track`, as its top and height.
#[cfg(test)]
pub fn thumb_for_test(handle: &Scroll, track: Bounds<Pixels>) -> (Pixels, Pixels) {
    let geometry = Geometry::of(handle, track);
    (geometry.thumb_top, geometry.thumb_height)
}

#[cfg(test)]
mod thumb_tests {
    use gpui_kit::{Hsla, hsla};

    /// Laid over dark and light surfaces alike, the track is clearly darker
    /// than what is beneath it without losing the surface's character, and
    /// the thumb, the surface itself at rest, lightens hovered, and lightens
    /// again while dragged.
    #[test]
    fn track_is_dark_and_thumb_lightens_as_it_is_used() {
        let over = |surface: Hsla, color: Hsla| surface.blend(color);
        for (dark, surfaces) in [
            (true, [0.18, 0.22, 0.27, 0.31]),
            (false, [0.74, 0.82, 0.91, 0.96]),
        ] {
            let colors = super::scroll_colors(dark);
            for color in [colors.track, colors.hover, colors.pressed] {
                assert!(color.a < 1., "laid over, not opaque");
            }
            for l in surfaces {
                let surface = hsla(0., 0., l, 1.);
                let track = over(surface, colors.track);
                let hover = over(surface, colors.hover);
                let pressed = over(surface, colors.pressed);
                assert!(
                    (0.05..0.25).contains(&(surface.l - track.l)),
                    "{track:?} isn't clearly but modestly darker than {surface:?}"
                );
                assert!(
                    hover.l > surface.l && pressed.l > hover.l,
                    "{surface:?} {hover:?} {pressed:?}"
                );
                assert!(pressed.l - surface.l < surface.l - track.l);
            }
        }
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

    use std::time::{Duration, Instant};

    use super::{Geometry, MIN_THUMB, PULSE_TIME, Pulse, PulseFrame, with_scrollbar};

    /// Locking starts a pulse, which runs out; a list first shown locked
    /// doesn't pulse, and unlocking stops one under way.
    #[test]
    fn locking_pulses_once() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);

        let mut shown_locked = Pulse::default();
        assert_eq!(shown_locked.observe(true, at(0)), None);
        assert_eq!(shown_locked.observe(true, at(16)), None);

        let mut pulse = Pulse::default();
        assert_eq!(pulse.observe(false, at(0)), None);
        assert_eq!(pulse.observe(true, at(100)), Some(Duration::ZERO));
        assert_eq!(
            pulse.observe(true, at(400)),
            Some(Duration::from_millis(300))
        );
        assert_eq!(pulse.observe(true, at(100) + PULSE_TIME), None);
        assert_eq!(pulse.observe(true, at(2000)), None, "it pulsed again");

        let mut stopped = Pulse::default();
        stopped.observe(false, at(0));
        assert!(stopped.observe(true, at(10)).is_some());
        assert_eq!(stopped.observe(false, at(50)), None);
        assert_eq!(stopped.observe(false, at(60)), None);
    }

    /// The glow blooms then fades; each ring spreads from the button's size
    /// as it fades and thins, the second setting out after the first; and
    /// all of it is gone by the end.
    #[test]
    fn pulse_blooms_ripples_and_fades() {
        let frame = |ms: u64| PulseFrame::at(Duration::from_millis(ms));
        assert_eq!(frame(0).glow, 0.);
        assert!(frame(120).glow > 0.99);
        assert!(frame(400).glow < frame(200).glow);
        assert!(
            frame(100).rings[1].opacity == 0.,
            "the second ring set out with the first"
        );

        let mut last = frame(0).rings[0].scale;
        assert!((last - 1.).abs() < 1e-3);
        for ms in (50..=850).step_by(50) {
            let ring = &frame(ms).rings[0];
            assert!(ring.scale >= last, "the ring shrank at {ms}ms");
            last = ring.scale;
        }
        assert!(frame(300).rings[0].opacity < frame(50).rings[0].opacity);
        assert!(frame(300).rings[0].width < frame(50).rings[0].width);

        let end = PulseFrame::at(PULSE_TIME);
        assert_eq!(end.glow, 0.);
        assert!(end.rings.iter().all(|ring| ring.opacity < 0.01), "{end:?}");
    }

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
            div()
                .size_full()
                .child(with_scrollbar("tall", &self.scroll, list, true, None, cx))
        }
    }

    /// The buttons sit just inside the track, as the thumb does: the track's
    /// line runs down beside them, unbroken the column's full height, and a
    /// pixel of the dark track lies between each and the track, drawn by
    /// nothing over the line.
    #[gpui_kit::test]
    async fn buttons_sit_just_inside_the_track(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| TallList {
                scroll: ScrollHandle::new(),
            });
            Root::new(view, window, cx)
        });
        let handle = window.into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let column = window.find("tall-scroll-column").bounds();
            let track = window.find("tall-scroll-track").bounds();
            let up = window.find("tall-scroll-up").bounds();
            let down = window.find("tall-scroll-down").bounds();
            for button in [up, down] {
                assert_eq!(
                    button.left(),
                    column.left() + px(1.),
                    "{button:?} covers the line"
                );
                assert_eq!(button.right(), column.right());
            }
            assert_eq!(
                track.top() - up.bottom(),
                px(1.),
                "no gap under the up button"
            );
            assert_eq!(
                down.top() - track.bottom(),
                px(1.),
                "no gap over the down button"
            );
            // The line beside the list, from the top of the column to its
            // bottom, in pieces that meet.
            let scale = window.scale_factor();
            let border = gpui_kit::component::ActiveTheme::theme(cx).border;
            let mut pieces: Vec<(f32, f32)> = window
                .painted_quads()
                .into_iter()
                .filter(|q| {
                    q.background.as_solid() == Some(border)
                        && (q.bounds.origin.x.0 - column.left().as_f32() * scale).abs() < 0.01
                        && (q.bounds.size.width.0 - scale).abs() < 0.01
                })
                .map(|q| {
                    (
                        q.bounds.origin.y.0,
                        q.bounds.origin.y.0 + q.bounds.size.height.0,
                    )
                })
                .collect();
            pieces.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut reach = column.top().as_f32() * scale;
            for (top, bottom) in pieces {
                assert!(top <= reach + 0.01, "the line breaks at {reach}");
                reach = reach.max(bottom);
            }
            assert!(
                reach >= column.bottom().as_f32() * scale - 0.01,
                "the line stops at {reach}"
            );
            // Nothing drawn after it, in the column, covers the line.
            let line_drawn = window
                .painted_quads()
                .into_iter()
                .filter(|q| {
                    q.background.as_solid() == Some(border)
                        && (q.bounds.origin.x.0 - column.left().as_f32() * scale).abs() < 0.01
                })
                .map(|q| q.order)
                .min()
                .unwrap();
            for q in window.painted_quads() {
                let b = &q.bounds;
                if q.order <= line_drawn
                    || b.size.width.0 > column.size.width.as_f32() * scale + 0.01
                {
                    continue;
                }
                let over_line = b.origin.x.0 < column.left().as_f32() * scale + scale - 0.01
                    && b.origin.x.0 + b.size.width.0 > column.left().as_f32() * scale + 0.01
                    && b.origin.y.0 >= column.top().as_f32() * scale
                    && b.origin.y.0 < column.bottom().as_f32() * scale;
                if over_line && q.background.as_solid() != Some(border) {
                    assert!(
                        q.background.as_solid().is_none_or(|c| c.a == 0.),
                        "{b:?} is drawn over the line"
                    );
                }
            }
        })
        .unwrap();
    }

    /// The up and down buttons have the theme's bevel while hovered, and its
    /// pressed bevel while pressed, inside their lines, and neither otherwise.
    #[gpui_kit::test]
    async fn direction_buttons_bevel_when_hovered_and_pressed(cx: &mut TestAppContext) {
        use crate::theme::{bevel_colors, palette};
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| TallList {
                scroll: ScrollHandle::new(),
            });
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        // How many of the button's quads are lit, and how many shaded, and
        // whether the lit ones lie along its top or its bottom.
        let bevel = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let (light, shade) = bevel_colors(palette(cx));
                let button = window.find("tall-scroll-up").bounds();
                let scale = window.scale_factor();
                let inside = |quad: &gpui_kit::Quad| {
                    let b = &quad.bounds;
                    b.origin.x.0 >= button.left().as_f32() * scale
                        && b.origin.x.0 < button.right().as_f32() * scale
                        && b.origin.y.0 >= button.top().as_f32() * scale
                        && b.origin.y.0 < button.bottom().as_f32() * scale
                };
                let quads: Vec<_> = window.painted_quads().into_iter().filter(inside).collect();
                let of = |color: gpui_kit::Hsla| {
                    quads
                        .iter()
                        .filter(|quad| quad.background.as_solid() == Some(color))
                        .map(|quad| quad.bounds.origin.y.0)
                        .collect::<Vec<_>>()
                };
                let (lit, shaded) = (of(light), of(shade));
                let lit_on_top = lit
                    .iter()
                    .any(|y| (*y - button.top().as_f32() * scale).abs() < 0.5);
                (lit.len(), shaded.len(), lit_on_top)
            })
            .unwrap()
        };
        assert_eq!(bevel(cx), (0, 0, false), "a bevel shows at rest");

        let center = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.find("tall-scroll-up").bounds().center()
            })
            .unwrap();
        let cx = &mut gpui_kit::VisualTestContext::from_window(handle, cx);
        cx.simulate_mouse_move(center, None, gpui_kit::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(bevel(cx), (2, 2, true), "no raised bevel on hover");

        cx.simulate_mouse_down(
            center,
            gpui_kit::MouseButton::Left,
            gpui_kit::Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(bevel(cx), (2, 2, false), "no pressed bevel while pressed");

        cx.simulate_mouse_up(
            center,
            gpui_kit::MouseButton::Left,
            gpui_kit::Modifiers::default(),
        );
        cx.simulate_mouse_move(
            gpui_kit::point(gpui_kit::px(10.), gpui_kit::px(300.)),
            None,
            gpui_kit::Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(bevel(cx), (0, 0, false), "the bevel stayed");
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

    /// The thumb sits inside the track's line: the line beside the list runs
    /// unbroken down the track's full height, and nothing is drawn over it.
    #[gpui_kit::test]
    async fn thumb_keeps_inside_the_track_line(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| TallList {
                scroll: ScrollHandle::new(),
            });
            Root::new(view, window, cx)
        });
        let handle = window.into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let scale = window.scale_factor();
            let track = window.find("tall-scroll-track").bounds();
            let (left, top, bottom, right) = (
                track.left().as_f32() * scale,
                track.top().as_f32() * scale,
                track.bottom().as_f32() * scale,
                track.right().as_f32() * scale,
            );
            let within = |q: &gpui_kit::Quad| {
                let b = &q.bounds;
                b.origin.x.0 >= left - 0.01
                    && b.origin.x.0 + b.size.width.0 <= right + 0.01
                    && b.origin.y.0 >= top - 0.01
                    && b.origin.y.0 + b.size.height.0 <= bottom + 0.01
            };
            let quads: Vec<_> = window.painted_quads().into_iter().filter(within).collect();
            let line = quads
                .iter()
                .position(|q| {
                    (q.bounds.origin.x.0 - left).abs() < 0.01
                        && (q.bounds.size.width.0 - scale).abs() < 0.01
                        && (q.bounds.origin.y.0 - top).abs() < 0.01
                        && (q.bounds.size.height.0 - (bottom - top)).abs() < 0.01
                })
                .expect("no line down the track's full height");
            for (ix, q) in quads.iter().enumerate() {
                if ix != line && q.order >= quads[line].order {
                    assert!(
                        q.bounds.origin.x.0 >= left + scale - 0.01,
                        "{:?} covers the track's line",
                        q.bounds
                    );
                }
            }
            // Whatever the thumb, and the dark track around it, stays right of
            // the line.
            for q in &quads {
                if q.background.as_solid().is_some_and(|c| c.l < 0.05) {
                    assert!(q.bounds.origin.x.0 >= left + scale - 0.01, "{:?}", q.bounds);
                }
            }
        })
        .unwrap();
    }

    /// With nothing to scroll, the thumb fills the track; otherwise it is as
    /// tall as the share of the list in view, and never too short to grab.
    #[test]
    fn thumb_fills_the_track_until_there_is_something_to_scroll() {
        let track = Bounds::new(point(px(0.), px(100.)), size(px(18.), px(200.)));
        let geometry = Geometry::of(&(&ScrollHandle::new()).into(), track);
        assert_eq!(geometry.thumb_top, px(100.));
        assert_eq!(geometry.thumb_height, px(200.));
        assert!(MIN_THUMB < px(200.));
    }
}
