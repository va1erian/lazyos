#![forbid(unsafe_code)]

//! Round brush coverage: a dab is a disc, a segment the capsule swept by a disc.
//!
//! A capsule is exactly the union of every dab along the segment, so a fast
//! drag has no gaps and no dab spacing to tune; and because coverage is a
//! function of the distance to the segment, one pass per mouse-move is enough.

use crate::model::bitmap::Bitmap;
use crate::model::rect::Rect;

use super::options::BrushOptions;
use super::stroke::StrokeBuffer;

/// The centre of pixel `(x, y)` in continuous image coordinates.
pub fn pixel_center(x: i32, y: i32) -> (f32, f32) {
    (x as f32 + 0.5, y as f32 + 0.5)
}

/// Smooth 0..1 ramp, flat at both ends.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The brush's profile: coverage 0..=1 at distance `d` from the centre line.
///
/// Hardness 100 is a disc with a one-pixel antialiased rim; below that the
/// centre `hardness`% of the radius is solid and coverage eases to zero at
/// the rim.
fn profile(d: f32, radius: f32, hardness: u8) -> f32 {
    if hardness >= 100 {
        return (radius - d + 0.5).clamp(0.0, 1.0);
    }
    let inner = radius * f32::from(hardness) / 100.0;
    if d <= inner {
        1.0
    } else if d >= radius {
        0.0
    } else {
        1.0 - smoothstep((d - inner) / (radius - inner))
    }
}

impl StrokeBuffer {
    /// Adds a round dab centred on `at` (continuous coordinates). Returns the
    /// pixels it touched.
    pub fn dab(&mut self, layer: &Bitmap, at: (f32, f32), brush: BrushOptions) -> Rect {
        self.segment(layer, at, at, brush)
    }

    /// Adds the brush swept from `from` to `to` (continuous coordinates).
    /// Returns the pixels it touched.
    pub fn segment(
        &mut self,
        layer: &Bitmap,
        from: (f32, f32),
        to: (f32, f32),
        brush: BrushOptions,
    ) -> Rect {
        let brush = brush.clamped();
        let radius = brush.width as f32 / 2.0;
        // Hostile coordinates would overflow the f32 -> i32 conversion's
        // meaning; the layer is the only place that matters anyway.
        let limit = 1.0e6;
        let from = (from.0.clamp(-limit, limit), from.1.clamp(-limit, limit));
        let to = (to.0.clamp(-limit, limit), to.1.clamp(-limit, limit));
        let reach = radius + 1.0;
        let wanted = Rect {
            x0: (from.0.min(to.0) - reach).floor() as i32,
            y0: (from.1.min(to.1) - reach).floor() as i32,
            x1: (from.0.max(to.0) + reach).ceil() as i32,
            y1: (from.1.max(to.1) + reach).ceil() as i32,
        };
        let rect = self.reserve(layer, wanted);
        if rect.is_empty() {
            return Rect::EMPTY;
        }
        let (ab_x, ab_y) = (to.0 - from.0, to.1 - from.1);
        let length2 = ab_x * ab_x + ab_y * ab_y;
        for y in rect.y0..rect.y1 {
            let (x_lo, x_hi) = row_span(from, to, y, reach, rect);
            let cy = y as f32 + 0.5;
            for x in x_lo..x_hi {
                let cx = x as f32 + 0.5;
                let t = if length2 > 0.0 {
                    (((cx - from.0) * ab_x + (cy - from.1) * ab_y) / length2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (dx, dy) = (cx - (from.0 + t * ab_x), cy - (from.1 + t * ab_y));
                let d2 = dx * dx + dy * dy;
                if d2 > reach * reach {
                    continue;
                }
                let c = profile(d2.sqrt(), radius, brush.hardness);
                let value = if brush.antialias {
                    (c * 255.0 + 0.5) as u8
                } else if c >= 0.5 {
                    255
                } else {
                    0
                };
                if value != 0 {
                    self.raise(x, y, value);
                }
            }
        }
        self.mark(rect);
        rect
    }
}

/// The columns of row `y` the swept brush can reach, within `rect`: the
/// segment's x extent over the rows `y +- reach`, widened by `reach`.
fn row_span(from: (f32, f32), to: (f32, f32), y: i32, reach: f32, rect: Rect) -> (i32, i32) {
    let (lo, hi) = (y as f32 - reach, y as f32 + 1.0 + reach);
    let (x_a, x_b) = if (to.1 - from.1).abs() < 1.0e-6 {
        (from.0, to.0)
    } else {
        // Where the segment enters and leaves the band of rows, as fractions.
        let t_at = |yy: f32| ((yy - from.1) / (to.1 - from.1)).clamp(0.0, 1.0);
        let (t0, t1) = (t_at(lo), t_at(hi));
        (from.0 + t0 * (to.0 - from.0), from.0 + t1 * (to.0 - from.0))
    };
    let x_lo = ((x_a.min(x_b) - reach).floor() as i32).max(rect.x0);
    let x_hi = ((x_a.max(x_b) + reach).ceil() as i32).min(rect.x1);
    (x_lo, x_hi.max(x_lo))
}
