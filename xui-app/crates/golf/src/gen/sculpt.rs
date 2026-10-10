//! Stage 5, terrain sculpting: the natural heightfield is blended toward
//! shaped targets through the features' signed distances,
//! `h' = lerp(h, target, smoothstep(blend, 0, sdf))`.

use crate::field::{distance_to, Field};
use crate::math::{lerp, smoothstep, Vec2};
use crate::rng::Rng;

use super::features::Features;
use super::layout::HolePlan;

/// Sculpts `elevation` in place. Water inside greens and tees is filled in
/// first; every pond then gets a flat bed and a bank.
pub fn sculpt(
    elevation: &mut Field<f32>,
    water: &mut Field<bool>,
    water_level: &Field<f32>,
    features: &Features,
    plans: &[HolePlan],
    clubhouse: Vec2,
    seed: u64,
) {
    let size = elevation.width;
    for i in 0..size * size {
        if features.green.data[i] < 3.0 || features.tee.data[i] < 3.0 {
            water.data[i] = false;
        }
    }
    fairways(elevation, features);
    pad(elevation, clubhouse, 34.0, 0.4);
    let mut rng = Rng::stage(seed, "sculpt");
    for plan in plans {
        for tee in &plan.tees {
            tee_pad(elevation, tee.pad, rng.range(0.4, 1.3));
        }
        green(elevation, plan, &mut rng);
        for bunker in &plan.bunkers {
            let depth = rng.range(0.4, 1.2);
            bunker_bowl(elevation, bunker, plan.green.center, depth);
        }
    }
    ponds(elevation, water, water_level);
}

/// Low-pass the ground inside the fairways (keep the roll, lose the
/// spikes), and a little across the rough corridor.
fn fairways(elevation: &mut Field<f32>, features: &Features) {
    let smooth = elevation.blurred(4, 2);
    for i in 0..elevation.data.len() {
        let fairway = smoothstep(14.0, -4.0, features.fairway.data[i]) * 0.6;
        let rough = smoothstep(22.0, 0.0, features.corridor.data[i]) * 0.2;
        let w = fairway.max(rough);
        if w > 0.0 {
            elevation.data[i] = lerp(elevation.data[i], smooth.data[i], w);
        }
    }
}

/// The mean height over a disc, from a few samples.
fn mean_height(elevation: &Field<f32>, center: Vec2, radius: f32) -> f32 {
    let mut sum = elevation.sample(center.x, center.y);
    for k in 0..12 {
        let p = center + Vec2::from_angle(k as f32 * std::f32::consts::FRAC_PI_6) * radius;
        sum += elevation.sample(p.x, p.y);
    }
    sum / 13.0
}

/// Blend toward `target(p)` with weight `weight(p)` over cells within
/// `reach` of `center`.
fn blend(
    elevation: &mut Field<f32>,
    center: Vec2,
    reach: f32,
    target: impl Fn(Vec2, f32) -> f32,
    weight: impl Fn(Vec2) -> f32,
) {
    let size = elevation.width as i64;
    let (x0, x1) = ((center.x - reach) as i64, (center.x + reach) as i64 + 1);
    let (y0, y1) = ((center.y - reach) as i64, (center.y + reach) as i64 + 1);
    for y in y0.max(0)..y1.min(size) {
        for x in x0.max(0)..x1.min(size) {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let w = weight(p);
            if w <= 0.0 {
                continue;
            }
            let i = (y * size + x) as usize;
            let h = elevation.data[i];
            elevation.data[i] = lerp(h, target(p, h), w);
        }
    }
}

/// A flat pad for the clubhouse.
fn pad(elevation: &mut Field<f32>, center: Vec2, radius: f32, raise: f32) {
    let level = mean_height(elevation, center, radius * 0.6) + raise;
    blend(
        elevation,
        center,
        radius + 14.0,
        |_, _| level,
        |p| smoothstep(radius + 14.0, radius, p.distance(center)),
    );
}

/// A flat tee raised `raise` metres with a 1% drainage fall forward.
fn tee_pad(elevation: &mut Field<f32>, pad: super::shape::Pad, raise: f32) {
    let base = mean_height(elevation, pad.center, pad.half.1) + raise;
    let reach = pad.half.0 + 6.0;
    blend(
        elevation,
        pad.center,
        reach,
        |p, _| base - 0.01 * (p - pad.center).dot(pad.dir),
        |p| smoothstep(5.0, 0.0, pad.sdf(p)),
    );
}

/// A green: a plane tilted 1-2.5% back to front, half of them with a low
/// tier across the middle. Slope stays under 3% away from the tier line.
fn green(elevation: &mut Field<f32>, plan: &HolePlan, rng: &mut Rng) {
    let blob = &plan.green;
    let base = mean_height(elevation, blob.center, blob.reach() * 0.6) + rng.range(0.0, 0.5);
    let back = plan.approach();
    let tilt = rng.range(0.01, 0.025);
    let tier = rng
        .chance(0.5)
        .then(|| (rng.range(-3.0, 3.0), rng.range(0.25, 0.45)));
    let reach = blob.reach() + 9.0;
    blend(
        elevation,
        blob.center,
        reach,
        |p, _| {
            let along = (p - blob.center).dot(back);
            let mut h = base + tilt * along;
            if let Some((offset, step)) = tier {
                h += step * smoothstep(-2.0, 2.0, along - offset);
            }
            h
        },
        |p| smoothstep(8.0, 0.0, blob.sdf(p)),
    );
}

/// A bunker: a bowl `depth` deep, steepest on the face toward the green,
/// with a raised lip on that side.
fn bunker_bowl(elevation: &mut Field<f32>, blob: &super::shape::Blob, toward: Vec2, depth: f32) {
    let face = (toward - blob.center).normalized();
    let reach = blob.reach() + 3.0;
    let r = blob.radii.0.max(1.0);
    blend(
        elevation,
        blob.center,
        reach,
        |p, h| {
            let d = blob.sdf(p);
            let side = ((p - blob.center).dot(face) / r).clamp(-1.0, 1.0);
            if d < 0.0 {
                let bowl = smoothstep(0.0, -2.5 - 1.5 * (1.0 - side.max(0.0)), d);
                h - depth * (1.0 + 0.4 * side) * bowl
            } else {
                // The lip: a low bank around the face side.
                h + 0.35 * side.max(0.0) * smoothstep(2.5, 0.0, d)
            }
        },
        |p| if blob.sdf(p) < 2.5 { 1.0 } else { 0.0 },
    );
}

/// Ponds and streams: the bed falls away from the shore under a flat
/// surface, and the ground just outside stands above the water.
fn ponds(elevation: &mut Field<f32>, water: &Field<bool>, level: &Field<f32>) {
    let (w, h) = (elevation.width, elevation.height);
    let into = distance_to(w, h, |i| !water.data[i]);
    let near = distance_to(w, h, |i| water.data[i]);
    // The surface beside each shore cell, spread outward a few metres.
    let mut shore = level.clone();
    for _ in 0..3 {
        let prev = shore.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                if water.data[i] {
                    continue;
                }
                for n in [i - 1, i + 1, i - w, i + w] {
                    if water.data[n] || prev.data[n] > 0.0 {
                        shore.data[i] = shore.data[i].max(prev.data[n]);
                    }
                }
            }
        }
    }
    for i in 0..w * h {
        if water.data[i] {
            let bed = level.data[i] - 0.3 - 1.2 * smoothstep(0.0, 8.0, into.data[i]);
            elevation.data[i] = elevation.data[i].min(bed);
        } else if near.data[i] <= 3.0 && shore.data[i] > 0.0 {
            let bank = shore.data[i] + 0.12 + 0.05 * near.data[i];
            elevation.data[i] = elevation.data[i].max(bank);
        }
    }
}
