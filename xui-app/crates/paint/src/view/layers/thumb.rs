#![forbid(unsafe_code)]

//! Layer thumbnails: a layer's picture scaled to fit a small box, over a
//! checkerboard so transparency shows.
//!
//! The cost follows the thumbnail, not the layer: each output pixel averages at
//! most [`TAPS`] x [`TAPS`] samples of its source block, so a 4096 x 4096 layer
//! is as cheap to redraw as a 64 x 64 one while a stroke is in progress.

use crate::model::{Bitmap, div255};

/// The most samples per output pixel along each axis.
const TAPS: u32 = 4;
/// The checkerboard cell edge in thumbnail pixels.
const CELL: u32 = 4;
const LIGHT: u32 = 255;
const DARK: u32 = 204;

/// The size `source` is scaled to inside a `bound` x `bound` box: the longer
/// side is `bound`, the shorter keeps the aspect (at least one pixel).
pub(super) fn fit(source: (u32, u32), bound: u32) -> (u32, u32) {
    let (w, h) = (source.0.max(1), source.1.max(1));
    let bound = bound.max(1);
    if w >= h {
        (bound, (h * bound / w).max(1))
    } else {
        ((w * bound / h).max(1), bound)
    }
}

/// `source` scaled into `bound` pixels as opaque RGBA over the checkerboard.
pub(super) fn pixels(source: &Bitmap, bound: u32) -> ((u32, u32), Vec<u8>) {
    let (out_w, out_h) = fit(source.size(), bound);
    let (src_w, src_h) = source.size();
    let mut rgba = Vec::with_capacity(out_w as usize * out_h as usize * 4);
    for oy in 0..out_h {
        let (y0, y1) = block(oy, out_h, src_h);
        for ox in 0..out_w {
            let (x0, x1) = block(ox, out_w, src_w);
            let [r, g, b, a] = average(source, (x0, x1), (y0, y1));
            let grey = if (ox / CELL + oy / CELL).is_multiple_of(2) {
                LIGHT
            } else {
                DARK
            };
            let a = u32::from(a);
            let mix = |c: u8| div255(u32::from(c) * a + grey * (255 - a)) as u8;
            rgba.extend_from_slice(&[mix(r), mix(g), mix(b), 255]);
        }
    }
    ((out_w, out_h), rgba)
}

/// The source columns (or rows) `[from, to)` that output cell `i` of `out`
/// covers; never empty.
fn block(i: u32, out: u32, source: u32) -> (u32, u32) {
    let from = (u64::from(i) * u64::from(source) / u64::from(out)) as u32;
    let to = (u64::from(i + 1) * u64::from(source) / u64::from(out)) as u32;
    (from.min(source - 1), to.max(from + 1).min(source))
}

/// The alpha-weighted average colour of a block, sampled on a coarse grid.
fn average(source: &Bitmap, (x0, x1): (u32, u32), (y0, y1): (u32, u32)) -> [u8; 4] {
    let step = |from: u32, to: u32| ((to - from).div_ceil(TAPS)).max(1);
    let (sx, sy) = (step(x0, x1), step(y0, y1));
    let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            if let Some(p) = source.get(x as i32, y as i32) {
                let w = u64::from(p[3]);
                r += u64::from(p[0]) * w;
                g += u64::from(p[1]) * w;
                b += u64::from(p[2]) * w;
                a += w;
                n += 1;
            }
            x += sx;
        }
        y += sy;
    }
    if a == 0 || n == 0 {
        return [0, 0, 0, 0];
    }
    [(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WHITE;

    #[test]
    fn the_longer_side_fits_the_box() {
        assert_eq!(fit((320, 240), 32), (32, 24));
        assert_eq!(fit((100, 400), 32), (8, 32));
        assert_eq!(fit((1, 5000), 32), (1, 32));
    }

    #[test]
    fn an_opaque_layer_keeps_its_colour_and_a_clear_one_shows_the_checker() {
        let (size, rgba) = pixels(&Bitmap::new(64, 64, [10, 20, 30, 255]), 16);
        assert_eq!(size, (16, 16));
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [10, 20, 30, 255])
        );
        let (_, clear) = pixels(&Bitmap::transparent(64, 64), 16);
        let first = &clear[0..4];
        let next = &clear[(CELL as usize) * 4..(CELL as usize) * 4 + 4];
        assert_eq!(first, [255, 255, 255, 255]);
        assert_eq!(next, [204, 204, 204, 255]);
        let _ = WHITE;
    }

    #[test]
    fn a_huge_layer_is_sampled_not_walked() {
        // 4096 x 4096 would be 16 M reads; the cap makes it a few thousand.
        let big = Bitmap::new(4096, 4096, [200, 0, 0, 255]);
        let (_, rgba) = pixels(&big, 32);
        assert_eq!(&rgba[0..4], [200, 0, 0, 255]);
    }
}
