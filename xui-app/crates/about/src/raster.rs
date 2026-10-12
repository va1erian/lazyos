//! The rasteriser: an RGBA framebuffer with a depth buffer, perspective-
//! correct triangles shaded per pixel by a caller's closure, and depth-tested
//! lines. Depth is stored as `1 / z` (larger is nearer, `0` is empty).

use crate::math::Vec3;

/// A vertex ready to rasterise: its pixel position and the view-space data
/// the shader interpolates.
#[derive(Clone, Copy, Debug)]
pub struct ScreenVertex {
    pub x: f32,
    pub y: f32,
    /// `1 / z` in view space.
    pub inv_z: f32,
    pub position: Vec3,
    pub normal: Vec3,
}

pub struct Framebuffer {
    width: usize,
    height: usize,
    /// `width * height * 4` bytes, top-down, alpha always 255.
    pub rgba: Vec<u8>,
    depth: Vec<f32>,
}

impl Framebuffer {
    /// A framebuffer of `width` x `height`, reusing `rgba`'s allocation.
    pub fn new(width: usize, height: usize, mut rgba: Vec<u8>) -> Framebuffer {
        rgba.clear();
        rgba.resize(width * height * 4, 255);
        Framebuffer {
            width,
            height,
            rgba,
            depth: vec![0.0; width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Forgets the depth, keeping the colours (the background is drawn
    /// first and has none).
    pub fn clear_depth(&mut self) {
        self.depth.fill(0.0);
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let at = (y * self.width + x) * 4;
        self.rgba[at..at + 3].copy_from_slice(&rgb);
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * 4;
        [self.rgba[at], self.rgba[at + 1], self.rgba[at + 2]]
    }

    /// Adds `rgb` to the pixel, clamping (light, not paint).
    #[inline]
    pub fn add(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let at = (y * self.width + x) * 4;
        for (k, v) in rgb.iter().enumerate() {
            self.rgba[at + k] = self.rgba[at + k].saturating_add(*v);
        }
    }

    /// Mixes `rgb` over the pixel by `alpha` (0..=255).
    #[inline]
    pub fn blend(&mut self, x: usize, y: usize, rgb: [u8; 3], alpha: u32) {
        let at = (y * self.width + x) * 4;
        for (k, v) in rgb.iter().enumerate() {
            let old = self.rgba[at + k] as u32;
            self.rgba[at + k] = ((old * (255 - alpha) + *v as u32 * alpha) / 255) as u8;
        }
    }

    /// Fills a triangle, calling `shade(position, normal)` for each pixel
    /// that passes the depth test. Both windings are drawn.
    pub fn triangle(
        &mut self,
        v: &[ScreenVertex; 3],
        mut shade: impl FnMut(Vec3, Vec3) -> [u8; 3],
    ) {
        let area = edge(v[0], v[1], v[2].x, v[2].y);
        if area.abs() < 1e-6 {
            return;
        }
        let inv_area = 1.0 / area;
        let min_x = v
            .iter()
            .map(|p| p.x)
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as usize;
        let min_y = v
            .iter()
            .map(|p| p.y)
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as usize;
        let max_x = (v.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil() as i64)
            .min(self.width as i64 - 1);
        let max_y = (v.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil() as i64)
            .min(self.height as i64 - 1);
        if max_x < min_x as i64 || max_y < min_y as i64 {
            return;
        }
        // Per-vertex data premultiplied by 1/z, so that a screen-space
        // linear blend is perspective correct after dividing by the blended
        // 1/z.
        let pos = v.map(|p| p.position * p.inv_z);
        let nor = v.map(|p| p.normal * p.inv_z);
        for y in min_y..=max_y as usize {
            let py = y as f32 + 0.5;
            for x in min_x..=max_x as usize {
                let px = x as f32 + 0.5;
                let w0 = edge(v[1], v[2], px, py) * inv_area;
                let w1 = edge(v[2], v[0], px, py) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let inv_z = w0 * v[0].inv_z + w1 * v[1].inv_z + w2 * v[2].inv_z;
                let at = y * self.width + x;
                if inv_z <= self.depth[at] {
                    continue;
                }
                self.depth[at] = inv_z;
                let z = 1.0 / inv_z;
                let p = (pos[0] * w0 + pos[1] * w1 + pos[2] * w2) * z;
                let n = (nor[0] * w0 + nor[1] * w1 + nor[2] * w2) * z;
                self.set(x, y, shade(p, n.normalized()));
            }
        }
    }

    /// A one-pixel line from `a` to `b` in light (`add`), each pixel
    /// brightened by `color(t)` for `t` along the line. With `occlude`, a
    /// pixel behind what the depth buffer holds is skipped.
    pub fn line(
        &mut self,
        a: &ScreenVertex,
        b: &ScreenVertex,
        occlude: bool,
        color: impl Fn(f32) -> [u8; 3],
    ) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0);
        if steps > 4.0 * (self.width + self.height) as f32 {
            return;
        }
        let n = steps as usize;
        for i in 0..=n {
            let t = i as f32 / steps;
            let (x, y) = (a.x + dx * t, a.y + dy * t);
            if x < 0.0 || y < 0.0 || x >= self.width as f32 || y >= self.height as f32 {
                continue;
            }
            let (xi, yi) = (x as usize, y as usize);
            if occlude {
                let inv_z = a.inv_z + (b.inv_z - a.inv_z) * t;
                // A small bias in front, so an edge lying on its own surface shows.
                if inv_z * 1.02 < self.depth[yi * self.width + xi] {
                    continue;
                }
            }
            self.add(xi, yi, color(t));
        }
    }
}

#[inline]
fn edge(a: ScreenVertex, b: ScreenVertex, px: f32, py: f32) -> f32 {
    (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(x: f32, y: f32, z: f32) -> ScreenVertex {
        ScreenVertex {
            x,
            y,
            inv_z: 1.0 / z,
            position: Vec3::new(x, y, z),
            normal: Vec3::new(0.0, 0.0, -1.0),
        }
    }

    #[test]
    fn a_triangle_covers_about_half_its_square_and_both_windings_draw() {
        for flip in [false, true] {
            let mut fb = Framebuffer::new(32, 32, Vec::new());
            let mut tri = [
                vertex(2.0, 2.0, 5.0),
                vertex(30.0, 2.0, 5.0),
                vertex(2.0, 30.0, 5.0),
            ];
            if flip {
                tri.swap(1, 2);
            }
            let mut count = 0;
            fb.triangle(&tri, |_, _| {
                count += 1;
                [1, 2, 3]
            });
            assert!((count - 392_i32).abs() < 40, "{count} pixels");
        }
    }

    #[test]
    fn the_nearer_triangle_wins_regardless_of_order() {
        for near_first in [true, false] {
            let mut fb = Framebuffer::new(8, 8, Vec::new());
            let near = [
                vertex(0.0, 0.0, 2.0),
                vertex(8.0, 0.0, 2.0),
                vertex(0.0, 8.0, 2.0),
            ];
            let far = [
                vertex(0.0, 0.0, 9.0),
                vertex(8.0, 0.0, 9.0),
                vertex(0.0, 8.0, 9.0),
            ];
            let (first, second) = if near_first { (near, far) } else { (far, near) };
            fb.triangle(
                &first,
                |p, _| if p.z < 5.0 { [200, 0, 0] } else { [0, 0, 200] },
            );
            fb.triangle(
                &second,
                |p, _| if p.z < 5.0 { [200, 0, 0] } else { [0, 0, 200] },
            );
            assert_eq!(fb.get(1, 1), [200, 0, 0]);
        }
    }

    #[test]
    fn offscreen_and_degenerate_triangles_are_harmless() {
        let mut fb = Framebuffer::new(8, 8, Vec::new());
        let off = [
            vertex(-50.0, -50.0, 3.0),
            vertex(-40.0, -50.0, 3.0),
            vertex(-50.0, -40.0, 3.0),
        ];
        fb.triangle(&off, |_, _| panic!("drawn offscreen"));
        let line = [
            vertex(1.0, 1.0, 3.0),
            vertex(4.0, 4.0, 3.0),
            vertex(7.0, 7.0, 3.0),
        ];
        fb.triangle(&line, |_, _| panic!("drawn degenerate"));
    }
}
