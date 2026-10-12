#![forbid(unsafe_code)]

//! The floating selection: pixels lifted off a layer (Move Selected Pixels) or
//! pasted, held in the air while the user places them.
//!
//! While floating, the layer already shows the composite (the hole where the
//! pixels were lifted, the pixels over what lies under them at their current
//! position), so the flattened cache, the screen image and a save need no
//! special case. Every move recomputes the union of the old and new footprint
//! from the layer's *original* pixels, which the undo capture keeps. The whole
//! lift-and-drop is one history entry, recorded when the float lands.

use super::{Drag, Model};
use crate::model::history::UndoCapture;
use crate::model::selection::mix_by;
use crate::model::{Bitmap, Edit, Rect, Selection, Tool, div255, source_over};

/// The history name of a lift and drop.
const MOVE_NAME: &str = "Move Selected Pixels";

/// Pixels being placed.
pub(super) struct Float {
    /// The layer they will land on.
    layer: usize,
    /// The pixels, transparent outside the selection they were lifted with.
    bitmap: Bitmap,
    /// Where the bitmap's top-left is now.
    pos: (i32, i32),
    /// Where the bitmap's top-left was when it was lifted or pasted.
    origin: (i32, i32),
    /// The selection at `origin`; it travels with the pixels.
    outline: Selection,
    /// Whether lifting left a transparent hole under `outline` (a paste does not).
    hole: bool,
    /// Backs up what the float overwrote, for the history and Esc.
    capture: UndoCapture,
    /// The selection before the float began.
    selection_before: Option<Selection>,
    /// The history name.
    name: &'static str,
}

impl Float {
    /// The rectangle the bitmap covers at `pos`.
    fn footprint(&self, pos: (i32, i32)) -> Rect {
        Rect::new(pos.0, pos.1, self.bitmap.width(), self.bitmap.height())
    }
}

/// Recomputes `area` of `layer` from the original pixels, the hole and the
/// float at its current position.
fn recompose(float: &mut Float, layer: &mut Bitmap, area: Rect) {
    let area = area.intersect(layer.rect());
    float.capture.touch(layer, area);
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let Some(mut pixel) = float.capture.original(layer, x, y) else {
                continue;
            };
            if float.hole {
                pixel = mix_by(float.outline.coverage(x, y), pixel, [0, 0, 0, 0]);
            }
            if let Some(over) = float.bitmap.get(x - float.pos.0, y - float.pos.1) {
                pixel = source_over(pixel, over, 255);
            }
            layer.put(x, y, pixel);
        }
    }
}

impl Model {
    /// Whether pixels are floating, waiting to be placed.
    pub fn is_floating(&self) -> bool {
        self.float.is_some()
    }

    /// Ends any drag and lands floating pixels: what every structural action
    /// (undo, resize, a layer change, ...) does first.
    pub(super) fn settle(&mut self) {
        self.end();
        self.commit_text();
        self.commit_float();
    }

    /// Lifts the selected pixels of the active layer (the whole layer when
    /// nothing is selected) into a float. Refused over the memory budget.
    fn lift(&mut self) -> Result<(), String> {
        let bounds = self.doc.bounds();
        let selection = match self.doc.selection() {
            Some(selection) => selection.clipped(bounds),
            None => Selection::rect(bounds),
        }
        .ok_or_else(|| "Nothing is selected".to_string())?;
        let area = selection.bounds();
        // The lifted bitmap, and the capture of the layer under it.
        self.check_extra_fits(area.area() as u64 * 4)?;
        self.check_capture(area)?;
        let layer = self.doc.active();
        let target = &mut self.doc.layer_mut(layer).ok_or("no such layer")?.pixels;
        let mut bitmap = target.crop(area).ok_or("Nothing is selected")?;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let cover = selection.coverage(x, y);
                if cover == 255 {
                    continue;
                }
                let (lx, ly) = (x - area.x0, y - area.y0);
                if let Some(mut pixel) = bitmap.get(lx, ly) {
                    pixel = if cover == 0 {
                        [0, 0, 0, 0]
                    } else {
                        pixel[3] = div255(u32::from(pixel[3]) * u32::from(cover)) as u8;
                        pixel
                    };
                    bitmap.put(lx, ly, pixel);
                }
            }
        }
        let origin = (area.x0, area.y0);
        let float = Float {
            layer,
            bitmap,
            pos: origin,
            origin,
            outline: selection.clone(),
            hole: true,
            capture: UndoCapture::new(),
            selection_before: self.doc.selection().cloned(),
            name: MOVE_NAME,
        };
        self.start_float(float);
        // Moving the whole layer selects it, so the outline shows.
        if self.doc.selection().is_none() {
            self.doc.set_selection(Some(selection));
            self.selection_changed();
        }
        Ok(())
    }

    /// Puts `float` in the air: shows its starting composite and counts its
    /// bytes against the budget.
    fn start_float(&mut self, mut float: Float) {
        self.set_floating_bytes(float.bitmap.byte_len() as u64);
        let area = float.outline.bounds().union(float.footprint(float.pos));
        if let Some(target) = self.doc.layer_mut(float.layer) {
            recompose(&mut float, &mut target.pixels, area);
        }
        self.damage_rect(area);
        self.float = Some(float);
        self.bump();
    }

    /// Floats `bitmap` with its top-left at `at` over the active layer, as a
    /// paste does. Refused over the memory budget. The caller selects the
    /// Move Selected Pixels tool.
    pub(super) fn float_bitmap(
        &mut self,
        bitmap: Bitmap,
        at: (i32, i32),
        name: &'static str,
    ) -> Result<(), String> {
        self.check_extra_fits(bitmap.byte_len() as u64)?;
        self.check_capture(Rect::new(at.0, at.1, bitmap.width(), bitmap.height()))?;
        let outline = Selection::rect(Rect::new(at.0, at.1, bitmap.width(), bitmap.height()))
            .ok_or("Nothing to paste")?;
        let float = Float {
            layer: self.doc.active(),
            bitmap,
            pos: at,
            origin: at,
            outline: outline.clone(),
            hole: false,
            capture: UndoCapture::new(),
            selection_before: self.doc.selection().cloned(),
            name,
        };
        self.start_float(float);
        self.doc.set_selection(outline.clipped(self.doc.bounds()));
        self.selection_changed();
        Ok(())
    }

    /// Moves the float to `pos`, recomputing the layer under both footprints
    /// and carrying the selection along.
    fn move_float_to(&mut self, pos: (i32, i32)) {
        // The capture grows to every place the float has been: ask before it
        // reaches a new one, and stay where it is when that would not fit.
        if let Some(float) = &self.float {
            let layer = self
                .doc
                .layer(float.layer)
                .map_or(Rect::EMPTY, |l| l.pixels.rect());
            let span = float
                .capture
                .bounds()
                .union(float.footprint(pos).intersect(layer));
            if let Err(error) = self.check_capture(span) {
                self.notice = Some(error);
                return;
            }
        }
        let Some(mut float) = self.float.take() else {
            return;
        };
        let area = float.footprint(float.pos).union(float.footprint(pos));
        float.pos = pos;
        if let Some(target) = self.doc.layer_mut(float.layer) {
            recompose(&mut float, &mut target.pixels, area);
        }
        let (dx, dy) = (pos.0 - float.origin.0, pos.1 - float.origin.1);
        let moved = float.outline.translated(dx, dy).clipped(self.doc.bounds());
        self.doc.set_selection(moved);
        self.damage_rect(area);
        self.float = Some(float);
        self.selection_changed();
    }

    /// Lands the floating pixels: one history entry for the lift (or paste)
    /// and the drop together. Nothing is recorded when nothing changed.
    pub fn commit_float(&mut self) {
        let Some(float) = self.float.take() else {
            return;
        };
        self.set_floating_bytes(0);
        if matches!(self.drag, Some(Drag::MoveFloat { .. })) {
            self.drag = None;
        }
        let name = float.name.to_string();
        let mut edits = Vec::new();
        if let Some(target) = self.doc.layer(float.layer)
            && let Some((rect, before)) = float.capture.finish(&target.pixels)
        {
            self.damage_rect(rect);
            edits.push(Edit::Pixels {
                name: name.clone(),
                layer: float.layer,
                rect,
                before,
            });
        }
        if self.doc.selection() != float.selection_before.as_ref() {
            edits.push(Edit::Selection {
                name: name.clone(),
                before: float.selection_before,
            });
        }
        match edits.len() {
            0 => {}
            1 => self.history.record(edits.remove(0)),
            _ => self.history.record(Edit::Group { name, edits }),
        }
        self.selection_changed();
    }

    /// Drops the floating pixels and puts the layer and the selection back as
    /// they were before the lift or paste (Esc). Nothing is recorded.
    pub fn cancel_float(&mut self) {
        let Some(float) = self.float.take() else {
            return;
        };
        self.set_floating_bytes(0);
        self.drag = None;
        if let Some(target) = self.doc.layer_mut(float.layer)
            && let Some((rect, mut before)) = float.capture.finish(&target.pixels)
        {
            target.pixels.swap_region(rect.x0, rect.y0, &mut before);
            self.damage_rect(rect);
        }
        self.doc.set_selection(float.selection_before);
        self.selection_changed();
    }

    /// Discards a float without touching the document (the document is being
    /// replaced).
    pub(super) fn forget_float(&mut self) {
        self.float = None;
        self.set_floating_bytes(0);
    }

    /// A press with a move tool.
    pub(super) fn begin_move(&mut self, x: i32, y: i32) {
        if self.tool == Tool::MoveSelection {
            if let Some(base) = self.doc.selection().cloned() {
                self.drag = Some(Drag::MoveOutline {
                    base: Some(base),
                    start: (x, y),
                });
            }
            return;
        }
        if let Some(float) = &self.float {
            // A press on the pixels drags them; a press elsewhere lands them.
            if float.footprint(float.pos).contains(x, y) {
                self.drag = Some(Drag::MoveFloat {
                    start: (x, y),
                    from: float.pos,
                });
            } else {
                self.commit_float();
            }
            return;
        }
        let inside = match self.doc.selection() {
            Some(selection) => selection.contains(x, y),
            None => self.doc.bounds().contains(x, y),
        };
        if !inside {
            return;
        }
        match self.lift() {
            Ok(()) => {
                let from = self.float.as_ref().map_or((0, 0), |f| f.pos);
                self.drag = Some(Drag::MoveFloat {
                    start: (x, y),
                    from,
                });
            }
            Err(error) => self.notice = Some(error),
        }
    }

    /// A drag with a move tool moved to `(x, y)`.
    pub(super) fn extend_move(&mut self, drag: Drag, x: i32, y: i32) -> Drag {
        match &drag {
            Drag::MoveFloat { start, from } => {
                let pos = (
                    from.0.saturating_add(x.saturating_sub(start.0)),
                    from.1.saturating_add(y.saturating_sub(start.1)),
                );
                self.move_float_to(pos);
            }
            Drag::MoveOutline { base, start } => {
                let moved = base.as_ref().map(|base| {
                    base.translated(x.saturating_sub(start.0), y.saturating_sub(start.1))
                });
                let moved = moved.and_then(|s| s.clipped(self.doc.bounds()));
                if self.doc.selection() != moved.as_ref() {
                    self.doc.set_selection(moved);
                    self.selection_changed();
                }
            }
            _ => {}
        }
        drag
    }

    /// Records the outline a Move Selection drag left.
    pub(super) fn end_move_outline(&mut self, base: Option<Selection>) {
        if self.doc.selection() != base.as_ref() {
            self.history.record(Edit::Selection {
                name: "Move Selection".to_string(),
                before: base,
            });
        }
        self.selection_changed();
    }

    /// Moves the float (lifting first if need be) or the outline by `(dx, dy)`
    /// pixels: the arrow keys. Successive nudges are one history step.
    pub fn nudge(&mut self, dx: i32, dy: i32) {
        match self.tool {
            Tool::MoveSelection => {
                self.end();
                let Some(selection) = self.doc.selection() else {
                    return;
                };
                let moved = selection.translated(dx, dy).clipped(self.doc.bounds());
                self.change_selection("Move Selection", moved);
            }
            Tool::MovePixels => {
                self.end();
                if self.float.is_none()
                    && let Err(error) = self.lift()
                {
                    self.notice = Some(error);
                    return;
                }
                if let Some(float) = &self.float {
                    let pos = (
                        float.pos.0.saturating_add(dx),
                        float.pos.1.saturating_add(dy),
                    );
                    self.move_float_to(pos);
                }
            }
            _ => {}
        }
    }
}
