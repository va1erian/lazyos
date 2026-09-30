//! Anti-aliased title-bar button glyphs, drawn as stroked line segments.
//!
//! Each pixel is sampled on a `SUB x SUB` grid and its coverage is the share
//! of samples within half a stroke width of the segment. Everything is
//! integer maths in 1/`UNIT` pixel units, so it needs no float support.

use user::messenger::display::{Canvas, Color, Rect};

/// Sub-pixel samples per axis.
const SUB: i32 = 8;
/// Fixed-point units per pixel.
const UNIT: i32 = 16;
/// Twice the stroke width in pixels: 1.5px reads crisp yet smooth here.
const STROKE_X2: i32 = 3;

/// A point in fixed-point units.
type Point = (i32, i32);
/// A line segment between two points.
type Segment = (Point, Point);

/// Squared distance from `p` to segment `a`-`b`, all in fixed-point units.
fn dist2(p: (i64, i64), a: (i64, i64), b: (i64, i64)) -> i64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0 {
        0
    } else {
        ((p.0 - a.0) * dx + (p.1 - a.1) * dy).clamp(0, len2)
    };
    // Closest point, scaled by `len2` to stay exact.
    let cx = a.0 * len2 + t * dx - p.0 * len2;
    let cy = a.1 * len2 + t * dy - p.1 * len2;
    // (cx² + cy²) / len2², computed without overflow for icon-sized inputs.
    if len2 == 0 {
        (p.0 - a.0).pow(2) + (p.1 - a.1).pow(2)
    } else {
        (cx * cx + cy * cy) / (len2 * len2)
    }
}

/// Stroke `lines` (fixed-point endpoints) over the pixels of `bounds`.
fn stroke(screen: &mut Canvas, bounds: Rect, lines: &[Segment], color: Color, clip: Rect) {
    let radius = (STROKE_X2 * UNIT / 4) as i64;
    // Skip supersampling pixels that `blend_pixel` would reject anyway.
    let bounds = bounds
        .intersect(clip)
        .intersect(Rect::new(0, 0, screen.width(), screen.height()));
    for py in bounds.y..bounds.y + bounds.h {
        for px in bounds.x..bounds.x + bounds.w {
            let mut hits = 0;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    let p = (
                        ((px * SUB + sx) * UNIT / SUB + UNIT / (2 * SUB)) as i64,
                        ((py * SUB + sy) * UNIT / SUB + UNIT / (2 * SUB)) as i64,
                    );
                    let near = lines.iter().any(|&(a, b)| {
                        dist2(p, (a.0 as i64, a.1 as i64), (b.0 as i64, b.1 as i64))
                            <= radius * radius
                    });
                    hits += near as i32;
                }
            }
            let alpha = (hits * 255 / (SUB * SUB)) as u8;
            screen.blend_pixel(px, py, color, alpha, clip);
        }
    }
}

/// The centre of `rect` in fixed-point units.
fn centre(rect: Rect) -> (i32, i32) {
    (
        (rect.x * 2 + rect.w) * UNIT / 2,
        (rect.y * 2 + rect.h) * UNIT / 2,
    )
}

/// A close "X" centred in `button`.
pub(super) fn draw_close(screen: &mut Canvas, button: Rect, color: Color, clip: Rect) {
    let (cx, cy) = centre(button);
    let h = 4 * UNIT + UNIT / 2; // half-extent 4.5px
    let lines = [
        ((cx - h, cy - h), (cx + h, cy + h)),
        ((cx - h, cy + h), (cx + h, cy - h)),
    ];
    stroke(screen, button, &lines, color, clip);
}

/// A minimize "-" centred in `button`.
pub(super) fn draw_minimize(screen: &mut Canvas, button: Rect, color: Color, clip: Rect) {
    let (cx, cy) = centre(button);
    let h = 5 * UNIT;
    stroke(screen, button, &[((cx - h, cy), (cx + h, cy))], color, clip);
}
