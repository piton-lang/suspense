//! Which of two overlapping things takes a press: the one with the smaller hit
//! area. A resize edge's hit area is a strip wider than the line it draws,
//! and lies over the edge of whatever is beside it, such as a list's
//! scrollbar; a scrollbar's button or thumb is smaller than the strip, so
//! takes a press there, while the strip is smaller than a scrollbar's track.
//!
//! Resize edges say where their hit areas are as they are painted, and
//! whatever overlaps them asks here, as it is pressed, which of them wins.
//! A resize and a scroll are never under way at the same time: while an edge
//! is dragged the wheel scrolls nothing.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::base::{ResizeHandleContext, ResizeHandleRenderer};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// How far a resize edge's hit area reaches either side of its line.
pub const RESIZE_REACH: Pixels = px(4.);

/// The resize edges' hit areas as last painted, by edge.
#[derive(Default)]
struct ResizeAreas {
    areas: HashMap<SharedString, Bounds<Pixels>>,
}

impl Global for ResizeAreas {}

fn area(bounds: &Bounds<Pixels>) -> f32 {
    f32::from(bounds.size.width) * f32::from(bounds.size.height)
}

/// Forgets the edges painted before, as a frame starts to be painted, and
/// keeps the wheel from scrolling anything while an edge is dragged: painted
/// first in the window, before anything else, so it hears the wheel first.
pub fn frame_start() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, cx| {
            if let Some(areas) = cx.try_global::<ResizeAreas>()
                && !areas.areas.is_empty()
            {
                cx.global_mut::<ResizeAreas>().areas.clear();
            }
            window.on_mouse_event(|_: &ScrollWheelEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && cx.has_active_drag() {
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// Notes the hit area of the resize edge `key`.
pub fn register_resize(key: SharedString, bounds: Bounds<Pixels>, cx: &mut App) {
    cx.default_global::<ResizeAreas>().areas.insert(key, bounds);
}

/// Whether a resize edge's hit area takes in `point`.
pub fn resize_at(point: Point<Pixels>, cx: &App) -> bool {
    cx.try_global::<ResizeAreas>()
        .is_some_and(|areas| areas.areas.values().any(|bounds| bounds.contains(&point)))
}

/// Whether a press at `point` belongs to what has the hit area `bounds`,
/// rather than to a resize edge whose smaller hit area also takes it in.
pub fn wins_at(bounds: Bounds<Pixels>, point: Point<Pixels>, cx: &App) -> bool {
    if !bounds.contains(&point) {
        return false;
    }
    let own = area(&bounds);
    cx.try_global::<ResizeAreas>().is_none_or(|areas| {
        !areas
            .areas
            .values()
            .any(|edge| edge.contains(&point) && area(edge) < own)
    })
}

/// Draws the line of a resize edge in group `group`, as gpui-kit does, and
/// notes the edge's hit area, which reaches [`RESIZE_REACH`] either side of it.
pub fn resize_edges(group: &'static str) -> ResizeHandleRenderer {
    Rc::new(move |edge: &ResizeHandleContext, _, cx| {
        let theme = cx.theme();
        let color = if edge.is_active() {
            theme.ring
        } else {
            theme.border
        };
        let horizontal = edge.axis() == Axis::Horizontal;
        let report = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, _, cx| {
                let (dx, dy) = if horizontal {
                    (RESIZE_REACH, px(0.))
                } else {
                    (px(0.), RESIZE_REACH)
                };
                let hit = Bounds::from_corners(
                    point(bounds.left() - dx, bounds.top() - dy),
                    point(bounds.right() + dx, bounds.bottom() + dy),
                );
                let key = format!(
                    "{group}@{:.0},{:.0}",
                    f32::from(bounds.left()),
                    f32::from(bounds.top())
                );
                register_resize(key.into(), hit, cx);
            },
        )
        .absolute()
        .size_full();
        Some(
            div()
                .relative()
                .flex_none()
                .bg(color)
                .when(horizontal, |line| line.h_full().w(px(1.)))
                .when(!horizontal, |line| line.w_full().h(px(1.)))
                .child(report)
                .into_any_element(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{register_resize, resize_at, wins_at};
    use gpui_kit::{Bounds, TestAppContext, point, px, size};

    #[gpui_kit::test]
    async fn the_smaller_hit_area_wins(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // A resize edge's strip, 9 wide and 400 tall.
            let edge = Bounds::new(point(px(96.), px(0.)), size(px(9.), px(400.)));
            register_resize("edge".into(), edge, cx);
            let at = point(px(98.), px(10.));
            // A scrollbar button beneath it, 18 square, is smaller: it wins.
            let button = Bounds::new(point(px(82.), px(0.)), size(px(18.), px(18.)));
            assert!(wins_at(button, at, cx));
            // The track, the column's full height, is larger: the edge wins.
            let track = Bounds::new(point(px(82.), px(18.)), size(px(18.), px(364.)));
            assert!(!wins_at(track, point(px(98.), px(100.)), cx));
            // Away from the edge, the track takes the press.
            assert!(wins_at(track, point(px(85.), px(100.)), cx));
            assert!(resize_at(at, cx));
            assert!(!resize_at(point(px(85.), px(100.)), cx));
        });
    }
}
