#![forbid(unsafe_code)]

//! Building a document from layers, and deriving a new one from an old one
//! (the image operations of P5). The caller checks the budget before the
//! derived forms; [`Document::from_layers`] checks it itself, since it takes
//! untrusted layers from a file.

use super::{DEFAULT_RESERVE, Document, Layer, check_budget};
use crate::model::bitmap::Bitmap;
use crate::model::blend::source_over;

impl Document {
    /// A document of `layers` (bottom first) with `active` selected (clamped).
    /// Fails when there are none, when they differ in size, or when they would
    /// not fit the memory budget.
    pub fn from_layers(layers: Vec<Layer>, active: usize) -> Result<Document, String> {
        let Some(first) = layers.first() else {
            return Err("an image needs at least one layer".to_string());
        };
        let (width, height) = first.pixels.size();
        if layers.iter().any(|l| l.pixels.size() != (width, height)) {
            return Err("the layers differ in size".to_string());
        }
        check_budget(width, height, layers.len(), DEFAULT_RESERVE).map_err(|e| e.to_string())?;
        let active = active.min(layers.len() - 1);
        Ok(Document {
            width,
            height,
            layers,
            active,
            selection: None,
        })
    }

    /// The same layers (names, visibility, opacity, active) with each pixel
    /// buffer replaced by `f(index, old)`, which must all be `width` x
    /// `height`. Used by resize, canvas size, crop and the turns.
    pub(crate) fn mapped(
        &self,
        width: u32,
        height: u32,
        f: impl Fn(usize, &Bitmap) -> Bitmap,
    ) -> Document {
        let layers: Vec<Layer> = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, layer)| layer.with_pixels(f(i, &layer.pixels)))
            .collect();
        debug_assert!(layers.iter().all(|l| l.pixels.size() == (width, height)));
        Document {
            width,
            height,
            layers,
            active: self.active,
            selection: None,
        }
    }

    /// One "Background" layer holding the visible layers composited: Flatten.
    pub(crate) fn flattened(&self) -> Document {
        Document::from_bitmap_unchecked(self.flatten())
    }

    /// Paints the layer at `index` onto the one below through its opacity (a
    /// hidden layer adds nothing), removes it and selects the lower layer.
    /// Returns the removed layer and the lower layer's pixels as they were, or
    /// `None` for the bottom layer.
    pub(crate) fn merge_into_below(&mut self, index: usize) -> Option<(Layer, Bitmap)> {
        if index == 0 || index >= self.layers.len() {
            return None;
        }
        let upper = self.layers.remove(index);
        let lower = &mut self.layers[index - 1];
        let before = lower.pixels.clone();
        if upper.is_shown() {
            let src = upper.pixels.pixels().as_chunks::<4>().0;
            let dst = lower.pixels.pixels_mut().as_chunks_mut::<4>().0;
            for (d, s) in dst.iter_mut().zip(src) {
                if s[3] != 0 {
                    *d = source_over(*d, *s, upper.opacity);
                }
            }
        }
        self.active = index - 1;
        Some((upper, before))
    }
}
