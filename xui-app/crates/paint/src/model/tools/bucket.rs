#![forbid(unsafe_code)]

//! The Paint Bucket: which pixels match the one clicked, within a tolerance,
//! as a coverage mask a caller composites (and later clips to a selection).
//!
//! This sits beside `model::fill`, which keeps serving the existing tool until
//! the tools switch over.

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::image::Document;
use crate::model::rect::Rect;

use super::coverage::Coverage;
use super::options::{FillArea, FillOptions, Sample};

/// The largest possible RGBA distance, `sqrt(4 * 255^2)`, squared.
const MAX_DISTANCE_SQ: u64 = 4 * 255 * 255;

/// Squared Euclidean distance between two RGBA pixels.
fn distance_sq(a: Pixel, b: Pixel) -> u64 {
    (0..4)
        .map(|i| {
            let d = i64::from(a[i]) - i64::from(b[i]);
            (d * d) as u64
        })
        .sum()
}

/// Whether `pixel` is within `tolerance` percent of `seed`. The percentage is
/// of the largest RGBA distance, compared exactly in integers: 0 matches
/// identical pixels only, 100 matches everything.
pub fn within_tolerance(seed: Pixel, pixel: Pixel, tolerance: u8) -> bool {
    let t = u64::from(tolerance.min(100));
    // distance <= t/100 * sqrt(MAX)  <=>  distance^2 * 100^2 <= t^2 * MAX.
    distance_sq(seed, pixel) * 10_000 <= t * t * MAX_DISTANCE_SQ
}

/// The pixels of `source` that `options` selects for a click at `seed`,
/// as a mask of 255 over their bounding rectangle. `None` when the seed is
/// outside the bitmap. `options.sample` is not consulted here: the caller
/// passes the layer or the flattened image ([`bucket_mask_in`] does both).
pub fn bucket_mask(source: &Bitmap, seed: (i32, i32), options: &FillOptions) -> Option<Coverage> {
    let target = source.get(seed.0, seed.1)?;
    let (width, height) = (source.width() as usize, source.height() as usize);
    let mut chosen = Bits::new(width * height);
    let matches = |x: usize, y: usize| {
        let at = (y * width + x) * 4;
        let p = &source.pixels()[at..at + 4];
        within_tolerance(target, [p[0], p[1], p[2], p[3]], options.tolerance)
    };
    let mut bounds = Rect::EMPTY;
    match options.area {
        FillArea::Global => {
            for y in 0..height {
                for x in 0..width {
                    if matches(x, y) {
                        chosen.set(y * width + x);
                        bounds = bounds.union(Rect::new(x as i32, y as i32, 1, 1));
                    }
                }
            }
        }
        FillArea::Contiguous => {
            bounds = scanline(&mut chosen, width, height, seed, &matches);
        }
    }
    let mut out = Coverage::new(bounds);
    let stride = bounds.width() as usize;
    for y in bounds.y0..bounds.y1 {
        for x in bounds.x0..bounds.x1 {
            if chosen.get(y as usize * width + x as usize) {
                out.data_mut()[(y - bounds.y0) as usize * stride + (x - bounds.x0) as usize] = 255;
            }
        }
    }
    Some(out)
}

/// [`bucket_mask`] over the document's active layer or its flattened image,
/// as `options.sample` says.
pub fn bucket_mask_in(doc: &Document, seed: (i32, i32), options: &FillOptions) -> Option<Coverage> {
    match options.sample {
        Sample::Layer => bucket_mask(&doc.active_layer().pixels, seed, options),
        Sample::Image => bucket_mask(&doc.flatten(), seed, options),
    }
}

/// Iterative scanline fill of the 4-connected region around `seed` whose
/// pixels satisfy `matches`; returns its bounds.
fn scanline(
    chosen: &mut Bits,
    width: usize,
    height: usize,
    seed: (i32, i32),
    matches: &impl Fn(usize, usize) -> bool,
) -> Rect {
    let mut bounds = Rect::EMPTY;
    let mut stack = vec![(seed.0 as usize, seed.1 as usize)];
    while let Some((x, y)) = stack.pop() {
        if chosen.get(y * width + x) || !matches(x, y) {
            continue;
        }
        let mut left = x;
        while left > 0 && !chosen.get(y * width + left - 1) && matches(left - 1, y) {
            left -= 1;
        }
        let mut right = x;
        while right + 1 < width && !chosen.get(y * width + right + 1) && matches(right + 1, y) {
            right += 1;
        }
        for px in left..=right {
            chosen.set(y * width + px);
        }
        bounds = bounds.union(Rect {
            x0: left as i32,
            y0: y as i32,
            x1: right as i32 + 1,
            y1: y as i32 + 1,
        });
        for row in [y.wrapping_sub(1), y + 1] {
            if row >= height {
                continue;
            }
            let mut open = false;
            for px in left..=right {
                let free = !chosen.get(row * width + px) && matches(px, row);
                if free && !open {
                    stack.push((px, row));
                }
                open = free;
            }
        }
    }
    bounds
}

/// A bit set, one bit per pixel, so a 16384 x 16384 fill needs 32 MiB.
struct Bits(Vec<u64>);

impl Bits {
    fn new(len: usize) -> Bits {
        Bits(vec![0; len.div_ceil(64)])
    }

    fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }

    fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
}
