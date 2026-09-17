//! Finds double borders in a painted frame: two visible one-pixel border
//! lines lying directly against each other, where a single line was meant.

use gpui_kit::{Bounds, Quad, ScaledPixels, Window};

/// A border line: which way it runs, where across it lies, and where along.
#[derive(Clone, Copy, Debug)]
struct Line {
    horizontal: bool,
    /// The line's span across its direction: y for a horizontal line.
    across: (f32, f32),
    /// Its span along its direction: x for a horizontal line.
    along: (f32, f32),
    quad: usize,
}

/// How long two lines must run side by side to count as a double border.
const MIN_OVERLAP: f32 = 6.;

/// The widest line, in scaled pixels, counted as a thin border: one pixel at
/// up to twice the scale.
const MAX_WIDTH: f32 = 2.5;

fn visible(bounds: &Bounds<ScaledPixels>, mask: &Bounds<ScaledPixels>) -> Option<Bounds<f32>> {
    let (x0, y0) = (
        bounds.origin.x.0.max(mask.origin.x.0),
        bounds.origin.y.0.max(mask.origin.y.0),
    );
    let x1 = (bounds.origin.x.0 + bounds.size.width.0).min(mask.origin.x.0 + mask.size.width.0);
    let y1 = (bounds.origin.y.0 + bounds.size.height.0).min(mask.origin.y.0 + mask.size.height.0);
    (x1 > x0 && y1 > y0).then(|| Bounds {
        origin: gpui_kit::point(x0, y0),
        size: gpui_kit::size(x1 - x0, y1 - y0),
    })
}

fn lines(quads: &[Quad]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (ix, quad) in quads.iter().enumerate() {
        let b = &quad.bounds;
        let (x0, y0) = (b.origin.x.0, b.origin.y.0);
        let (x1, y1) = (x0 + b.size.width.0, y0 + b.size.height.0);
        let w = &quad.border_widths;
        // A thin filled strip, as a divider is drawn, is a line too.
        let borderless = w.top.0 <= 0. && w.bottom.0 <= 0. && w.left.0 <= 0. && w.right.0 <= 0.;
        if borderless {
            let solid = quad
                .background
                .as_solid()
                .is_some_and(|color| color.a >= 0.05 && color.s <= 0.2);
            let (width, height) = (b.size.width.0, b.size.height.0);
            let strip = if height > 0. && height <= MAX_WIDTH && width >= MIN_OVERLAP {
                Some(Line {
                    horizontal: true,
                    across: (y0, y1),
                    along: (x0, x1),
                    quad: ix,
                })
            } else if width > 0. && width <= MAX_WIDTH && height >= MIN_OVERLAP {
                Some(Line {
                    horizontal: false,
                    across: (x0, x1),
                    along: (y0, y1),
                    quad: ix,
                })
            } else {
                None
            };
            // A thin strip sharing an edge with a fill of the same colour, as
            // the track shows around a scrollbar's thumb, is part of that fill
            // rather than a line.
            let joined = |_: &Line| {
                quads.iter().enumerate().any(|(other_ix, other)| {
                    if other_ix == ix || other.background != quad.background {
                        return false;
                    }
                    let ob = &other.bounds;
                    let (ox0, oy0) = (ob.origin.x.0, ob.origin.y.0);
                    let (ox1, oy1) = (ox0 + ob.size.width.0, oy0 + ob.size.height.0);
                    let across = (ox1 - x0).abs() < 0.5 || (ox0 - x1).abs() < 0.5;
                    let down = (oy1 - y0).abs() < 0.5 || (oy0 - y1).abs() < 0.5;
                    (across && oy0 < y1 && oy1 > y0) || (down && ox0 < x1 && ox1 > x0)
                })
            };
            if let Some(line) = strip.filter(|line| solid && !joined(line)) {
                let mask = &quad.content_mask.bounds;
                if visible(&quad.bounds, mask).is_some() {
                    lines.push(line);
                }
            }
            continue;
        }
        // Only neutral lines: a coloured outline, such as focus, marks state
        // rather than structure, and may sit against a border.
        if quad.border_color.a < 0.05 || quad.border_color.s > 0.2 {
            continue;
        }
        let sides = [
            (w.top.0, true, (y0, y0 + w.top.0), (x0, x1)),
            (w.bottom.0, true, (y1 - w.bottom.0, y1), (x0, x1)),
            (w.left.0, false, (x0, x0 + w.left.0), (y0, y1)),
            (w.right.0, false, (x1 - w.right.0, x1), (y0, y1)),
        ];
        for (width, horizontal, across, along) in sides {
            // Only thin lines: a focus ring or an accent bar is meant to sit
            // against a border.
            if width <= 0. || width > MAX_WIDTH {
                continue;
            }
            // Clip the line to what is actually shown.
            let rect = if horizontal {
                Bounds {
                    origin: gpui_kit::point(ScaledPixels(along.0), ScaledPixels(across.0)),
                    size: gpui_kit::size(
                        ScaledPixels(along.1 - along.0),
                        ScaledPixels(across.1 - across.0),
                    ),
                }
            } else {
                Bounds {
                    origin: gpui_kit::point(ScaledPixels(across.0), ScaledPixels(along.0)),
                    size: gpui_kit::size(
                        ScaledPixels(across.1 - across.0),
                        ScaledPixels(along.1 - along.0),
                    ),
                }
            };
            let Some(shown) = visible(&rect, &quad.content_mask.bounds) else {
                continue;
            };
            let (across, along) = if horizontal {
                (
                    (shown.origin.y, shown.origin.y + shown.size.height),
                    (shown.origin.x, shown.origin.x + shown.size.width),
                )
            } else {
                (
                    (shown.origin.x, shown.origin.x + shown.size.width),
                    (shown.origin.y, shown.origin.y + shown.size.height),
                )
            };
            lines.push(Line {
                horizontal,
                across,
                along,
                quad: ix,
            });
        }
    }
    lines
}

/// Each pair of border lines, from different quads, lying directly side by
/// side along at least a few pixels, described with both quads' bounds.
pub fn find(window: &Window) -> Vec<String> {
    let mut quads = window.painted_quads();
    // In the order they are drawn, so what comes later is on top.
    quads.sort_by_key(|quad| quad.order);
    let lines = lines(&quads);
    // Translucent black laid over much of the window, such as behind an inset
    // panel: lines on either side of it are in different layers.
    let dims: Vec<usize> = quads
        .iter()
        .enumerate()
        .filter(|(_, quad)| {
            quad.background
                .as_solid()
                .is_some_and(|c| c.l < 0.05 && c.a > 0. && c.a < 0.95)
                && quad.bounds.size.width.0 >= 400.
                && quad.bounds.size.height.0 >= 400.
        })
        .map(|(ix, _)| ix)
        .collect();
    // Whether an opaque quad drawn after `line`'s covers the stretch of it
    // from `from` to `to`.
    let covered = |line: &Line, from: f32, to: f32| {
        quads
            .iter()
            .enumerate()
            .skip(line.quad + 1)
            .any(|(_, quad)| {
                if !quad.background.as_solid().is_some_and(|c| c.a >= 0.99) {
                    return false;
                }
                let b = &quad.bounds;
                let (x0, y0) = (b.origin.x.0, b.origin.y.0);
                let (x1, y1) = (x0 + b.size.width.0, y0 + b.size.height.0);
                let (lx0, lx1, ly0, ly1) = if line.horizontal {
                    (from, to, line.across.0, line.across.1)
                } else {
                    (line.across.0, line.across.1, from, to)
                };
                x0 <= lx0 + 0.01 && x1 >= lx1 - 0.01 && y0 <= ly0 + 0.01 && y1 >= ly1 - 0.01
            })
    };
    let mut found = Vec::new();
    for (i, a) in lines.iter().enumerate() {
        for b in &lines[i + 1..] {
            if a.horizontal != b.horizontal || a.quad == b.quad {
                continue;
            }
            // Touching: one ends where the other starts, within half a pixel.
            let touching =
                (a.across.1 - b.across.0).abs() < 0.5 || (b.across.1 - a.across.0).abs() < 0.5;
            if !touching {
                continue;
            }
            let overlap = a.along.1.min(b.along.1) - a.along.0.max(b.along.0);
            if overlap < MIN_OVERLAP {
                continue;
            }
            let (from, to) = (a.along.0.max(b.along.0), a.along.1.min(b.along.1));
            if covered(a, from, to) || covered(b, from, to) {
                continue;
            }
            let (low, high) = (a.quad.min(b.quad), a.quad.max(b.quad));
            if dims.iter().any(|&dim| dim > low && dim < high) {
                continue;
            }
            let (qa, qb) = (&quads[a.quad], &quads[b.quad]);
            found.push(format!(
                "{} lines at {:.0}/{:.0} along {:.0}..{:.0}: {:?} (borders {:?}, fill {:?}) and {:?} (borders {:?}, fill {:?})",
                if a.horizontal { "horizontal" } else { "vertical" },
                a.across.0,
                b.across.0,
                a.along.0.max(b.along.0),
                a.along.1.min(b.along.1),
                qa.bounds,
                qa.border_widths,
                qa.background.as_solid(),
                qb.bounds,
                qb.border_widths,
                qb.background.as_solid(),
            ));
        }
    }
    found
}

/// Fails if the frame last painted in `window` has any double borders.
pub fn assert_none(window: &Window) {
    let found = find(window);
    assert!(
        found.is_empty(),
        "double borders, where a single line was meant:\n{}",
        found.join("\n")
    );
}
