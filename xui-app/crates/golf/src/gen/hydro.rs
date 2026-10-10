//! Stage 2, hydrology: fill sinks (priority flood), D8 flow directions and
//! accumulation, ponds in the large enclosed sinks, streams where the flow
//! gathers, and the moisture map that drives rough and tree species.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::field::{distance_to, Field};

/// The smallest sink that holds a pond, m².
const MIN_POND: usize = 400;
/// The largest pond: a bigger basin is only flooded to this area.
const MAX_POND: usize = 16_000;
/// A sink must be this deep (m) before it holds water.
const POND_DEPTH: f32 = 0.6;
/// At most this share of the site is ponds; the deepest sinks fill first.
const POND_BUDGET: f32 = 0.035;
/// Catchment (cells) above which the flow is a stream.
const STREAM_CATCHMENT: f32 = 18_000.0;

pub struct Hydrology {
    /// Water cells (ponds, lakes and streams).
    pub water: Field<bool>,
    /// The flat water surface for each water cell.
    pub water_level: Field<f32>,
    /// 0 dry .. 1 wet.
    pub moisture: Field<f32>,
}

/// Runs the hydrology over `elevation`, carving stream beds into it.
pub fn run(elevation: &mut Field<f32>) -> Hydrology {
    let (filled, order) = priority_flood(elevation);
    let accum = accumulate(&filled, &order);
    let (w, h) = (elevation.width, elevation.height);
    let mut water = Field::new(w, h, false);
    let mut water_level = Field::new(w, h, 0.0f32);
    ponds(elevation, &filled, &mut water, &mut water_level);
    streams(elevation, &accum, &mut water, &mut water_level);
    let moisture = moisture(&accum, &water);
    Hydrology {
        water,
        water_level,
        moisture,
    }
}

/// Priority-flood sink filling (Barnes et al.): every cell is raised to its
/// spill height plus a tiny gradient, so all water reaches the border.
/// Returns the filled surface and the cells in the order they were settled
/// (lowest first), which is a valid upstream-last order for accumulation.
fn priority_flood(elevation: &Field<f32>) -> (Field<f32>, Vec<u32>) {
    const EPSILON: f32 = 1e-4;
    let (w, h) = (elevation.width, elevation.height);
    let mut filled = elevation.clone();
    let mut done = vec![false; w * h];
    let mut order = Vec::with_capacity(w * h);
    let mut heap = BinaryHeap::new();
    let key =
        |height: f32, i: usize| Reverse((u64::from(height.max(0.0).to_bits()) << 32) | i as u64);
    for i in 0..w * h {
        let (x, y) = (i % w, i / w);
        if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
            done[i] = true;
            heap.push(key(filled.data[i], i));
        }
    }
    while let Some(Reverse(packed)) = heap.pop() {
        let i = (packed & 0xFFFF_FFFF) as usize;
        order.push(i as u32);
        let level = filled.data[i];
        for n in neighbours(i, w, h) {
            if done[n] {
                continue;
            }
            done[n] = true;
            filled.data[n] = filled.data[n].max(level + EPSILON);
            heap.push(key(filled.data[n], n));
        }
    }
    (filled, order)
}

/// The 8 neighbours of cell `i` inside the grid.
fn neighbours(i: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let (x, y) = ((i % w) as i64, (i / w) as i64);
    [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ]
    .into_iter()
    .filter_map(move |(dx, dy)| {
        let (nx, ny) = (x + dx, y + dy);
        (nx >= 0 && ny >= 0 && nx < w as i64 && ny < h as i64)
            .then(|| ny as usize * w + nx as usize)
    })
}

/// D8 flow accumulation over the filled surface: each cell drains to its
/// steepest lower neighbour; cells are visited highest first.
fn accumulate(filled: &Field<f32>, order: &[u32]) -> Field<f32> {
    let (w, h) = (filled.width, filled.height);
    let mut accum = Field::new(w, h, 1.0f32);
    for &cell in order.iter().rev() {
        let i = cell as usize;
        let here = filled.data[i];
        let mut best = None;
        let mut steepest = 0.0f32;
        for n in neighbours(i, w, h) {
            let dist = if n % w != i % w && n / w != i / w {
                1.414
            } else {
                1.0
            };
            let drop = (here - filled.data[n]) / dist;
            if drop > steepest {
                steepest = drop;
                best = Some(n);
            }
        }
        if let Some(n) = best {
            accum.data[n] += accum.data[i];
        }
    }
    accum
}

/// Floods the deep enclosed sinks: each connected sink becomes a pond at
/// its spill level, or lower when the basin is larger than [`MAX_POND`].
/// The deepest sinks are flooded first until [`POND_BUDGET`] is used.
fn ponds(
    elevation: &Field<f32>,
    filled: &Field<f32>,
    water: &mut Field<bool>,
    level: &mut Field<f32>,
) {
    let (w, h) = (elevation.width, elevation.height);
    let mut sinks: Vec<(f32, Vec<usize>)> = sinks(elevation, filled)
        .into_iter()
        .filter(|cells| cells.len() >= MIN_POND)
        .map(|cells| {
            let depth = cells
                .iter()
                .map(|&i| filled.data[i] - elevation.data[i])
                .fold(0.0, f32::max);
            (depth, cells)
        })
        .collect();
    sinks.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut budget = (POND_BUDGET * (w * h) as f32) as usize;
    for (_, cells) in sinks {
        let cap = MAX_POND.min(budget);
        if cap < MIN_POND {
            break;
        }
        let spill = cells
            .iter()
            .map(|&i| filled.data[i])
            .fold(f32::MIN, f32::max);
        let surface = if cells.len() > cap {
            // Lower the surface until only `cap` cells lie under it.
            let mut ground: Vec<f32> = cells.iter().map(|&i| elevation.data[i]).collect();
            ground.sort_by(f32::total_cmp);
            ground[cap]
        } else {
            spill - 0.15
        };
        for &i in &cells {
            if elevation.data[i] < surface {
                water.data[i] = true;
                level.data[i] = surface;
                budget = budget.saturating_sub(1);
            }
        }
    }
}

/// The connected groups of cells whose fill depth exceeds [`POND_DEPTH`].
fn sinks(elevation: &Field<f32>, filled: &Field<f32>) -> Vec<Vec<usize>> {
    let (w, h) = (elevation.width, elevation.height);
    let deep = |i: usize| filled.data[i] - elevation.data[i] > POND_DEPTH;
    let mut seen = vec![false; w * h];
    let mut stack = Vec::new();
    let mut out = Vec::new();
    for start in 0..w * h {
        if seen[start] || !deep(start) {
            continue;
        }
        let mut cells = Vec::new();
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            cells.push(i);
            let (x, y) = (i % w, i / w);
            let four = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for n in four.into_iter().flatten() {
                if !seen[n] && deep(n) {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        out.push(cells);
    }
    out
}

/// Streams where the catchment is large: a channel 1-4 m wide whose bed is
/// cut below the surrounding ground.
fn streams(
    elevation: &mut Field<f32>,
    accum: &Field<f32>,
    water: &mut Field<bool>,
    level: &mut Field<f32>,
) {
    let (w, h) = (elevation.width, elevation.height);
    let original = elevation.clone();
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let i = y * w + x;
            let flow = accum.data[i];
            if flow < STREAM_CATCHMENT || water.data[i] {
                continue;
            }
            let radius = (1.0 + (flow / STREAM_CATCHMENT).log2() * 0.5).min(2.5);
            let r = radius.ceil() as i64;
            let surface = original.data[i] - 0.25;
            for dy in -r..=r {
                for dx in -r..=r {
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    if d > radius {
                        continue;
                    }
                    let n = (y as i64 + dy) as usize * w + (x as i64 + dx) as usize;
                    if water.data[n] && level.data[n] <= surface {
                        continue;
                    }
                    water.data[n] = true;
                    level.data[n] = surface;
                    elevation.data[n] = elevation.data[n].min(surface - 0.6 + 0.3 * d / radius);
                }
            }
        }
    }
}

/// Blurred log-accumulation plus closeness to water, in `0.0..=1.0`. The
/// flow term is scaled by a stream's catchment, not by the largest flow on
/// the map (which is wherever the whole site drains off its edge).
fn moisture(accum: &Field<f32>, water: &Field<bool>) -> Field<f32> {
    let (w, h) = (accum.width, accum.height);
    let mut wet = Field::new(w, h, 0.0f32);
    for i in 0..w * h {
        wet.data[i] = accum.data[i].ln();
    }
    let wet = wet.blurred(12, 2);
    let full = (STREAM_CATCHMENT * 4.0).ln();
    let to_water = distance_to(w, h, |i| water.data[i]);
    let mut out = wet;
    for i in 0..w * h {
        let flow = (out.data[i] / full).clamp(0.0, 1.0);
        let near = (-to_water.data[i] / 45.0).exp();
        out.data[i] = (0.2 + 0.5 * flow + 0.7 * near).clamp(0.0, 1.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bowl_becomes_a_pond_and_water_drains() {
        // Big enough that the pond budget (a share of the site) covers the bowl.
        let n = 320;
        let mut f = Field::new(n, n, 0.0f32);
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = (x as f32 - 160.0, y as f32 - 160.0);
                // A rim at radius 30 around a 3 m deep bowl; outside, the land falls away.
                let r = (dx * dx + dy * dy).sqrt();
                f.set(
                    x,
                    y,
                    if r < 30.0 {
                        5.0 - 3.0 * (1.0 - r / 30.0)
                    } else {
                        5.0 - (r - 30.0) * 0.01
                    },
                );
            }
        }
        let hydro = run(&mut f);
        assert!(hydro.water.get(160, 160), "the bowl holds water");
        assert!(!hydro.water.get(2, 2));
        let level = hydro.water_level.get(160, 160);
        assert!(level > 2.0 && level <= 5.0, "{level}");
        let (wet, dry) = (hydro.moisture.get(160, 160), hydro.moisture.get(2, 2));
        assert!(wet > dry, "{wet} {dry}");
    }
}
