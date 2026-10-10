//! Stage 7a, trees: Poisson-disk-style dart throwing driven by the density
//! field, species from elevation and moisture, and specimen trees guarding
//! dogleg corners.

use crate::course::{Archetype, Material, Object, ObjectKind, Species};
use crate::field::Field;
use crate::math::{polyline_at, Vec2};
use crate::rng::Rng;

use super::layout::HolePlan;
use super::paint::{tree_density, PaintInput};

/// The spacing of candidate points, metres.
const GRID: f32 = 3.2;
/// The spatial hash cell for the minimum-distance test.
const HASH: f32 = 8.0;

/// Places the trees.
pub fn place(
    input: &PaintInput,
    material: &Field<Material>,
    plans: &[HolePlan],
    rng: &mut Rng,
) -> Vec<Object> {
    let size = material.width;
    let (lo, hi) = input.elevation.min_max();
    let cells = (size as f32 / HASH) as usize + 1;
    let mut hash: Vec<Vec<(Vec2, f32)>> = vec![Vec::new(); cells * cells];
    let mut out = Vec::new();
    let mut try_place =
        |p: Vec2, species: Species, height: f32, rng: &mut Rng, out: &mut Vec<Object>| {
            let spacing = crown(species) * 1.1;
            let (hx, hy) = ((p.x / HASH) as usize, (p.y / HASH) as usize);
            for ny in hy.saturating_sub(1)..(hy + 2).min(cells) {
                for nx in hx.saturating_sub(1)..(hx + 2).min(cells) {
                    if hash[ny * cells + nx]
                        .iter()
                        .any(|&(q, r)| q.distance(p) < spacing.max(r))
                    {
                        return;
                    }
                }
            }
            hash[hy * cells + hx].push((p, spacing));
            out.push(Object {
                kind: ObjectKind::Tree {
                    species,
                    variant: rng.below(3) as u8,
                    mirrored: rng.chance(0.5),
                },
                position: p,
                base: input.elevation.sample(p.x, p.y),
                height,
            });
        };
    // Specimen trees first, so the general scatter makes room for them.
    for plan in plans {
        if let Some(corner) = plan.route.dogleg {
            let turn = (corner - plan.route.tee)
                .cross(plan.route.green - corner)
                .signum();
            let (p, dir) = polyline_at(&plan.centerline, plan.landing[0] + 25.0);
            let s = plan.landing[0];
            let inside = p + dir.perp() * (turn * (plan.half_width(s) + 9.0) + plan.offset(s));
            if material.at(inside.x, inside.y) == Material::Rough {
                let species = if input.archetype == Archetype::Mountain {
                    Species::Pine
                } else {
                    Species::Oak
                };
                try_place(inside, species, rng.range(18.0, 24.0), rng, &mut out);
            }
        }
    }
    let steps = (size as f32 / GRID) as usize;
    for gy in 0..steps {
        for gx in 0..steps {
            let p = Vec2::new(
                (gx as f32 + rng.next_f32()) * GRID,
                (gy as f32 + rng.next_f32()) * GRID,
            );
            let (x, y) = (p.x as usize, p.y as usize);
            if x >= size || y >= size {
                continue;
            }
            let m = material.get(x, y);
            let mut density = match m {
                Material::Woodland => 0.95,
                Material::DeepRough | Material::WasteArea | Material::Rough => {
                    tree_density(input, x, y) * 0.55
                }
                Material::OutOfBounds => 0.75 * tree_density(input, x, y).max(0.4),
                _ => 0.0,
            };
            if input.archetype == Archetype::Links {
                density *= 0.6;
            }
            if density <= 0.0 || !rng.chance(density) {
                continue;
            }
            let i = y * size + x;
            let altitude = (input.elevation.data[i] - lo) / (hi - lo).max(1.0);
            let (species, height) = species(input.archetype, input.moisture.data[i], altitude, rng);
            try_place(p, species, height, rng, &mut out);
        }
    }
    out
}

/// A species' crown radius, metres: trees keep about this far apart.
pub fn crown(species: Species) -> f32 {
    match species {
        Species::Oak => 4.5,
        Species::Pine => 3.2,
        Species::Willow => 4.0,
        Species::Poplar => 2.6,
        Species::UmbrellaPine => 4.5,
        Species::Gorse => 1.6,
    }
}

/// Willow by water, pine on dry ridges, gorse on a links, oak and poplar in
/// the parkland, umbrella pines on warm dry slopes.
fn species(archetype: Archetype, moisture: f32, altitude: f32, rng: &mut Rng) -> (Species, f32) {
    let roll = rng.next_f32();
    let species = if moisture > 0.78 && roll < 0.6 {
        Species::Willow
    } else {
        match archetype {
            Archetype::Links => {
                if roll < 0.75 {
                    Species::Gorse
                } else {
                    Species::Pine
                }
            }
            Archetype::Mountain => {
                if altitude > 0.45 || roll < 0.6 {
                    Species::Pine
                } else {
                    Species::Oak
                }
            }
            Archetype::Parkland => {
                if altitude > 0.65 && moisture < 0.5 {
                    if roll < 0.5 {
                        Species::UmbrellaPine
                    } else {
                        Species::Pine
                    }
                } else if moisture > 0.6 && roll < 0.3 {
                    Species::Poplar
                } else if roll < 0.8 {
                    Species::Oak
                } else {
                    Species::Pine
                }
            }
        }
    };
    let (lo, hi) = match species {
        Species::Oak => (11.0, 19.0),
        Species::Pine => (13.0, 23.0),
        Species::Willow => (7.0, 11.0),
        Species::Poplar => (17.0, 25.0),
        Species::UmbrellaPine => (9.0, 14.0),
        Species::Gorse => (1.2, 2.4),
    };
    (species, rng.range(lo, hi))
}
