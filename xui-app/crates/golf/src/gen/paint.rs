//! Stage 6, material painting: one material per cell from the features'
//! signed distances, in priority order, plus the edge distance field the
//! renderer uses for lips and collars.

use crate::course::{Archetype, Material};
use crate::field::{chamfer, Field};
use crate::noise::Noise;

use super::features::Features;

/// Cells this close to the site edge are outside the property.
pub const PROPERTY_INSET: usize = 14;

/// Everything the painter reads.
pub struct PaintInput<'a> {
    pub elevation: &'a Field<f32>,
    pub water: &'a Field<bool>,
    pub moisture: &'a Field<f32>,
    pub features: &'a Features,
    pub archetype: Archetype,
    pub seed: u64,
    pub landmarks: &'a super::landmarks::Landmarks,
}

/// How dense the woods are at a cell, 0..1: nothing in the corridor,
/// ramping up past 15 m, scaled by moisture, the archetype and a patchy
/// noise so the woods come in stands.
pub fn tree_density(input: &PaintInput, x: usize, y: usize) -> f32 {
    let i = y * input.elevation.width + x;
    let corridor = input.features.corridor.data[i];
    let ramp = ((corridor - 4.0) / 22.0).clamp(0.0, 1.0);
    let noise = Noise::new(input.seed ^ 0x7EE5);
    let patches = crate::math::smoothstep(
        -0.25,
        0.3,
        noise.fbm(x as f32 / 140.0, y as f32 / 140.0, 3, 0.5),
    );
    let archetype = match input.archetype {
        Archetype::Links => 0.18,
        Archetype::Parkland => 1.0,
        Archetype::Mountain => 0.8,
    };
    (ramp * (0.4 + 0.6 * input.moisture.data[i]) * archetype * (0.1 + 0.9 * patches))
        .clamp(0.0, 1.0)
}

/// Paints every cell. Cart paths are laid on top afterwards.
pub fn paint(input: &PaintInput) -> Field<Material> {
    let size = input.elevation.width;
    let f = input.features;
    let noise = Noise::new(input.seed ^ 0xF35C);
    let mut out = Field::new(size, size, Material::Rough);
    for y in 0..size {
        for x in 0..size {
            let i = y * size + x;
            let edge = x.min(y).min(size - 1 - x).min(size - 1 - y);
            out.data[i] = if edge < PROPERTY_INSET {
                Material::OutOfBounds
            } else if input.water.data[i] {
                Material::Water
            } else if f.bunker.data[i] < 0.0 {
                Material::Sand
            } else if f.green.data[i] < 0.0 {
                Material::Green
            } else if f.green.data[i] < 1.5 {
                Material::Fringe
            } else if f.tee.data[i] < 0.0 {
                Material::TeeBox
            } else if f.fairway.data[i] < 0.0 {
                Material::Fairway
            } else if f.fairway.data[i] < 2.0 {
                Material::FirstCut
            } else if input.landmarks.rock.data[i] {
                Material::Rock
            } else if input.landmarks.forest.data[i] && f.corridor.data[i] > 0.0 {
                Material::Woodland
            } else if f.corridor.data[i] < 0.0 || f.green.data[i] < 22.0 || f.tee.data[i] < 12.0 {
                Material::Rough
            } else {
                outside(input, &noise, x, y)
            };
        }
    }
    out
}

/// Beyond the corridors: woodland where the trees stand thick, else deep
/// rough (fescue on a links), and sandy waste on dry, steep links ground.
fn outside(input: &PaintInput, noise: &Noise, x: usize, y: usize) -> Material {
    let i = y * input.elevation.width + x;
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    if input.archetype == Archetype::Links {
        let slope = input.elevation.slope(fx, fy, 2.0);
        let sandy = noise.fbm(fx / 40.0, fy / 40.0, 3, 0.5);
        if input.moisture.data[i] < 0.55 && (slope > 0.22 || sandy > 0.35) {
            return Material::WasteArea;
        }
    }
    if tree_density(input, x, y) > 0.55 {
        Material::Woodland
    } else {
        Material::DeepRough
    }
}

/// Distance from each cell to the nearest cell of another material, minus
/// half a cell (so a cell on a boundary reads 0.5).
pub fn edge_distance(material: &Field<Material>) -> Field<f32> {
    let (w, h) = (material.width, material.height);
    let mut d = Field::new(w, h, f32::MAX / 4.0);
    for y in 0..h {
        for x in 0..w {
            let m = material.get(x, y);
            let differs = (x > 0 && material.get(x - 1, y) != m)
                || (x + 1 < w && material.get(x + 1, y) != m)
                || (y > 0 && material.get(x, y - 1) != m)
                || (y + 1 < h && material.get(x, y + 1) != m);
            if differs {
                d.set(x, y, 0.0);
            }
        }
    }
    chamfer(&mut d);
    d.data.iter_mut().for_each(|v| *v = v.min(255.0) + 0.5);
    d
}
