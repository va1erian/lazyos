#![forbid(unsafe_code)]

//! Whole-document actions: undo/redo, clear, resize, new and load.

use super::Model;
use crate::model::{Bitmap, Document, Edit, MAX_SIDE, MIN_SIDE, WHITE};

impl Model {
    /// Undoes the last committed action.
    pub fn undo(&mut self) {
        self.end();
        if let Some(damage) = self.history.undo(&mut self.doc) {
            self.after_history(damage);
        }
    }

    /// Redoes the last undone action.
    pub fn redo(&mut self) {
        self.end();
        if let Some(damage) = self.history.redo(&mut self.doc) {
            self.after_history(damage);
        }
    }

    fn after_history(&mut self, damage: crate::model::Damage) {
        self.damage.set(self.damage.get().merge(damage));
        self.preview = None;
        self.drag = None;
        self.bump();
    }

    /// Clears the active layer to white as one undo step.
    pub fn clear(&mut self) {
        self.end();
        let layer = self.doc.active();
        let Some(target) = self.doc.layer_mut(layer) else {
            return;
        };
        let rect = target.pixels.rect();
        let before = target.pixels.clone();
        target.pixels.fill(WHITE);
        self.preview = None;
        if target.pixels != before {
            self.history.record(Edit::Pixels {
                name: "Clear".to_string(),
                layer,
                rect,
                before,
            });
            self.damage_all();
            self.bump();
        }
    }

    /// Resizes the canvas to `width` x `height`: the old pixels stay at the
    /// top-left (cropped when smaller) and new area is white on the bottom
    /// layer, transparent above it. One undo step; the same size is a no-op.
    /// A side outside `MIN_SIDE..=MAX_SIDE`, or a size over the memory budget,
    /// is rejected and changes nothing.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        let range = MIN_SIDE..=MAX_SIDE;
        if !range.contains(&width) || !range.contains(&height) {
            return Err(format!(
                "size must be {MIN_SIDE}-{MAX_SIDE} pixels per side"
            ));
        }
        self.end();
        if self.doc.size() == (width, height) {
            return Ok(());
        }
        // Before allocating: the new layers, the old ones still held by the
        // undo entry, and the cache are all live at once.
        self.check_fits(width, height, self.doc.layers().len())?;
        let mut resized = self.doc.resized(width, height);
        self.doc.swap_contents(&mut resized);
        // `resized` now holds the old document: exactly what undo restores.
        self.history.record(Edit::Document {
            name: "Resize Image".to_string(),
            before: Box::new(resized),
        });
        self.preview = None;
        self.damage_all();
        self.bump();
        Ok(())
    }

    /// Replaces the document with a fresh white one-layer document and an
    /// empty history.
    pub fn reset(&mut self, width: u32, height: u32) -> Result<(), String> {
        let width = width.clamp(MIN_SIDE, MAX_SIDE);
        let height = height.clamp(MIN_SIDE, MAX_SIDE);
        self.check_fits(width, height, 1)?;
        let doc = Document::new(width, height, WHITE).map_err(|e| e.to_string())?;
        self.replace(doc);
        Ok(())
    }

    /// Replaces the document with a decoded bitmap (one layer) and empties the
    /// history; refused, with the document untouched, over the memory budget.
    pub fn load(&mut self, bitmap: Bitmap) -> Result<(), String> {
        self.check_fits(bitmap.width(), bitmap.height(), 1)?;
        let doc = Document::from_bitmap(bitmap).map_err(|e| e.to_string())?;
        self.replace(doc);
        Ok(())
    }

    pub(super) fn replace(&mut self, doc: Document) {
        self.doc = doc;
        self.history.clear();
        self.preview = None;
        self.drag = None;
        self.damage_all();
        self.bump();
    }
}
