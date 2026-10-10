//! Terrain: each visible chunk as a grid mesh at its level of detail, with
//! skirts hiding the cracks between levels, shaded per pixel from the
//! cells under it (the mesh only gives the shape; material, light and
//! patterns come from the 1 m cells, so distant low-detail chunks keep
//! their detail).

use crate::course::Material;
use crate::math::Vec3;
use crate::noise::{cell_hash, value_noise};

use super::bake::{Bake, CHUNK, EDGE_SHIFT, HOLE_SHIFT, LIGHT_SHIFT, MATERIAL_BITS};
use super::camera::View;
use super::palette::{self as pal, shade_index, BAYER4};
use super::raster::{triangle, CamVertex, Frame};

/// How far skirts hang below a chunk's edge, metres.
const SKIRT: f32 = 4.0;
/// The jitter on material lookups, metres either way: blocky 1 m
/// boundaries become dithered edges. It is anchored to the ground (a hash
/// of the world position), not to the screen, so edges stay put as the
/// camera moves instead of crawling.
const EDGE_JITTER: f32 = 0.5;

/// The mesh step for a chunk at `distance` metres.
pub fn lod(distance: f32) -> usize {
    match distance {
        d if d < 70.0 => 1,
        d if d < 170.0 => 2,
        d if d < 340.0 => 4,
        _ => 8,
    }
}

/// Per-frame shading parameters.
#[derive(Clone, Copy)]
pub struct Shading<'a> {
    pub bake: &'a Bake,
    pub fog: &'a [u8],
    /// Haze starts at `fog_start` and is full at the far plane.
    pub fog_start: f32,
    pub fog_scale: f32,
    /// Ground metres per pixel per metre of depth (1 / focal length).
    pub per_pixel: f32,
    /// The eye's height above the ground, for the grazing angle.
    pub eye_height: f32,
}

impl<'a> Shading<'a> {
    pub fn new(bake: &'a Bake, fog: &'a [u8], view: &View) -> Shading<'a> {
        let fog_start = view.far * 0.22;
        let ground = bake.height_at(view.eye.x, view.eye.z);
        Shading {
            bake,
            fog,
            fog_start,
            fog_scale: 240.0 / (view.far - fog_start),
            per_pixel: 1.0 / view.focal,
            eye_height: (view.eye.y - ground).max(1.5),
        }
    }

    /// How much of a pattern with `scale` cycles per metre a pixel at depth
    /// `z` can show, 1 (all) to 0 (none). A pixel looking along the ground
    /// covers `z / focal` metres across and `z / height` times that in
    /// depth; a pattern finer than that is not drawn but aliased: every
    /// small move lands the pixel on a different cell and it sparkles. So
    /// fine patterns fade out with distance, a cheap mipmap for procedural
    /// textures.
    #[inline]
    pub fn detail(&self, scale: f32, z: f32) -> f32 {
        let footprint = z * self.per_pixel * (z / self.eye_height).max(1.0);
        let f = footprint * scale;
        (1.0 - (f - 0.3) / 0.7).clamp(0.0, 1.0)
    }

    /// `index` hazed for depth `z`, the haze level dithered.
    #[inline]
    pub fn haze(&self, index: u8, z: f32, x: usize, y: usize) -> u8 {
        let f = ((z - self.fog_start) * self.fog_scale).clamp(0.0, 240.0) as u32;
        let level = (f >> 4) + u32::from((f & 15) as u8 > BAYER4[y & 3][x & 3]);
        self.fog[(level.min(15) as usize) << 8 | usize::from(index)]
    }

    /// The terrain colour at world (`u`, `v`), pixel (`x`, `y`), depth `z`.
    #[inline]
    pub fn ground(&self, x: usize, y: usize, z: f32, u: f32, v: f32) -> u8 {
        let bake = self.bake;
        // Half-metre jitter cells, kept until a pixel covers about a metre:
        // past that the 1 m steps are below a pixel anyway.
        let fine = self.detail(1.0, z);
        let (jx, jz) = if fine > 0.0 {
            let k = EDGE_JITTER * 2.0 * fine;
            let (hu, hv) = ((u * 2.0) as i32, (v * 2.0) as i32);
            (
                (cell_hash(hu, hv, 41) - 0.5) * k,
                (cell_hash(hu, hv, 43) - 0.5) * k,
            )
        } else {
            (0.0, 0.0)
        };
        let last = (bake.size - 1) as f32;
        let cx = (u + jx).clamp(0.0, last) as usize;
        let cz = (v + jz).clamp(0.0, last) as usize;
        let cell = bake.cells[cz * bake.size + cx];
        // Interpolated, so 1 m blocks of light neither show up close nor
        // flip from one cell to the next in the distance.
        let light = self.smooth_light(u, v);
        let material = Material::from_u8((cell & MATERIAL_BITS) as u8);
        let index = self.material(material, cell, light, u, v, x, y, z);
        self.haze(index, z, x, y)
    }

    /// Bilinear light between the four cells around (`u`, `v`).
    #[inline]
    fn smooth_light(&self, u: f32, v: f32) -> i32 {
        let bake = self.bake;
        let last = bake.size - 2;
        let (fx, fz) = ((u - 0.5).max(0.0), (v - 0.5).max(0.0));
        let (ix, iz) = ((fx as usize).min(last), (fz as usize).min(last));
        let (tx, tz) = (fx - ix as f32, fz - iz as f32);
        let at =
            |x: usize, z: usize| ((bake.cells[z * bake.size + x] >> LIGHT_SHIFT) & 0xFF) as f32;
        let top = at(ix, iz) + (at(ix + 1, iz) - at(ix, iz)) * tx;
        let bottom = at(ix, iz + 1) + (at(ix + 1, iz + 1) - at(ix, iz + 1)) * tx;
        (top + (bottom - top) * tz.min(1.0)) as i32
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn material(
        &self,
        m: Material,
        cell: u32,
        light: i32,
        u: f32,
        v: f32,
        x: usize,
        y: usize,
        z: f32,
    ) -> u8 {
        let edge = ((cell >> EDGE_SHIFT) & 0xFF) as i32; // eighths of a metre
                                                         // Each pattern fades out where it is too fine for the pixel.
        let noise = |scale: f32, salt: u32| {
            let fade = self.detail(scale, z);
            if fade <= 0.0 {
                return 0.0;
            }
            (cell_hash((u * scale) as i32, (v * scale) as i32, salt) - 0.5) * fade
        };
        match m {
            Material::Fairway | Material::TeeBox => {
                let axis = self.bake.hole_axes[((cell >> HOLE_SHIFT) & 0xFF) as usize];
                let stripe = ((u * axis.x + v * axis.y) / 8.0).floor() as i32 & 1;
                let bump = (stripe as f32 * 14.0 * self.detail(1.0 / 8.0, z)) as i32;
                let tee = if m == Material::TeeBox { 14 } else { 0 };
                shade_index(pal::FAIRWAY, light + bump + tee - 6, x, y)
            }
            Material::Green => {
                let axis = self.bake.hole_axes[((cell >> HOLE_SHIFT) & 0xFF) as usize];
                let stripe = ((u * axis.y - v * axis.x) / 3.0).floor() as i32 & 1;
                let bump = (stripe as f32 * 8.0 * self.detail(1.0 / 3.0, z)) as i32;
                shade_index(pal::GREEN, light + bump + 4, x, y)
            }
            Material::Fringe => shade_index(pal::GREEN, light - 14, x, y),
            Material::FirstCut => {
                shade_index(pal::ROUGH, light + 26 + (noise(7.0, 3) * 16.0) as i32, x, y)
            }
            Material::Rough => {
                shade_index(pal::ROUGH, light + 8 + (noise(7.0, 5) * 40.0) as i32, x, y)
            }
            Material::DeepRough => {
                // Darker tussocks: soft patches of smooth noise (a hashed
                // 1.4 m grid with a hard threshold drew black squares).
                let tussock = value_noise(u * 0.55, v * 0.55, 9) * 0.65
                    + value_noise(u * 1.3, v * 1.3, 10) * 0.35;
                let clump = (-36.0
                    * crate::math::smoothstep(0.52, 0.72, tussock)
                    * self.detail(0.55, z)) as i32;
                shade_index(
                    pal::FESCUE,
                    light - 4 + (noise(6.0, 7) * 56.0) as i32 + clump,
                    x,
                    y,
                )
            }
            Material::Sand => {
                // A darker lip where the sand meets the grass.
                let lip = if edge < 6 { -36 } else { 0 };
                shade_index(
                    pal::SAND,
                    light + 20 + (noise(9.0, 11) * 22.0) as i32 + lip,
                    x,
                    y,
                )
            }
            Material::WasteArea => {
                shade_index(pal::SAND, light - 30 + (noise(4.0, 13) * 50.0) as i32, x, y)
            }
            Material::Woodland => shade_index(
                pal::FOREST_FLOOR,
                light + (noise(4.0, 15) * 40.0) as i32,
                x,
                y,
            ),
            Material::OutOfBounds => shade_index(
                pal::FOREST_FLOOR,
                light - 10 + (noise(4.0, 17) * 36.0) as i32,
                x,
                y,
            ),
            Material::CartPath => {
                let rim = if edge < 5 { -28 } else { 0 };
                shade_index(
                    pal::STONE,
                    light + 10 + rim + (noise(7.0, 19) * 12.0) as i32,
                    x,
                    y,
                )
            }
            Material::Rock => {
                // Blocky faces with cracks between them.
                let crack = if noise(0.9, 29) > 0.38 { -60 } else { 0 };
                let face = (noise(0.35, 31) * 70.0) as i32 + (noise(5.0, 37) * 24.0) as i32;
                shade_index(pal::STONE, light - 16 + face + crack, x, y)
            }
            Material::Water => {
                // Darker near the viewer (less sky reflected), ripples in
                // world space whose phases the palette cycles.
                let level = ((light - 70) / 50 + (z / 140.0) as i32).clamp(0, 3) as u8;
                // Far off the ripples would alias and a single phase would
                // pulse the whole lake: still water there, dithered into
                // the ripples as they come within reach.
                let ripples = self.detail(0.5, z);
                if ripples * 16.0 <= f32::from(BAYER4[y & 3][x & 3]) {
                    return pal::CALM_WATER + level;
                }
                let ripple = ((u * 0.45 + v * 0.2 + noise(0.3, 23) * 3.0).floor() as i32 & 3) as u8;
                pal::WATER + level * 4 + ripple
            }
        }
    }
}

/// Draws chunk `index` at mesh `step`, skirts included. `scratch` keeps its
/// vertex buffers between calls.
pub fn draw_chunk(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    index: usize,
    step: usize,
    scratch: &mut Scratch,
) {
    let bake = shading.bake;
    let chunk = &bake.chunks[index];
    let n = CHUNK / step;
    let side = n + 1;
    scratch.world.clear();
    scratch.cam.clear();
    let last = bake.size - 1;
    for j in 0..side {
        for i in 0..side {
            let gx = (chunk.x0 + i * step).min(last);
            let gz = (chunk.z0 + j * step).min(last);
            let p = Vec3::new(gx as f32 + 0.5, bake.height(gx, gz), gz as f32 + 0.5);
            scratch.world.push(p);
            scratch.cam.push(CamVertex {
                c: view.to_camera(p),
                u: p.x,
                v: p.z,
            });
        }
    }
    let mut shade = |x: usize, y: usize, z: f32, u: f32, v: f32| shading.ground(x, y, z, u, v);
    let eye = view.eye;
    for j in 0..n {
        for i in 0..n {
            let k = j * side + i;
            let quad = [k, k + 1, k + side, k + side + 1];
            // Winding (x0,z0) (x0,z1) (x1,z0) and (x1,z0) (x0,z1) (x1,z1): up.
            for [a, b, c] in [[quad[0], quad[2], quad[1]], [quad[1], quad[2], quad[3]]] {
                let (wa, wb, wc) = (scratch.world[a], scratch.world[b], scratch.world[c]);
                let normal = (wb - wa).cross(wc - wa);
                if normal.dot(eye - wa) <= 0.0 {
                    continue;
                }
                triangle(
                    frame,
                    view,
                    [scratch.cam[a], scratch.cam[b], scratch.cam[c]],
                    &mut shade,
                );
            }
        }
    }
    // Skirts along the four edges, both faces.
    let edges: [Vec<usize>; 4] = [
        (0..side).collect(),
        (0..side).map(|i| n * side + i).collect(),
        (0..side).map(|j| j * side).collect(),
        (0..side).map(|j| j * side + n).collect(),
    ];
    for edge in &edges {
        for pair in edge.windows(2) {
            let (a, b) = (scratch.cam[pair[0]], scratch.cam[pair[1]]);
            let drop = |p: CamVertex, w: Vec3| CamVertex {
                c: view.to_camera(w - Vec3::new(0.0, SKIRT, 0.0)),
                ..p
            };
            let (a2, b2) = (
                drop(a, scratch.world[pair[0]]),
                drop(b, scratch.world[pair[1]]),
            );
            triangle(frame, view, [a, b, b2], &mut shade);
            triangle(frame, view, [a, b2, a2], &mut shade);
        }
    }
}

/// Vertex buffers reused across chunks.
#[derive(Default)]
pub struct Scratch {
    world: Vec<Vec3>,
    cam: Vec<CamVertex>,
}
