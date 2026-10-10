//! Stage 1, the site terrain: domain-warped fBm, a landform archetype layer,
//! then droplet hydraulic erosion so the valleys look water-carved.

use crate::course::Archetype;
use crate::field::Field;
use crate::math::smoothstep;
use crate::noise::Noise;
use crate::rng::Rng;

/// The noise fields and tilt a site's landform is drawn from.
struct Landform {
    archetype: Archetype,
    noise: Noise,
    dunes: Noise,
    tilt: (f32, f32),
}

impl Landform {
    fn new(seed: u64, archetype: Archetype) -> Landform {
        let mut rng = Rng::stage(seed, "terrain.tilt");
        Landform {
            archetype,
            noise: Noise::new(seed ^ 0x51_7E),
            dunes: Noise::new(seed ^ 0xD0_E5),
            tilt: (rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)),
        }
    }

    /// The landform's height at world point (`fx`, `fy`), before erosion.
    fn height(&self, fx: f32, fy: f32) -> f32 {
        let (noise, tilt) = (&self.noise, self.tilt);
        match self.archetype {
            Archetype::Parkland => {
                let base = noise.warped(fx / 430.0, fy / 430.0, 5, 1.1);
                let detail = noise.fbm(fx / 95.0 + 40.0, fy / 95.0, 3, 0.5);
                26.0 * base + 4.0 * detail + (tilt.0 * fx + tilt.1 * fy) * 0.012
            }
            Archetype::Links => {
                let base = noise.warped(fx / 480.0, fy / 480.0, 4, 1.4);
                // Dune fields come and go across the site, ridges between
                // hollows: links land is rumpled, never a table.
                let field_mask =
                    smoothstep(-0.35, 0.3, noise.fbm(fx / 300.0 - 9.0, fy / 300.0, 2, 0.5));
                let ridges = self.dunes.ridged(fx / 70.0, fy / 52.0, 4);
                let swales = noise.fbm(fx / 140.0 + 17.0, fy / 140.0, 3, 0.5);
                9.0 * base
                    + 13.0 * field_mask * ridges * ridges
                    + 3.0 * swales
                    + (tilt.0 * fx + tilt.1 * fy) * 0.006
            }
            Archetype::Mountain => {
                let base = noise.warped(fx / 600.0, fy / 600.0, 5, 1.3);
                let ridges = self.dunes.ridged(fx / 260.0, fy / 260.0, 4);
                let h = 42.0 * base + 16.0 * ridges + (tilt.0 * fx + tilt.1 * fy) * 0.025;
                terrace(h, 7.0)
            }
        }
    }
}

/// The natural heightfield for `archetype`, metres, lowest point at 0.
pub fn site(seed: u64, archetype: Archetype, size: usize) -> Field<f32> {
    let land = Landform::new(seed, archetype);
    let mut field = Field::new(size, size, 0.0f32);
    for y in 0..size {
        for x in 0..size {
            field.data[y * size + x] = land.height(x as f32, y as f32);
        }
    }
    let droplets = size * size / 12;
    erode(
        &mut field,
        &mut Rng::stage(seed, "terrain.erosion"),
        droplets,
    );
    let (lo, _) = field.min_max();
    field.data.iter_mut().for_each(|h| *h -= lo);
    field
}

/// The same landform sampled every `stride` metres, without erosion: a
/// cheap preview for judging a site before building it.
pub fn preview(seed: u64, archetype: Archetype, size: usize, stride: usize) -> Field<f32> {
    let land = Landform::new(seed, archetype);
    let n = size / stride;
    let mut field = Field::new(n, n, 0.0f32);
    for y in 0..n {
        for x in 0..n {
            field.data[y * n + x] = land.height((x * stride) as f32, (y * stride) as f32);
        }
    }
    field
}

/// Soft terraces `step` metres apart, half blended with the slope between.
fn terrace(h: f32, step: f32) -> f32 {
    let t = h / step;
    let stepped = step * (t.floor() + smoothstep(0.3, 0.7, t.fract()));
    0.5 * (h + stepped)
}

/// Droplet hydraulic erosion (after Hans Theobald Beyer's particle model):
/// each droplet runs downhill, eroding where it can carry more sediment and
/// depositing where it slows or climbs.
pub fn erode(field: &mut Field<f32>, rng: &mut Rng, droplets: usize) {
    const INERTIA: f32 = 0.08;
    const CAPACITY: f32 = 4.0;
    const MIN_CAPACITY: f32 = 0.01;
    const ERODE: f32 = 0.25;
    const DEPOSIT: f32 = 0.3;
    const EVAPORATE: f32 = 0.02;
    const GRAVITY: f32 = 4.0;
    const STEPS: usize = 40;
    let (w, h) = (field.width, field.height);
    let limit = (w - 2) as f32;
    let limit_y = (h - 2) as f32;
    for _ in 0..droplets {
        let (mut px, mut py) = (rng.range(1.0, limit), rng.range(1.0, limit_y));
        let (mut dx, mut dy) = (0.0f32, 0.0f32);
        let (mut speed, mut water, mut sediment) = (1.0f32, 1.0f32, 0.0f32);
        for _ in 0..STEPS {
            let (height, gx, gy) = height_gradient(field, px, py);
            dx = dx * INERTIA - gx * (1.0 - INERTIA);
            dy = dy * INERTIA - gy * (1.0 - INERTIA);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-6 {
                break;
            }
            dx /= len;
            dy /= len;
            let (nx, ny) = (px + dx, py + dy);
            if !(1.0..limit).contains(&nx) || !(1.0..limit_y).contains(&ny) {
                break;
            }
            let delta = height_gradient(field, nx, ny).0 - height;
            let capacity = (-delta * speed * water * CAPACITY).max(MIN_CAPACITY);
            if sediment > capacity || delta > 0.0 {
                let amount = if delta > 0.0 {
                    delta.min(sediment)
                } else {
                    (sediment - capacity) * DEPOSIT
                };
                sediment -= amount;
                spread(field, px, py, amount);
            } else {
                let amount = ((capacity - sediment) * ERODE).min(-delta);
                spread(field, px, py, -amount);
                sediment += amount;
            }
            speed = (speed * speed + delta * GRAVITY).max(0.0).sqrt();
            water *= 1.0 - EVAPORATE;
            px = nx;
            py = ny;
        }
    }
}

/// Height and gradient at a point, bilinear over the four cells around it
/// (cell indices, not cell centres: erosion works on the lattice).
fn height_gradient(field: &Field<f32>, x: f32, y: f32) -> (f32, f32, f32) {
    let (ix, iy) = (x as usize, y as usize);
    let (u, v) = (x - ix as f32, y - iy as f32);
    let i = iy * field.width + ix;
    let (nw, ne) = (field.data[i], field.data[i + 1]);
    let (sw, se) = (field.data[i + field.width], field.data[i + field.width + 1]);
    let gx = (ne - nw) * (1.0 - v) + (se - sw) * v;
    let gy = (sw - nw) * (1.0 - u) + (se - ne) * u;
    let height = nw * (1.0 - u) * (1.0 - v) + ne * u * (1.0 - v) + sw * (1.0 - u) * v + se * u * v;
    (height, gx, gy)
}

/// Add `amount` to the four cells around a point, bilinearly weighted.
fn spread(field: &mut Field<f32>, x: f32, y: f32, amount: f32) {
    let (ix, iy) = (x as usize, y as usize);
    let (u, v) = (x - ix as f32, y - iy as f32);
    let i = iy * field.width + ix;
    let w = field.width;
    field.data[i] += amount * (1.0 - u) * (1.0 - v);
    field.data[i + 1] += amount * u * (1.0 - v);
    field.data[i + w] += amount * (1.0 - u) * v;
    field.data[i + w + 1] += amount * u * v;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erosion_conserves_roughly_and_smooths() {
        let mut f = Field::new(64, 64, 0.0f32);
        for y in 0..64 {
            for x in 0..64 {
                f.set(x, y, (x as f32 * 0.4).sin() * 3.0 + y as f32 * 0.2);
            }
        }
        let before: f32 = f.data.iter().sum();
        erode(&mut f, &mut Rng::new(5), 2000);
        let after: f32 = f.data.iter().sum();
        assert!(f.data.iter().all(|h| h.is_finite()));
        // Sediment still carried when a droplet dies is lost; never much.
        assert!(
            (before - after).abs() / before.abs() < 0.05,
            "{before} {after}"
        );
    }

    #[test]
    fn archetypes_have_their_relief() {
        let relief = |a| {
            let (lo, hi) = site(11, a, 256).min_max();
            hi - lo
        };
        let (links, mountain) = (relief(Archetype::Links), relief(Archetype::Mountain));
        assert!(links < mountain, "links {links} mountain {mountain}");
        assert!(links > 1.0);
    }
}
