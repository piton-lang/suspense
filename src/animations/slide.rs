//! The quick slide: something moved slides from where it was to where it now
//! is, in about 150 milliseconds, eased out, never fading or bouncing. Only
//! where it is painted moves; where it is laid out, and so where focus and
//! everything else take it to be, is its new place at once. A slide begun
//! while another is under way starts from wherever it is then.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;

/// How long every quick motion takes.
pub const QUICK: Duration = Duration::from_millis(150);

#[cfg(test)]
thread_local! {
    /// Under test, everything is in its place at once, unless a test of the
    /// motion itself says otherwise.
    pub static AT_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// How long a quick motion takes here.
fn quick() -> Duration {
    #[cfg(test)]
    if AT_ONCE.get() {
        return Duration::ZERO;
    }
    QUICK
}

/// Whether a quick motion begun at `since` has ended.
pub fn ended(since: Instant) -> bool {
    since.elapsed() >= quick()
}

/// How far through a quick motion begun at `since` it is, eased out, from 0
/// to 1.
pub fn eased(since: Instant) -> f32 {
    if ended(since) {
        return 1.;
    }
    let t = (since.elapsed().as_secs_f32() / quick().as_secs_f32()).clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// A value easing from `from` toward 0 or 1, as whatever it shows opens or
/// closes, turning back from wherever it is when told to go the other way.
#[derive(Clone, Copy, Debug)]
pub struct Toward {
    open: bool,
    from: f32,
    since: Instant,
}

impl Toward {
    /// Already where it is going.
    pub fn at(open: bool) -> Self {
        Self {
            open,
            from: if open { 1. } else { 0. },
            since: Instant::now()
                .checked_sub(QUICK)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Where it is now, from 0 for closed to 1 for open.
    pub fn now(&self) -> f32 {
        let to = if self.open { 1. } else { 0. };
        self.from + (to - self.from) * eased(self.since)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether it is still on its way.
    pub fn moving(&self) -> bool {
        !ended(self.since)
    }

    /// Goes to open, or closed, from wherever it is.
    pub fn set(&mut self, open: bool) {
        if open != self.open {
            *self = Self {
                open,
                from: self.now(),
                since: Instant::now(),
            };
        }
    }
}

struct Slide {
    /// How far from its place it was painted as it began.
    from: Point<Pixels>,
    since: Instant,
}

impl Slide {
    fn offset(&self) -> Point<Pixels> {
        self.from * (1. - eased(self.since))
    }
}

#[derive(Default)]
struct SlidesState {
    /// Where each was last laid out, however what it scrolls in has scrolled.
    last: HashMap<SharedString, Point<Pixels>>,
    sliding: HashMap<SharedString, Slide>,
    /// A move has just been made: what has changed place since it was last
    /// laid out slides there.
    armed: bool,
}

/// The things that slide to their places, each by its key, kept across
/// frames.
#[derive(Clone, Default)]
pub struct Slides(Rc<RefCell<SlidesState>>);

impl Slides {
    /// Called once each frame before anything is laid out, with whether a
    /// move is to slide this frame.
    pub fn frame(&self, armed: bool) {
        let mut state = self.0.borrow_mut();
        state.armed = armed;
        state.sliding.retain(|_, slide| !ended(slide.since));
    }

    /// Forgets where `key` was, so it next appears in its place at once.
    pub fn forget(&self, key: &str) {
        let mut state = self.0.borrow_mut();
        state.last.remove(key);
        state.sliding.remove(key);
    }

    /// Where `key` was is moved `by`, as though it had been there all
    /// along: the list it is in shifted beneath it, so it hasn't moved.
    pub fn shift(&self, key: &str, by: Point<Pixels>) {
        if let Some(last) = self.0.borrow_mut().last.get_mut(key) {
            *last = *last + by;
        }
    }

    /// `child`, slid to its place whenever it is laid out somewhere new
    /// while armed. `scrolls` are what it scrolls in, so scrolling never
    /// counts as a move. Sliding sideways, it is drawn over what holds it,
    /// so nothing clips it on its way.
    pub fn slide(
        &self,
        key: impl Into<SharedString>,
        scrolls: Vec<ScrollHandle>,
        child: impl IntoElement,
    ) -> SlideElement {
        SlideElement {
            key: key.into(),
            slides: self.clone(),
            scrolls,
            child: Some(child.into_any_element()),
            over: false,
        }
    }
}

pub struct SlideElement {
    key: SharedString,
    slides: Slides,
    scrolls: Vec<ScrollHandle>,
    child: Option<AnyElement>,
    /// Drawn over what holds it, this frame.
    over: bool,
}

impl IntoElement for SlideElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SlideElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.as_mut().expect("laid out once");
        (child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let scrolled = self
            .scrolls
            .iter()
            .fold(Point::default(), |at, scroll| at + scroll.offset());
        let place = bounds.origin - scrolled;
        let offset = {
            let mut state = self.slides.0.borrow_mut();
            let now = state
                .sliding
                .get(&self.key)
                .map(Slide::offset)
                .unwrap_or_default();
            let last = state.last.insert(self.key.clone(), place);
            match last {
                Some(last) if state.armed && last != place => {
                    let from = last - place + now;
                    state.sliding.insert(
                        self.key.clone(),
                        Slide {
                            from,
                            since: Instant::now(),
                        },
                    );
                    from
                }
                _ => now,
            }
        };
        if offset == Point::default() {
            self.child.as_mut().unwrap().prepaint(window, cx);
            return;
        }
        window.request_animation_frame();
        if offset.x != px(0.) {
            // Across columns: over them, so neither clips it.
            self.over = true;
            let child = self.child.take().unwrap();
            window.defer_draw(child, window.element_offset() + offset, 0, None);
        } else {
            window.with_element_offset(offset, |window| {
                self.child.as_mut().unwrap().prepaint(window, cx)
            });
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if !self.over {
            self.child.as_mut().unwrap().paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AT_ONCE, QUICK, Toward};

    /// Told to go the other way partway, it turns back from where it is,
    /// and arrives within the quick motion's time.
    #[test]
    fn turns_back_from_where_it_is() {
        AT_ONCE.set(false);
        let mut toward = Toward::at(false);
        assert_eq!(toward.now(), 0.);
        toward.set(true);
        std::thread::sleep(QUICK / 3);
        let partway = toward.now();
        assert!(partway > 0. && partway < 1.);
        toward.set(false);
        assert!(toward.now() <= partway + 0.01);
        std::thread::sleep(QUICK + QUICK / 5);
        assert_eq!(toward.now(), 0.);
        assert!(!toward.moving());
    }
}
