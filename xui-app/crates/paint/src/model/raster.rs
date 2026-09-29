#![forbid(unsafe_code)]

//! Stroke rasterization: brush-aware lines, rectangles and ellipses.
//!
//! Every entry point clamps its coordinates to a bounded range first, so a
//! hostile `i32::MIN`/`i32::MAX` pair can never overflow the Bresenham deltas.

use super::bitmap::{Bitmap, Pixel};

/// Coordinates are clamped to this magnitude before any arithmetic.
const LIMIT: i32 = 1 << 20;
/// The largest brush diameter the model accepts.
pub const MAX_BRUSH: u32 = 64;

/// A point pair, clamped and ordered for a bounding-box shape.
fn bounds(from: (i32, i32), to: (i32, i32)) -> (i32, i32, i32, i32) {
    let (x0, y0) = (from.0.clamp(-LIMIT, LIMIT), from.1.clamp(-LIMIT, LIMIT));
    let (x1, y1) = (to.0.clamp(-LIMIT, LIMIT), to.1.clamp(-LIMIT, LIMIT));
    (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
}

/// Stamps a filled disc of `diameter` centred on `(cx, cy)`.
pub fn stamp(bitmap: &mut Bitmap, cx: i32, cy: i32, diameter: u32, color: Pixel) {
    let diameter = diameter.clamp(1, MAX_BRUSH) as i32;
    let radius = diameter as f32 / 2.0;
    let reach = radius.ceil() as i32;
    let limit = radius * radius;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let x = cx.saturating_add(dx);
            let y = cy.saturating_add(dy);
            if !bitmap.contains(x, y) {
                continue;
            }
            let d = (dx * dx + dy * dy) as f32;
            if d <= limit {
                bitmap.put(x, y, color);
            }
        }
    }
}

/// Rasterizes a brush-aware line from `from` to `to`, stamping a disc of
/// `diameter` at every Bresenham cell so a fast drag leaves no gap.
pub fn line(bitmap: &mut Bitmap, from: (i32, i32), to: (i32, i32), diameter: u32, color: Pixel) {
    let mut x = from.0.clamp(-LIMIT, LIMIT);
    let mut y = from.1.clamp(-LIMIT, LIMIT);
    let x1 = to.0.clamp(-LIMIT, LIMIT);
    let y1 = to.1.clamp(-LIMIT, LIMIT);
    let dx = (x1 - x).abs();
    let dy = -(y1 - y).abs();
    let sx = if x < x1 { 1 } else { -1 };
    let sy = if y < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        stamp(bitmap, x, y, diameter, color);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * error;
        if e2 >= dy {
            error += dy;
            x += sx;
        }
        if e2 <= dx {
            error += dx;
            y += sy;
        }
    }
}

/// Rasterizes a rectangle outline through `from` and `to`.
pub fn rectangle(
    bitmap: &mut Bitmap,
    from: (i32, i32),
    to: (i32, i32),
    diameter: u32,
    color: Pixel,
) {
    let (left, top, right, bottom) = bounds(from, to);
    for x in left..=right {
        stamp(bitmap, x, top, diameter, color);
        stamp(bitmap, x, bottom, diameter, color);
    }
    for y in top..=bottom {
        stamp(bitmap, left, y, diameter, color);
        stamp(bitmap, right, y, diameter, color);
    }
}

/// Rasterizes an ellipse outline inscribed in the box through `from` and `to`.
pub fn ellipse(bitmap: &mut Bitmap, from: (i32, i32), to: (i32, i32), diameter: u32, color: Pixel) {
    let (left, top, right, bottom) = bounds(from, to);
    let radius_x = (right - left) as f32 / 2.0;
    let radius_y = (bottom - top) as f32 / 2.0;
    if radius_x < 0.5 || radius_y < 0.5 {
        let cx = (left + right) / 2;
        let cy = (top + bottom) / 2;
        stamp(bitmap, cx, cy, diameter, color);
        return;
    }
    let center_x = (left + right) as f32 / 2.0;
    let center_y = (top + bottom) as f32 / 2.0;
    // Enough samples that adjacent points do not leave a gap on the outline.
    let circumference = std::f32::consts::TAU * (radius_x + radius_y) / 2.0;
    let steps = (circumference * 2.0).ceil().clamp(16.0, 8192.0) as i32;
    for step in 0..steps {
        let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
        let x = center_x + radius_x * angle.cos();
        let y = center_y + radius_y * angle.sin();
        stamp(bitmap, x.round() as i32, y.round() as i32, diameter, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(bitmap: &Bitmap) -> usize {
        bitmap
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| **pixel != [255, 255, 255, 255])
            .count()
    }

    #[test]
    fn a_single_pixel_line_touches_only_its_cell() {
        let mut bitmap = Bitmap::white(8, 8);
        line(&mut bitmap, (2, 3), (2, 3), 1, [0, 0, 0, 255]);
        assert_eq!(lit(&bitmap), 1);
        assert_eq!(bitmap.get(2, 3), Some([0, 0, 0, 255]));
    }

    #[test]
    fn a_diagonal_line_has_no_gaps() {
        let mut bitmap = Bitmap::white(16, 16);
        line(&mut bitmap, (0, 0), (10, 10), 1, [0, 0, 0, 255]);
        for i in 0..=10 {
            assert_eq!(bitmap.get(i, i), Some([0, 0, 0, 255]), "gap at {i}");
        }
    }

    #[test]
    fn a_shallow_line_has_no_gaps() {
        let mut bitmap = Bitmap::white(32, 8);
        line(&mut bitmap, (0, 0), (30, 3), 1, [0, 0, 0, 255]);
        for x in 0..=30 {
            let y = (x as f32 * 3.0 / 30.0).round() as i32;
            assert!(
                (y - 1..=y + 1).any(|candidate| bitmap.get(x, candidate) == Some([0, 0, 0, 255])),
                "gap near x={x}"
            );
        }
    }

    #[test]
    fn a_brush_paints_more_than_a_pencil() {
        let mut thin = Bitmap::white(32, 32);
        let mut thick = Bitmap::white(32, 32);
        line(&mut thin, (4, 16), (28, 16), 1, [0, 0, 0, 255]);
        line(&mut thick, (4, 16), (28, 16), 8, [0, 0, 0, 255]);
        assert!(lit(&thick) > lit(&thin));
    }

    #[test]
    fn extreme_coordinates_do_not_panic_or_overflow() {
        let mut bitmap = Bitmap::white(16, 16);
        line(
            &mut bitmap,
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
            1,
            [0, 0, 0, 255],
        );
        rectangle(&mut bitmap, (i32::MAX, 0), (i32::MIN, 0), 4, [0, 0, 0, 255]);
        ellipse(&mut bitmap, (i32::MIN, 0), (i32::MAX, 0), 4, [0, 0, 0, 255]);
        assert!(bitmap.contains(0, 0));
    }

    #[test]
    fn a_rectangle_outline_is_hollow() {
        let mut bitmap = Bitmap::white(16, 16);
        rectangle(&mut bitmap, (2, 2), (12, 12), 1, [0, 0, 0, 255]);
        assert_eq!(bitmap.get(2, 2), Some([0, 0, 0, 255]));
        assert_eq!(bitmap.get(7, 7), Some([255, 255, 255, 255]));
    }
}
