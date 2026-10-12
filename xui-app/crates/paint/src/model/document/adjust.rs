#![forbid(unsafe_code)]

//! The Adjustments menu on the model: a colour adjustment of the active layer
//! as one `Pixels` edit.

use super::Model;
use crate::model::ops::Adjustment;
use crate::model::{Edit, Rect};

impl Model {
    /// Applies `adjustment` to the whole active layer.
    pub fn adjust(&mut self, adjustment: Adjustment) -> Result<(), String> {
        let rect = self.doc.bounds();
        self.adjust_in(adjustment, rect)
    }

    /// Applies `adjustment` to `rect` (clipped to the layer) of the active
    /// layer. P2 passes the selection's bounds here and a mask alongside.
    /// Nothing is recorded when no pixel changes.
    pub fn adjust_in(&mut self, adjustment: Adjustment, rect: Rect) -> Result<(), String> {
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
        adjustment.apply(&mut after, whole);
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
