//! The tilt: something picked up, drawn turned a few degrees about its
//! middle. Nothing in the window can be drawn turned, so it is drawn as
//! narrow upright strips side by side, each a clipped copy of the whole,
//! each lowered by how far its middle is from the whole's middle: over a
//! few degrees this reads as the turn itself, its top and foot sloping.

use gpui_kit::*;

/// How wide each strip is: narrow enough that the steps between them,
/// under half a pixel at a few degrees, don't show.
const STRIP: Pixels = px(8.);

/// How far past its edges each strip reaches, to keep its shadow.
const REACH: Pixels = px(24.);

/// `size`, drawn turned `degrees` clockwise, from copies `build` makes, one
/// for each strip; not turned at all, the one copy as it is. What holds it
/// is `size`, and the turned copy may reach past it.
pub fn tilted(size: Size<Pixels>, degrees: f32, build: impl Fn() -> AnyElement) -> Div {
    let holder = div().relative().w(size.width).h(size.height).flex_none();
    if degrees.abs() < 0.05 {
        return holder.child(build());
    }
    let slope = degrees.to_radians().tan();
    let mut x = -REACH;
    let mut strips = Vec::new();
    while x < size.width + REACH {
        let lower = (x + STRIP / 2. - size.width / 2.) * slope;
        strips.push(
            div()
                .absolute()
                .left(x)
                .top(lower - REACH)
                .w(STRIP)
                .h(size.height + REACH * 2.)
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .left(-x)
                        .top(REACH)
                        .w(size.width)
                        .h(size.height)
                        .child(build()),
                ),
        );
        x += STRIP;
    }
    holder.children(strips)
}
