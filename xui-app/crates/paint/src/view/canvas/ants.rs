#![forbid(unsafe_code)]

//! The marching ants: the selection's outline as alternating black and white
//! dashes, drawn in screen space over the picture, never into pixels.
//!
//! Each outline segment is laid down white, then the odd dashes of a fixed
//! pattern are painted black over it, so the line shows on any picture. The
//! pattern is positioned by view coordinates plus a phase the canvas timer
//! advances, which is what makes the dashes crawl.

use xui_core::Color;
use xui_core::backend::Canvas;
use xui_core::geometry::{Point, Rect};

use super::state::CanvasState;
use crate::model::Edge;
use crate::viewport::scaled_px;

/// The length of one dash in view pixels at 96 dpi; black and white alternate.
pub(super) const DASH: i32 = 4;

/// Draws the outline of the state's selection clipped to `bounds` (the node).
pub(super) fn draw(canvas: &mut dyn Canvas, state: &CanvasState, bounds: Rect) {
    let viewport = state.viewport();
    let dash = scaled_px(DASH, state.dpi);
    // The crawl keeps its speed in dashes per tick.
    let phase = state.phase as i32 * dash / DASH;
    let (white, black) = (Color::rgb(255, 255, 255), Color::rgb(0, 0, 0));
    let (width, height) = (bounds.width(), bounds.height());
    for edge in &state.edges {
        let (x0, y0) = viewport.to_view(edge.x0, edge.y0);
        let (x1, y1) = viewport.to_view(edge.x1, edge.y1);
        if edge.horizontal() {
            if y0 < 0 || y0 >= height || x1 < 0 || x0 > width {
                continue;
            }
            let (from, to) = (x0.max(-dash), x1.min(width + dash));
            let at = |p: i32| Point::new(bounds.left + p, bounds.top + y0);
            canvas.draw_line(at(from), at(to), white, 1.0);
            for (a, b) in black_dashes(from, to, phase, dash) {
                canvas.draw_line(at(a), at(b), black, 1.0);
            }
        } else {
            if x0 < 0 || x0 >= width || y1 < 0 || y0 > height {
                continue;
            }
            let (from, to) = (y0.max(-dash), y1.min(height + dash));
            let at = |p: i32| Point::new(bounds.left + x0, bounds.top + p);
            canvas.draw_line(at(from), at(to), white, 1.0);
            for (a, b) in black_dashes(from, to, phase, dash) {
                canvas.draw_line(at(a), at(b), black, 1.0);
            }
        }
    }
}

/// The lasso being drawn, as an open polyline in the ants' style: white, then
/// black dashes along each segment.
pub(super) fn draw_lasso(canvas: &mut dyn Canvas, state: &CanvasState, bounds: Rect) {
    if state.lasso.len() < 2 {
        return;
    }
    let viewport = state.viewport();
    let dash = scaled_px(DASH, state.dpi);
    let phase = state.phase as i32 * dash / DASH;
    let (white, black) = (Color::rgb(255, 255, 255), Color::rgb(0, 0, 0));
    let at = |(x, y): (i32, i32)| {
        let (vx, vy) = viewport.to_view(x, y);
        (bounds.left + vx, bounds.top + vy)
    };
    let mut walked = 0.0f64;
    for pair in state.lasso.windows(2) {
        let (a, b) = (at(pair[0]), at(pair[1]));
        let point = |p: (i32, i32)| Point::new(p.0, p.1);
        canvas.draw_line(point(a), point(b), white, 1.0);
        let length = f64::from(b.0 - a.0).hypot(f64::from(b.1 - a.1));
        if length > 0.0 {
            // Dashes are laid along the whole line by distance walked.
            let at_distance = |d: f64| {
                let t = (d - walked) / length;
                (
                    a.0 + (f64::from(b.0 - a.0) * t).round() as i32,
                    a.1 + (f64::from(b.1 - a.1) * t).round() as i32,
                )
            };
            for (from, to) in black_dashes(walked as i32, (walked + length) as i32, phase, dash) {
                let (s, e) = (at_distance(f64::from(from)), at_distance(f64::from(to)));
                canvas.draw_line(point(s), point(e), black, 1.0);
            }
        }
        walked += length;
    }
}

/// The black dashes of `from..to` along an axis: the odd cells of a pattern
/// shifted by `phase`, clipped to the span.
pub(super) fn black_dashes(from: i32, to: i32, phase: i32, dash: i32) -> Vec<(i32, i32)> {
    let mut dashes = Vec::new();
    if to <= from {
        return dashes;
    }
    let first = (from + phase).div_euclid(dash);
    let last = (to + phase).div_euclid(dash);
    for cell in first..=last {
        if cell & 1 == 0 {
            continue;
        }
        let a = (cell * dash - phase).max(from);
        let b = ((cell + 1) * dash - phase).min(to);
        if a < b {
            dashes.push((a, b));
        }
    }
    dashes
}

/// Whether the outline has anything to draw.
pub(super) fn is_empty(edges: &[Edge]) -> bool {
    edges.is_empty()
}
