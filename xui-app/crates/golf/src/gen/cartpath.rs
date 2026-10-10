//! The cart path: A* on a 4 m grid from each green to the next tee (and to
//! and from the clubhouse), avoiding the play corridor, slopes and water,
//! then smoothed and painted 2.5 m wide. Crossing water makes a bridge.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::course::Material;
use crate::field::Field;
use crate::math::{segment_distance, Vec2};

use super::layout::{smooth, HolePlan};

const CELL: f32 = 4.0;
const HALF_WIDTH: f32 = 1.25;

/// A bridge to place: centre, run direction (radians) and length.
pub struct Bridge {
    pub center: Vec2,
    pub angle: f32,
    pub length: f32,
}

/// What a 4 m step through a material costs, relative to open rough.
fn material_cost(m: Material) -> f32 {
    match m {
        Material::Rough | Material::CartPath => 1.0,
        Material::DeepRough | Material::WasteArea => 1.6,
        Material::FirstCut => 6.0,
        Material::Woodland => 4.0,
        Material::Water => 25.0,
        Material::Fairway | Material::Fringe => 40.0,
        Material::Green | Material::TeeBox | Material::Sand => 400.0,
        Material::OutOfBounds | Material::Rock => 1000.0,
    }
}

/// The per-node cost grid.
fn cost_grid(material: &Field<Material>, elevation: &Field<f32>) -> Field<f32> {
    let n = (material.width as f32 / CELL) as usize;
    let mut grid = Field::new(n, n, 1.0f32);
    for gy in 0..n {
        for gx in 0..n {
            let c = Vec2::new((gx as f32 + 0.5) * CELL, (gy as f32 + 0.5) * CELL);
            let worst = [
                (0.0, 0.0),
                (-1.5, -1.5),
                (1.5, -1.5),
                (-1.5, 1.5),
                (1.5, 1.5),
            ]
            .iter()
            .map(|(dx, dy)| material_cost(material.at(c.x + dx, c.y + dy)))
            .fold(0.0, f32::max);
            let slope = elevation.slope(c.x, c.y, 2.0);
            grid.set(gx, gy, worst + slope * 30.0);
        }
    }
    grid
}

/// A* between two world points; the path as world points.
fn astar(cost: &Field<f32>, from: Vec2, to: Vec2) -> Option<Vec<Vec2>> {
    let n = cost.width;
    let node = |p: Vec2| {
        let clamp = |v: f32| ((v / CELL) as usize).min(n - 1);
        clamp(p.y) * n + clamp(p.x)
    };
    let (start, goal) = (node(from), node(to));
    let (gx, gy) = ((goal % n) as f32, (goal / n) as f32);
    let heuristic =
        |i: usize| (((i % n) as f32 - gx).powi(2) + ((i / n) as f32 - gy).powi(2)).sqrt();
    let mut best = vec![f32::MAX; n * n];
    let mut came = vec![u32::MAX; n * n];
    let mut open = BinaryHeap::new();
    best[start] = 0.0;
    open.push(Reverse((heuristic(start).to_bits(), start)));
    while let Some(Reverse((_, i))) = open.pop() {
        if i == goal {
            let mut path = vec![to];
            let mut at = i;
            while at != start {
                path.push(Vec2::new(
                    ((at % n) as f32 + 0.5) * CELL,
                    ((at / n) as f32 + 0.5) * CELL,
                ));
                at = came[at] as usize;
            }
            path.push(from);
            path.reverse();
            return Some(path);
        }
        let (x, y) = ((i % n) as i64, (i / n) as i64);
        for (dx, dy) in [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
        ] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= n as i64 || ny >= n as i64 {
                continue;
            }
            let j = ny as usize * n + nx as usize;
            let step = if dx != 0 && dy != 0 { 1.414 } else { 1.0 };
            let g = best[i] + step * 0.5 * (cost.data[i] + cost.data[j]);
            if g < best[j] {
                best[j] = g;
                came[j] = i as u32;
                // Non-negative floats order like their bit patterns.
                open.push(Reverse(((g + heuristic(j)).to_bits(), j)));
            }
        }
    }
    None
}

/// Plans every path: clubhouse to 1, each green to the next tee, 9 to the
/// clubhouse to 10, 18 back home.
pub fn plan(
    material: &Field<Material>,
    elevation: &Field<f32>,
    plans: &[HolePlan],
    clubhouse: Vec2,
) -> Vec<Vec<Vec2>> {
    let cost = cost_grid(material, elevation);
    let green_exit = |p: &HolePlan, toward: Vec2| {
        let dir = (toward - p.green.center).normalized();
        p.green.center + dir * (p.green.radius_at(dir.angle()) + 9.0)
    };
    let tee_side = |p: &HolePlan| {
        let pad = p.tees[0].pad;
        pad.center + pad.dir.perp() * (pad.half.1 + 6.0)
    };
    let mut legs = Vec::new();
    for (i, hole) in plans.iter().enumerate() {
        let tee = tee_side(hole);
        let from = match i {
            0 | 9 => clubhouse,
            _ => green_exit(&plans[i - 1], tee),
        };
        legs.push((from, tee));
        if i == 8 || i + 1 == plans.len() {
            legs.push((green_exit(hole, clubhouse), clubhouse));
        }
    }
    legs.into_iter()
        .filter_map(|(a, b)| astar(&cost, a, b))
        .map(|path| smooth(&simplify(&path), 2))
        .collect()
}

/// Drops the points that continue straight on.
fn simplify(path: &[Vec2]) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(path.len());
    for &p in path {
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            if (b - a).normalized().dot((p - b).normalized()) > 0.999 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Paints the paths onto the rough-type cells and returns the bridges
/// where a path crosses water.
pub fn lay(material: &mut Field<Material>, paths: &[Vec<Vec2>]) -> Vec<Bridge> {
    let size = material.width as i64;
    let mut bridges = Vec::new();
    for path in paths {
        for pair in path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (x0, x1) = ((a.x.min(b.x) - 2.0) as i64, (a.x.max(b.x) + 2.0) as i64);
            let (y0, y1) = ((a.y.min(b.y) - 2.0) as i64, (a.y.max(b.y) + 2.0) as i64);
            for y in y0.max(0)..=y1.min(size - 1) {
                for x in x0.max(0)..=x1.min(size - 1) {
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    if segment_distance(p, a, b).0 > HALF_WIDTH {
                        continue;
                    }
                    let (ux, uy) = (x as usize, y as usize);
                    if matches!(
                        material.get(ux, uy),
                        Material::Rough
                            | Material::DeepRough
                            | Material::Woodland
                            | Material::FirstCut
                            | Material::WasteArea
                    ) {
                        material.set(ux, uy, Material::CartPath);
                    }
                }
            }
        }
        bridges.extend(crossings(material, path));
    }
    bridges
}

/// The stretches of `path` over water, as bridges overhanging each bank.
fn crossings(material: &Field<Material>, path: &[Vec2]) -> Vec<Bridge> {
    let length = crate::math::polyline_length(path);
    let mut out = Vec::new();
    let mut start: Option<f32> = None;
    let mut s = 0.0;
    while s <= length + 1.0 {
        let (p, _) = crate::math::polyline_at(path, s);
        let wet = material.at(p.x, p.y) == Material::Water;
        match (wet, start) {
            (true, None) => start = Some(s),
            (false, Some(begin)) => {
                let (a, _) = crate::math::polyline_at(path, begin - 2.0);
                let (b, _) = crate::math::polyline_at(path, s + 1.0);
                out.push(Bridge {
                    center: a.lerp(b, 0.5),
                    angle: (b - a).angle(),
                    length: a.distance(b),
                });
                start = None;
            }
            _ => {}
        }
        s += 1.0;
    }
    out
}
