#![forbid(unsafe_code)]

//! The document: a stack of same-sized layers.
//!
//! A selection will live here too (P2); nothing in this module assumes there
//! is exactly one layer or that layers are all visible.

mod budget;
mod flatten;
mod layer;
mod rebuild;

pub use budget::{BudgetError, MEMORY_BUDGET, Reserved, check as check_budget, required_bytes};
pub use layer::{Layer, LayerProps};

use super::bitmap::{Bitmap, Pixel};
use super::rect::Rect;

/// What a standalone document reserves for undo: the history's default cap.
const DEFAULT_RESERVE: Reserved = Reserved {
    floating: 0,
    clipboard: 0,
    history: crate::model::DEFAULT_CAP as u64,
};

/// The name of the first layer of a new document.
pub const BACKGROUND: &str = "Background";

/// Every layer, bottom first, and which one tools draw on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    width: u32,
    height: u32,
    layers: Vec<Layer>,
    active: usize,
}

/// A whole document's contents, swapped in and out by structural history
/// entries (resize, and later crop and rotate).
pub type DocumentSnapshot = Document;

impl Document {
    /// A document of one "Background" layer filled with `fill`. Fails when the
    /// size exceeds the memory budget; sides are clamped like [`Bitmap::new`].
    pub fn new(width: u32, height: u32, fill: Pixel) -> Result<Document, BudgetError> {
        let (width, height) = (
            width.clamp(super::MIN_SIDE, super::MAX_SIDE),
            height.clamp(super::MIN_SIDE, super::MAX_SIDE),
        );
        check_budget(width, height, 1, DEFAULT_RESERVE)?;
        Ok(Document::from_bitmap_unchecked(Bitmap::new(
            width, height, fill,
        )))
    }

    /// A one-layer document over `bitmap`, if it fits the budget.
    pub fn from_bitmap(bitmap: Bitmap) -> Result<Document, BudgetError> {
        check_budget(bitmap.width(), bitmap.height(), 1, DEFAULT_RESERVE)?;
        Ok(Document::from_bitmap_unchecked(bitmap))
    }

    fn from_bitmap_unchecked(bitmap: Bitmap) -> Document {
        Document {
            width: bitmap.width(),
            height: bitmap.height(),
            layers: vec![Layer::new(BACKGROUND, bitmap)],
            active: 0,
        }
    }

    /// The width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The size as `(width, height)`.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The document's bounds at the origin.
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// The layers, bottom first.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The layer at `index`.
    pub fn layer(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    /// The index of the layer tools draw on.
    pub fn active(&self) -> usize {
        self.active
    }

    /// The layer tools draw on.
    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    /// The pixel bytes of every layer.
    pub fn bytes(&self) -> usize {
        self.layers.iter().map(|l| l.pixels.byte_len()).sum()
    }

    pub(crate) fn layer_mut(&mut self, index: usize) -> Option<&mut Layer> {
        self.layers.get_mut(index)
    }

    pub(crate) fn set_active(&mut self, index: usize) {
        self.active = index.min(self.layers.len() - 1);
    }

    pub(crate) fn insert_layer(&mut self, index: usize, layer: Layer) {
        let index = index.min(self.layers.len());
        self.layers.insert(index, layer);
    }

    /// Removes a layer; the last one is never removed.
    pub(crate) fn remove_layer(&mut self, index: usize) -> Option<Layer> {
        if self.layers.len() <= 1 || index >= self.layers.len() {
            return None;
        }
        Some(self.layers.remove(index))
    }

    pub(crate) fn move_layer_raw(&mut self, from: usize, to: usize) {
        if from < self.layers.len() && to < self.layers.len() {
            let layer = self.layers.remove(from);
            self.layers.insert(to, layer);
        }
    }

    /// Exchanges the whole contents with `other`.
    pub(crate) fn swap_contents(&mut self, other: &mut Document) {
        std::mem::swap(self, other);
    }

    /// Each layer resized to `width` x `height` with the pixels kept at the
    /// top-left; the bottom layer pads white, the others transparent. The
    /// caller checks the budget first.
    pub(crate) fn resized(&self, width: u32, height: u32) -> Document {
        let layers = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, layer)| {
                let fill = if i == 0 { super::WHITE } else { [0, 0, 0, 0] };
                Layer {
                    pixels: layer.pixels.resized_with(width, height, fill),
                    ..layer.clone()
                }
            })
            .collect();
        Document {
            width: width.clamp(super::MIN_SIDE, super::MAX_SIDE),
            height: height.clamp(super::MIN_SIDE, super::MAX_SIDE),
            layers,
            active: self.active,
        }
    }
}

#[cfg(test)]
mod tests;
