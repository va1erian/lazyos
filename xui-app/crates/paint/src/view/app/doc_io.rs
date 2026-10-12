#![forbid(unsafe_code)]

//! The storage-backed Save and Open, for a store with no paths (the default
//! `MemoryStorage`, a single start-up file). A host with file dialogs goes
//! through [`files`](super::files) and [`save_flow`](super::save_flow)
//! instead.

use super::PaintApp;
use crate::model::Format;
use crate::view::Report;

impl PaintApp {
    /// Saves the flattened picture as PNG through the storage, reporting the
    /// outcome. A path-less store has no file name to pick a format from, so
    /// it is always PNG.
    pub(super) fn save(&mut self) -> bool {
        let result = self
            .model
            .save_bytes(Format::Png)
            .and_then(|bytes| self.storage.save(&bytes));
        let saved = result.is_ok();
        self.message = Some(match result {
            Ok(()) => "Saved".to_string(),
            Err(error) => format!("Save failed: {error}"),
        });
        if saved {
            self.mark_clean();
            self.announce(Report::Saved {
                path: self.storage.default_path(),
                format: Format::Png,
            });
        }
        saved
    }

    /// Opens whatever the storage holds, in any format Paint reads. Decodes
    /// first, then swaps, so a failed load leaves the canvas intact.
    pub(super) fn open(&mut self) -> bool {
        let Some(bytes) = self.storage.load() else {
            self.message = Some("Open failed: nothing stored".to_string());
            return false;
        };
        match self.model.open_bytes(&bytes) {
            Ok(_) => {
                self.cursor = None;
                self.mark_clean();
                self.message = Some("Opened".to_string());
                self.announce_open(self.storage.default_path());
                true
            }
            Err(error) => {
                self.message = Some(format!("Open failed: {error}"));
                false
            }
        }
    }
}
