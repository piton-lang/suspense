//! Draws a painted frame's quads, backgrounds and borders, into an image, so
//! a test can show what a window looks like where no renderer is available.
//! Text and icons aren't drawn. For looking at a frame while working on it;
//! no test keeps an image.
#![allow(dead_code)]

use gpui_kit::{Hsla, Rgba, Window};

/// Writes the window's last frame as a binary PPM at `path`.
pub fn save(window: &Window, path: &std::path::Path) {
    let mut quads = window.painted_quads();
    quads.sort_by_key(|quad| quad.order);
    let (w, h) = quads.iter().fold((1usize, 1usize), |(w, h), q| {
        let m = &q.content_mask.bounds;
        (
            w.max((m.origin.x.0 + m.size.width.0).ceil() as usize),
            h.max((m.origin.y.0 + m.size.height.0).ceil() as usize),
        )
    });
    let (w, h) = (w.min(8000), h.min(8000));
    let mut px = vec![[0f32; 3]; w * h];
    let mut blend = |x: usize, y: usize, color: Hsla| {
        if x >= w || y >= h || color.a <= 0. {
            return;
        }
        let c: Rgba = color.into();
        let p = &mut px[y * w + x];
        let a = c.a;
        p[0] = p[0] * (1. - a) + c.r * a;
        p[1] = p[1] * (1. - a) + c.g * a;
        p[2] = p[2] * (1. - a) + c.b * a;
    };
    for q in &quads {
        let (bx0, by0) = (q.bounds.origin.x.0, q.bounds.origin.y.0);
        let (bx1, by1) = (bx0 + q.bounds.size.width.0, by0 + q.bounds.size.height.0);
        let m = &q.content_mask.bounds;
        let (x0, y0) = (bx0.max(m.origin.x.0).max(0.), by0.max(m.origin.y.0).max(0.));
        let (x1, y1) = (
            bx1.min(m.origin.x.0 + m.size.width.0),
            by1.min(m.origin.y.0 + m.size.height.0),
        );
        let bw = &q.border_widths;
        let fill = q.background.as_solid();
        for y in (y0.floor() as usize)..(y1.ceil().max(0.) as usize) {
            for x in (x0.floor() as usize)..(x1.ceil().max(0.) as usize) {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let border = fy < by0 + bw.top.0
                    || fy > by1 - bw.bottom.0
                    || fx < bx0 + bw.left.0
                    || fx > bx1 - bw.right.0;
                if border {
                    blend(x, y, q.border_color);
                } else if let Some(fill) = fill {
                    blend(x, y, fill);
                }
            }
        }
    }
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in px {
        for c in p {
            out.push((c.clamp(0., 1.) * 255.).round() as u8);
        }
    }
    std::fs::write(path, out).unwrap();
}
