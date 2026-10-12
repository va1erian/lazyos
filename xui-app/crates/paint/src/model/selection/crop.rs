#![forbid(unsafe_code)]

//! Crop to Selection: the document cut down to the selection's bounds.

use super::Selection;
use crate::model::div255;
use crate::model::image::{Document, Layer};
use crate::model::{Bitmap, Rect};

/// A copy of `doc` cropped to its selection's bounds, with every layer's
/// pixels outside the mask made transparent (scaled by the coverage at a soft
/// edge) and no selection left. `None` when nothing is selected or the
/// selection already is the whole picture.
pub(crate) fn crop_document(doc: &Document) -> Option<Document> {
    let selection = doc.selection()?;
    let area = selection.bounds().intersect(doc.bounds());
    if area.is_empty() || (area == doc.bounds() && selection.is_rect()) {
        return None;
    }
    let layers: Option<Vec<Layer>> = doc
        .layers()
        .iter()
        .map(|layer| {
            let mut pixels = layer.pixels.crop(area)?;
            clear_outside_mask(&mut pixels, area, selection);
            Some(Layer {
                pixels,
                ..layer.clone()
            })
        })
        .collect();
    Some(Document::from_parts(
        area.width(),
        area.height(),
        layers?,
        doc.active(),
    ))
}

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
