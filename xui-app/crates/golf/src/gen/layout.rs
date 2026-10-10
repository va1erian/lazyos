//! Stage 4, hole layout: each routed corridor becomes a smoothed centerline
//! with landing zones, a fairway of varying width, a green sized for the
//! approach, strategic bunkers and 3-5 tee sets.

use crate::math::{polyline_at, polyline_length, Vec2};
use crate::noise::Noise;
use crate::rng::Rng;

use super::routing::Route;
use super::shape::{Blob, Pad, Ribbon};

/// Scratch and bogey drive distances, metres.
pub const SCRATCH_DRIVE: f32 = 240.0;
pub const BOGEY_DRIVE: f32 = 190.0;

pub const TEE_NAMES: [&str; 5] = ["Black", "White", "Yellow", "Red", "Green"];

/// One tee set's pad.
#[derive(Clone, Debug, PartialEq)]
pub struct TeePad {
    pub name: &'static str,
    pub pad: Pad,
    /// Distance along the centerline from the back tee.
    pub along: f32,
}

/// One hole's layout, in metres on the site.
#[derive(Clone, Debug, PartialEq)]
pub struct HolePlan {
    pub route: Route,
    pub centerline: Vec<Vec2>,
    pub length: f32,
    pub fairway: Option<Ribbon>,
    pub green: Blob,
    pub bunkers: Vec<Blob>,
    pub tees: Vec<TeePad>,
    /// Scratch and bogey landing zones, metres along the centerline.
    pub landing: [f32; 2],
    /// The rough corridor's half-width around the centerline.
    pub corridor: f32,
}

impl HolePlan {
    /// The direction the approach arrives at the green from.
    pub fn approach(&self) -> Vec2 {
        polyline_at(&self.centerline, self.length - 1.0).1
    }

    /// How far the fairway's middle sits off the centerline at `s`.
    pub fn offset(&self, s: f32) -> f32 {
        self.fairway.as_ref().map_or(0.0, |f| f.offset(s))
    }

    /// The fairway's half-width at `s` (0 where there is none).
    pub fn half_width(&self, s: f32) -> f32 {
        self.fairway
            .as_ref()
            .filter(|f| (f.start..=f.end).contains(&s))
            .map_or(0.0, |f| f.half_width(s))
    }
}

/// Lays out every routed hole.
pub fn lay_out(routes: &[Route], seed: u64) -> Vec<HolePlan> {
    let mut plans: Vec<HolePlan> = routes
        .iter()
        .enumerate()
        .map(|(i, route)| {
            plan(
                route,
                &mut Rng::stage(seed, &format!("layout.{i}")),
                seed ^ i as u64,
            )
        })
        .collect();
    prune_bunkers(&mut plans);
    plans
}

/// Drops the bunkers that would spill onto another hole's tees, green or
/// fairway: a hole's hazards belong to it.
fn prune_bunkers(plans: &mut [HolePlan]) {
    let shapes: Vec<(Vec<super::shape::Pad>, Blob, Vec<Vec2>, f32)> = plans
        .iter()
        .map(|p| {
            let fairway = p.fairway.as_ref().map_or(0.0, Ribbon::reach);
            (
                p.tees.iter().map(|t| t.pad).collect(),
                p.green.clone(),
                p.centerline.clone(),
                fairway,
            )
        })
        .collect();
    for (i, plan) in plans.iter_mut().enumerate() {
        plan.bunkers.retain(|b| {
            let r = b.reach() + 3.0;
            shapes
                .iter()
                .enumerate()
                .all(|(j, (tees, green, line, fairway))| {
                    if j == i {
                        return true;
                    }
                    let near_line = crate::math::polyline_distance(b.center, line).0 < fairway + r;
                    tees.iter().all(|t| t.sdf(b.center) > r)
                        && green.sdf(b.center) > r
                        && !near_line
                })
        });
    }
}

/// Chaikin corner cutting, keeping both ends: a polyline becomes a smooth
/// curve through its corners' neighbourhoods.
pub fn smooth(points: &[Vec2], rounds: usize) -> Vec<Vec2> {
    let mut line = points.to_vec();
    for _ in 0..rounds {
        if line.len() < 3 {
            return line;
        }
        let mut next = vec![line[0]];
        for pair in line.windows(2) {
            next.push(pair[0].lerp(pair[1], 0.25));
            next.push(pair[0].lerp(pair[1], 0.75));
        }
        next.push(*line.last().unwrap());
        // The first and last cut points sit too near the ends; drop them.
        next.remove(1);
        next.remove(next.len() - 2);
        line = next;
    }
    line
}

fn plan(route: &Route, rng: &mut Rng, noise_seed: u64) -> HolePlan {
    let centerline = smooth(&route.line(), 3);
    let length = polyline_length(&centerline);
    let landing = [
        SCRATCH_DRIVE.min(length - 30.0),
        BOGEY_DRIVE.min(length - 40.0),
    ];
    let approach_len = match route.par {
        3 => length,
        4 => length - SCRATCH_DRIVE,
        _ => length - 2.0 * SCRATCH_DRIVE + 40.0,
    };
    let area = 420.0 + 260.0 * ((approach_len - 80.0) / 140.0).clamp(0.0, 1.0);
    let approach = polyline_at(&centerline, length - 1.0).1;
    let green = Blob::random(
        route.green,
        area,
        rng.range(1.15, 1.5),
        approach.angle(),
        0.09,
        rng,
    );
    let mut bunkers = Vec::new();
    let mut fairway = fairway(route, &centerline, length, &green, rng, noise_seed);
    if let Some(ribbon) = fairway.as_mut() {
        fairway_bunkers(route, &centerline, ribbon, landing, rng, &mut bunkers);
    }
    greenside_bunkers(&green, approach, route.par, rng, &mut bunkers);
    let tees = tees(route, &centerline, length, rng);
    let widest = fairway.as_ref().map_or(12.0, Ribbon::reach);
    HolePlan {
        route: *route,
        centerline,
        length,
        fairway,
        green,
        bunkers,
        tees,
        landing,
        // Rough beyond the fairway; past it the trees may start.
        corridor: widest + 10.0,
    }
}

/// The fairway: from past the forward tees to the green front, 25-45 m wide,
/// widest at the bogey landing zone and narrowing into the green.
fn fairway(
    route: &Route,
    line: &[Vec2],
    length: f32,
    green: &Blob,
    rng: &mut Rng,
    seed: u64,
) -> Option<Ribbon> {
    let approach = polyline_at(line, length - 1.0).1;
    let front = length - green.radius_at((approach * -1.0).angle()) * 0.6;
    let (start, end) = match route.par {
        // A short par 3 is all carry; a longer one gets an apron.
        3 if length < 170.0 => return None,
        3 => (length - 55.0, front),
        _ => (rng.range(55.0, 85.0), front),
    };
    let noise = Noise::new(seed ^ 0xFA1);
    let base = rng.range(12.5, 18.0);
    let samples = (length / Ribbon::STEP) as usize + 2;
    let half_widths = (0..samples)
        .map(|k| {
            let s = k as f32 * Ribbon::STEP;
            let wave = noise.sample(s / 60.0, 0.5) * 2.5;
            let bulge = 3.5 * (-((s - BOGEY_DRIVE) / 35.0).powi(2)).exp();
            let taper = ((end - s) / 50.0).clamp(0.0, 1.0);
            let w = base + wave + bulge;
            (10.0 + (w - 10.0) * taper).clamp(9.0, 22.5)
        })
        .collect();
    // The fairway wanders off the line and back, never near the green.
    let swing = if rng.chance(0.75) {
        rng.range(3.0, 9.0)
    } else {
        0.0
    };
    let phase = rng.range(0.0, 100.0);
    let offsets = (0..samples)
        .map(|k| {
            let s = k as f32 * Ribbon::STEP;
            let ends =
                ((s - start) / 60.0).clamp(0.0, 1.0) * ((end - 30.0 - s) / 60.0).clamp(0.0, 1.0);
            noise.sample(s / 110.0 + phase, 7.7) * 1.6 * swing * ends
        })
        .collect();
    Some(Ribbon {
        start,
        end,
        half_widths,
        offsets,
    })
}

/// A fairway bunker at the scratch landing zone, on the outside of a
/// dogleg (or either side of a straight hole), and sometimes a second one
/// short of it on the other side. The fairway pinches beside each.
fn fairway_bunkers(
    route: &Route,
    line: &[Vec2],
    ribbon: &mut Ribbon,
    landing: [f32; 2],
    rng: &mut Rng,
    out: &mut Vec<Blob>,
) {
    let outside = match route.dogleg {
        // The corner turns toward the green; the outside is the other side.
        Some(corner) => {
            let turn = (corner - route.tee).cross(route.green - corner);
            if turn > 0.0 {
                -1.0
            } else {
                1.0
            }
        }
        None if rng.chance(0.5) => 1.0,
        None => -1.0,
    };
    let mut place = |s: f32, side: f32, rng: &mut Rng, ribbon: &mut Ribbon| {
        if s < ribbon.start + 20.0 || s > ribbon.end - 40.0 {
            return;
        }
        pinch(ribbon, s, 2.5);
        let (p, dir) = polyline_at(line, s);
        let radius = rng.range(5.0, 9.0);
        let offset = ribbon.half_width(s) + radius * 0.9 + 1.5;
        let center = p + dir.perp() * (side * offset + ribbon.offset(s));
        out.push(Blob::random(
            center,
            radius * radius * 2.6,
            rng.range(1.3, 2.0),
            dir.angle(),
            0.12,
            rng,
        ));
    };
    place(landing[0], outside, rng, ribbon);
    if rng.chance(0.55) {
        place(landing[1] + rng.range(-10.0, 10.0), -outside, rng, ribbon);
    }
}

/// Narrow the fairway around `s`.
fn pinch(ribbon: &mut Ribbon, s: f32, by: f32) {
    for (k, w) in ribbon.half_widths.iter_mut().enumerate() {
        let d = (k as f32 * Ribbon::STEP - s) / 25.0;
        *w = (*w - by * (-d * d).exp()).max(9.0);
    }
}

/// 1-3 bunkers around the green, first on the side the approach tempts a
/// player to miss (front left or front right), then the other side or long.
fn greenside_bunkers(green: &Blob, approach: Vec2, par: u8, rng: &mut Rng, out: &mut Vec<Blob>) {
    let facing = (approach * -1.0).angle();
    let first_side = if rng.chance(0.5) { 1.0 } else { -1.0 };
    let count = match par {
        3 => 2 + rng.below(2),
        _ => 1 + rng.below(3),
    };
    let spots = [
        facing + first_side * rng.range(0.7, 1.2),
        facing - first_side * rng.range(0.9, 1.5),
        facing + std::f32::consts::PI + rng.range(-0.5, 0.5),
    ];
    for &phi in spots.iter().take(count) {
        let radius = rng.range(4.0, 7.0);
        let reach = green.radius_at(phi) + radius * 0.85 + 2.5;
        let center = green.center + Vec2::from_angle(phi) * reach;
        let tangent = phi + std::f32::consts::FRAC_PI_2;
        out.push(Blob::random(
            center,
            radius * radius * 2.8,
            rng.range(1.4, 2.2),
            tangent,
            0.12,
            rng,
        ));
    }
}

/// 3-5 tee pads stepped forward along the centerline, back to front.
fn tees(route: &Route, line: &[Vec2], length: f32, rng: &mut Rng) -> Vec<TeePad> {
    let count = 3 + rng.below(3);
    // The forward tees stay well short of the green: a par 3 keeps a real
    // shot, a longer hole keeps most of its length.
    let limit = if route.par == 3 {
        length - 95.0
    } else {
        (length - 200.0).max(length * 0.25)
    };
    let wanted = if route.par == 3 {
        rng.range(9.0, 14.0)
    } else {
        rng.range(16.0, 26.0)
    };
    let step = wanted.min(limit.max(6.0) / (count - 1) as f32);
    TEE_NAMES
        .iter()
        .take(count)
        .enumerate()
        .map(|(k, name)| {
            let along = k as f32 * step;
            let (center, dir) = polyline_at(line, along);
            let half = if k == 0 { (7.0, 4.0) } else { (5.5, 3.5) };
            TeePad {
                name,
                pad: Pad { center, dir, half },
                along,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_keeps_the_ends_and_cuts_corners() {
        let line = [
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 100.0),
        ];
        let s = smooth(&line, 3);
        assert_eq!(s[0], line[0]);
        assert_eq!(*s.last().unwrap(), line[2]);
        assert!(polyline_length(&s) < 200.0);
        assert!(s.iter().all(|p| p.distance(Vec2::new(100.0, 0.0)) > 5.0));
    }

    #[test]
    fn a_par_four_gets_its_features() {
        let route = Route {
            tee: Vec2::new(100.0, 500.0),
            dogleg: Some(Vec2::new(100.0, 270.0)),
            bend: None,
            green: Vec2::new(250.0, 160.0),
            par: 4,
        };
        let plan = plan(&route, &mut Rng::new(9), 9);
        assert!(plan.fairway.is_some());
        assert!(plan.tees.len() >= 3);
        assert!(!plan.bunkers.is_empty());
        let a = plan.green.area();
        assert!((350.0..800.0).contains(&a), "{a}");
        // Bunkers never sit on the green.
        for b in &plan.bunkers {
            assert!(plan.green.sdf(b.center) > 0.0);
        }
    }
}
