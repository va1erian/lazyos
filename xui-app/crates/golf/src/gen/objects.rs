//! Stage 7b, the fixed objects: flagsticks on a flat pin position, tee
//! markers, yardage posts, out-of-bounds stakes, benches and ball washers,
//! bridges, the clubhouse with its practice green.

use crate::course::{Material, Object, ObjectKind};
use crate::field::Field;
use crate::math::{polyline_at, Vec2};
use crate::rng::Rng;

use super::cartpath::Bridge;
use super::layout::HolePlan;
use super::paint::PROPERTY_INSET;
use super::shape::Blob;

/// The pin: a random spot well inside the green where the slope is under
/// 3%, or the flattest one if none is.
pub fn pin(plan: &HolePlan, elevation: &Field<f32>, rng: &mut Rng) -> Vec2 {
    let blob = &plan.green;
    let r = blob.reach() as i32;
    let mut flat = Vec::new();
    let mut best = (f32::MAX, blob.center);
    for dy in -r..=r {
        for dx in -r..=r {
            let p = blob.center + Vec2::new(dx as f32, dy as f32);
            if blob.sdf(p) > -3.0 {
                continue;
            }
            let slope = elevation.slope(p.x, p.y, 1.5);
            if slope < best.0 {
                best = (slope, p);
            }
            if slope < 0.03 {
                flat.push(p);
            }
        }
    }
    if flat.is_empty() {
        best.1
    } else {
        flat[rng.below(flat.len())]
    }
}

/// Everything but the trees and the flagsticks.
pub fn fixed(
    plans: &[HolePlan],
    elevation: &Field<f32>,
    clubhouse: Vec2,
    bridges: &[Bridge],
    size: usize,
) -> Vec<Object> {
    let mut out = Vec::new();
    let mut put = |kind, p: Vec2, height| {
        out.push(Object {
            kind,
            position: p,
            base: elevation.sample(p.x, p.y),
            height,
        })
    };
    for plan in plans {
        for (set, tee) in plan.tees.iter().enumerate() {
            let pad = tee.pad;
            let front = pad.center + pad.dir * (pad.half.0 - 1.5);
            for side in [-1.0, 1.0] {
                put(
                    ObjectKind::TeeMarker { set: set as u8 },
                    front + pad.dir.perp() * (2.6 * side),
                    0.22,
                );
            }
        }
        let back = plan.tees[0].pad;
        let behind = back.center - back.dir * (back.half.0 + 3.0);
        put(ObjectKind::Bench, behind + back.dir.perp() * 4.0, 0.9);
        put(ObjectKind::BallWasher, behind - back.dir.perp() * 4.0, 1.1);
        let forward = plan.tees.last().map_or(0.0, |t| t.along) + 20.0;
        for metres in [200u16, 150, 100] {
            let s = plan.length - f32::from(metres);
            if s < forward {
                continue;
            }
            let (p, dir) = polyline_at(&plan.centerline, s);
            let edge = plan.half_width(s).max(10.0) + 1.5 + plan.offset(s);
            put(
                ObjectKind::YardageMarker { metres },
                p + dir.perp() * edge,
                1.0,
            );
        }
    }
    let line = PROPERTY_INSET as f32;
    let far = size as f32 - line;
    let mut s = line;
    while s <= far {
        for p in [
            Vec2::new(s, line),
            Vec2::new(s, far),
            Vec2::new(line, s),
            Vec2::new(far, s),
        ] {
            put(ObjectKind::OutOfBoundsStake, p, 0.9);
        }
        s += 25.0;
    }
    for bridge in bridges {
        let mut obj = Object {
            kind: ObjectKind::Bridge {
                angle: bridge.angle,
                length: bridge.length,
            },
            position: bridge.center,
            base: 0.0,
            height: 1.0,
        };
        // A bridge spans between its banks: rest it on the higher one.
        let half = Vec2::from_angle(bridge.angle) * (bridge.length * 0.5);
        let (a, b) = (bridge.center - half, bridge.center + half);
        obj.base = elevation.sample(a.x, a.y).max(elevation.sample(b.x, b.y)) + 0.3;
        out.push(obj);
    }
    let facing = plans
        .first()
        .map_or(0.0, |p| (p.tees[0].pad.center - clubhouse).angle());
    out.push(Object {
        kind: ObjectKind::Clubhouse { angle: facing },
        position: clubhouse,
        base: elevation.sample(clubhouse.x, clubhouse.y),
        height: 7.0,
    });
    out
}

/// Ground the clubhouse grounds may take over.
fn open_ground(m: Material) -> bool {
    matches!(
        m,
        Material::Rough
            | Material::DeepRough
            | Material::Woodland
            | Material::WasteArea
            | Material::CartPath
    )
}

/// The clubhouse grounds: a paved terrace round the building and a
/// practice green beside it, in whichever direction has the most open
/// ground. Neither ever covers a hole's tees, greens, sand or water.
pub fn clubhouse_grounds(material: &mut Field<Material>, clubhouse: Vec2, rng: &mut Rng) {
    let size = material.width as i64;
    let open_share = |dir: Vec2| {
        let c = clubhouse + dir * 27.0;
        let mut open = 0;
        for k in 0..25 {
            let p = c + Vec2::new((k % 5) as f32 * 4.0 - 8.0, (k / 5) as f32 * 4.0 - 8.0);
            if open_ground(material.at(p.x, p.y)) {
                open += 1;
            }
        }
        open
    };
    let dir = (0..8)
        .map(|k| Vec2::from_angle(k as f32 * std::f32::consts::FRAC_PI_4))
        .max_by_key(|&d| open_share(d))
        .unwrap_or(Vec2::new(1.0, 0.0));
    let practice = Blob::random(clubhouse + dir * 27.0, 320.0, 1.4, dir.angle(), 0.08, rng);
    let r = 30i64;
    for dy in -r..=r {
        for dx in -r..=r {
            let (x, y) = (clubhouse.x as i64 + dx, clubhouse.y as i64 + dy);
            if x < 0 || y < 0 || x >= size || y >= size {
                continue;
            }
            let (ux, uy) = (x as usize, y as usize);
            if !open_ground(material.get(ux, uy)) {
                continue;
            }
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let d = practice.sdf(p);
            if d < 0.0 {
                material.set(ux, uy, Material::Green);
            } else if d < 1.5 {
                material.set(ux, uy, Material::Fringe);
            } else if p.distance(clubhouse) < 15.0 {
                material.set(ux, uy, Material::CartPath);
            }
        }
    }
}
