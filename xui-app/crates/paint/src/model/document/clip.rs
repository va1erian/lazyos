#![forbid(unsafe_code)]

//! The in-app clipboard (Cut, Copy, Copy Merged, the three Pastes) and Crop to
//! Selection.
//!
//! The slot holds one bitmap and where it was copied from. It is a field of
//! the model, which lives as long as the app, so it survives New and Paste into
//! New Image. Its bytes count against the memory budget like the floating
//! selection's do, and a copy that would not fit is refused with the old
//! clipboard intact.

use super::Model;
use crate::model::selection::crop_document;
use crate::model::{Bitmap, Document, Edit, Tool, check_budget, div255};

/// A copied picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    bitmap: Bitmap,
    offset: (i32, i32),
}

impl Clipboard {
    /// The copied pixels, transparent outside the copied selection.
    pub fn bitmap(&self) -> &Bitmap {
        &self.bitmap
    }

    /// The top-left of the selection it was copied from, in picture pixels.
    pub fn offset(&self) -> (i32, i32) {
        self.offset
    }
}

impl Model {
    /// The clipboard slot.
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.clipboard.as_ref()
    }

    /// Puts `bitmap` (copied from `offset`) in the clipboard, replacing what
    /// was there. Refused, with the clipboard unchanged, when the budget has no
    /// room for it next to the document, the history and a floating selection.
    pub fn set_clipboard(&mut self, bitmap: Bitmap, offset: (i32, i32)) -> Result<(), String> {
        let mut reserved = self.reserved();
        reserved.clipboard = bitmap.byte_len() as u64;
        let (width, height) = self.doc.size();
        check_budget(width, height, self.doc.layers().len(), reserved)
            .map_err(|error| error.to_string())?;
        self.set_clipboard_bytes(reserved.clipboard);
        self.clipboard = Some(Clipboard { bitmap, offset });
        Ok(())
    }

    /// Empties the clipboard.
    pub fn clear_clipboard(&mut self) {
        self.clipboard = None;
        self.set_clipboard_bytes(0);
    }

    /// Copies the selected pixels of the active layer, or with `merged` of
    /// every visible layer, to the clipboard. Pixels outside the selection's
    /// mask are transparent in the copy.
    pub fn copy(&mut self, merged: bool) -> Result<(), String> {
        self.settle();
        let selection = self
            .doc
            .selection()
            .ok_or_else(|| "Nothing is selected".to_string())?;
        let area = selection.bounds().intersect(self.doc.bounds());
        let flat;
        let source = if merged {
            flat = self.doc.flatten();
            &flat
        } else {
            &self.doc.active_layer().pixels
        };
        let mut copy = source
            .crop(area)
            .ok_or_else(|| "Nothing is selected".to_string())?;
        if !selection.is_rect() {
            for y in area.y0..area.y1 {
                for x in area.x0..area.x1 {
                    let cover = selection.coverage(x, y);
                    if cover == 255 {
                        continue;
                    }
                    let (cx, cy) = (x - area.x0, y - area.y0);
                    if let Some(mut pixel) = copy.get(cx, cy) {
                        pixel = if cover == 0 {
                            [0, 0, 0, 0]
                        } else {
                            pixel[3] = div255(u32::from(pixel[3]) * u32::from(cover)) as u8;
                            pixel
                        };
                        copy.put(cx, cy, pixel);
                    }
                }
            }
        }
        self.set_clipboard(copy, (area.x0, area.y0))
    }

    /// Copies the selection, then erases it from the active layer.
    pub fn cut(&mut self) -> Result<(), String> {
        self.copy(false)?;
        self.paint_selection("Cut", |_| [0, 0, 0, 0]);
        Ok(())
    }

    /// Pastes the clipboard as a floating selection with its top-left at `at`
    /// (the top-left of what the view shows) and selects Move Selected Pixels
    /// so it can be placed. Esc cancels, any other action lands it.
    pub fn paste(&mut self, at: (i32, i32)) -> Result<(), String> {
        self.settle();
        let bitmap = self.pasteable()?;
        self.set_tool(Tool::MovePixels);
        self.float_bitmap(bitmap, at, "Paste")
    }

    /// Like [`Model::paste`], into a new transparent layer above the active one.
    pub fn paste_new_layer(&mut self, at: (i32, i32)) -> Result<(), String> {
        self.settle();
        let bitmap = self.pasteable()?;
        // The layer and the float both need room.
        let (width, height) = self.doc.size();
        let mut reserved = self.reserved();
        reserved.floating = bitmap.byte_len() as u64;
        check_budget(width, height, self.doc.layers().len() + 1, reserved)
            .map_err(|error| error.to_string())?;
        self.add_layer()?;
        self.set_tool(Tool::MovePixels);
        self.float_bitmap(bitmap, at, "Paste")
    }

    /// Replaces the document with a new one the size of the clipboard, holding
    /// it as its only layer. Starts a fresh history.
    pub fn paste_new_image(&mut self) -> Result<(), String> {
        self.settle();
        let bitmap = self.pasteable()?;
        self.check_fits(bitmap.width(), bitmap.height(), 1)?;
        let doc = Document::from_bitmap(bitmap).map_err(|error| error.to_string())?;
        self.replace(doc);
        Ok(())
    }

    /// The clipboard's picture, or why there is none.
    fn pasteable(&self) -> Result<Bitmap, String> {
        self.clipboard
            .as_ref()
            .map(|clip| clip.bitmap.clone())
            .ok_or_else(|| "The clipboard has no picture".to_string())
    }

    /// Crops every layer to the selection's bounds, making what lies outside
    /// its mask transparent, as one undo step. The selection is gone after.
    pub fn crop_to_selection(&mut self) -> Result<(), String> {
        self.settle();
        // The old document stays held by the undo entry, even past the
        // history cap: check before the cropped copy is allocated.
        if let Some(selection) = self.doc.selection() {
            let area = selection.bounds().intersect(self.doc.bounds());
            let old = self.doc.bytes() as u64;
            let layers = self.doc.layers().len();
            self.check_fits_recording(area.width(), area.height(), layers, old)?;
        }
        let Some(mut cropped) = crop_document(&self.doc) else {
            return Err(if self.doc.selection().is_some() {
                "The selection is the whole picture".to_string()
            } else {
                "Nothing is selected".to_string()
            });
        };
        self.doc.swap_contents(&mut cropped);
        // `cropped` now holds the old document, which is what undo restores.
        self.history.record(Edit::Document {
            name: "Crop to Selection".to_string(),
            before: Box::new(cropped),
        });
        self.preview = None;
        self.damage_all();
        self.selection_changed();
        Ok(())
    }
}
