//! Reading a green: a lattice of short arrows over it, pointing downhill
//! (the way a putt breaks), as the putting views of the old games drew
//! them. Computed once per green from the elevation.

use crate::course::Hole;
use crate::math::{Vec2, Vec3};
use crate::render::overlay::GridArrow;
use crate::render::palette as pal;
use crate::render::Bake;

/// Metres between arrows.
const SPACING: f32 = 1.5;
/// Slope (rise over run) from which an arrow is drawn at all, and the
/// steepness of a yellow and of a red one.
const FLAT: f32 = 0.004;
const GENTLE: f32 = 0.015;
const STEEP: f32 = 0.04;

/// Whether `p` is inside the polygon (even-odd rule).
pub fn inside(polygon: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = polygon.len();
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// The arrows for `hole`'s green.
pub fn grid(hole: &Hole, bake: &Bake) -> Vec<GridArrow> {
    let poly = &hole.green;
    if poly.len() < 3 {
        return Vec::new();
    }
    let (mut lo, mut hi) = (Vec2::new(f32::MAX, f32::MAX), Vec2::new(f32::MIN, f32::MIN));
    for p in poly {
        lo = Vec2::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Vec2::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    let mut out = Vec::new();
    let mut z = lo.y;
    while z <= hi.y {
        let mut x = lo.x;
        while x <= hi.x {
            let p = Vec2::new(x, z);
            if inside(poly, p) {
                if let Some(a) = arrow(bake, p) {
                    out.push(a);
                }
            }
            x += SPACING;
        }
        z += SPACING;
    }
    out
}

fn arrow(bake: &Bake, p: Vec2) -> Option<GridArrow> {
    let h = |x: f32, z: f32| bake.height_at(x, z);
    let gx = (h(p.x + 1.0, p.y) - h(p.x - 1.0, p.y)) * 0.5;
    let gz = (h(p.x, p.y + 1.0) - h(p.x, p.y - 1.0)) * 0.5;
    let slope = (gx * gx + gz * gz).sqrt();
    if slope < FLAT {
        return None;
    }
    // Downhill is against the gradient; the arrow grows with the slope.
    let down = Vec2::new(-gx, -gz) * (1.0 / slope);
    let length = 0.35 + (slope / STEEP).min(1.0) * 0.8;
    let end = p + down * length;
    let lift = 0.04;
    let color = if slope >= STEEP {
        pal::RED
    } else if slope >= GENTLE {
        pal::YELLOW
    } else {
        pal::WHITE
    };
    Some(GridArrow {
        from: Vec3::new(p.x, h(p.x, p.y) + lift, p.y),
        to: Vec3::new(end.x, h(end.x, end.y) + lift, end.y),
        color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_is_inside_a_square_or_not() {
        let sq = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        assert!(inside(&sq, Vec2::new(5.0, 5.0)));
        assert!(!inside(&sq, Vec2::new(11.0, 5.0)));
        assert!(!inside(&sq, Vec2::new(5.0, -0.1)));
    }
}
