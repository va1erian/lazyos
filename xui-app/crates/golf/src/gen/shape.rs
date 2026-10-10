//! Ground shapes with a signed distance: noise-perturbed ellipses for greens
//! and bunkers, oriented boxes for tee pads, and a variable-width ribbon for
//! fairways.

use crate::math::Vec2;
use crate::rng::Rng;

/// A wobbly ellipse: an ellipse whose radius is modulated by a few angular
/// harmonics, like a hand-drawn green or bunker.
#[derive(Clone, Debug, PartialEq)]
pub struct Blob {
    pub center: Vec2,
    /// Semi-axes along and across `angle`, metres.
    pub radii: (f32, f32),
    pub angle: f32,
    /// (amplitude, phase) for harmonics 2, 3 and 5.
    pub wobble: [(f32, f32); 3],
}

impl Blob {
    /// A random blob of about `area` m², elongated `stretch` times along
    /// `angle`, wobbling up to `wobble` of its radius.
    pub fn random(
        center: Vec2,
        area: f32,
        stretch: f32,
        angle: f32,
        wobble: f32,
        rng: &mut Rng,
    ) -> Blob {
        let r = (area / std::f32::consts::PI).sqrt();
        let s = stretch.max(1.0).sqrt();
        Blob {
            center,
            radii: (r * s, r / s),
            angle,
            wobble: [
                (
                    rng.range(0.0, wobble),
                    rng.range(0.0, std::f32::consts::TAU),
                ),
                (
                    rng.range(0.0, wobble * 0.7),
                    rng.range(0.0, std::f32::consts::TAU),
                ),
                (
                    rng.range(0.0, wobble * 0.4),
                    rng.range(0.0, std::f32::consts::TAU),
                ),
            ],
        }
    }

    /// The boundary's distance from the centre in world direction `phi`.
    pub fn radius_at(&self, phi: f32) -> f32 {
        let local = phi - self.angle;
        let (a, b) = self.radii;
        let (c, s) = (local.cos(), local.sin());
        let ellipse = a * b / ((b * c).powi(2) + (a * s).powi(2)).sqrt();
        let mut k = 1.0;
        for (&(amp, phase), harmonic) in self.wobble.iter().zip([2.0, 3.0, 5.0]) {
            k += amp * (harmonic * local + phase).cos();
        }
        ellipse * k
    }

    /// Approximate signed distance: negative inside.
    pub fn sdf(&self, p: Vec2) -> f32 {
        let d = p - self.center;
        d.length() - self.radius_at(d.angle())
    }

    /// The largest distance from the centre to the boundary.
    pub fn reach(&self) -> f32 {
        let amp: f32 = self.wobble.iter().map(|w| w.0).sum();
        self.radii.0.max(self.radii.1) * (1.0 + amp)
    }

    pub fn polygon(&self, points: usize) -> Vec<Vec2> {
        (0..points)
            .map(|i| {
                let phi = i as f32 / points as f32 * std::f32::consts::TAU;
                self.center + Vec2::from_angle(phi) * self.radius_at(phi)
            })
            .collect()
    }

    #[cfg(test)]
    pub fn area(&self) -> f32 {
        let n = 64;
        let step = std::f32::consts::TAU / n as f32;
        (0..n)
            .map(|i| 0.5 * self.radius_at(i as f32 * step).powi(2) * step)
            .sum()
    }
}

/// A rectangle centred on `center`, `half.0` along `dir` and `half.1` across.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pad {
    pub center: Vec2,
    pub dir: Vec2,
    pub half: (f32, f32),
}

impl Pad {
    pub fn sdf(&self, p: Vec2) -> f32 {
        let d = p - self.center;
        let (u, v) = (
            d.dot(self.dir).abs() - self.half.0,
            d.dot(self.dir.perp()).abs() - self.half.1,
        );
        let outside = Vec2::new(u.max(0.0), v.max(0.0)).length();
        outside + u.max(v).min(0.0)
    }
}

/// A fairway: the stretch `start..end` of a centerline with a half-width
/// and a sideways offset of its middle (so it meanders) sampled every
/// [`Ribbon::STEP`] metres.
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    pub start: f32,
    pub end: f32,
    pub half_widths: Vec<f32>,
    /// Toward the centerline's `perp` side, metres.
    pub offsets: Vec<f32>,
}

impl Ribbon {
    pub const STEP: f32 = 10.0;

    /// The half-width at distance `s` along the centerline.
    pub fn half_width(&self, s: f32) -> f32 {
        sampled(&self.half_widths, s)
    }

    /// How far the fairway's middle sits off the centerline at `s`.
    pub fn offset(&self, s: f32) -> f32 {
        sampled(&self.offsets, s)
    }

    /// The widest the fairway reaches from the centerline on either side.
    pub fn reach(&self) -> f32 {
        self.half_widths
            .iter()
            .zip(&self.offsets)
            .map(|(w, o)| w + o.abs())
            .fold(0.0, f32::max)
    }

    /// Signed distance across the fairway for a point `signed` metres off
    /// the centerline at `s` (inside `start..end`).
    pub fn across(&self, signed: f32, s: f32) -> f32 {
        (signed - self.offset(s)).abs() - self.half_width(s)
    }

    /// Signed distance from `p` to the fairway along `line`, with rounded
    /// ends where it starts and stops.
    #[cfg(test)]
    pub fn sdf(&self, line: &[Vec2], p: Vec2) -> f32 {
        let (signed, s) = crate::math::polyline_signed(p, line);
        self.sdf_at(line, p, signed, s)
    }

    /// [`Ribbon::sdf`] when the signed centerline distance is known.
    pub fn sdf_at(&self, line: &[Vec2], p: Vec2, signed: f32, s: f32) -> f32 {
        if s < self.start || s > self.end {
            // Beyond an end, round it with the fairway's half-width.
            let along = s.clamp(self.start, self.end);
            let (at, dir) = crate::math::polyline_at(line, along);
            let end = at + dir.perp() * self.offset(along);
            p.distance(end) - self.half_width(along)
        } else {
            self.across(signed, s)
        }
    }
}

/// Linear interpolation in a table sampled every [`Ribbon::STEP`].
fn sampled(table: &[f32], s: f32) -> f32 {
    if table.is_empty() {
        return 0.0;
    }
    let t = (s / Ribbon::STEP).max(0.0);
    let i = (t as usize).min(table.len() - 1);
    let j = (i + 1).min(table.len() - 1);
    let f = (t - i as f32).clamp(0.0, 1.0);
    table[i] + (table[j] - table[i]) * f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_area_and_sign() {
        let mut rng = Rng::new(4);
        let blob = Blob::random(Vec2::new(50.0, 50.0), 550.0, 1.3, 0.4, 0.08, &mut rng);
        assert!((blob.area() - 550.0).abs() < 60.0, "{}", blob.area());
        assert!(blob.sdf(blob.center) < 0.0);
        assert!(blob.sdf(Vec2::new(200.0, 50.0)) > 0.0);
    }

    #[test]
    fn pad_distance() {
        let pad = Pad {
            center: Vec2::new(0.0, 0.0),
            dir: Vec2::new(1.0, 0.0),
            half: (5.0, 3.0),
        };
        assert_eq!(pad.sdf(Vec2::new(0.0, 0.0)), -3.0);
        assert!((pad.sdf(Vec2::new(8.0, 0.0)) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn ribbon_widens_and_ends() {
        let line = [Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)];
        let mut ribbon = Ribbon {
            start: 20.0,
            end: 90.0,
            half_widths: vec![10.0; 11],
            offsets: vec![0.0; 11],
        };
        assert!(ribbon.sdf(&line, Vec2::new(50.0, 5.0)) < 0.0);
        assert!(ribbon.sdf(&line, Vec2::new(50.0, 15.0)) > 0.0);
        assert!(ribbon.sdf(&line, Vec2::new(5.0, 0.0)) > 0.0);
        // Shifted 8 m toward +y (the line's perp side), it covers y = 15.
        ribbon.offsets = vec![8.0; 11];
        assert!(ribbon.sdf(&line, Vec2::new(50.0, 15.0)) < 0.0);
        assert!(ribbon.sdf(&line, Vec2::new(50.0, -5.0)) > 0.0);
    }
}
