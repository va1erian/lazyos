//! The synthwave set the model floats in: a gradient sky with stars, a
//! striped sun, and an infinite neon grid floor scrolling toward the
//! camera. The floor is ray-cast per pixel against the plane the model's
//! shadow falls on, so it shares the model's camera exactly.

use crate::raster::Framebuffer;

/// How far below the camera (world units) the floor lies.
pub const FLOOR_DEPTH: f32 = 1.5;

/// The camera as the backdrop needs it, in framebuffer pixels.
#[derive(Clone, Copy, Debug)]
pub struct Eye {
    /// Screen x of the vanishing point (and the model's centre).
    pub cx: f32,
    /// The horizon row.
    pub horizon: f32,
    /// Focal length in pixels.
    pub focal: f32,
    /// Distance to the model's centre.
    pub dist: f32,
}

#[derive(Clone, Copy)]
struct Star {
    x: f32,
    y: f32,
    size: u8,
    phase: f32,
}

pub struct Backdrop {
    stars: Vec<Star>,
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

fn rgb(c: [f32; 3]) -> [u8; 3] {
    [byte(c[0]), byte(c[1]), byte(c[2])]
}

impl Backdrop {
    pub fn new() -> Backdrop {
        // A fixed xorshift sequence: the same sky every launch.
        let mut s = 0x2545_F491_u32;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32
        };
        let stars = (0..170)
            .map(|_| Star {
                x: next(),
                y: next(),
                size: if next() > 0.9 { 2 } else { 1 },
                phase: next() * std::f32::consts::TAU,
            })
            .collect();
        Backdrop { stars }
    }

    /// Paints the whole framebuffer: sky, stars, sun, floor.
    pub fn paint(&self, fb: &mut Framebuffer, eye: &Eye, time: f32) {
        let (w, h) = (fb.width(), fb.height());
        let horizon = eye.horizon.clamp(1.0, h as f32 - 1.0);
        self.sky(fb, horizon, time);
        self.sun(fb, eye, horizon);
        let top = horizon as usize;
        for y in top..h {
            self.floor_row(fb, eye, y, w, time);
        }
    }

    fn sky(&self, fb: &mut Framebuffer, horizon: f32, time: f32) {
        let (w, h) = (fb.width(), fb.height());
        let (top, mid, low) = ([6.0, 3.0, 28.0], [58.0, 10.0, 96.0], [255.0, 64.0, 140.0]);
        for y in 0..h {
            let t = (y as f32 / horizon).min(1.0);
            let c = if t < 0.62 {
                lerp3(top, mid, t / 0.62)
            } else {
                let u = (t - 0.62) / 0.38;
                lerp3(mid, low, u * u)
            };
            let c = rgb(c);
            for x in 0..w {
                fb.set(x, y, c);
            }
        }
        for s in &self.stars {
            let (x, y) = ((s.x * w as f32) as usize, (s.y * horizon * 0.9) as usize);
            let glint = 0.55 + 0.45 * (time * 1.7 + s.phase).sin();
            let v = byte(255.0 * glint);
            for dy in 0..s.size as usize {
                for dx in 0..s.size as usize {
                    if x + dx < w && y + dy < h {
                        fb.add(x + dx, y + dy, [v, v, byte(v as f32 * 0.9)]);
                    }
                }
            }
        }
    }

    /// A striped sun: yellow to magenta, with gaps that widen downward, and
    /// a soft halo.
    fn sun(&self, fb: &mut Framebuffer, eye: &Eye, horizon: f32) {
        let (w, h) = (fb.width() as i32, fb.height() as i32);
        let radius = (eye.focal * 0.36).max(8.0);
        let cy = horizon - radius * 0.55;
        let reach = (radius * 1.9) as i32;
        let stripe = (radius * 0.11).max(2.0);
        let top = [255.0, 236.0, 100.0];
        let bottom = [255.0, 40.0, 150.0];
        for y in ((cy as i32) - reach).max(0)..((cy as i32) + reach).min(horizon as i32) {
            for x in ((eye.cx as i32) - reach).max(0)..((eye.cx as i32) + reach).min(w) {
                if y >= h {
                    continue;
                }
                let (dx, dy) = (x as f32 - eye.cx, y as f32 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                if d <= radius {
                    let v = (dy + radius) / (2.0 * radius);
                    let gap = v > 0.5 && ((y as f32 / stripe).fract()) < (v - 0.5) * 1.5;
                    if !gap {
                        let c = lerp3(top, bottom, (v - 0.15) / 0.75);
                        fb.set(x as usize, y as usize, rgb(c));
                    }
                } else {
                    let k = (d - radius) / radius;
                    let glow = 130.0 / (1.0 + k * k * 22.0);
                    fb.add(
                        x as usize,
                        y as usize,
                        [byte(glow), byte(glow * 0.25), byte(glow * 0.55)],
                    );
                }
            }
        }
    }

    fn floor_row(&self, fb: &mut Framebuffer, eye: &Eye, y: usize, w: usize, time: f32) {
        let below = y as f32 + 0.5 - eye.horizon;
        if below <= 0.0 {
            return;
        }
        // Where this row meets the floor: its depth, and a pixel's footprint.
        let z = FLOOR_DEPTH * eye.focal / below;
        let foot = z / eye.focal;
        let scroll = time * 0.9;
        let near = 1.0 / (1.0 + z * 0.07);
        let tint = (z / 36.0).min(1.0);
        let line = lerp3([0.0, 235.0, 255.0], [255.0, 50.0, 210.0], tint);
        let base = lerp3(
            [34.0, 5.0, 62.0],
            [130.0, 18.0, 108.0],
            1.0 - near.powf(1.4),
        );
        let gz = (z + scroll).rem_euclid(1.0);
        let lz = gz.min(1.0 - gz);
        let width = 0.02 + foot * 1.1;
        let row_line = (1.0 - lz / width).clamp(0.0, 1.0);
        for x in 0..w {
            let xw = (x as f32 + 0.5 - eye.cx) * foot;
            let gx = xw.rem_euclid(1.0);
            let lx = gx.min(1.0 - gx);
            let col_line = (1.0 - lx / width).clamp(0.0, 1.0);
            let i = row_line.max(col_line) * near.max(0.12);
            let c = [
                base[0] + line[0] * i,
                base[1] + line[1] * i,
                base[2] + line[2] * i,
            ];
            fb.set(x, y, rgb(c));
        }
    }

    /// Darkens the floor under the model: an ellipse that softens with the
    /// model's height.
    pub fn shadow(&self, fb: &mut Framebuffer, eye: &Eye, radius: f32) {
        let centre_y = eye.horizon + FLOOR_DEPTH / eye.dist * eye.focal;
        let rx = radius / eye.dist * eye.focal;
        let ry = (rx * FLOOR_DEPTH / eye.dist * 1.6).max(2.0);
        let (w, h) = (fb.width() as i32, fb.height() as i32);
        for y in ((centre_y - ry) as i32).max(0)..((centre_y + ry) as i32 + 1).min(h) {
            for x in ((eye.cx - rx) as i32).max(0)..((eye.cx + rx) as i32 + 1).min(w) {
                let (u, v) = ((x as f32 - eye.cx) / rx, (y as f32 - centre_y) / ry);
                let d = u * u + v * v;
                if d < 1.0 {
                    let a = ((1.0 - d) * 215.0) as u32;
                    fb.blend(x as usize, y as usize, [2, 0, 14], a);
                }
            }
        }
    }
}

impl Default for Backdrop {
    fn default() -> Self {
        Backdrop::new()
    }
}
