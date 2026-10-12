#![forbid(unsafe_code)]

//! Opening and saving documents as bytes (P6). The caller moves the bytes
//! through a [`Storage`](crate::storage::Storage) (`load_from` / `save_to`),
//! which are the same for every format.

use super::Model;
use crate::model::Document;
use crate::model::format::{self, Format};

impl Model {
    /// Replaces the document with `doc` (a decoded file) and empties the
    /// history; refused, with the document untouched, over the memory budget.
    pub fn load_document(&mut self, doc: Document) -> Result<(), String> {
        self.check_fits(doc.width(), doc.height(), doc.layers().len())?;
        self.replace(doc);
        Ok(())
    }

    /// Opens a file's bytes, whatever its format (PNG, JPEG, BMP or
    /// OpenRaster, found from the bytes). On success the document is replaced
    /// and the history emptied; on failure nothing changes and the message is
    /// fit for the status bar.
    pub fn open_bytes(&mut self, bytes: &[u8]) -> Result<Format, String> {
        let (format, doc) = format::open(bytes)?;
        self.load_document(doc)?;
        Ok(format)
    }

    /// The document encoded as `format`. A format without layers gets a
    /// flattened copy; the document is not changed. Ask
    /// [`Model::needs_flatten`] first to offer "Flatten image?".
    ///
    /// Every format builds a flattened copy (the file itself, or OpenRaster's
    /// merged image), so that copy is checked against the budget first.
    pub fn save_bytes(&self, format: Format) -> Result<Vec<u8>, String> {
        let (width, height) = self.doc.size();
        self.check_extra_fits(u64::from(width) * u64::from(height) * 4)?;
        format::save(format, &self.doc)
    }

    /// Whether saving as `format` would drop layers (more than one layer in a
    /// format that has none).
    pub fn needs_flatten(&self, format: Format) -> bool {
        format::needs_flatten(format, &self.doc)
    }
}
