//! Seeded lattice noise: 2D gradient noise, its fBm and ridged sums, domain
//! warping, and a 1D variant for the backdrop's hill silhouette.

use crate::rng::mix;

/// A seeded noise field.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    seed: u64,
}

impl Noise {
    pub fn new(seed: u64) -> Noise {
        Noise { seed: mix(seed) }
    }

    /// The gradient at lattice point (`ix`, `iy`): one of 8 unit directions.
    #[inline]
    fn gradient(&self, ix: i32, iy: i32) -> (f32, f32) {
        let h = mix(self.seed ^ (ix as u32 as u64) << 32 ^ iy as u32 as u64);
        const D: f32 = std::f32::consts::FRAC_1_SQRT_2;
        match h & 7 {
            0 => (1.0, 0.0),
            1 => (-1.0, 0.0),
            2 => (0.0, 1.0),
            3 => (0.0, -1.0),
            4 => (D, D),
            5 => (-D, D),
            6 => (D, -D),
            _ => (-D, -D),
        }
    }

    /// Gradient noise at (`x`, `y`), roughly in `-1.0..1.0`.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (fx, fy) = (x.floor(), y.floor());
        let (ix, iy) = (fx as i32, fy as i32);
        let (tx, ty) = (x - fx, y - fy);
        let corner = |dx: i32, dy: i32| {
            let (gx, gy) = self.gradient(ix + dx, iy + dy);
            gx * (tx - dx as f32) + gy * (ty - dy as f32)
        };
        let (u, v) = (fade(tx), fade(ty));
        let top = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * u;
        let bottom = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * u;
        (top + (bottom - top) * v) * 1.41
    }

    /// Fractal Brownian motion: `octaves` layers, each at twice the
    /// frequency and `gain` times the amplitude. Normalised to about -1..1.
    pub fn fbm(&self, x: f32, y: f32, octaves: u32, gain: f32) -> f32 {
        let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
        for octave in 0..octaves {
            // Each octave is offset so the lattice points never line up.
            let shift = octave as f32 * 17.13;
            sum += amp * self.sample(x * freq + shift, y * freq - shift);
            norm += amp;
            amp *= gain;
            freq *= 2.0;
        }
        sum / norm
    }

    /// Ridged multifractal: sharp crests where the noise crosses zero, for
    /// dunes and ridges. In `0.0..1.0`.
    pub fn ridged(&self, x: f32, y: f32, octaves: u32) -> f32 {
        let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
        for octave in 0..octaves {
            let shift = octave as f32 * 31.7;
            let n = 1.0 - self.sample(x * freq + shift, y * freq + shift).abs();
            sum += amp * n * n;
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm
    }

    /// fBm sampled through a warped domain, so ridges are not grid-aligned.
    pub fn warped(&self, x: f32, y: f32, octaves: u32, warp: f32) -> f32 {
        let wx = self.fbm(x + 5.2, y + 1.3, 3, 0.5);
        let wy = self.fbm(x - 3.7, y + 9.2, 3, 0.5);
        self.fbm(x + warp * wx, y + warp * wy, octaves, 0.5)
    }

    /// Periodic 1D noise over `0.0..period`, for a 360-degree silhouette:
    /// the first octave has 6 lattice cells per period, each next one twice
    /// as many.
    pub fn periodic(&self, t: f32, period: u32, octaves: u32) -> f32 {
        let (mut sum, mut amp, mut norm) = (0.0, 1.0, 0.0);
        let mut cells = 6u32;
        for octave in 0..octaves {
            let x = t / period as f32 * cells as f32;
            let i = x.floor() as i64;
            let f = x - i as f32;
            let at = |i: i64| {
                let k = i.rem_euclid(i64::from(cells)) as u64;
                (mix(self.seed ^ k ^ (u64::from(octave) << 40)) >> 40) as f32 / (1u64 << 24) as f32
            };
            sum += amp * (at(i) + (at(i + 1) - at(i)) * fade(f));
            norm += amp;
            amp *= 0.5;
            cells *= 2;
        }
        sum / norm
    }
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Smooth value noise in `0.0..1.0`: the cell hashes at the four corners
/// around (`x`, `y`), blended with a smoothstep, so it has no square edges.
#[inline]
pub fn value_noise(x: f32, y: f32, salt: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    let (ix, iy) = (fx as i32, fy as i32);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (tx, ty) = (s(x - fx), s(y - fy));
    let top =
        cell_hash(ix, iy, salt) + (cell_hash(ix + 1, iy, salt) - cell_hash(ix, iy, salt)) * tx;
    let bottom = cell_hash(ix, iy + 1, salt)
        + (cell_hash(ix + 1, iy + 1, salt) - cell_hash(ix, iy + 1, salt)) * tx;
    top + (bottom - top) * ty
}

/// A cheap hash of a grid cell to `0.0..1.0`, for per-pixel patterns.
#[inline]
pub fn cell_hash(x: i32, y: i32, salt: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ salt.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5BD1_E995);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65536.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_bounded_and_continuous() {
        let n = Noise::new(3);
        let mut prev = n.sample(0.0, 0.5);
        for i in 1..2000 {
            let x = i as f32 * 0.01;
            let v = n.sample(x, 0.5);
            assert!(v.abs() <= 1.5, "{v}");
            assert!((v - prev).abs() < 0.1, "jump at {x}");
            prev = v;
        }
        let r = n.ridged(1.3, 2.7, 4);
        assert!((0.0..=1.0).contains(&r));
    }

    #[test]
    fn periodic_wraps() {
        let n = Noise::new(9);
        let a = n.periodic(0.0, 16, 4);
        let b = n.periodic(16.0, 16, 4);
        assert!((a - b).abs() < 1e-5);
    }
}
