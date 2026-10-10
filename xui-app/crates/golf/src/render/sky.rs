//! The sky and the backdrop, drawn first: a dithered gradient from the
//! horizon haze to zenith blue, clouds from thresholded fBm, and two ranges
//! of distant hills, a 360-degree silhouette sampled by yaw.

use std::f32::consts::TAU;

use crate::noise::Noise;

use super::camera::View;
use super::palette::{self as pal, shade_index, BAYER4};
use super::raster::Frame;

const AZIMUTHS: usize = 2048;
const ELEVATIONS: usize = 192;
/// The cloud map covers the sky from the horizon to this elevation.
const CLOUD_TOP: f32 = 0.9;

pub struct Sky {
    /// Cloud density per (azimuth, elevation), 0..255.
    clouds: Vec<u8>,
    /// The far and near ranges' crest elevations (radians) per azimuth.
    far: Vec<f32>,
    near: Vec<f32>,
}

impl Sky {
    pub fn new(seed: u64) -> Sky {
        let noise = Noise::new(seed ^ 0xC10D);
        let mut clouds = vec![0u8; AZIMUTHS * ELEVATIONS];
        for e in 0..ELEVATIONS {
            let el = e as f32 / ELEVATIONS as f32 * CLOUD_TOP;
            for a in 0..AZIMUTHS {
                let t = a as f32 / AZIMUTHS as f32;
                // Blend two samples a full turn apart: seamless at north.
                let sample = |az: f32| noise.fbm(az * 7.0, el * 16.0 + 3.0, 5, 0.55);
                let n = sample(t * TAU) * (1.0 - t) + sample(t * TAU - TAU) * t;
                // Fewer, flatter clouds low on the horizon.
                let fade = (el / 0.08).min(1.0) * (1.0 - el / CLOUD_TOP);
                clouds[e * AZIMUTHS + a] =
                    (((n - 0.08) * 3.2 * fade).clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
        let ridge = Noise::new(seed ^ 0x4111);
        let crest = |a: usize, base: f32, height: f32, salt: f32| {
            let n = ridge.periodic(a as f32 + salt, AZIMUTHS as u32, 6);
            (base + height * n * n).to_radians()
        };
        let far = (0..AZIMUTHS).map(|a| crest(a, 1.2, 6.0, 0.0)).collect();
        let near = (0..AZIMUTHS).map(|a| crest(a, 0.4, 3.5, 517.0)).collect();
        Sky { clouds, far, near }
    }

    /// Fills the framebuffer with sky and hills, depth infinitely far.
    pub fn draw(&self, frame: &mut Frame, view: &View, fog: &[u8]) {
        let columns: Vec<usize> = (0..frame.width)
            .map(|x| {
                let az = view.yaw + ((x as f32 + 0.5 - view.cx) / view.focal).atan();
                (az.rem_euclid(TAU) / TAU * AZIMUTHS as f32) as usize % AZIMUTHS
            })
            .collect();
        for y in 0..frame.height {
            let el = view.pitch + ((view.cy - y as f32 - 0.5) / view.focal).atan();
            let row = &mut frame.index[y * frame.width..(y + 1) * frame.width];
            let sky_light = ((el / 1.1).clamp(0.0, 1.0) * 240.0) as i32;
            let cloud_row = ((el / CLOUD_TOP) * ELEVATIONS as f32) as isize;
            for (x, (pixel, &a)) in row.iter_mut().zip(&columns).enumerate() {
                *pixel = if el < self.near[a] {
                    let depth = ((self.near[a] - el) * 900.0) as i32;
                    let index = shade_index(pal::HILLS, 120 - depth.min(90), x, y);
                    fog[7 << 8 | usize::from(index)]
                } else if el < self.far[a] {
                    let depth = ((self.far[a] - el) * 700.0) as i32;
                    let index = shade_index(pal::HILLS, 200 - depth.min(60), x, y);
                    fog[11 << 8 | usize::from(index)]
                } else {
                    let density = if (0..ELEVATIONS as isize).contains(&cloud_row) {
                        self.clouds[cloud_row as usize * AZIMUTHS + a]
                    } else {
                        0
                    };
                    cloud(density, x, y).unwrap_or_else(|| shade_index(pal::SKY, sky_light, x, y))
                };
            }
        }
        frame.depth.iter_mut().for_each(|d| *d = 0.0);
    }
}

/// A cloud pixel for `density`, dithered into two whites, or `None` for
/// clear sky.
fn cloud(density: u8, x: usize, y: usize) -> Option<u8> {
    let threshold = u16::from(BAYER4[y & 3][x & 3]) * 8;
    let density = u16::from(density);
    if density < 40 + threshold {
        None
    } else if density > 150 + threshold {
        Some(pal::CLOUD_LIGHT)
    } else {
        Some(pal::CLOUD)
    }
}
