#![forbid(unsafe_code)]

//! Crop to Selection: the document cut down to the selection's bounds.

use super::Selection;
use crate::model::div255;
use crate::model::{Bitmap, Rect};

/// Makes the pixels of `pixels` (a crop of the picture at `area`) that the
/// selection does not cover transparent, scaling alpha by the coverage at a
/// soft edge. A separate step so it can follow a plain crop of the bounds.
pub(crate) fn clear_outside_mask(pixels: &mut Bitmap, area: Rect, selection: &Selection) {
    if selection.is_rect() {
        return;
    }
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let cover = selection.coverage(x, y);
            if cover == 255 {
                continue;
            }
            let (px, py) = (x - area.x0, y - area.y0);
            if let Some(mut pixel) = pixels.get(px, py) {
                pixel = if cover == 0 {
                    [0, 0, 0, 0]
                } else {
                    pixel[3] = div255(u32::from(pixel[3]) * u32::from(cover)) as u8;
                    pixel
                };
                pixels.put(px, py, pixel);
            }
        }
    }
}
