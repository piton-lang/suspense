//! A permanent scrollbar for a scrolling list, as a narrow column beside it:
//! a square button at the top that scrolls up, a track whose thumb shows how
//! much of the list is in view and can be dragged or clicked to, and a square
//! button at the bottom that scrolls down. Lists that follow new output, like
//! a task's, add a button below that locks the scroll to the bottom.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::measured_list::MeasuredList;

/// How wide the column is, which is also each button's width.
pub const COLUMN_WIDTH: Pixels = px(18.);

/// How far a button scrolls the list.
const STEP: Pixels = px(48.);

/// The shortest the thumb gets, so it can always be grabbed.
const MIN_THUMB: Pixels = px(16.);

/// How long the pulse lasts when the scroll locks.
const PULSE_TIME: Duration = Duration::from_millis(900);

/// What a scroll column scrolls: an element tracking a [`ScrollHandle`], or a
/// virtualized list that knows its rows' heights.
#[derive(Clone)]
pub enum Scroll {
    Handle(ScrollHandle),
    Measured(MeasuredList),
}

impl From<&ScrollHandle> for Scroll {
    fn from(handle: &ScrollHandle) -> Self {
        Scroll::Handle(handle.clone())
    }
}

impl From<&MeasuredList> for Scroll {
    fn from(list: &MeasuredList) -> Self {
        list.scroll()
    }
}

impl Scroll {
    /// How far it is scrolled, negative going down.
    pub fn offset(&self) -> Point<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.offset(),
            Scroll::Measured(list) => list.offset(),
        }
    }

    /// How far it can scroll.
    pub fn max_offset(&self) -> Point<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.max_offset(),
            Scroll::Measured(list) => list.max_offset(),
        }
    }

    pub fn set_offset(&self, offset: Point<Pixels>) {
        match self {
            Scroll::Handle(handle) => handle.set_offset(offset),
            Scroll::Measured(list) => list.set_offset(offset),
        }
    }

    /// The part of it in view.
    fn viewport(&self) -> Bounds<Pixels> {
        match self {
            Scroll::Handle(handle) => handle.bounds(),
            Scroll::Measured(list) => list.viewport(),
        }
    }
}

/// The colours the column draws, each laid over whatever surface it sits on
/// rather than a colour of its own, so the scrollbar looks the same on any
/// surface: the track, black laid half way over it; the single row at each end
/// of the thumb, black laid a quarter of the way; the buttons' arrows, white
/// laid about a fifth of the way; and the thumb's lightening while hovered and
/// dragged. The thumb at rest, like the buttons, has no fill at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollColors {
    pub track: Hsla,
    pub thumb_end: Hsla,
    pub arrow: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
}

pub fn scroll_colors(dark: bool) -> ScrollColors {
    let (black, white) = (hsla(0., 0., 0., 1.), hsla(0., 0., 1., 1.));
    if dark {
        ScrollColors {
            // #111111 on #222222.
            track: black.opacity(0.5),
            // #191919 on #222222: a quarter, a touch over, so it rounds to
            // the mockup's rather than to #1a1a1a.
            thumb_end: black.opacity(0.26),
            // #4d4d4d on #222222.
            arrow: white.opacity(0.195),
            hover: white.opacity(0.035),
            pressed: white.opacity(0.07),
        }
    } else {
        // The spec gives dark mode's; light mode takes the same steps, gentler,
        // with the arrow darker rather than lighter, as white would all but
        // vanish on a light surface.
        ScrollColors {
            track: black.opacity(0.2),
            thumb_end: black.opacity(0.1),
            arrow: black.opacity(0.35),
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

/// How tall each button is.
pub const BUTTON_HEIGHT: Pixels = px(17.);

/// How far the track is inset from either side of the column.
const TRACK_INSET: Pixels = px(1.);

/// The column itself: no background of its own, so the colour of whatever it
/// sits on, and no line anywhere.
fn scrollbar(
    id: SharedString,
    handle: &Scroll,
    lock: Option<(bool, SetLock)>,
    cx: &App,
) -> AnyElement {
    let colors = scroll_colors(cx.theme().is_dark());
    let follow = follower(handle, &lock);
    let scroll_by = |delta: Pixels| {
        let handle = handle.clone();
        let follow = follow.clone();
        let press: Press = Rc::new(move |window: &mut Window, cx: &mut App| {
            let offset = handle.offset();
            let max = handle.max_offset().y;
            let y = (offset.y + delta).min(px(0.)).max(-max);
            handle.set_offset(point(offset.x, y));
            if let Some(follow) = &follow {
                follow(window, cx);
            }
            window.refresh();
        });
        press
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
                // The track is dark above and below the thumb, inset a pixel
                // from either side of the column, while the thumb is left the
                // colour of what the column sits on.
                let (inside, inner_width) = inset(bounds);
                for (top, bottom) in [
                    (bounds.top(), thumb.top()),
                    (thumb.bottom(), bounds.bottom()),
                ] {
                    if bottom > top {
                        window.paint_quad(fill(
                            Bounds::new(point(inside, top), size(inner_width, bottom - top)),
                            colors.track,
                        ));
                    }
                }
                // The thumb, the column's full width, lightening while hovered
                // or dragged. Nothing is hovered while something else, like a
                // sidebar's edge, is being dragged across it.
                let is_hovered = !cx.has_active_drag() && thumb.contains(&window.mouse_position());
                hovered.set(is_hovered);
                if grab.get().is_some() {
                    // No edge beneath the pointer is offered while dragging.
                    window.set_window_cursor_style(CursorStyle::Arrow);
                    window.paint_quad(fill(thumb, colors.pressed));
                } else if is_hovered {
                    window.paint_quad(fill(thumb, colors.hover));
                }
                // A pixel at each end, between the track's colour and the
                // surface's, across the track.
                for (edge, color) in thumb_ends(thumb, bounds, &colors) {
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
                        if !bounds.contains(&event.position) {
                            return;
                        }
                        let geometry = Geometry::of(&handle, bounds);
                        let y = event.position.y;
                        // The thumb, or the track pressed off it, against any
                        // resize edge over it there: the smaller takes the
                        // press, taking it first when it is contested.
                        let on_thumb = y >= geometry.thumb_top
                            && y <= geometry.thumb_top + geometry.thumb_height;
                        let target = if on_thumb {
                            thumb_bounds(&geometry, bounds)
                        } else {
                            bounds
                        };
                        if !crate::hit_areas::wins_at(target, event.position, cx) {
                            return;
                        }
                        let contested = crate::hit_areas::resize_at(event.position, cx);
                        let turn = if contested {
                            DispatchPhase::Capture
                        } else {
                            DispatchPhase::Bubble
                        };
                        if phase != turn {
                            return;
                        }
                        if contested {
                            cx.stop_propagation();
                        }
                        // Grabbed where it was pressed; pressed off the thumb, the
                        // thumb first jumps to centre on the press.
                        let at = if on_thumb {
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
        .child(square(
            format!("{id}-scroll-up"),
            Glyph::Up,
            "Scroll up".into(),
            false,
            scroll_by(STEP),
            None,
            colors,
        ))
        .child(gpui_kit::TestSupportExt::test_support(
            div()
                .id(SharedString::from(format!("{id}-scroll-track")))
                .flex_1()
                .min_h(px(8.))
                .child(track),
        ))
        .child(square(
            format!("{id}-scroll-down"),
            Glyph::Down,
            "Scroll down".into(),
            false,
            scroll_by(-STEP),
            None,
            colors,
        ))
        .when_some(lock, |column, (locked, set_lock)| {
            let press: Press = Rc::new(move |window, cx| set_lock(!locked, window, cx));
            let tooltip = if locked {
                "Unlock the scroll from the bottom"
            } else {
                "Lock the scroll to the bottom"
            };
            column
                // A pixel of the surface between it and the down button.
                .child(div().flex_none().h(LOCK_GAP))
                .child(square(
                    format!("{id}-scroll-lock"),
                    Glyph::ToBottom,
                    tooltip.into(),
                    locked,
                    press,
                    Some(pulse(locked, cx).into_any_element()),
                    colors,
                ))
        });
    // Lets UI tests find the column; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(column).into_any_element()
}

/// The space between the down button and the lock button.
const LOCK_GAP: Pixels = px(1.);

/// Where the track lies across a column at `bounds`: its left and its width,
/// a pixel in from either side.
fn inset(bounds: Bounds<Pixels>) -> (Pixels, Pixels) {
    (
        bounds.origin.x + TRACK_INSET,
        (bounds.size.width - TRACK_INSET * 2.).max(px(0.)),
    )
}

/// The single row at each end of a thumb at `thumb`, in a track at `track`,
/// with its colour: across the track's width, inside the thumb. None where the
/// thumb is too short to have two.
fn thumb_ends(
    thumb: Bounds<Pixels>,
    track: Bounds<Pixels>,
    colors: &ScrollColors,
) -> Vec<(Bounds<Pixels>, Hsla)> {
    let row = px(1.);
    if thumb.size.height < row * 2. {
        return Vec::new();
    }
    let (left, width) = inset(track);
    vec![
        (
            Bounds::new(point(left, thumb.top()), size(width, row)),
            colors.thumb_end,
        ),
        (
            Bounds::new(point(left, thumb.bottom() - row), size(width, row)),
            colors.thumb_end,
        ),
    ]
}

/// What a button in the column shows: a small solid arrow, pointing up or
/// down, or down to a line for the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    Up,
    Down,
    ToBottom,
}

impl Glyph {
    /// The glyph's rows in a button at `bounds`, each as its top and width
    /// from the button's top, centred across it: a triangle 6 pixels wide
    /// and 4 tall, 7 pixels from the button's top pointing up or from its
    /// bottom pointing down, and for the lock the down arrow with a bar
    /// beneath it.
    fn rows(self) -> &'static [(f32, f32)] {
        match self {
            Glyph::Up => &[(7., 2.), (8., 4.), (9., 6.), (10., 6.)],
            Glyph::Down => &[(6., 6.), (7., 6.), (8., 4.), (9., 2.)],
            Glyph::ToBottom => &[(5., 6.), (6., 6.), (7., 4.), (8., 2.), (10., 6.)],
        }
    }

    pub(crate) fn quads(self, bounds: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
        // Rounded to a whole pixel, so it stays crisp.
        let center = (bounds.origin.x + bounds.size.width / 2.).round();
        self.rows()
            .iter()
            .map(|&(top, width)| {
                Bounds::new(
                    point(center - px(width / 2.), bounds.origin.y + px(top)),
                    size(px(width), px(1.)),
                )
            })
            .collect()
    }
}

/// A button in the column, `id`, as wide as the column and a button tall,
/// showing only `glyph`, with no background, border, or line of its own. It
/// has the theme's bevel while hovered, and its pressed bevel while pressed
/// with the pointer still over it, or while `selected`. Pressing it does
/// `press`. `behind` is drawn beneath everything else in it.
fn square(
    id: String,
    glyph: Glyph,
    tooltip: SharedString,
    selected: bool,
    press: Press,
    behind: Option<AnyElement>,
    colors: ScrollColors,
) -> impl IntoElement {
    let arrow = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            for row in glyph.quads(bounds) {
                window.paint_quad(fill(row, colors.arrow));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let button = div()
        .id(SharedString::from(id.clone()))
        .relative()
        .flex_none()
        .w(COLUMN_WIDTH)
        .h(BUTTON_HEIGHT)
        .children(behind)
        .child(arrow)
        .child(bevel(format!("{id}-bevel"), selected))
        .child(over_edges(format!("{id}-claim").into(), press.clone()))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(move |_, window, cx| press(window, cx));
    // Lets UI tests find the button; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(button)
}

/// The bevel over a button: the theme's bevel while hovered, and its pressed
/// bevel while pressed with the pointer still over it, or while `selected`.
/// It takes no mouse: it watches the mouse before the button does.
fn bevel(key: String, selected: bool) -> impl IntoElement {
    use crate::theme::{Bevel, bevel_edges, palette};
    let key = SharedString::from(key);
    canvas(
        move |_, window, cx| {
            // Whether the button was pressed, and not yet let go.
            window.use_keyed_state(key, cx, |_, _| Rc::new(Cell::new(false)))
        },
        move |bounds, pressed: Entity<Rc<Cell<bool>>>, window, cx| {
            let pressed = pressed.read(cx).clone();
            // Not while something else, like a sidebar's edge, is dragged.
            let hovered = !cx.has_active_drag() && bounds.contains(&window.mouse_position());
            let bevel = match (hovered, pressed.get(), selected) {
                (true, true, _) | (_, _, true) => Some(Bevel::Pressed),
                (true, false, false) => Some(Bevel::Raised),
                (false, _, false) => None,
            };
            if let Some(bevel) = bevel {
                for (edge, color) in bevel_edges(bounds, bevel, palette(cx)) {
                    window.paint_quad(fill(edge, color));
                }
            }
            window.on_mouse_event({
                let pressed = pressed.clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && crate::hit_areas::wins_at(bounds, event.position, cx)
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
    .size_full()
}

/// What pressing a button in the column does.
type Press = Rc<dyn Fn(&mut Window, &mut App)>;

/// Lets the button it is laid over take a press, doing `press`, where a
/// resize edge beside the list overlaps it and has the larger hit area, as
/// the edge would otherwise take it: see [`crate::hit_areas`]. Over the
/// button there, the cursor is an arrow rather than the edge's.
fn over_edges(key: SharedString, press: Press) -> impl IntoElement {
    canvas(
        move |_, window, cx| {
            // Whether a press on the button is under way.
            window.use_keyed_state(key, cx, |_, _| Rc::new(Cell::new(false)))
        },
        move |bounds, claimed: Entity<Rc<Cell<bool>>>, window, cx| {
            let claimed = claimed.read(cx).clone();
            let contested = move |at: Point<Pixels>, cx: &App| {
                crate::hit_areas::resize_at(at, cx) && crate::hit_areas::wins_at(bounds, at, cx)
            };
            if !cx.has_active_drag() && contested(window.mouse_position(), cx) {
                window.set_window_cursor_style(CursorStyle::Arrow);
            }
            window.on_mouse_event({
                let claimed = claimed.clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && contested(event.position, cx)
                    {
                        claimed.set(true);
                        cx.stop_propagation();
                        window.refresh();
                    }
                }
            });
            window.on_mouse_event({
                let press = press.clone();
                move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && claimed.replace(false) {
                        if bounds.contains(&event.position) {
                            press(window, cx);
                        }
                        cx.stop_propagation();
                    }
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
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

/// The thumb in a track at `track`: the column's full width, covering the
/// track's inset.
fn thumb_bounds(geometry: &Geometry, track: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(track.origin.x, geometry.thumb_top),
        size(track.size.width, geometry.thumb_height),
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
    /// than what is beneath it without losing the surface's character, the
    /// thumb's ends lie between the track and the surface, and the thumb, the
    /// surface itself at rest, lightens hovered, and lightens again while
    /// dragged. Nothing is opaque: all of it is laid over the surface.
    #[test]
    fn track_is_dark_and_thumb_lightens_as_it_is_used() {
        let over = |surface: Hsla, color: Hsla| surface.blend(color);
        for (dark, surfaces) in [
            (true, [0.067, 0.133, 0.18, 0.27]),
            (false, [0.74, 0.82, 0.91, 0.96]),
        ] {
            let colors = super::scroll_colors(dark);
            for color in [
                colors.track,
                colors.thumb_end,
                colors.arrow,
                colors.hover,
                colors.pressed,
            ] {
                assert!(color.a < 1., "laid over, not opaque");
            }
            for l in surfaces {
                let surface = hsla(0., 0., l, 1.);
                let track = over(surface, colors.track);
                let end = over(surface, colors.thumb_end);
                let arrow = over(surface, colors.arrow);
                let hover = over(surface, colors.hover);
                let pressed = over(surface, colors.pressed);
                assert!(
                    track.l < surface.l && surface.l - track.l < 0.25,
                    "{track:?} isn't darker than {surface:?}, without losing it"
                );
                assert!(
                    track.l < end.l && end.l < surface.l,
                    "{end:?} isn't between {track:?} and {surface:?}"
                );
                assert!((arrow.l - surface.l).abs() > 0.05, "{arrow:?} on {surface:?}");
                assert!(
                    hover.l > surface.l && pressed.l > hover.l,
                    "{surface:?} {hover:?} {pressed:?}"
                );
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

    /// Each button is the column's full width and 17 pixels tall, the track
    /// running straight between them, and nothing in the column is drawn in
    /// the theme's border colour: it has no line anywhere.
    #[gpui_kit::test]
    async fn buttons_fill_the_column_with_no_line(cx: &mut TestAppContext) {
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
            assert_eq!(column.size.width, px(18.));
            for button in [up, down] {
                assert_eq!(
                    (button.left(), button.right()),
                    (column.left(), column.right()),
                    "{button:?} isn't the column's width"
                );
                assert_eq!(button.size.height, px(17.));
            }
            assert_eq!((up.top(), down.bottom()), (column.top(), column.bottom()));
            assert_eq!((track.top(), track.bottom()), (up.bottom(), down.top()));
            assert_eq!((track.left(), track.right()), (column.left(), column.right()));
            let scale = window.scale_factor();
            let border = gpui_kit::component::ActiveTheme::theme(cx).border;
            for q in window.painted_quads() {
                let b = &q.bounds;
                let in_column = b.origin.x.0 + b.size.width.0 > column.left().as_f32() * scale
                    && b.origin.x.0 < column.right().as_f32() * scale
                    && b.origin.y.0 + b.size.height.0 > column.top().as_f32() * scale
                    && b.origin.y.0 < column.bottom().as_f32() * scale;
                if in_column && b.size.width.0 <= column.size.width.as_f32() * scale + 0.01 {
                    assert_ne!(q.background.as_solid(), Some(border), "a line at {b:?}");
                    assert!(
                        q.border_widths.left.0 == 0.
                            && q.border_widths.right.0 == 0.
                            && q.border_widths.top.0 == 0.
                            && q.border_widths.bottom.0 == 0.,
                        "a border at {b:?}"
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

    /// A list whose scrollbar lies against the edge it is resized by.
    struct SplitList {
        scroll: ScrollHandle,
        split: gpui_kit::Entity<gpui_kit::component::resizable::ResizableState>,
    }

    impl Render for SplitList {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::resizable::{h_resizable, resizable_panel};
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
                .child(crate::hit_areas::frame_start())
                .child(
                    h_resizable("test-split")
                        .with_state(&self.split)
                        .with_handle_appearance(crate::hit_areas::resize_edges("test-split"))
                        .children([
                            resizable_panel().size(px(300.)).child(with_scrollbar(
                                "tall",
                                &self.scroll,
                                list,
                                true,
                                None,
                                cx,
                            )),
                            resizable_panel().child(div().size_full()),
                        ]),
                )
        }
    }

    /// Where the edge a list is resized by overlaps its scrollbar, the smaller
    /// hit area takes a press, and only it: the scrollbar's button over the
    /// edge's strip, and the edge over the track. A scroll and a resize never
    /// happen together.
    #[gpui_kit::test]
    async fn the_smaller_of_a_scrollbar_and_an_edge_takes_a_press(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let scroll = ScrollHandle::new();
        let split = cx.new(|_| gpui_kit::component::resizable::ResizableState::default());
        let window = cx.add_window({
            let (scroll, split) = (scroll.clone(), split.clone());
            |window, cx| {
                let view = cx.new(|_| SplitList { scroll, split });
                Root::new(view, window, cx)
            }
        });
        let handle = window.into();
        let width = |cx: &mut TestAppContext| split.read_with(cx, |state, _| state.sizes()[0]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        let before = width(cx);

        // The down button, at the column's right edge, where the edge's strip
        // lies over it: it scrolls, and nothing is resized.
        cx.update_window(handle, |_, window, cx| {
            let down = window.find("tall-scroll-down").bounds();
            let at = point(down.size.width - px(2.), down.size.height / 2.);
            window.click_at("tall-scroll-down", at, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(scroll.offset().y < px(0.), "the down button didn't scroll");
        assert_eq!(width(cx), before, "pressing the button resized the list");

        // The track, taller than the edge's strip is wide, gives way to it:
        // dragging there resizes, and scrolls nothing.
        let scrolled = scroll.offset().y;
        cx.update_window(handle, |_, window, cx| {
            let track = window.find("tall-scroll-track").bounds();
            let from = point(track.right() - px(2.), track.bottom() - px(20.));
            window.drag(from, from + point(px(60.), px(0.)), cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(scroll.offset().y, scrolled, "resizing scrolled the list");
        assert!(width(cx) > before, "dragging the edge didn't resize");
    }

    /// While the edge a list is resized by is being dragged, the pointer
    /// passing over the list's scrollbar lights up none of it: neither the
    /// thumb nor a button's bevel.
    #[gpui_kit::test]
    async fn nothing_in_the_scrollbar_hovers_while_an_edge_is_dragged(cx: &mut TestAppContext) {
        use crate::theme::{bevel_colors, palette};
        cx.update(gpui_kit::init);
        let split = cx.new(|_| gpui_kit::component::resizable::ResizableState::default());
        let window = cx.add_window({
            let split = split.clone();
            |window, cx| {
                let view = cx.new(|_| SplitList {
                    scroll: ScrollHandle::new(),
                    split,
                });
                Root::new(view, window, cx)
            }
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        let (up, thumb) = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
                let track = window.find("tall-scroll-track").bounds();
                (
                    window.find("tall-scroll-up").bounds(),
                    point(track.center().x, track.top() + px(4.)),
                )
            })
            .unwrap();
        // Anything lit in the window: a button's bevel, and a hover.
        let lit = |cx: &mut gpui_kit::VisualTestContext| {
            cx.update(|window, cx| {
                window.render_frame(cx);
                let (light, _) = bevel_colors(palette(cx));
                use gpui_kit::component::ActiveTheme as _;
                let hover = super::scroll_colors(cx.theme().is_dark()).hover;
                window
                    .painted_quads()
                    .into_iter()
                    .filter(|quad| {
                        quad.background.as_solid() == Some(light)
                            || quad.background.as_solid() == Some(hover)
                    })
                    .count()
            })
        };
        let cx = &mut gpui_kit::VisualTestContext::from_window(handle, cx);
        // Picked up on the far side of the edge, clear of the column.
        let edge = point(up.right() + px(3.), up.bottom() + px(200.));
        cx.simulate_mouse_move(edge, None, gpui_kit::Modifiers::default());
        cx.run_until_parked();
        // Whatever is lit at rest.
        let rest = lit(cx);
        cx.simulate_mouse_down(
            edge,
            gpui_kit::MouseButton::Left,
            gpui_kit::Modifiers::default(),
        );
        for to in [up.center(), thumb] {
            cx.simulate_mouse_move(
                to,
                Some(gpui_kit::MouseButton::Left),
                gpui_kit::Modifiers::default(),
            );
            cx.run_until_parked();
            assert!(
                cx.update(|_, cx| cx.has_active_drag()),
                "the edge isn't being dragged"
            );
            assert_eq!(
                lit(cx),
                rest,
                "the scrollbar lit up at {to:?} while the edge was dragged"
            );
        }
        cx.simulate_mouse_up(
            thumb,
            gpui_kit::MouseButton::Left,
            gpui_kit::Modifiers::default(),
        );
        cx.run_until_parked();
        // The column moved with the edge.
        let up = cx.update(|window, cx| {
            window.render_frame(cx);
            window.find("tall-scroll-up").bounds()
        });
        cx.simulate_mouse_move(up.center(), None, gpui_kit::Modifiers::default());
        cx.run_until_parked();
        assert!(
            lit(cx) > rest,
            "the button no longer lights up once the drag is over"
        );
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

    /// A tall list on a surface of `surface`, in dark mode, scrolled with
    /// `scroll`, with the lock button when `lock` is given.
    struct OnSurface {
        scroll: ScrollHandle,
        surface: u32,
        lock: Option<bool>,
    }

    impl Render for OnSurface {
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
            let lock = self.lock.map(|locked| {
                let set: super::SetLock = std::rc::Rc::new(|_, _, _| {});
                (locked, set)
            });
            div()
                .size_full()
                .bg(crate::theme::color(self.surface))
                .child(with_scrollbar("tall", &self.scroll, list, true, lock, cx))
        }
    }

    /// A window in dark mode holding the list on `surface`, scrolled to the
    /// middle, so the track shows above and below the thumb.
    fn on_surface(
        surface: u32,
        lock: Option<bool>,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::AnyWindowHandle, ScrollHandle) {
        use gpui_kit::component::{Theme, ThemeMode};
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let scroll = ScrollHandle::new();
        let window = cx.add_window({
            let scroll = scroll.clone();
            move |window, cx| {
                let view = cx.new(|_| OnSurface {
                    scroll,
                    surface,
                    lock,
                });
                Root::new(view, window, cx)
            }
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        let max = scroll.max_offset().y;
        assert!(max > px(0.), "nothing to scroll");
        scroll.set_offset(point(px(0.), -(max / 2.).round()));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        (handle, scroll)
    }

    /// `color` laid over `surface`, as 0xRRGGBB.
    fn over(surface: u32, color: gpui_kit::Hsla) -> u32 {
        let c: gpui_kit::Rgba = color.into();
        [(16, c.r), (8, c.g), (0, c.b)]
            .into_iter()
            .fold(0, |rgb, (shift, channel)| {
                let s = ((surface >> shift) & 0xff) as f32 / 255.;
                let v = s * (1. - c.a) + channel * c.a;
                rgb | ((v * 255.).round() as u32) << shift
            })
    }

    /// Whether two colours are the same, to a step in each channel.
    fn same(a: u32, b: u32) -> bool {
        (0..3).all(|ix| {
            let channel = |c: u32| ((c >> (ix * 8)) & 0xff) as i32;
            (channel(a) - channel(b)).abs() <= 1
        })
    }

    /// Where the thumb is, from the frame: the first and last rows, from the
    /// track's top, whose middle isn't the track's colour.
    fn thumb_rows(
        frame: &crate::frame_image::Frame,
        track: Bounds<gpui_kit::Pixels>,
        dark_track: u32,
    ) -> (i32, i32) {
        let x = track.left() + px(8.5);
        let rows: Vec<i32> = (0..track.size.height.as_f32() as i32)
            .filter(|y| !same(frame.at(point(x, track.top() + px(*y as f32 + 0.5))), dark_track))
            .collect();
        (*rows.first().unwrap(), *rows.last().unwrap())
    }

    /// On the file tree's darkest surface and on a panel body's, the column
    /// is drawn as the mockup is: no background and no line of its own, down
    /// its sides or against the list; the track, black laid half way over the
    /// surface, inset a pixel from either side; the thumb the surface's own
    /// colour, the column's full width, with a row of black laid a quarter
    /// over the surface at either end across the track; and each button only
    /// a small solid arrow in white laid about a fifth over the surface.
    #[gpui_kit::test]
    async fn the_column_is_laid_over_its_surface(cx: &mut TestAppContext) {
        for (surface, dark_track, end, arrow) in [
            (0x111111, 0x090909, 0x0d0d0d, 0x3e3e3e),
            (0x222222, 0x111111, 0x191919, 0x4d4d4d),
        ] {
            let (handle, _) = on_surface(surface, None, cx);
            cx.update_window(handle, |_, window, _| {
                let colors = super::scroll_colors(true);
                assert!(same(over(surface, colors.track), dark_track));
                assert!(same(over(surface, colors.thumb_end), end));
                assert!(same(over(surface, colors.arrow), arrow));
                let frame = crate::frame_image::Frame::of(window);
                let at = |x: f32, y: f32, from: Bounds<gpui_kit::Pixels>| {
                    frame.at(point(from.left() + px(x + 0.5), from.top() + px(y + 0.5)))
                };
                let column = window.find("tall-scroll-column").bounds();
                let track = window.find("tall-scroll-track").bounds();
                let up = window.find("tall-scroll-up").bounds();
                let down = window.find("tall-scroll-down").bounds();
                let (first, last) = thumb_rows(&frame, track, dark_track);
                assert!(first > 10 && last < track.size.height.as_f32() as i32 - 10);
                // Down both sides of the column, and just left of it against
                // the list, only the surface, from its top to its bottom.
                for y in 0..column.size.height.as_f32() as i32 {
                    for x in [-1., 0., 17.] {
                        let c = at(x, y as f32, column);
                        assert!(
                            same(c, surface),
                            "{surface:06x}: {c:06x} at ({x}, {y}) beside the track"
                        );
                    }
                }
                // The track, inset, above and below the thumb.
                for y in [0, first - 1, last + 1, track.size.height.as_f32() as i32 - 1] {
                    for x in 1..17 {
                        let c = at(x as f32, y as f32, track);
                        assert!(
                            same(c, dark_track),
                            "{surface:06x}: the track is {c:06x} at ({x}, {y})"
                        );
                    }
                }
                // The thumb's ends, and the surface between them.
                for x in 1..17 {
                    for y in [first, last] {
                        let c = at(x as f32, y as f32, track);
                        assert!(same(c, end), "{surface:06x}: thumb end {c:06x} at ({x}, {y})");
                    }
                    for y in [first + 1, (first + last) / 2, last - 1] {
                        let c = at(x as f32, y as f32, track);
                        assert!(same(c, surface), "{surface:06x}: the thumb is {c:06x} at {y}");
                    }
                }
                // The buttons: the arrow's rows, as the mockup's, and the
                // surface everywhere else.
                let up_rows = [(7, 8, 2), (8, 7, 4), (9, 6, 6), (10, 6, 6)];
                let down_rows = [(6, 6, 6), (7, 6, 6), (8, 7, 4), (9, 8, 2)];
                for (button, rows) in [(up, up_rows), (down, down_rows)] {
                    for y in 0..17 {
                        for x in 0..18 {
                            let lit = rows
                                .iter()
                                .any(|&(row, left, width)| y == row && (left..left + width).contains(&x));
                            let c = at(x as f32, y as f32, button);
                            let expected = if lit { arrow } else { surface };
                            assert!(
                                same(c, expected),
                                "{surface:06x}: {c:06x} at ({x}, {y}) in {button:?}, not {expected:06x}"
                            );
                        }
                    }
                }
            })
            .unwrap();
        }
    }

    /// Under the pointer the thumb lightens across the column's full width,
    /// as white laid over the surface, ends and all, on either surface; the
    /// track and the buttons don't change.
    #[gpui_kit::test]
    async fn the_thumb_lightens_on_either_surface(cx: &mut TestAppContext) {
        for (surface, dark_track) in [(0x111111, 0x090909), (0x222222, 0x111111)] {
            let (handle, _) = on_surface(surface, None, cx);
            let (track, rows) = cx
                .update_window(handle, |_, window, _| {
                    let frame = crate::frame_image::Frame::of(window);
                    let track = window.find("tall-scroll-track").bounds();
                    (track, thumb_rows(&frame, track, dark_track))
                })
                .unwrap();
            let middle = point(
                track.left() + px(9.),
                track.top() + px(((rows.0 + rows.1) / 2) as f32),
            );
            let cx = &mut gpui_kit::VisualTestContext::from_window(handle, cx);
            cx.simulate_mouse_move(middle, None, gpui_kit::Modifiers::default());
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.render_frame(cx);
                let colors = super::scroll_colors(true);
                let frame = crate::frame_image::Frame::of(window);
                let hover = over(surface, colors.hover);
                assert_ne!(hover, surface, "hovering doesn't lighten {surface:06x}");
                for x in [0., 1., 9., 16., 17.] {
                    let c = frame.at(point(track.left() + px(x + 0.5), middle.y + px(0.5)));
                    assert!(same(c, hover), "{surface:06x}: hovered, the thumb is {c:06x} at {x}");
                }
                // The ends, laid over the lightened thumb.
                let end = frame.at(point(track.left() + px(8.5), track.top() + px(rows.0 as f32 + 0.5)));
                let expected = over(hover, colors.thumb_end);
                assert!(same(end, expected), "{surface:06x}: hovered end {end:06x}");
                // Just outside the thumb, the track is as it was.
                let above = frame.at(point(track.left() + px(8.5), track.top() + px(rows.0 as f32 - 0.5)));
                assert!(same(above, dark_track));
            });
        }
    }

    /// The lock button is drawn as the others are, a button tall, with a
    /// pixel of the surface between it and the down button: no background,
    /// only its arrow.
    #[gpui_kit::test]
    async fn the_lock_button_is_drawn_as_the_others(cx: &mut TestAppContext) {
        let surface = 0x222222;
        let (handle, _) = on_surface(surface, Some(false), cx);
        cx.update_window(handle, |_, window, _| {
            let frame = crate::frame_image::Frame::of(window);
            let down = window.find("tall-scroll-down").bounds();
            let lock = window.find("tall-scroll-lock").bounds();
            let column = window.find("tall-scroll-column").bounds();
            assert_eq!(lock.top() - down.bottom(), px(1.));
            assert_eq!(lock.size.height, px(17.));
            assert_eq!((lock.left(), lock.right()), (column.left(), column.right()));
            assert_eq!(lock.bottom(), column.bottom());
            let arrow = over(surface, super::scroll_colors(true).arrow);
            let mut lit = 0;
            for y in -1..17 {
                for x in 0..18 {
                    let c = frame.at(point(
                        lock.left() + px(x as f32 + 0.5),
                        lock.top() + px(y as f32 + 0.5),
                    ));
                    if same(c, arrow) {
                        lit += 1;
                    } else {
                        assert!(same(c, surface), "{c:06x} at ({x}, {y}) in the lock button");
                    }
                }
            }
            assert!(lit > 0, "the lock button shows nothing");
        })
        .unwrap();
    }

    /// Writes the column on #222222 to the path in `SCROLLBAR_FRAME`, as a
    /// PPM, to compare with a mockup by eye or by script. Does nothing
    /// without it.
    #[gpui_kit::test]
    async fn capture_the_column(cx: &mut TestAppContext) {
        let Ok(path) = std::env::var("SCROLLBAR_FRAME") else {
            return;
        };
        let (handle, _) = on_surface(0x222222, None, cx);
        cx.update_window(handle, |_, window, _| {
            let frame = crate::frame_image::Frame::of(window);
            let column = window.find("tall-scroll-column").bounds();
            let (w, h) = (18, column.size.height.as_f32() as usize);
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for y in 0..h {
                for x in 0..w {
                    let c = frame.at(point(
                        column.left() + px(x as f32 + 0.5),
                        column.top() + px(y as f32 + 0.5),
                    ));
                    out.extend([(c >> 16) as u8, (c >> 8) as u8, c as u8]);
                }
            }
            std::fs::write(path, out).unwrap();
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
