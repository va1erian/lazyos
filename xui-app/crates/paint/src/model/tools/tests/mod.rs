//! Host tests for the drawing tools' building blocks.

mod shapes;
mod stroke;
mod values;

use crate::model::bitmap::{Bitmap, Pixel};

pub(super) const BLACK: Pixel = [0, 0, 0, 255];
pub(super) const RED: Pixel = [255, 0, 0, 255];
pub(super) const BLUE: Pixel = [0, 0, 255, 255];

/// How many pixels differ from `fill`.
pub(super) fn count_not(bitmap: &Bitmap, fill: Pixel) -> usize {
    bitmap
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| **p != fill)
        .count()
}
