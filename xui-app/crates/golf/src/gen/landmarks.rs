//! Landmarks: the obstacles that make holes bend. Tall knolls (long dune
//! ridges on a links), rocky crags and stands of old-growth forest are set
//! into the land after it is raised; their cores are *blocked*, so no tee,
//! green or line of play may cross them and the routing has to dogleg
//! around them, as real holes play around a hill or a wood.

use crate::course::Archetype;
use crate::field::Field;
use crate::math::{smoothstep, Vec2};
use crate::noise::Noise;
use crate::rng::Rng;

/// What the landmarks left on the site.
pub struct Landmarks {
    /// No play may cross these cells.
    pub blocked: Field<bool>,
    /// Bare rock (crag faces).
    pub rock: Field<bool>,
    /// Old-growth woods: always dense woodland.
    pub forest: Field<bool>,
}

impl Landmarks {
    /// A site with none (tests, previews).
    pub fn none(size: usize) -> Landmarks {
        Landmarks {
            blocked: Field::new(size, size, false),
            rock: Field::new(size, size, false),
            forest: Field::new(size, size, false),
        }
    }
}

/// How many of each landmark an archetype gets: knolls, crags, woods.
fn counts(archetype: Archetype) -> (usize, usize, usize) {
    match archetype {
        Archetype::Links => (7, 0, 1),
        Archetype::Parkland => (5, 1, 3),
        Archetype::Mountain => (3, 5, 2),
    }
}

/// Raises the landmarks into `elevation` and returns where they block play.
pub fn place(elevation: &mut Field<f32>, archetype: Archetype, seed: u64) -> Landmarks {
    let size = elevation.width;
    let mut out = Landmarks::none(size);
    let mut rng = Rng::stage(seed, "landmarks");
    let (knolls, crags, woods) = counts(archetype);
    let margin = 110.0;
    let mut spots: Vec<Vec2> = Vec::new();
    // Spread out: a new landmark keeps clear of the ones already placed.
    let mut spot = |rng: &mut Rng, clear: f32| -> Vec2 {
        let mut best = Vec2::new(size as f32 / 2.0, size as f32 / 2.0);
        let mut best_gap = -1.0;
        for _ in 0..24 {
            let p = Vec2::new(
                rng.range(margin, size as f32 - margin),
                rng.range(margin, size as f32 - margin),
            );
            let gap = spots.iter().map(|q| q.distance(p)).fold(f32::MAX, f32::min);
            if gap > clear {
                best = p;
                break;
            }
            if gap > best_gap {
                best_gap = gap;
                best = p;
            }
        }
        spots.push(best);
        best
    };
    for _ in 0..knolls {
        let center = spot(&mut rng, 200.0);
        knoll(elevation, &mut out, archetype, center, &mut rng);
    }
    for _ in 0..crags {
        let center = spot(&mut rng, 180.0);
        crag(elevation, &mut out, center, &mut rng);
    }
    let noise = Noise::new(seed ^ 0xF0_4E57);
    for _ in 0..woods {
        let center = spot(&mut rng, 160.0);
        wood(&mut out, center, &noise, &mut rng);
    }
    out
}

/// Runs `f(cell, offset from centre)` over the cells within `reach`.
fn around(size: usize, center: Vec2, reach: f32, mut f: impl FnMut(usize, Vec2)) {
    let r = reach.ceil() as i64;
    for dy in -r..=r {
        for dx in -r..=r {
            let (x, y) = (center.x as i64 + dx, center.y as i64 + dy);
            if x < 0 || y < 0 || x >= size as i64 || y >= size as i64 {
                continue;
            }
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            f(y as usize * size + x as usize, p - center);
        }
    }
}

/// A rounded hill (or, on a links, a long dune ridge) 12-30 m tall; its
/// upper part is blocked.
fn knoll(
    elevation: &mut Field<f32>,
    out: &mut Landmarks,
    archetype: Archetype,
    center: Vec2,
    rng: &mut Rng,
) {
    let (height, radius, stretch) = match archetype {
        Archetype::Links => (
            rng.range(9.0, 15.0),
            rng.range(45.0, 70.0),
            rng.range(2.0, 3.2),
        ),
        Archetype::Parkland => (
            rng.range(14.0, 26.0),
            rng.range(55.0, 90.0),
            rng.range(1.0, 1.6),
        ),
        Archetype::Mountain => (
            rng.range(20.0, 34.0),
            rng.range(60.0, 95.0),
            rng.range(1.0, 1.5),
        ),
    };
    let axis = Vec2::from_angle(rng.range(0.0, std::f32::consts::PI));
    let wobble = (rng.range(0.0, 0.18), rng.range(0.0, std::f32::consts::TAU));
    let size = elevation.width;
    around(size, center, radius * stretch * 1.2, |i, d| {
        let (u, v) = (d.dot(axis) / stretch, d.dot(axis.perp()));
        let r = (u * u + v * v).sqrt();
        let edge = radius * (1.0 + wobble.0 * (3.0 * v.atan2(u) + wobble.1).cos());
        let t = (r / edge).min(1.0);
        let bump = (1.0 - t * t).powi(2);
        elevation.data[i] += height * bump;
        if t < 0.5 {
            out.blocked.data[i] = true;
        }
    });
}

/// A rocky crag: steep-sided, bare rock on its faces, blocked throughout.
fn crag(elevation: &mut Field<f32>, out: &mut Landmarks, center: Vec2, rng: &mut Rng) {
    let height = rng.range(18.0, 32.0);
    let radius = rng.range(30.0, 55.0);
    let size = elevation.width;
    let wobble = [
        (rng.range(0.1, 0.3), rng.range(0.0, std::f32::consts::TAU)),
        (rng.range(0.05, 0.15), rng.range(0.0, std::f32::consts::TAU)),
    ];
    around(size, center, radius * 1.6, |i, d| {
        let phi = d.y.atan2(d.x);
        let edge = radius
            * (1.0
                + wobble[0].0 * (2.0 * phi + wobble[0].1).cos()
                + wobble[1].0 * (5.0 * phi + wobble[1].1).cos());
        let t = d.length() / edge;
        // Steep sides up to a broad top: a plateau of rock.
        let rise = smoothstep(1.25, 0.75, t);
        elevation.data[i] += height * rise;
        if t < 1.05 {
            out.blocked.data[i] = true;
        }
        if (0.6..1.15).contains(&t) {
            out.rock.data[i] = true;
        }
    });
}

/// A stand of old-growth wood with a ragged edge, blocked to play.
fn wood(out: &mut Landmarks, center: Vec2, noise: &Noise, rng: &mut Rng) {
    let radius = rng.range(40.0, 75.0);
    let size = out.forest.width;
    around(size, center, radius * 1.4, |i, d| {
        let p = center + d;
        let ragged = noise.fbm(p.x / 45.0, p.y / 45.0, 3, 0.5) * 0.35;
        if d.length() / radius < 1.0 + ragged {
            out.forest.data[i] = true;
            out.blocked.data[i] = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landmarks_raise_hills_and_block_their_cores() {
        let mut elevation = Field::new(1024, 1024, 0.0f32);
        let marks = place(&mut elevation, Archetype::Parkland, 3);
        let blocked = marks.blocked.data.iter().filter(|&&b| b).count();
        let share = blocked as f32 / (1024.0 * 1024.0);
        assert!(share > 0.04 && share < 0.3, "{share}");
        let (_, hi) = elevation.min_max();
        assert!(hi > 12.0, "{hi}");
        assert!(marks.forest.data.iter().any(|&f| f));
        let mut elevation = Field::new(1024, 1024, 0.0f32);
        let marks = place(&mut elevation, Archetype::Mountain, 3);
        assert!(marks.rock.data.iter().any(|&r| r));
    }
}
