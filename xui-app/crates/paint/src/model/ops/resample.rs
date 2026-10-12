#![forbid(unsafe_code)]

//! Scaling a bitmap to a new size.

use crate::model::bitmap::Bitmap;

/// How Resize Image picks the new pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resample {
    /// The nearest source pixel: hard edges, no new colours (pixel art).
    Nearest,
    /// A weighted mix of the four nearest source pixels, in premultiplied
    /// alpha so a transparent neighbour never darkens an edge.
    Bilinear,
}

/// `src` scaled to `width` x `height`. The caller has checked the budget.
pub fn resample(src: &Bitmap, width: u32, height: u32, how: Resample) -> Bitmap {
    let mut out = Bitmap::transparent(width, height);
    let (dw, dh) = out.size();
    if (dw, dh) == src.size() {
        out.pixels_mut().copy_from_slice(src.pixels());
        return out;
    }
    match how {
        Resample::Nearest => nearest(src, &mut out),
        Resample::Bilinear => bilinear(src, &mut out),
    }
    out
}

fn nearest(src: &Bitmap, out: &mut Bitmap) {
    let (sw, sh) = (u64::from(src.width()), u64::from(src.height()));
    let (dw, dh) = (u64::from(out.width()), u64::from(out.height()));
    // The centre of each destination pixel, mapped back and floored.
    let columns: Vec<usize> = (0..dw)
        .map(|x| ((2 * x + 1) * sw / (2 * dw)) as usize)
        .collect();
    let from = src.pixels().as_chunks::<4>().0;
    let to = out.pixels_mut().as_chunks_mut::<4>().0;
    for y in 0..dh {
        let sy = ((2 * y + 1) * sh / (2 * dh)) as usize;
        let row = &from[sy * sw as usize..(sy + 1) * sw as usize];
        let dst = &mut to[(y * dw) as usize..((y + 1) * dw) as usize];
        for (d, &sx) in dst.iter_mut().zip(&columns) {
            *d = row[sx];
        }
    }
}

/// For each destination index, the two source indices and the weight (0..=256)
/// of the second, sampling at pixel centres.
fn taps(src_len: u32, dst_len: u32) -> Vec<(usize, usize, u32)> {
    let (s, d) = (i64::from(src_len), i64::from(dst_len));
    (0..d)
        .map(|i| {
            // Source coordinate of the centre, in 1/256 pixels, minus half a pixel.
            let at = ((2 * i + 1) * s * 256 / (2 * d) - 128).max(0);
            let i0 = ((at >> 8) as usize).min(src_len as usize - 1);
            let i1 = (i0 + 1).min(src_len as usize - 1);
            (i0, i1, (at & 255) as u32)
        })
        .collect()
}

fn bilinear(src: &Bitmap, out: &mut Bitmap) {
    let xs = taps(src.width(), out.width());
    let ys = taps(src.height(), out.height());
    let from = src.pixels().as_chunks::<4>().0;
    let sw = src.width() as usize;
    let dw = out.width() as usize;
    let to = out.pixels_mut().as_chunks_mut::<4>().0;
    for (y, &(y0, y1, fy)) in ys.iter().enumerate() {
        for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
            let taps = [
                (from[y0 * sw + x0], (256 - fx) * (256 - fy)),
                (from[y0 * sw + x1], fx * (256 - fy)),
                (from[y1 * sw + x0], (256 - fx) * fy),
                (from[y1 * sw + x1], fx * fy),
            ];
            to[y * dw + x] = mix(&taps);
        }
    }
}

/// Weighted average of pixels (weights sum to 65536) in premultiplied alpha.
fn mix(taps: &[([u8; 4], u32)]) -> [u8; 4] {
    let (mut alpha, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
    for &(p, w) in taps {
        let wa = u64::from(w) * u64::from(p[3]);
        alpha += wa;
        r += wa * u64::from(p[0]);
        g += wa * u64::from(p[1]);
        b += wa * u64::from(p[2]);
    }
    if alpha == 0 {
        return [0, 0, 0, 0];
    }
    let c = |sum: u64| ((sum + alpha / 2) / alpha).min(255) as u8;
    [c(r), c(g), c(b), ((alpha + 32768) >> 16).min(255) as u8]
}

/// `src` shrunk to fit inside `max` x `max`, keeping the aspect ratio, by
/// averaging the source pixels under each result pixel (a thumbnail). A
/// picture already inside the box is returned as is.
pub fn box_fit(src: &Bitmap, max: u32) -> Bitmap {
    let (sw, sh) = src.size();
    if sw <= max && sh <= max {
        return src.clone();
    }
    let scale =
        |side: u32, long: u32| ((u64::from(side) * u64::from(max)) / u64::from(long)).max(1) as u32;
    let long = sw.max(sh);
    let (dw, dh) = (scale(sw, long), scale(sh, long));
    let mut out = Bitmap::transparent(dw, dh);
    let from = src.pixels().as_chunks::<4>().0;
    let to = out.pixels_mut().as_chunks_mut::<4>().0;
    let edge = |i: u32, s: u32, d: u32| (u64::from(i) * u64::from(s) / u64::from(d)) as usize;
    for y in 0..dh {
        let (y0, y1) = (
            edge(y, sh, dh),
            edge(y + 1, sh, dh).max(edge(y, sh, dh) + 1),
        );
        for x in 0..dw {
            let (x0, x1) = (
                edge(x, sw, dw),
                edge(x + 1, sw, dw).max(edge(x, sw, dw) + 1),
            );
            let (mut n, mut alpha, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for p in &from[sy * sw as usize + x0..sy * sw as usize + x1] {
                    let a = u64::from(p[3]);
                    n += 1;
                    alpha += a;
                    r += a * u64::from(p[0]);
                    g += a * u64::from(p[1]);
                    b += a * u64::from(p[2]);
                }
            }
            to[(y * dw + x) as usize] = if alpha == 0 {
                [0, 0, 0, 0]
            } else {
                let c = |sum: u64| ((sum + alpha / 2) / alpha).min(255) as u8;
                [c(r), c(g), c(b), ((alpha + n / 2) / n).min(255) as u8]
            };
        }
    }
    out
}
