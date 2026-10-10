//! Tree sprites, baked once: each species is a cluster of foliage
//! ellipsoids around a trunk, ray-cast with Lambert light from the sun's
//! side, quantized to its foliage ramp with index 0 as the colour key, and
//! rendered at a ladder of pre-scaled heights (hand-scaling small sprites
//! looks better than shrinking big ones at run time).

use crate::course::Species;
use crate::math::Vec3;
use crate::noise::cell_hash;
use crate::rng::Rng;

use super::palette::{self, shade_index};

/// The baked heights, pixels.
pub const SIZES: [usize; 10] = [6, 9, 12, 16, 24, 32, 48, 64, 96, 128];
pub const VARIANTS: usize = 3;
/// The transparent colour key.
pub const KEY: u8 = 0;

/// One baked sprite: `width` x `height` palette indices, row-major.
pub struct Sprite {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// An ellipsoid of foliage, in units of the tree's height.
#[derive(Clone, Copy)]
struct Puff {
    center: Vec3,
    radius: f32,
    /// Vertical squash (< 1 flattens).
    squash: f32,
}

/// A species' shape: foliage, trunk, colours, width.
struct Model {
    puffs: Vec<Puff>,
    trunk_radius: f32,
    trunk_top: f32,
    ramp: u8,
    flowers: bool,
    /// Width over height.
    aspect: f32,
}

/// Every species x variant x size.
pub struct SpriteSet {
    sprites: Vec<Sprite>,
}

impl SpriteSet {
    pub fn new(sun: Vec3) -> SpriteSet {
        // The sprite faces the viewer; light it from the sun's side and above.
        let light = Vec3::new(-0.55, sun.y.max(0.4), -0.5).normalized();
        let mut sprites = Vec::with_capacity(Species::COUNT * VARIANTS * SIZES.len());
        for s in 0..Species::COUNT {
            for v in 0..VARIANTS {
                let model = model(s, v);
                for &h in &SIZES {
                    sprites.push(render(&model, h, light, (s * 7 + v) as u32));
                }
            }
        }
        SpriteSet { sprites }
    }

    /// The baked sprite nearest to `pixels` tall.
    pub fn get(&self, species: Species, variant: u8, pixels: f32) -> &Sprite {
        let size = SIZES
            .iter()
            .position(|&s| s as f32 >= pixels * 0.85)
            .unwrap_or(SIZES.len() - 1);
        let v = usize::from(variant) % VARIANTS;
        &self.sprites[(species as usize * VARIANTS + v) * SIZES.len() + size]
    }
}

fn model(species: usize, variant: usize) -> Model {
    let mut rng = Rng::new((species * 131 + variant * 17 + 5) as u64);
    let mut puffs = Vec::new();
    let mut puff = |x: f32, y: f32, z: f32, r: f32, squash: f32| {
        puffs.push(Puff {
            center: Vec3::new(x, y, z),
            radius: r,
            squash,
        })
    };
    let m = match species {
        // Oak: a broad round crown on a short trunk.
        0 => {
            for _ in 0..9 {
                puff(
                    rng.range(-0.22, 0.22),
                    rng.range(0.5, 0.78),
                    rng.range(-0.2, 0.2),
                    rng.range(0.16, 0.24),
                    0.85,
                );
            }
            (0.04, 0.5, palette::FOLIAGE_A, false, 0.95)
        }
        // Pine: stacked tiers narrowing to a point.
        1 => {
            for k in 0..7 {
                let t = k as f32 / 6.0;
                let r = 0.2 * (1.0 - t) + 0.04;
                puff(rng.range(-0.02, 0.02), 0.3 + 0.62 * t, 0.0, r, 0.55);
            }
            (0.03, 0.5, palette::FOLIAGE_B, false, 0.5)
        }
        // Willow: wide and low, drooping.
        2 => {
            for _ in 0..10 {
                puff(
                    rng.range(-0.3, 0.3),
                    rng.range(0.35, 0.7),
                    rng.range(-0.2, 0.2),
                    rng.range(0.16, 0.24),
                    1.25,
                );
            }
            (0.05, 0.45, palette::FOLIAGE_A, false, 1.1)
        }
        // Poplar: a tall narrow column.
        3 => {
            for k in 0..8 {
                let t = k as f32 / 7.0;
                puff(
                    rng.range(-0.02, 0.02),
                    0.22 + 0.7 * t,
                    0.0,
                    0.11 - 0.05 * t * t,
                    1.4,
                );
            }
            (0.025, 0.3, palette::FOLIAGE_A, false, 0.32)
        }
        // Umbrella pine: a bare trunk under a flat, wide crown.
        4 => {
            for _ in 0..8 {
                puff(
                    rng.range(-0.38, 0.38),
                    rng.range(0.78, 0.9),
                    rng.range(-0.25, 0.25),
                    rng.range(0.13, 0.2),
                    0.45,
                );
            }
            (0.035, 0.85, palette::FOLIAGE_B, false, 1.2)
        }
        // Gorse: a low flowering bush.
        _ => {
            for _ in 0..7 {
                puff(
                    rng.range(-0.4, 0.4),
                    rng.range(0.25, 0.55),
                    rng.range(-0.2, 0.2),
                    rng.range(0.25, 0.38),
                    0.8,
                );
            }
            (0.0, 0.0, palette::FOLIAGE_A, true, 1.5)
        }
    };
    Model {
        puffs,
        trunk_radius: m.0,
        trunk_top: m.1,
        ramp: m.2,
        flowers: m.3,
        aspect: m.4,
    }
}

/// Ray-casts `model` front-on at `height` pixels tall.
fn render(model: &Model, height: usize, light: Vec3, salt: u32) -> Sprite {
    let width = ((height as f32 * model.aspect).round() as usize).max(2);
    let mut pixels = vec![KEY; width * height];
    let h = height as f32;
    for py in 0..height {
        for px in 0..width {
            let x = (px as f32 + 0.5 - width as f32 * 0.5) / h;
            let y = 1.0 - (py as f32 + 0.5) / h;
            pixels[py * width + px] = shade(model, x, y, light, px, py, salt);
        }
    }
    Sprite {
        width,
        height,
        pixels,
    }
}

/// The colour of the sprite at (`x`, `y`) in tree units.
fn shade(model: &Model, x: f32, y: f32, light: Vec3, px: usize, py: usize, salt: u32) -> u8 {
    let mut best: Option<(f32, Vec3)> = None;
    for p in &model.puffs {
        let dx = x - p.center.x;
        let dy = (y - p.center.y) / p.squash;
        let d2 = dx * dx + dy * dy;
        let r2 = p.radius * p.radius;
        if d2 >= r2 {
            continue;
        }
        let dz = (r2 - d2).sqrt();
        let z = p.center.z - dz;
        if best.is_none_or(|(bz, _)| z < bz) {
            best = Some((z, Vec3::new(dx, dy, -dz) * (1.0 / p.radius)));
        }
    }
    if let Some((_, normal)) = best {
        let noise = cell_hash(px as i32, py as i32, salt);
        if model.flowers && noise > 0.86 {
            return palette::FLOWER;
        }
        let lambert = normal.dot(light).max(0.0);
        // Leaf clumps: a speckle, and darker toward the inside and bottom.
        let intensity = (0.18 + 0.72 * lambert + (noise - 0.5) * 0.3) * (0.75 + 0.3 * y.min(1.0));
        let index = shade_index(model.ramp, (intensity * 240.0) as i32, px, py);
        // Never the colour key, even in the darkest dither.
        return index.max(model.ramp + 1);
    }
    if x.abs() < model.trunk_radius && y < model.trunk_top {
        let lit = if x < 0.0 { 5 } else { 2 };
        return palette::STONE + lit;
    }
    KEY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprites_have_shape_and_a_clear_border() {
        let set = SpriteSet::new(Vec3::new(-0.6, 0.6, 0.5).normalized());
        for species in [Species::Oak, Species::Pine, Species::Poplar, Species::Gorse] {
            let s = set.get(species, 1, 64.0);
            assert_eq!(s.height, 64);
            let opaque = s.pixels.iter().filter(|&&p| p != KEY).count();
            assert!(opaque > s.width * s.height / 6, "{species:?} {opaque}");
            // The top corners are sky.
            assert_eq!(s.pixels[0], KEY);
            assert_eq!(s.pixels[s.width - 1], KEY);
        }
        assert_eq!(set.get(Species::Oak, 0, 5.0).height, 6);
        assert_eq!(set.get(Species::Oak, 0, 900.0).height, 128);
    }
}
