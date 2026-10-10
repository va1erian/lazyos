//! The hole layouts rasterized into per-cell signed distance fields, which
//! both the sculpting (stage 5) and the material painting (stage 6) read.
//! Each shape is only evaluated over its own bounding box.

use crate::field::Field;
use crate::math::{polyline_signed, Vec2};

use super::layout::HolePlan;

/// Far from every shape of a kind.
pub const FAR: f32 = 1.0e4;

/// The minimum signed distance to each kind of shape, per cell.
pub struct Features {
    pub green: Field<f32>,
    pub tee: Field<f32>,
    pub fairway: Field<f32>,
    pub bunker: Field<f32>,
    /// Distance to the nearest centerline minus that hole's corridor
    /// half-width: negative inside a corridor.
    pub corridor: Field<f32>,
    /// The hole whose corridor is nearest (255: none within reach).
    pub hole_of: Field<u8>,
}

/// Cells of the `size` grid inside `lo..hi`, as index ranges.
fn cells(lo: Vec2, hi: Vec2, size: usize) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let clamp = |v: f32| (v.max(0.0) as usize).min(size);
    (
        (clamp(lo.x)..clamp(hi.x + 1.0)),
        (clamp(lo.y)..clamp(hi.y + 1.0)),
    )
}

/// Runs `f(cell index, cell centre)` over the cells within `margin` of the
/// bounding box of `points`.
fn over_box(points: &[Vec2], margin: f32, size: usize, mut f: impl FnMut(usize, Vec2)) {
    let lo = points.iter().fold(Vec2::new(f32::MAX, f32::MAX), |a, p| {
        Vec2::new(a.x.min(p.x), a.y.min(p.y))
    });
    let hi = points.iter().fold(Vec2::new(f32::MIN, f32::MIN), |a, p| {
        Vec2::new(a.x.max(p.x), a.y.max(p.y))
    });
    let (xs, ys) = cells(
        lo - Vec2::new(margin, margin),
        hi + Vec2::new(margin, margin),
        size,
    );
    for y in ys {
        for x in xs.clone() {
            f(y * size + x, Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
        }
    }
}

/// Rasterizes every hole's shapes.
pub fn rasterize(plans: &[HolePlan], size: usize) -> Features {
    let mut out = Features {
        green: Field::new(size, size, FAR),
        tee: Field::new(size, size, FAR),
        fairway: Field::new(size, size, FAR),
        bunker: Field::new(size, size, FAR),
        corridor: Field::new(size, size, FAR),
        hole_of: Field::new(size, size, 255u8),
    };
    for (index, plan) in plans.iter().enumerate() {
        corridor(plan, index as u8, size, &mut out);
        let green = &plan.green;
        over_box(&[green.center], green.reach() + 12.0, size, |i, p| {
            let d = green.sdf(p);
            if d < out.green.data[i] {
                out.green.data[i] = d;
            }
        });
        for bunker in &plan.bunkers {
            over_box(&[bunker.center], bunker.reach() + 4.0, size, |i, p| {
                out.bunker.data[i] = out.bunker.data[i].min(bunker.sdf(p));
            });
        }
        for tee in &plan.tees {
            let pad = tee.pad;
            over_box(&[pad.center], pad.half.0 + 8.0, size, |i, p| {
                out.tee.data[i] = out.tee.data[i].min(pad.sdf(p));
            });
        }
    }
    out
}

/// The corridor distance and the fairway, which share one centerline
/// distance per cell.
fn corridor(plan: &HolePlan, index: u8, size: usize, out: &mut Features) {
    let reach = plan.corridor + 45.0;
    let line = &plan.centerline;
    over_box(line, reach, size, |i, p| {
        let (signed, s) = polyline_signed(p, line);
        let d = signed.abs();
        if d > reach {
            return;
        }
        let c = d - plan.corridor;
        if c < out.corridor.data[i] {
            out.corridor.data[i] = c;
            out.hole_of.data[i] = index;
        }
        if let Some(ribbon) = &plan.fairway {
            let f = ribbon.sdf_at(line, p, signed, s);
            out.fairway.data[i] = out.fairway.data[i].min(f);
        }
    });
}
