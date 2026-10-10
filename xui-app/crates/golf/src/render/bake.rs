//! The render bake: everything that does not depend on the camera, done
//! once per course. Sun shading with cast shadows (terrain and trees), the
//! per-cell lookups packed into one word, and the 32 x 32 chunks with
//! their height bounds and object lists.

use crate::course::{Course, Material, Object, ObjectKind};
use crate::math::{Vec2, Vec3};

use super::sprites::SpriteSet;

/// Cells per chunk side.
pub const CHUNK: usize = 32;

/// Packed cell layout: material, light, hole, edge distance.
pub const MATERIAL_BITS: u32 = 0xF;
pub const LIGHT_SHIFT: u32 = 4;
pub const HOLE_SHIFT: u32 = 12;
pub const EDGE_SHIFT: u32 = 20;

/// One chunk: its cell origin, height bounds and the objects standing in it.
pub struct Chunk {
    pub x0: usize,
    pub z0: usize,
    pub min_h: f32,
    pub max_h: f32,
    pub objects: Vec<u32>,
}

/// What the renderer reads per frame.
pub struct Bake {
    pub size: usize,
    /// The drawn surface per cell: ground, or the water level on water.
    pub surface: Vec<f32>,
    /// Per cell: material | light << 4 | hole << 12 | edge(1/8 m) << 20.
    pub cells: Vec<u32>,
    pub chunks: Vec<Chunk>,
    pub chunks_x: usize,
    /// Each hole's mowing direction (index: hole id in the cell word).
    pub hole_axes: Vec<Vec2>,
    /// Unit vector toward the sun.
    pub sun: Vec3,
    pub sprites: SpriteSet,
    pub max_height: f32,
    /// The course's objects; chunks list indexes into it.
    pub objects: Vec<Object>,
}

impl Bake {
    pub fn new(course: &Course) -> Bake {
        let size = course.width as usize;
        let sun = Vec3::new(-0.62, 0.62, 0.48).normalized();
        let mut surface = course.elevation.data.clone();
        for (i, h) in surface.iter_mut().enumerate() {
            if course.material.data[i] == Material::Water {
                *h = h.max(course.water_level.data[i]);
            }
        }
        let max_height = surface.iter().copied().fold(f32::MIN, f32::max);
        let light = lighting(&surface, size, sun, max_height, course);
        let mut cells = vec![0u32; size * size];
        for i in 0..size * size {
            let edge = (course.edge_sdf.data[i] * 8.0).clamp(0.0, 255.0) as u32;
            cells[i] = course.material.data[i] as u32
                | u32::from(light[i]) << LIGHT_SHIFT
                | u32::from(course.hole_of.data[i]) << HOLE_SHIFT
                | edge << EDGE_SHIFT;
        }
        let mut hole_axes = vec![Vec2::new(1.0, 0.0); 256];
        for (i, hole) in course.holes.iter().enumerate() {
            hole_axes[i] = hole.axis();
        }
        let (chunks, chunks_x) = chunks(&surface, size, course);
        Bake {
            size,
            surface,
            cells,
            chunks,
            chunks_x,
            hole_axes,
            sun,
            sprites: SpriteSet::new(sun),
            max_height,
            objects: course.objects.clone(),
        }
    }

    /// The surface height at a cell, clamped to the grid.
    #[inline]
    pub fn height(&self, x: usize, z: usize) -> f32 {
        let s = self.size - 1;
        self.surface[z.min(s) * self.size + x.min(s)]
    }

    /// Bilinear surface height at a world point (cell centres at +0.5).
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let fx = (x - 0.5).clamp(0.0, (self.size - 1) as f32);
        let fz = (z - 0.5).clamp(0.0, (self.size - 1) as f32);
        let (ix, iz) = (fx as usize, fz as usize);
        let (tx, tz) = (fx - ix as f32, fz - iz as f32);
        let a = self.height(ix, iz) + (self.height(ix + 1, iz) - self.height(ix, iz)) * tx;
        let b =
            self.height(ix, iz + 1) + (self.height(ix + 1, iz + 1) - self.height(ix, iz + 1)) * tx;
        a + (b - a) * tz
    }
}

/// Per-cell light, `0..=240`: ambient plus Lambert from the sun, reduced
/// where the terrain or a tree casts a shadow.
fn lighting(surface: &[f32], size: usize, sun: Vec3, max_height: f32, course: &Course) -> Vec<u8> {
    let at = |x: i64, z: i64| {
        surface
            [(z.clamp(0, size as i64 - 1) as usize) * size + x.clamp(0, size as i64 - 1) as usize]
    };
    let mut shadow = vec![false; size * size];
    terrain_shadows(surface, size, sun, max_height, &mut shadow);
    tree_shadows(course, size, sun, &mut shadow);
    let mut out = vec![0u8; size * size];
    for z in 0..size as i64 {
        for x in 0..size as i64 {
            let i = z as usize * size + x as usize;
            // Normals over 2 m each way: single-cell bumps would shade as
            // a checkerboard up close.
            let n = Vec3::new(
                -(at(x + 2, z) - at(x - 2, z)) * 0.25,
                1.0,
                -(at(x, z + 2) - at(x, z - 2)) * 0.25,
            )
            .normalized();
            let direct = n.dot(sun).max(0.0);
            let shade = if shadow[i] {
                0.36 + 0.64 * direct * 0.25
            } else {
                0.36 + 0.64 * direct
            };
            out[i] = (shade * 200.0 + 18.0).clamp(0.0, 240.0) as u8;
        }
    }
    soften(&mut out, size);
    out
}

/// A 3 x 3 box blur of the light: shadow edges and slope changes blend over
/// a cell or two instead of stepping.
fn soften(light: &mut [u8], size: usize) {
    let src = light.to_vec();
    for z in 1..size - 1 {
        for x in 1..size - 1 {
            let mut sum = 0u32;
            for dz in 0..3 {
                let row = (z + dz - 1) * size + x - 1;
                sum += src[row..row + 3].iter().map(|&v| u32::from(v)).sum::<u32>();
            }
            light[z * size + x] = (sum / 9) as u8;
        }
    }
}

/// Marches from each cell toward the sun over the heightfield.
fn terrain_shadows(surface: &[f32], size: usize, sun: Vec3, max_height: f32, shadow: &mut [bool]) {
    const STEP: f32 = 1.5;
    let toward = Vec2::new(sun.x, sun.z).normalized();
    let rise = sun.y / Vec2::new(sun.x, sun.z).length() * STEP;
    for z in 0..size {
        for x in 0..size {
            let i = z * size + x;
            let mut h = surface[i] + 0.3;
            let (mut px, mut pz) = (x as f32 + 0.5, z as f32 + 0.5);
            while h < max_height {
                px += toward.x * STEP;
                pz += toward.y * STEP;
                h += rise;
                if px < 0.0 || pz < 0.0 || px >= size as f32 || pz >= size as f32 {
                    break;
                }
                if surface[pz as usize * size + px as usize] > h {
                    shadow[i] = true;
                    break;
                }
            }
        }
    }
}

/// Stamps each tree's shadow: a disc under the crown, offset away from the
/// sun by the crown's height and stretched along the light.
fn tree_shadows(course: &Course, size: usize, sun: Vec3, shadow: &mut [bool]) {
    let away = Vec2::new(-sun.x, -sun.z).normalized();
    let slope = Vec2::new(sun.x, sun.z).length() / sun.y;
    for object in &course.objects {
        let ObjectKind::Tree { species, .. } = object.kind else {
            continue;
        };
        let crown = crate::gen::crown(species);
        let center = object.position + away * (object.height * 0.6 * slope);
        let stretch = 1.0 + object.height * 0.25 * slope / crown;
        let reach = (crown * stretch).ceil() as i64 + 1;
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                let (x, z) = (center.x as i64 + dx, center.y as i64 + dz);
                if x < 0 || z < 0 || x >= size as i64 || z >= size as i64 {
                    continue;
                }
                let d = Vec2::new(x as f32 + 0.5, z as f32 + 0.5) - center;
                let (along, across) = (d.dot(away) / stretch, d.cross(away));
                if along * along + across * across <= crown * crown {
                    shadow[z as usize * size + x as usize] = true;
                }
            }
        }
    }
}

/// The chunks, their height bounds and the objects standing in each.
fn chunks(surface: &[f32], size: usize, course: &Course) -> (Vec<Chunk>, usize) {
    let n = size.div_ceil(CHUNK);
    let mut out: Vec<Chunk> = Vec::with_capacity(n * n);
    for cz in 0..n {
        for cx in 0..n {
            let (x0, z0) = (cx * CHUNK, cz * CHUNK);
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for z in z0..(z0 + CHUNK + 1).min(size) {
                for x in x0..(x0 + CHUNK + 1).min(size) {
                    let h = surface[z * size + x];
                    lo = lo.min(h);
                    hi = hi.max(h);
                }
            }
            out.push(Chunk {
                x0,
                z0,
                min_h: lo - 3.0,
                max_h: hi,
                objects: Vec::new(),
            });
        }
    }
    for (i, object) in course.objects.iter().enumerate() {
        let (x, z) = (
            object.position.x.max(0.0) as usize / CHUNK,
            object.position.y.max(0.0) as usize / CHUNK,
        );
        if x < n && z < n {
            let chunk = &mut out[z * n + x];
            chunk.objects.push(i as u32);
            chunk.max_h = chunk.max_h.max(object.base + object.height);
        }
    }
    (out, n)
}
