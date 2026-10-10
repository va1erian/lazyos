//! Stage 8, par by simulated play: value iteration for a player model over
//! a 5 m grid around each hole,
//! `E(x) = 1 + min_a sum P(x'|x,a) [E(x') + penalty(x')]`,
//! with Gaussian dispersion scaled by club and lie and a fitted putting
//! curve on the green. Par comes from the scratch player's strokes to reach
//! the green; the bogey player gives the slope and the stroke indexes.

use crate::course::Material;
use crate::field::Field;
use crate::math::{polyline_distance, Vec2};

use super::layout::HolePlan;

const CELL: f32 = 5.0;
const SWEEPS: usize = 4;

/// A player: club carries (the first is the longest) and how widely shots
/// scatter.
pub struct Player {
    pub clubs: &'static [f32],
    /// Standard deviations as fractions of the shot length.
    pub along: f32,
    pub across: f32,
    /// The smallest standard deviation, metres (a chip is never exact).
    pub min_sd: f32,
    /// Putts relative to the fitted curve.
    pub putting: f32,
}

pub const SCRATCH: Player = Player {
    clubs: &[
        240.0, 215.0, 195.0, 180.0, 165.0, 150.0, 138.0, 125.0, 110.0, 95.0, 80.0, 60.0, 40.0, 20.0,
    ],
    along: 0.08,
    across: 0.07,
    min_sd: 2.0,
    putting: 1.0,
};

pub const BOGEY: Player = Player {
    clubs: &[
        190.0, 172.0, 158.0, 145.0, 132.0, 120.0, 108.0, 95.0, 80.0, 65.0, 50.0, 30.0, 15.0,
    ],
    along: 0.13,
    across: 0.15,
    min_sd: 3.5,
    putting: 1.15,
};

/// Strokes to hole out from `d` metres on the green: about 1.0 at 1 m,
/// 1.5 at 3 m, 2.0 at 12 m.
pub fn putts(d: f32) -> f32 {
    const CURVE: [(f32, f32); 5] = [
        (0.6, 1.0),
        (1.0, 1.05),
        (3.0, 1.5),
        (12.0, 2.0),
        (30.0, 2.4),
    ];
    let d = d.max(CURVE[0].0);
    for w in CURVE.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if d <= d1 {
            let t = (d.ln() - d0.ln()) / (d1.ln() - d0.ln());
            return p0 + (p1 - p0) * t;
        }
    }
    CURVE[4].1
}

/// How a lie changes a shot: (distance factor, dispersion factor).
fn lie(m: Material) -> (f32, f32) {
    match m {
        Material::Green
        | Material::Fringe
        | Material::TeeBox
        | Material::Fairway
        | Material::CartPath => (1.0, 1.0),
        Material::FirstCut => (0.96, 1.1),
        Material::Rough => (0.85, 1.3),
        Material::WasteArea => (0.85, 1.4),
        Material::DeepRough => (0.7, 1.6),
        Material::Sand => (0.75, 1.8),
        Material::Woodland | Material::Rock => (0.45, 2.0),
        Material::Water | Material::OutOfBounds => (0.5, 2.0),
    }
}

/// The hole's grid: the states are the cells near the line of play.
struct Grid {
    origin: Vec2,
    w: usize,
    h: usize,
    /// Per cell: the state index, or `u32::MAX` outside the play area.
    state: Vec<u32>,
    cells: Vec<Vec2>,
    material: Vec<Material>,
    /// On this hole's green (another hole's green is just short grass).
    green: Vec<bool>,
}

impl Grid {
    fn new(plan: &HolePlan, material: &Field<Material>) -> Grid {
        let reach = plan.corridor + 35.0;
        let lo = plan
            .centerline
            .iter()
            .fold(Vec2::new(f32::MAX, f32::MAX), |a, p| {
                Vec2::new(a.x.min(p.x), a.y.min(p.y))
            });
        let hi = plan
            .centerline
            .iter()
            .fold(Vec2::new(f32::MIN, f32::MIN), |a, p| {
                Vec2::new(a.x.max(p.x), a.y.max(p.y))
            });
        let origin = lo - Vec2::new(reach, reach);
        let w = ((hi.x - lo.x + 2.0 * reach) / CELL) as usize + 1;
        let h = ((hi.y - lo.y + 2.0 * reach) / CELL) as usize + 1;
        let mut grid = Grid {
            origin,
            w,
            h,
            state: vec![u32::MAX; w * h],
            cells: Vec::new(),
            material: Vec::new(),
            green: Vec::new(),
        };
        for gy in 0..h {
            for gx in 0..w {
                let p = origin + Vec2::new((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
                if polyline_distance(p, &plan.centerline).0 > reach && plan.green.sdf(p) > 10.0 {
                    continue;
                }
                let m = material.at(p.x, p.y);
                if matches!(m, Material::Water | Material::OutOfBounds) {
                    continue;
                }
                grid.state[gy * w + gx] = grid.cells.len() as u32;
                grid.cells.push(p);
                grid.material.push(m);
                grid.green
                    .push(m == Material::Green && plan.green.sdf(p) < 0.0);
            }
        }
        grid
    }

    fn state_at(&self, p: Vec2) -> Option<usize> {
        let (fx, fy) = ((p.x - self.origin.x) / CELL, (p.y - self.origin.y) / CELL);
        if fx < 0.0 || fy < 0.0 || fx >= self.w as f32 || fy >= self.h as f32 {
            return None;
        }
        let s = self.state[fy as usize * self.w + fx as usize];
        (s != u32::MAX).then_some(s as usize)
    }
}

/// Expected strokes from the back tee for `player`: (to hole out, to reach
/// the green).
pub fn expected(
    plan: &HolePlan,
    pin: Vec2,
    material: &Field<Material>,
    player: &Player,
) -> (f32, f32) {
    let grid = Grid::new(plan, material);
    let n = grid.cells.len();
    let on_green = |i: usize| grid.green[i];
    let mut out: Vec<f32> = (0..n)
        .map(|i| 2.0 + grid.cells[i].distance(pin) / 150.0)
        .collect();
    let mut reach: Vec<f32> = vec![1.5; n];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        grid.cells[a]
            .distance(pin)
            .total_cmp(&grid.cells[b].distance(pin))
    });
    for &i in &order {
        if on_green(i) {
            out[i] = putts(grid.cells[i].distance(pin)) * player.putting;
            reach[i] = 0.0;
        }
    }
    for _ in 0..SWEEPS {
        for &i in &order {
            if on_green(i) {
                continue;
            }
            let (o, r) = best_shot(&grid, i, pin, player, material, &out, &reach);
            out[i] = o;
            reach[i] = r;
        }
    }
    let tee = plan.tees[0].pad.center;
    match grid.state_at(tee) {
        Some(i) => (out[i], reach[i]),
        None => (4.0, 2.0),
    }
}

/// The best aim from state `i`: the expected strokes to hole out under it,
/// and to reach the green under the same aim.
fn best_shot(
    grid: &Grid,
    i: usize,
    pin: Vec2,
    player: &Player,
    material: &Field<Material>,
    out: &[f32],
    reach: &[f32],
) -> (f32, f32) {
    let x = grid.cells[i];
    let to_pin = pin - x;
    let dist = to_pin.length();
    let (carry, scatter) = lie(grid.material[i]);
    let mut best = (f32::MAX, 0.0);
    // Any length up to the longest club: a player can always hit it short,
    // so the better player's choices include the weaker one's.
    let longest = player.clubs[0] * carry;
    let mut distances: Vec<f32> = (1..)
        .map(|k| k as f32 * 15.0)
        .take_while(|&d| d <= longest && d < dist + 25.0)
        .collect();
    if dist < longest {
        distances.push(dist);
    }
    if distances.is_empty() {
        distances.push(longest.min(dist));
    }
    for &d in &distances {
        for turn in [0.0f32, -0.17, 0.17, -0.35, 0.35, -0.6, 0.6] {
            let dir = Vec2::from_angle(to_pin.angle() + turn);
            let (o, r) = outcome(grid, i, x, dir, d, player, scatter, material, out, reach);
            if o < best.0 {
                best = (o, r);
            }
        }
    }
    best
}

/// The expected values of one aim: five samples of the dispersion.
#[allow(clippy::too_many_arguments)]
fn outcome(
    grid: &Grid,
    i: usize,
    x: Vec2,
    dir: Vec2,
    d: f32,
    player: &Player,
    scatter: f32,
    material: &Field<Material>,
    out: &[f32],
    reach: &[f32],
) -> (f32, f32) {
    let sl = (d * player.along * scatter).max(player.min_sd);
    let sc = (d * player.across * scatter).max(player.min_sd);
    let aim = x + dir * d;
    let side = dir.perp();
    let samples = [
        (aim, 0.36),
        (aim + dir * sl, 0.16),
        (aim - dir * sl, 0.16),
        (aim + side * sc, 0.16),
        (aim - side * sc, 0.16),
    ];
    let (mut o, mut r) = (1.0, 1.0);
    for (p, w) in samples {
        let landed = material.at(p.x, p.y);
        let (vo, vr) = match grid.state_at(p) {
            _ if matches!(landed, Material::Water | Material::OutOfBounds) => {
                (out[i] + 1.0, reach[i] + 1.0)
            }
            Some(j) => (out[j], reach[j]),
            // Lost in the trees beyond the play area: a recovery and a drop.
            None => (out[i] + 1.5, reach[i] + 1.5),
        };
        o += w * vo;
        r += w * vr;
    }
    (o, r)
}

/// The centerline length adjusted for elevation (1 m per metre of rise),
/// forced carries over water, and a dogleg whose corner cannot be cut.
pub fn effective_length(
    plan: &HolePlan,
    elevation: &Field<f32>,
    material: &Field<Material>,
) -> f32 {
    let (tee, green) = (plan.tees[0].pad.center, plan.green.center);
    let rise = elevation.sample(green.x, green.y) - elevation.sample(tee.x, tee.y);
    let mut length = plan.length + rise;
    let mut s = 60.0;
    let mut carry = 0.0;
    while s < plan.length - 10.0 {
        let (p, _) = crate::math::polyline_at(&plan.centerline, s);
        if material.at(p.x, p.y) == Material::Water {
            carry += 5.0;
        }
        s += 5.0;
    }
    length += carry * 0.3;
    if plan.route.dogleg.is_some() {
        let blocked = (1..20).any(|k| {
            let p = tee.lerp(green, k as f32 / 20.0);
            matches!(
                material.at(p.x, p.y),
                Material::Woodland | Material::OutOfBounds
            )
        });
        if blocked {
            length += 15.0;
        }
    }
    length
}

/// Par from the scratch player's strokes to reach the green, cross-checked
/// against the length bands: a hole whose effective length is far outside
/// the solver's par's band takes the band's par instead.
pub fn par(reach: f32, effective: f32) -> u8 {
    let solved = ((reach.round() as i32) + 2).clamp(3, 5) as u8;
    let (lo, hi) = super::routing::cost::band(solved);
    if effective < lo - 40.0 || effective > hi + 40.0 {
        match effective {
            e if e < 240.0 => 3,
            e if e < 450.0 => 4,
            _ => 5,
        }
    } else {
        solved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn putting_curve_matches_the_design() {
        assert!((putts(1.0) - 1.05).abs() < 0.01);
        assert!((putts(3.0) - 1.5).abs() < 1e-4);
        assert!((putts(12.0) - 2.0).abs() < 1e-4);
        assert!(putts(50.0) <= 2.4);
    }

    #[test]
    fn par_follows_reach_unless_far_off_band() {
        assert_eq!(par(1.1, 170.0), 3);
        assert_eq!(par(2.0, 380.0), 4);
        assert_eq!(par(3.1, 520.0), 5);
        // A 600 m hole is never a par 4, whatever the model says.
        assert_eq!(par(2.0, 600.0), 5);
    }
}
