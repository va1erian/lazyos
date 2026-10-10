//! The framebuffer and a plain scanline triangle rasterizer.
//!
//! Attributes (`1/z`, `u/z`, `v/z`) are planes over the screen, so every
//! pixel steps them by a constant and divides once for perspective-correct
//! world coordinates: the mowing stripes underfoot never swim. A pixel is
//! covered when its centre is inside the triangle, with edges evaluated
//! from the same two vertices in the same order for both triangles that
//! share them, so shared edges have neither gaps nor double coverage.
//! Triangles that cross the near plane are clipped in camera space first.

use crate::math::Vec3;

use super::camera::{View, NEAR};

/// An indexed framebuffer with a depth buffer of `1/z` (0 = infinitely far).
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub index: Vec<u8>,
    pub depth: Vec<f32>,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Frame {
        Frame {
            width,
            height,
            index: vec![0; width * height],
            depth: vec![0.0; width * height],
        }
    }

    pub fn clear_depth(&mut self) {
        self.depth.iter_mut().for_each(|d| *d = 0.0);
    }
}

/// A vertex in camera space with its world ground coordinates.
#[derive(Clone, Copy, Debug)]
pub struct CamVertex {
    pub c: Vec3,
    pub u: f32,
    pub v: f32,
}

/// A projected vertex: screen position and the attributes divided by z.
#[derive(Clone, Copy, Debug)]
struct Screen {
    x: f32,
    y: f32,
    iz: f32,
    uz: f32,
    vz: f32,
}

fn project(view: &View, p: CamVertex) -> Screen {
    let iz = 1.0 / p.c.z;
    let (x, y) = view.project(p.c);
    Screen {
        x,
        y,
        iz,
        uz: p.u * iz,
        vz: p.v * iz,
    }
}

fn lerp_vertex(a: CamVertex, b: CamVertex, t: f32) -> CamVertex {
    CamVertex {
        c: a.c + (b.c - a.c) * t,
        u: a.u + (b.u - a.u) * t,
        v: a.v + (b.v - a.v) * t,
    }
}

/// Draws a camera-space triangle, clipping it to the near plane;
/// `shade(x, y, z, u, v)` colours each covered pixel that passes the
/// depth test.
#[inline]
pub fn triangle<F>(frame: &mut Frame, view: &View, tri: [CamVertex; 3], shade: &mut F)
where
    F: FnMut(usize, usize, f32, f32, f32) -> u8,
{
    let inside = tri.map(|p| p.c.z >= NEAR);
    match inside.iter().filter(|&&b| b).count() {
        3 => fill(frame, tri.map(|p| project(view, p)), shade),
        0 => {}
        _ => {
            // Sutherland-Hodgman against z = NEAR: at most a quad.
            let mut poly = [tri[0]; 4];
            let mut n = 0;
            for i in 0..3 {
                let (a, b) = (tri[i], tri[(i + 1) % 3]);
                let (ia, ib) = (a.c.z >= NEAR, b.c.z >= NEAR);
                if ia {
                    poly[n] = a;
                    n += 1;
                }
                if ia != ib {
                    let t = (NEAR - a.c.z) / (b.c.z - a.c.z);
                    poly[n] = lerp_vertex(a, b, t);
                    n += 1;
                }
            }
            let s = poly.map(|p| project(view, p));
            for k in 1..n.saturating_sub(1) {
                fill(frame, [s[0], s[k], s[k + 1]], shade);
            }
        }
    }
}

/// Scan-converts a projected triangle.
fn fill<F>(frame: &mut Frame, tri: [Screen; 3], shade: &mut F)
where
    F: FnMut(usize, usize, f32, f32, f32) -> u8,
{
    let mut v = tri;
    v.sort_by(|a, b| a.y.total_cmp(&b.y));
    let [a, b, c] = v;
    let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
    if area.abs() < 1e-6 || !area.is_finite() {
        return;
    }
    // Attribute planes: value = a + dx * (x - a.x) + dy * (y - a.y).
    let plane = |fa: f32, fb: f32, fc: f32| {
        let dx = ((fb - fa) * (c.y - a.y) - (fc - fa) * (b.y - a.y)) / area;
        let dy = ((fc - fa) * (b.x - a.x) - (fb - fa) * (c.x - a.x)) / area;
        (dx, dy)
    };
    let (izx, izy) = plane(a.iz, b.iz, c.iz);
    let (uzx, uzy) = plane(a.uz, b.uz, c.uz);
    let (vzx, vzy) = plane(a.vz, b.vz, c.vz);
    let (w, h) = (frame.width as i64, frame.height as i64);
    let y_start = ((a.y - 0.5).ceil() as i64).max(0);
    let y_end = ((c.y - 0.5).ceil() as i64).min(h);
    let edge = |p: Screen, q: Screen, y: f32| p.x + (y - p.y) * (q.x - p.x) / (q.y - p.y);
    for py in y_start..y_end {
        let yc = py as f32 + 0.5;
        let long = edge(a, c, yc);
        let short = if yc < b.y {
            edge(a, b, yc)
        } else {
            edge(b, c, yc)
        };
        let (xl, xr) = if long < short {
            (long, short)
        } else {
            (short, long)
        };
        let x_start = ((xl - 0.5).ceil() as i64).max(0);
        let x_end = ((xr - 0.5).ceil() as i64).min(w);
        if x_start >= x_end {
            continue;
        }
        let (fx, fy) = (x_start as f32 + 0.5 - a.x, yc - a.y);
        let mut iz = a.iz + izx * fx + izy * fy;
        let mut uz = a.uz + uzx * fx + uzy * fy;
        let mut vz = a.vz + vzx * fx + vzy * fy;
        let row = py as usize * frame.width;
        for px in x_start as usize..x_end as usize {
            let i = row + px;
            if iz > frame.depth[i] {
                let z = 1.0 / iz;
                frame.index[i] = shade(px, py as usize, z, uz * z, vz * z);
                frame.depth[i] = iz;
            }
            iz += izx;
            uz += uzx;
            vz += vzx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::camera::Camera;

    fn cam(x: f32, y: f32, z: f32) -> CamVertex {
        CamVertex {
            c: Vec3::new(x, y, z),
            u: x,
            v: y,
        }
    }

    #[test]
    fn two_triangles_tile_a_square_exactly() {
        let view = Camera::new(Vec3::new(0.0, 0.0, 0.0), 0.0, 0.0).view(64, 64);
        let mut frame = Frame::new(64, 64);
        let mut count = vec![0u8; 64 * 64];
        let (p, q, r, s) = (
            cam(-1.0, -1.0, 4.0),
            cam(1.3, -1.0, 4.0),
            cam(-1.0, 1.1, 4.0),
            cam(1.3, 1.1, 4.0),
        );
        for tri in [[p, q, r], [q, s, r]] {
            frame.clear_depth();
            triangle(&mut frame, &view, tri, &mut |x, y, _, _, _| {
                count[y * 64 + x] += 1;
                1
            });
        }
        assert!(count.iter().all(|&c| c <= 1), "a pixel was drawn twice");
        let covered = count.iter().filter(|&&c| c == 1).count();
        assert!(covered > 300, "{covered}");
    }

    #[test]
    fn near_clipping_keeps_the_visible_part() {
        let view = Camera::new(Vec3::new(0.0, 0.0, 0.0), 0.0, 0.0).view(64, 64);
        let mut frame = Frame::new(64, 64);
        let mut drawn = 0;
        let tri = [
            cam(-2.0, -1.0, -1.0),
            cam(2.0, -1.0, -1.0),
            cam(0.0, -1.0, 10.0),
        ];
        triangle(&mut frame, &view, tri, &mut |_, _, z, _, _| {
            assert!(z >= NEAR * 0.99);
            drawn += 1;
            1
        });
        assert!(drawn > 0);
    }

    #[test]
    fn nearer_wins_the_depth_test() {
        let view = Camera::new(Vec3::new(0.0, 0.0, 0.0), 0.0, 0.0).view(32, 32);
        let mut frame = Frame::new(32, 32);
        let far = [
            cam(-5.0, -5.0, 10.0),
            cam(5.0, -5.0, 10.0),
            cam(0.0, 5.0, 10.0),
        ];
        let near = [
            cam(-5.0, -5.0, 5.0),
            cam(5.0, -5.0, 5.0),
            cam(0.0, 5.0, 5.0),
        ];
        triangle(&mut frame, &view, near, &mut |_, _, _, _, _| 2);
        triangle(&mut frame, &view, far, &mut |_, _, _, _, _| 1);
        assert_eq!(frame.index[16 * 32 + 16], 2);
    }
}
