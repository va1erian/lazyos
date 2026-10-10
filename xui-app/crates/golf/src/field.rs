//! A row-major 2D grid of cells, with the whole-grid passes the generator
//! needs: bilinear sampling, gradients, box blur and distance transforms.

/// A `width` x `height` grid, one value per 1 m cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Field<T> {
    pub width: usize,
    pub height: usize,
    pub data: Vec<T>,
}

impl<T: Copy> Field<T> {
    pub fn new(width: usize, height: usize, fill: T) -> Field<T> {
        Field {
            width,
            height,
            data: vec![fill; width * height],
        }
    }

    #[inline]
    pub fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> T {
        self.data[y * self.width + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, value: T) {
        let i = y * self.width + x;
        self.data[i] = value;
    }

    /// The cell under world point (`x`, `y`), clamped to the grid.
    #[inline]
    pub fn at(&self, x: f32, y: f32) -> T {
        let cx = (x.max(0.0) as usize).min(self.width - 1);
        let cy = (y.max(0.0) as usize).min(self.height - 1);
        self.get(cx, cy)
    }

    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height
    }
}

impl Field<f32> {
    /// Bilinear sample at a world point; cell centres sit at integer + 0.5.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let fx = (x - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy = (y - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (ix, iy) = (fx as usize, fy as usize);
        let (x1, y1) = ((ix + 1).min(self.width - 1), (iy + 1).min(self.height - 1));
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let top = self.get(ix, iy) + (self.get(x1, iy) - self.get(ix, iy)) * tx;
        let bottom = self.get(ix, y1) + (self.get(x1, y1) - self.get(ix, y1)) * tx;
        top + (bottom - top) * ty
    }

    /// The slope (rise over run) at a world point, measured across `span`
    /// metres so a single bumpy cell does not dominate.
    pub fn slope(&self, x: f32, y: f32, span: f32) -> f32 {
        let dx = self.sample(x + span, y) - self.sample(x - span, y);
        let dy = self.sample(x, y + span) - self.sample(x, y - span);
        (dx * dx + dy * dy).sqrt() / (2.0 * span)
    }

    pub fn min_max(&self) -> (f32, f32) {
        self.data
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    }

    /// A box blur of `radius` cells, applied `passes` times (three passes
    /// approach a Gaussian). Running sums keep it O(cells) per pass.
    pub fn blurred(&self, radius: usize, passes: usize) -> Field<f32> {
        let mut out = self.clone();
        let mut scratch = vec![0.0f32; self.width.max(self.height)];
        for _ in 0..passes {
            for y in 0..self.height {
                let row = &mut out.data[y * self.width..(y + 1) * self.width];
                blur_line(row, radius, &mut scratch);
            }
            let mut column = vec![0.0f32; self.height];
            for x in 0..self.width {
                for y in 0..self.height {
                    column[y] = out.data[y * self.width + x];
                }
                blur_line(&mut column, radius, &mut scratch);
                for y in 0..self.height {
                    out.data[y * self.width + x] = column[y];
                }
            }
        }
        out
    }
}

/// One line of a box blur, edges clamped.
fn blur_line(line: &mut [f32], radius: usize, scratch: &mut [f32]) {
    let n = line.len();
    let r = radius as i64;
    let at = |i: i64| line[i.clamp(0, n as i64 - 1) as usize];
    let mut sum: f32 = (-r..=r).map(at).sum();
    let norm = 1.0 / (2 * radius + 1) as f32;
    for i in 0..n as i64 {
        scratch[i as usize] = sum * norm;
        sum += at(i + r + 1) - at(i - r);
    }
    line.copy_from_slice(&scratch[..n]);
}

/// Euclidean-ish distance (chamfer 3-4, in metres) from every cell to the
/// nearest cell where `seed` is true; seeds are 0.
pub fn distance_to(width: usize, height: usize, seed: impl Fn(usize) -> bool) -> Field<f32> {
    let mut d = Field::new(width, height, f32::MAX / 4.0);
    for (i, v) in d.data.iter_mut().enumerate() {
        if seed(i) {
            *v = 0.0;
        }
    }
    chamfer(&mut d);
    d
}

/// Two-pass 3-4 chamfer distance transform in place (orthogonal steps cost
/// 1, diagonal steps 1.4).
pub fn chamfer(d: &mut Field<f32>) {
    const DIAG: f32 = 1.4;
    let (w, h) = (d.width, d.height);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d.data[i];
            if x > 0 {
                v = v.min(d.data[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d.data[i - w] + 1.0);
                if x > 0 {
                    v = v.min(d.data[i - w - 1] + DIAG);
                }
                if x + 1 < w {
                    v = v.min(d.data[i - w + 1] + DIAG);
                }
            }
            d.data[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d.data[i];
            if x + 1 < w {
                v = v.min(d.data[i + 1] + 1.0);
            }
            if y + 1 < h {
                v = v.min(d.data[i + w] + 1.0);
                if x + 1 < w {
                    v = v.min(d.data[i + w + 1] + DIAG);
                }
                if x > 0 {
                    v = v.min(d.data[i + w - 1] + DIAG);
                }
            }
            d.data[i] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_transform_measures() {
        let d = distance_to(20, 20, |i| i == 10 * 20 + 10);
        assert_eq!(d.get(10, 10), 0.0);
        assert_eq!(d.get(15, 10), 5.0);
        assert!((d.get(13, 13) - 4.2).abs() < 1e-4);
    }

    #[test]
    fn blur_keeps_constants_and_smooths_spikes() {
        let mut f = Field::new(16, 16, 2.0f32);
        assert!(f.blurred(2, 2).data.iter().all(|&v| (v - 2.0).abs() < 1e-5));
        f.set(8, 8, 27.0);
        let b = f.blurred(2, 1);
        assert!(b.get(8, 8) < 4.0 && b.get(9, 9) > 2.0);
    }

    #[test]
    fn bilinear_interpolates() {
        let mut f = Field::new(2, 1, 0.0f32);
        f.set(1, 0, 10.0);
        assert!((f.sample(1.0, 0.5) - 5.0).abs() < 1e-5);
    }
}
