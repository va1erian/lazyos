#![forbid(unsafe_code)]

//! The Image and Layers menu operations (P5): resize, canvas size, crop,
//! flips and turns, flatten and merge down. Each is one undoable edit with a
//! display name, refused with a message (and nothing changed) when it would
//! not fit the memory budget.

use super::Model;
use crate::model::ops::{self, Anchor, CanvasFill, Flip, Resample, Turn};
use crate::model::selection::{Selection, clear_outside_mask};
use crate::model::{Bitmap, Document, Edit, LayerCommand, MAX_SIDE, MIN_SIDE, Rect};

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

fn check_sides(width: u32, height: u32) -> Result<(), String> {
    let range = MIN_SIDE..=MAX_SIDE;
    if range.contains(&width) && range.contains(&height) {
        Ok(())
    } else {
        Err(format!(
            "size must be {MIN_SIDE}-{MAX_SIDE} pixels per side"
        ))
    }
}

impl Model {
    /// Checks that a new `width` x `height` document of `layers` layers fits
    /// next to the current one, which the undo entry keeps (even past the
    /// history cap).
    fn check_replacement(&self, width: u32, height: u32, layers: usize) -> Result<(), String> {
        let old = self.doc.bytes() as u64;
        self.check_fits_recording(width, height, layers, old)
    }

    /// Swaps in `new` as the document and records the old one as the undo
    /// entry `name`.
    fn replace_document(&mut self, name: String, mut new: Document) {
        self.doc.swap_contents(&mut new);
        // `new` now holds the old document: exactly what undo restores.
        self.history.record(Edit::Document {
            name,
            before: Box::new(new),
        });
        self.damage_all();
        self.bump();
    }

    /// Resize Image: scales every layer to `width` x `height`.
    pub fn resize_image(&mut self, width: u32, height: u32, how: Resample) -> Result<(), String> {
        check_sides(width, height)?;
        self.settle();
        if self.doc.size() == (width, height) {
            return Ok(());
        }
        self.check_replacement(width, height, self.doc.layers().len())?;
        let new = self
            .doc
            .mapped(width, height, |_, b| ops::resample(b, width, height, how));
        self.replace_document("Resize Image".to_string(), new);
        Ok(())
    }

    /// Canvas Size: a new canvas of `width` x `height` with the picture at
    /// `anchor`. New area is transparent, or with [`CanvasFill::Secondary`]
    /// the secondary colour on the bottom layer (transparent above it, as for
    /// a plain resize). A smaller canvas crops.
    pub fn canvas_size(
        &mut self,
        width: u32,
        height: u32,
        anchor: Anchor,
        fill: CanvasFill,
    ) -> Result<(), String> {
        check_sides(width, height)?;
        self.settle();
        if self.doc.size() == (width, height) {
            return Ok(());
        }
        self.check_replacement(width, height, self.doc.layers().len())?;
        let (x, y) = anchor.offset(self.doc.size(), (width, height));
        let bottom = match fill {
            CanvasFill::Transparent => TRANSPARENT,
            CanvasFill::Secondary => self.secondary,
        };
        let new = self.doc.mapped(width, height, |i, b| {
            let pad = if i == 0 { bottom } else { TRANSPARENT };
            ops::placed(b, width, height, pad, x, y)
        });
        self.replace_document("Canvas Size".to_string(), new);
        Ok(())
    }

    /// Crop to Selection: keeps `rect` (clipped to the image) of every layer.
    /// See [`Model::crop_to_selection`] for the masked form.
    pub fn crop(&mut self, rect: Rect) -> Result<(), String> {
        self.crop_masked(rect, None)
    }

    /// Crops to `rect`; when `mask` is given, what it does not cover becomes
    /// transparent (scaled by the coverage at a soft edge) in the same
    /// history entry. The selection is gone afterwards.
    pub(super) fn crop_masked(
        &mut self,
        rect: Rect,
        mask: Option<&Selection>,
    ) -> Result<(), String> {
        self.settle();
        let rect = rect.intersect(self.doc.bounds());
        if rect.is_empty() {
            return Err("nothing to crop to".to_string());
        }
        if rect == self.doc.bounds() && mask.is_none_or(Selection::is_rect) {
            return Ok(());
        }
        let (width, height) = (rect.width(), rect.height());
        self.check_replacement(width, height, self.doc.layers().len())?;
        let new = self.doc.mapped(width, height, |_, b| {
            let mut pixels = b
                .crop(rect)
                .unwrap_or_else(|| Bitmap::transparent(width, height));
            if let Some(mask) = mask {
                clear_outside_mask(&mut pixels, rect, mask);
            }
            pixels
        });
        self.replace_document("Crop to Selection".to_string(), new);
        self.selection_changed();
        Ok(())
    }

    /// Flip Image: mirrors every layer.
    pub fn flip_image(&mut self, flip: Flip) -> Result<(), String> {
        self.settle();
        let (w, h) = self.doc.size();
        self.check_replacement(w, h, self.doc.layers().len())?;
        let new = self.doc.mapped(w, h, |_, b| flip.apply(b));
        self.replace_document(format!("Flip Image {}", flip.label()), new);
        Ok(())
    }

    /// Rotate Image: turns every layer; a quarter turn swaps width and height.
    pub fn rotate_image(&mut self, turn: Turn) -> Result<(), String> {
        self.settle();
        let (w, h) = self.doc.size();
        let (nw, nh) = if turn.swaps_sides() { (h, w) } else { (w, h) };
        self.check_replacement(nw, nh, self.doc.layers().len())?;
        let new = self.doc.mapped(nw, nh, |_, b| turn.apply(b));
        self.replace_document(format!("Rotate Image {}", turn.label()), new);
        Ok(())
    }

    /// Flip Layer: mirrors the active layer alone.
    pub fn flip_layer(&mut self, flip: Flip) -> Result<(), String> {
        self.replace_active_pixels(format!("Flip Layer {}", flip.label()), |b| flip.apply(b))
    }

    /// Rotate Layer: turns the active layer alone, about the image's centre.
    /// The layer keeps the image's size, so a quarter turn of a layer in a
    /// picture that is not square crops what leaves the canvas and leaves the
    /// rest transparent.
    pub fn rotate_layer(&mut self, turn: Turn) -> Result<(), String> {
        let (w, h) = self.doc.size();
        self.replace_active_pixels(format!("Rotate Layer {}", turn.label()), move |b| {
            let turned = turn.apply(b);
            if turned.size() == (w, h) {
                return turned;
            }
            let (x, y) = Anchor::Center.offset(turned.size(), (w, h));
            ops::placed(&turned, w, h, TRANSPARENT, x, y)
        })
    }

    /// Swaps the active layer's pixels for `f(old)` as one `Pixels` edit
    /// covering the whole layer. No edit when nothing changes.
    fn replace_active_pixels(
        &mut self,
        name: String,
        f: impl FnOnce(&Bitmap) -> Bitmap,
    ) -> Result<(), String> {
        self.settle();
        let bytes = self.doc.active_layer().pixels.byte_len() as u64;
        self.check_extra_fits(bytes)?;
        let layer = self.doc.active();
        let Some(target) = self.doc.layer_mut(layer) else {
            return Err("no such layer".to_string());
        };
        let mut new = f(&target.pixels);
        if new == target.pixels {
            return Ok(());
        }
        let rect = new.rect();
        std::mem::swap(&mut target.pixels, &mut new);
        self.history.record(Edit::Pixels {
            name,
            layer,
            rect,
            before: new,
        });
        self.damage_all();
        self.bump();
        Ok(())
    }

    /// Flatten: one "Background" layer holding the visible layers composited.
    /// A one-layer image is left alone.
    pub fn flatten_image(&mut self) -> Result<(), String> {
        self.settle();
        let layers = self.doc.layers().len();
        if layers <= 1 {
            return Ok(());
        }
        // The old layers stay held by the undo entry, so the flattened layer
        // is new memory on top of them.
        self.check_replacement(self.doc.width(), self.doc.height(), 1)?;
        let new = self.doc.flattened();
        self.replace_document("Flatten".to_string(), new);
        Ok(())
    }

    /// Merge Layer Down: paints the active layer onto the one below it
    /// through its opacity (a hidden layer adds nothing), and selects the
    /// lower layer, which keeps its own name, opacity and visibility.
    pub fn merge_layer_down(&mut self) -> Result<(), String> {
        self.settle();
        let index = self.doc.active();
        if index == 0 {
            return Err("there is no layer below".to_string());
        }
        // Undo keeps a copy of the lower layer's picture.
        let lower = self.doc.layer(index - 1).map_or(0, |l| l.pixels.byte_len());
        self.check_extra_fits(lower as u64)?;
        let Some((upper, lower)) = self.doc.merge_into_below(index) else {
            return Err("there is no layer below".to_string());
        };
        self.history.record(Edit::Layer {
            name: "Merge Layer Down".to_string(),
            command: LayerCommand::Unmerge {
                index,
                upper,
                lower,
                active: index,
            },
        });
        self.damage_all();
        self.bump();
        Ok(())
    }
}
