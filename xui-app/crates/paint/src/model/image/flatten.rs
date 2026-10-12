#![forbid(unsafe_code)]

//! Compositing the visible layers into one picture, by rectangle.

use super::Document;
use crate::model::bitmap::Bitmap;
use crate::model::blend::source_over;
use crate::model::rect::Rect;

impl Document {
    /// The visible layers composited over transparent: the picture a viewer
    /// shows and an exporter writes.
    pub fn flatten(&self) -> Bitmap {
        let mut out = Bitmap::transparent(self.width, self.height);
        let all = out.rect();
        self.flatten_into(&mut out, all);
        out
    }

    /// Recomputes `rect` (clipped to the document) of `out`, which must be
    /// document-sized, from the layers. Pixels outside `rect` are untouched,
    /// so a stroke costs the stroke's area, not the picture's.
    pub fn flatten_into(&self, out: &mut Bitmap, rect: Rect) {
        let rect = rect.intersect(self.bounds()).intersect(out.rect());
        if rect.is_empty() {
            return;
        }
        let width = rect.width() as usize;
        let stride = out.width() as usize * 4;
        let mut shown = self.layers.iter().filter(|layer| layer.is_shown());
        let first = shown.next();
        for y in rect.y0..rect.y1 {
            let at = y as usize * stride + rect.x0 as usize * 4;
            let row = &mut out.pixels_mut()[at..at + width * 4];
            match first {
                None => row.fill(0),
                Some(layer) => paint_first(row, layer.pixels.row(y, rect.x0, width), layer.opacity),
            }
        }
        for layer in shown {
            for y in rect.y0..rect.y1 {
                let at = y as usize * stride + rect.x0 as usize * 4;
                let row = &mut out.pixels_mut()[at..at + width * 4];
                paint_over(row, layer.pixels.row(y, rect.x0, width), layer.opacity);
            }
        }
    }
}

/// Writes the bottom layer's row: a copy when fully opaque (so a lone layer
/// round-trips byte for byte), else the source scaled over nothing.
fn paint_first(dst: &mut [u8], src: &[u8], opacity: u8) {
    if opacity == 255 {
        dst.copy_from_slice(src);
        return;
    }
    for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
        *d = source_over([0, 0, 0, 0], *s, opacity);
    }
}

/// Composites a layer row over `dst`, skipping what cannot change it.
fn paint_over(dst: &mut [u8], src: &[u8], opacity: u8) {
    for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
        if s[3] != 0 {
            *d = source_over(*d, *s, opacity);
        }
    }
}
