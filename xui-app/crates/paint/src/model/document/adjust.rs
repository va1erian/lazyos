#![forbid(unsafe_code)]

//! The Adjustments menu on the model: a colour adjustment of the active layer
//! as one `Pixels` edit.

use super::Model;
use crate::model::ops::{Adjustment, Coverage};
use crate::model::selection::Selection;
use crate::model::{Edit, Rect};

impl Model {
    /// Applies `adjustment` to the active layer, clipped to the selection
    /// (through its soft edge) when there is one, else to the whole layer.
    pub fn adjust(&mut self, adjustment: Adjustment) -> Result<(), String> {
        self.settle();
        match self.doc.selection().cloned() {
            Some(selection) => self.adjust_masked(adjustment, selection.bounds(), Some(&selection)),
            None => {
                let rect = self.doc.bounds();
                self.adjust_masked(adjustment, rect, None)
            }
        }
    }

    /// Applies `adjustment` to `rect` (clipped to the layer) of the active
    /// layer, ignoring any selection. Nothing is recorded when no pixel
    /// changes.
    pub fn adjust_in(&mut self, adjustment: Adjustment, rect: Rect) -> Result<(), String> {
        self.adjust_masked(adjustment, rect, None)
    }

    fn adjust_masked(
        &mut self,
        adjustment: Adjustment,
        rect: Rect,
        selection: Option<&Selection>,
    ) -> Result<(), String> {
        self.end();
        let rect = rect.intersect(self.doc.bounds());
        if rect.is_empty() {
            return Ok(());
        }
        // The undo copy and the working copy of the rectangle.
        self.check_extra_fits(rect.area() as u64 * 8)?;
        let layer = self.doc.active();
        let Some(target) = self.doc.layer_mut(layer) else {
            return Err("no such layer".to_string());
        };
        let Some(before) = target.pixels.crop(rect) else {
            return Ok(());
        };
        let mut after = before.clone();
        let whole = after.rect();
        // The working copy starts at `rect`; the selection is in picture
        // coordinates.
        let coverage =
            |x: i32, y: i32| selection.map_or(255, |s| s.coverage(x + rect.x0, y + rect.y0));
        let mask: Option<Coverage<'_>> = selection.map(|_| &coverage as Coverage<'_>);
        adjustment.apply_masked(&mut after, whole, mask);
        if after == before {
            return Ok(());
        }
        target.pixels.blit(rect.x0, rect.y0, &after, after.rect());
        self.history.record(Edit::Pixels {
            name: adjustment.name().to_string(),
            layer,
            rect,
            before,
        });
        self.damage_rect(rect);
        self.bump();
        Ok(())
    }
}
