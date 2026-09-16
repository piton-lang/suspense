//! The rise-in animation: how something floating over the window comes in and
//! goes away. The surface fades in as it rises about 28 pixels into place, and
//! the dimming behind it darkens, both on one quick spring; going away plays
//! it backwards. Only how they look moves, never where the surface takes the
//! mouse or focus. Whatever uses it keeps what it showed last, to go away with
//! once it's closed (see [`Leaving`]).

use std::time::{Duration, Instant};

use gpui_kit::*;

/// How dark the window behind is, once in.
const DIM: f32 = 0.4;

/// How far below its place the surface starts, and sinks to.
const DROP: Pixels = px(28.);

/// The spring both parts follow.
const SPRING: SpringConfig = SpringConfig::new(380., 38., 1.);

/// How long going away takes, after which it's gone.
pub const LEAVE_TIME: Duration = Duration::from_millis(260);

/// Which way it plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// How far in it is, from 0 for gone to 1 for fully in.
fn spring(direction: Direction) -> SpringAnimation<f32> {
    match direction {
        Direction::In => SpringAnimation::new(SPRING).to(1.).from(0.),
        Direction::Out => SpringAnimation::new(SPRING).to(0.).from(1.),
    }
}

/// `surface`, positioned to fill its place, rising in or sinking away. `id`
/// and `run`, which counts each time it plays, keep each play its own.
pub fn surface<E>(
    surface: E,
    id: impl Into<SharedString>,
    run: usize,
    direction: Direction,
) -> SpringAnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    let id = format!("{}-{direction:?}", id.into());
    surface.with_spring(
        ElementId::NamedInteger(id.into(), run as u64),
        spring(direction),
        |surface, progress| {
            let progress = progress.clamp(0., 1.);
            surface
                .opacity(progress)
                .top(DROP * (1. - progress))
                .bottom(-DROP * (1. - progress))
        },
    )
}

/// The dimming behind, filling whatever holds it, darkening in or fading away.
pub fn dimming(
    id: impl Into<SharedString>,
    run: usize,
    direction: Direction,
) -> SpringAnimationElement<Div> {
    let id = format!("{}-dim-{direction:?}", id.into());
    div().absolute().inset_0().with_spring(
        ElementId::NamedInteger(id.into(), run as u64),
        spring(direction),
        |dim, progress| dim.bg(black().opacity(DIM * progress.clamp(0., 1.))),
    )
}

/// Keeps track of what a user of the animation shows, to know when it comes
/// in and when it goes away, and what to go away with.
pub struct Leaving<T: Clone> {
    shown: Option<T>,
    leaving: Option<(T, Instant)>,
    /// How many times it has come in, and gone away.
    pub came_in: usize,
    pub went_away: usize,
}

impl<T: Clone> Default for Leaving<T> {
    fn default() -> Self {
        Self {
            shown: None,
            leaving: None,
            came_in: 0,
            went_away: 0,
        }
    }
}

impl<T: Clone> Leaving<T> {
    /// Notes what is shown this frame, if anything, returning what is going
    /// away, while something is and has time left to go.
    pub fn frame(&mut self, shown: Option<T>) -> Option<T> {
        match (&shown, self.shown.take()) {
            (Some(_), None) => self.came_in += 1,
            (None, Some(last)) => {
                self.went_away += 1;
                self.leaving = Some((last, Instant::now()));
            }
            _ => {}
        }
        self.shown = shown;
        if self.shown.is_some()
            || self
                .leaving
                .as_ref()
                .is_some_and(|(_, left)| left.elapsed() >= LEAVE_TIME)
        {
            self.leaving = None;
        }
        self.leaving.as_ref().map(|(leaving, _)| leaving.clone())
    }

    /// Whether something is going away.
    #[cfg(test)]
    pub fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{LEAVE_TIME, Leaving};

    /// Showing something counts a coming in; no longer showing it counts a
    /// going away, and keeps it to go away with until its time is up; showing
    /// something again straight away stops it going.
    #[test]
    fn keeps_what_goes_away_until_it_has_gone() {
        let mut motion = Leaving::default();
        assert_eq!(motion.frame(None), None);
        assert_eq!(motion.frame(Some("a")), None);
        assert_eq!(motion.frame(Some("b")), None, "swapping doesn't go away");
        assert_eq!((motion.came_in, motion.went_away), (1, 0));
        assert_eq!(motion.frame(None), Some("b"));
        assert_eq!(motion.frame(None), Some("b"));
        assert_eq!((motion.came_in, motion.went_away), (1, 1));
        std::thread::sleep(LEAVE_TIME + Duration::from_millis(20));
        assert_eq!(motion.frame(None), None);
        assert!(!motion.is_leaving());

        motion.frame(Some("c"));
        assert_eq!(motion.frame(None), Some("c"));
        assert_eq!(motion.frame(Some("d")), None, "coming in stops the going");
        assert_eq!((motion.came_in, motion.went_away), (3, 2));
    }
}
