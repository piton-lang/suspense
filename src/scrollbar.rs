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
    // The full width of the column. A button draws only the line beside the
    // list and the one between it and the track; whatever holds the column
    // draws the lines around it, so no two lines ever lie side by side.
    let square = |name: &str, icon: IconName| {
        Button::new(SharedString::from(format!("{id}-{name}")))
            .ghost()
            .xsmall()
            .icon(icon)
            .w(COLUMN_WIDTH)
            .h(COLUMN_WIDTH)
            .rounded_none()
            .border_l_1()
            .border_color(border)
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
                let drawn = thumb_drawn(thumb);
                // The track is dark all around the thumb, inside its line,
                // the thumb inset from it on every side, while the thumb is
                // left the colour of what the column sits on.
                let (left, right) = (bounds.origin.x + TRACK_LINE, bounds.right());
                for rect in [
                    // Above and below, the track's full width.
                    (left, bounds.top(), right, drawn.top()),
                    (left, drawn.bottom(), right, bounds.bottom()),
                    // Either side of it.
                    (left, drawn.top(), drawn.left(), drawn.bottom()),
                    (drawn.right(), drawn.top(), right, drawn.bottom()),
                ] {
                    let (x0, y0, x1, y1) = rect;
                    if x1 > x0 && y1 > y0 {
                        window.paint_quad(fill(
                            Bounds::new(point(x0, y0), size(x1 - x0, y1 - y0)),
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
                // The thumb, inset in the track, square and flat, with no lines
                // of its own, lightening while hovered or dragged; the pointer
                // is on it anywhere in the room it takes.
                let is_hovered = thumb.contains(&window.mouse_position());
                hovered.set(is_hovered);
                if grab.get().is_some() {
                    window.paint_quad(fill(drawn, colors.pressed));
                } else if is_hovered {
                    window.paint_quad(fill(drawn, colors.hover));
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
        .child(
            square("scroll-up", IconName::ChevronUp)
                .border_b_1()
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
                .border_t_1()
                .tooltip("Scroll down")
                .on_click(scroll_by(-STEP)),
        )
        .when_some(lock, |column, (locked, set_lock)| {
            let button = square("scroll-lock", IconName::ArrowDownToLine)
                .border_t_1()
                .selected(locked)
                .tooltip(if locked {
                    "Unlock the scroll from the bottom"
                } else {
                    "Lock the scroll to the bottom"
                })
                .on_click(move |_, window, cx| set_lock(!locked, window, cx));
            column.child(
                div()
                    .id(SharedString::from(format!("{id}-scroll-lock-pulse")))
                    .relative()
                    .child(pulse(locked, cx))
                    .child(button),
            )
        });
    // Lets UI tests find the column; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(column).into_any_element()
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

/// How far the thumb is drawn in from every side of the room it takes in the
/// track, the dark track showing around it.
const THUMB_INSET: Pixels = px(1.);

/// The thumb as drawn: `slot`, the room it takes in the track, less
/// [`THUMB_INSET`] on every side.
fn thumb_drawn(slot: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(slot.origin.x + THUMB_INSET, slot.origin.y + THUMB_INSET),
        size(
            (slot.size.width - THUMB_INSET * 2.).max(px(0.)),
            (slot.size.height - THUMB_INSET * 2.).max(px(0.)),
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

    /// The thumb is inset a pixel on every side: the dark track shows all
    /// around it, and not over it.
    #[gpui_kit::test]
    async fn thumb_is_inset_a_pixel_all_round(cx: &mut TestAppContext) {
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
            let scale = window.scale_factor();
            let track = window.find("tall-scroll-track").bounds();
            let (top, height) = super::thumb_for_test(&(&scroll).into(), track);
            assert!(height < track.size.height, "nothing to scroll");
            let dark: Vec<_> = window
                .painted_quads()
                .into_iter()
                .filter(|q| {
                    q.background
                        .as_solid()
                        .is_some_and(|c| c.l < 0.05 && c.a > 0.)
                })
                .map(|q| q.bounds)
                .collect();
            // Whether a dark quad covers the point, in logical pixels.
            let covered = |x: f32, y: f32| {
                let (x, y) = (x * scale, y * scale);
                dark.iter().any(|b| {
                    x >= b.origin.x.0
                        && x < b.origin.x.0 + b.size.width.0
                        && y >= b.origin.y.0
                        && y < b.origin.y.0 + b.size.height.0
                })
            };
            let (left, right) = (track.left().as_f32() + 1., track.right().as_f32());
            let (thumb_top, thumb_bottom) = (top.as_f32(), (top + height).as_f32());
            let middle_y = (thumb_top + thumb_bottom) / 2.;
            let middle_x = (left + right) / 2.;
            assert!(
                covered(middle_x, thumb_top + 0.5),
                "no track above the thumb"
            );
            assert!(
                covered(middle_x, thumb_bottom - 0.5),
                "no track below the thumb"
            );
            assert!(covered(left + 0.5, middle_y), "no track left of the thumb");
            assert!(
                covered(right - 0.5, middle_y),
                "no track right of the thumb"
            );
            assert!(!covered(middle_x, middle_y), "the track covers the thumb");
            assert!(
                !covered(left + 1.5, thumb_top + 1.5),
                "the inset is more than a pixel"
            );
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
