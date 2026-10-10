//! Judging candidates, so the generator can search for a good course
//! instead of keeping whatever a seed gives: a site from its terrain
//! preview, a routing from how its holes play over that terrain.
//!
//! A good course has relief (holes that rise, fall and roll, never a
//! table), vistas (tees that look down over the hole), holes cleanly apart,
//! variety (directions, shapes, lengths) and water in play; it has no blind
//! shots over a hill and no landing zones on a side slope.

use crate::field::Field;
use crate::math::{polyline_at, polyline_distance, polyline_length, smoothstep, Vec2};

use super::routing::cost::{self, Site, CLUBHOUSE_ZONE};
use super::routing::Route;

/// A site's promise, roughly 0..10, from its landform `preview` sampled
/// every `stride` metres.
/// `relief` scales what counts as lively for the landform.
pub fn site(preview: &Field<f32>, stride: f32, relief: f32) -> f32 {
    let n = preview.width;
    let lo = n / 16;
    let hi = n - lo;
    let mut heights = Vec::new();
    let mut flat = 0usize;
    let mut cells = 0usize;
    for y in lo..hi {
        for x in lo..hi {
            let h = preview.get(x, y);
            heights.push(h);
            let gx = (preview.get(x + 1, y) - preview.get(x - 1, y)) / (2.0 * stride);
            let gy = (preview.get(x, y + 1) - preview.get(x, y - 1)) / (2.0 * stride);
            if (gx * gx + gy * gy).sqrt() < 0.15 {
                flat += 1;
            }
            cells += 1;
        }
    }
    heights.sort_by(f32::total_cmp);
    let range = heights[heights.len() * 95 / 100] - heights[heights.len() * 5 / 100];
    // Undulation: what is left after smoothing away the big landforms.
    let smooth = preview.blurred((60.0 / stride) as usize, 2);
    let mut sum = 0.0;
    for y in lo..hi {
        for x in lo..hi {
            let d = preview.get(x, y) - smooth.get(x, y);
            sum += d * d;
        }
    }
    let undulation = (sum / cells as f32).sqrt();
    let playable = flat as f32 / cells as f32;
    3.0 * smoothstep(8.0 * relief, 26.0 * relief, range) * (1.0 - smoothstep(95.0, 150.0, range))
        + 3.0 * smoothstep(0.6 * relief, 3.0 * relief, undulation)
        + 2.5 * smoothstep(0.4, 0.75, playable)
        + 1.5 * hilly_mix(preview, lo, hi, stride)
}

/// How well the site mixes hilly and gentle ground, 0..1: best when about
/// half of its 64 m blocks have a few metres of relief.
fn hilly_mix(preview: &Field<f32>, lo: usize, hi: usize, stride: f32) -> f32 {
    let block = ((64.0 / stride) as usize).max(2);
    let (mut hilly, mut blocks) = (0, 0);
    for by in (lo..hi - block).step_by(block) {
        for bx in (lo..hi - block).step_by(block) {
            let (mut a, mut b) = (f32::MAX, f32::MIN);
            for y in by..by + block {
                for x in bx..bx + block {
                    a = a.min(preview.get(x, y));
                    b = b.max(preview.get(x, y));
                }
            }
            hilly += usize::from(b - a > 4.0);
            blocks += 1;
        }
    }
    let share = hilly as f32 / blocks.max(1) as f32;
    (1.0 - (share - 0.55).abs() / 0.55).clamp(0.0, 1.0)
}

/// A routing's score and what it is made of.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quality {
    pub total: f32,
    /// Each 0..1.
    pub relief: f32,
    pub vistas: f32,
    pub separation: f32,
    pub variety: f32,
    pub water: f32,
    /// A full-length course (short holes fit steep land too easily).
    pub length: f32,
    /// Blind shots, side-sloped landing zones, loosened constraints.
    pub penalties: f32,
}

/// One hole's terrain story: relief and vista rewards, blind-shot and
/// side-slope penalties, each 0..1.
pub struct HoleView {
    pub relief: f32,
    pub vista: f32,
    pub blind: f32,
    pub side_slope: f32,
    pub water: bool,
}

/// Scores a routing over its site; `relaxed` is how often the routing had
/// to loosen its constraints (`None`: its fallback grid).
pub fn routing(site: &Site, routes: &[Route], relaxed: Option<usize>) -> Quality {
    let views: Vec<HoleView> = routes.iter().map(|r| hole(site, r)).collect();
    let n = views.len().max(1) as f32;
    let relief = views.iter().map(|v| v.relief).sum::<f32>() / n;
    let vistas = views.iter().map(|v| v.vista).sum::<f32>() / n;
    let water = (views.iter().filter(|v| v.water).count() as f32 / 9.0).min(1.0);
    let separation = separation(site, routes);
    let variety = variety(routes);
    // By rung of the routing's ladder: 74 m spacing is a bonus, tighter
    // rungs cost more and more.
    const RELAX: [f32; 6] = [-0.4, -0.1, 0.4, 1.2, 2.2, 4.0];
    let relax = relaxed.map_or(5.0, |level| RELAX[level.min(RELAX.len() - 1)]);
    let penalties = views.iter().map(|v| v.blind + v.side_slope).sum::<f32>() * 0.4 + relax;
    let metres: f32 = routes.iter().map(Route::length).sum();
    let length = smoothstep(4600.0, 5700.0, metres);
    let total =
        2.6 * relief + 2.2 * vistas + 2.0 * separation + 2.0 * variety + 0.8 * water + 1.5 * length
            - penalties;
    Quality {
        total,
        relief,
        vistas,
        separation,
        variety,
        water,
        length,
        penalties,
    }
}

pub fn hole(site: &Site, route: &Route) -> HoleView {
    let line = route.line();
    let length = polyline_length(&line);
    let h = |p: Vec2| site.elevation.sample(p.x, p.y);
    let steps = (length / 10.0) as usize + 1;
    let profile: Vec<f32> = (0..=steps)
        .map(|k| h(polyline_at(&line, k as f32 * length / steps as f32).0))
        .collect();
    let (start, end) = (profile[0], profile[steps]);
    // Roll: how far the ground strays from the straight tee-to-green grade.
    let mut stray = 0.0;
    let mut blind = 0.0f32;
    for (k, &z) in profile.iter().enumerate() {
        let t = k as f32 / steps as f32;
        let grade = start + (end - start) * t;
        stray += (z - grade).powi(2);
        // A hill between the player's eye and the green hides it.
        let sight = start + 1.7 + (end - start - 1.7) * t;
        blind = blind.max(z - sight);
    }
    let roll = (stray / profile.len() as f32).sqrt();
    let k = site.relief;
    let relief = smoothstep(2.0 * k, 16.0 * k, (start - end).abs() + 2.0 * roll);
    let (landing, dir) = polyline_at(&line, (length * 0.6).min(230.0));
    let side = dir.perp() * 10.0;
    let side_slope = ((h(landing + side) - h(landing - side)) / 20.0).abs();
    // The view from the tee: how far the land falls away ahead.
    let heading = (line[1] - line[0]).normalized();
    let mut drop = 0.0;
    let mut samples = 0.0;
    for d in [120.0, 200.0, 300.0] {
        for a in [-0.4f32, -0.2, 0.0, 0.2, 0.4] {
            let p = route.tee + Vec2::from_angle(heading.angle() + a) * d;
            drop += (start - h(p)).max(0.0);
            samples += 1.0;
        }
    }
    let water = (0..steps).any(|k| {
        let (p, _) = polyline_at(&line, k as f32 * length / steps as f32);
        site.water.at(p.x, p.y)
    }) || (0..8).any(|k| {
        let p = route.green + Vec2::from_angle(k as f32 * 0.785) * 28.0;
        site.water.at(p.x, p.y)
    });
    HoleView {
        relief,
        vista: smoothstep(1.0 * k, 10.0 * k, drop / samples),
        blind: smoothstep(3.0, 9.0, blind),
        side_slope: smoothstep(0.06, 0.16, side_slope),
        water,
    }
}

/// 1 when no two holes squeeze within about 70 m of each other, less for
/// each pair that does (the clubhouse and green-to-tee handovers excepted;
/// the routing cost already pushes holes toward 95 m).
fn separation(site: &Site, routes: &[Route]) -> f32 {
    let mut tight = 0.0;
    for (i, a) in routes.iter().enumerate() {
        let la = a.line();
        let length = polyline_length(&la);
        for (j, b) in routes.iter().enumerate().skip(i + 1) {
            let lb = b.line();
            let consecutive = j == i + 1;
            let mut closest = f32::MAX;
            let mut s = 0.0;
            while s <= length {
                let (p, _) = polyline_at(&la, s);
                s += 12.0;
                let handover =
                    consecutive && (p.distance(a.green) < 60.0 || p.distance(b.tee) < 60.0);
                if p.distance(site.clubhouse) < CLUBHOUSE_ZONE || handover {
                    continue;
                }
                closest = closest.min(polyline_distance(p, &lb).0);
            }
            tight += smoothstep(72.0, 45.0, closest);
        }
    }
    1.0 - (tight / 6.0).min(1.0)
}

/// Directions, shapes and lengths, each 0..1, averaged.
fn variety(routes: &[Route]) -> f32 {
    let mut bins = [0f32; 8];
    for r in routes {
        let a = r.axis().angle().rem_euclid(std::f32::consts::TAU);
        bins[((a / std::f32::consts::TAU * 8.0) as usize).min(7)] += 1.0;
    }
    let n = routes.len().max(1) as f32;
    let entropy: f32 = bins
        .iter()
        .filter(|&&c| c > 0.0)
        .map(|&c| -(c / n) * (c / n).ln())
        .sum();
    let directions = entropy / 8f32.ln();
    // Straight, left, right, S-shaped.
    let mut kinds = [false; 4];
    for r in routes {
        let kind = match (r.dogleg, r.bend) {
            (Some(_), Some(_)) => 3,
            (Some(c), None) => 1 + usize::from((c - r.tee).cross(r.green - c) > 0.0),
            _ => 0,
        };
        kinds[kind] = true;
    }
    // Every kind present, and most long holes bending rather than straight.
    let long: Vec<&Route> = routes.iter().filter(|r| r.par > 3).collect();
    let bent = long.iter().filter(|r| r.dogleg.is_some()).count() as f32 / long.len().max(1) as f32;
    let shapes =
        0.5 * kinds.iter().filter(|&&k| k).count() as f32 / 4.0 + 0.5 * smoothstep(0.3, 0.75, bent);
    let mut spread = 0.0;
    let mut groups = 0.0;
    for par in 3..=5u8 {
        let lengths: Vec<f32> = routes
            .iter()
            .filter(|r| r.par == par)
            .map(Route::length)
            .collect();
        if lengths.len() < 2 {
            continue;
        }
        let mean = lengths.iter().sum::<f32>() / lengths.len() as f32;
        let var = lengths.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / lengths.len() as f32;
        spread += smoothstep(0.02, 0.12, var.sqrt() / mean);
        groups += 1.0;
    }
    let lengths = if groups > 0.0 { spread / groups } else { 0.0 };
    // Short walks between holes make the routing read as one course.
    let walks = walk_score(routes);
    (directions + shapes + lengths + walks) / 4.0
}

/// Short walks from green to tee, 0..1.
fn walk_score(routes: &[Route]) -> f32 {
    let walks: Vec<f32> = routes
        .windows(2)
        .enumerate()
        .filter(|(i, _)| *i != 8)
        .map(|(_, w)| w[0].green.distance(w[1].tee))
        .collect();
    let mean = walks.iter().sum::<f32>() / walks.len().max(1) as f32;
    1.0 - smoothstep(45.0, cost::MAX_WALK, mean)
}
