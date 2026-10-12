#![forbid(unsafe_code)]

//! Colour picker sampling.

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::image::Document;

use super::options::{PickerOptions, Sample, SampleSize};

/// The colour under `(x, y)` in `source`: the pixel itself, or the average of
/// the 3x3 / 5x5 window around it.
///
/// Colour is averaged weighted by alpha, so transparent pixels add no colour
/// of their own; alpha is the plain mean over the window's in-bounds pixels.
/// `None` when `(x, y)` is outside the bitmap.
pub fn sample(source: &Bitmap, x: i32, y: i32, size: SampleSize) -> Option<Pixel> {
    let first = source.get(x, y)?;
    if size == SampleSize::One {
        return Some(first);
    }
    let reach = size.side() / 2;
    let (mut r, mut g, mut b, mut a, mut count) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for yy in y - reach..=y + reach {
        for xx in x - reach..=x + reach {
            let Some(p) = source.get(xx, yy) else {
                continue;
            };
            let weight = u64::from(p[3]);
            r += u64::from(p[0]) * weight;
            g += u64::from(p[1]) * weight;
            b += u64::from(p[2]) * weight;
            a += weight;
            count += 1;
        }
    }
    if a == 0 {
        return Some([first[0], first[1], first[2], 0]);
    }
    let round = |sum: u64, div: u64| ((sum + div / 2) / div) as u8;
    Some([round(r, a), round(g, a), round(b, a), round(a, count)])
}

/// [`sample`] from the document's active layer or flattened image, as
/// `options` ask.
pub fn pick(doc: &Document, x: i32, y: i32, options: &PickerOptions) -> Option<Pixel> {
    match options.sample {
        Sample::Layer => sample(&doc.active_layer().pixels, x, y, options.size),
        Sample::Image => sample(&doc.flatten(), x, y, options.size),
    }
}
