#![forbid(unsafe_code)]

//! The storage-backed Save and Open and the Resize prompt's answer.

use xui_core::app::Ui;

use super::{Msg, PaintApp};

impl PaintApp {
    /// Saves the bitmap through the storage, reporting the outcome.
    pub(super) fn save(&mut self) {
        let result = self
            .model
            .flatten()
            .encode_png()
            .map_err(|error| error.to_string())
            .and_then(|bytes| self.storage.save(&bytes));
        self.message = Some(match result {
            Ok(()) => "Saved".to_string(),
            Err(error) => format!("Save failed: {error}"),
        });
    }

    /// Decodes first, then swaps, so a failed load leaves the canvas intact.
    pub(super) fn open(&mut self) -> bool {
        let Some(bytes) = self.storage.load() else {
            self.message = Some("Open failed: nothing stored".to_string());
            return false;
        };
        match crate::model::Bitmap::decode(&bytes) {
            Ok(bitmap) => match self.model.load(bitmap) {
                Ok(()) => {
                    self.cursor = None;
                    self.message = Some("Opened".to_string());
                    true
                }
                Err(error) => {
                    self.message = Some(format!("Open failed: {error}"));
                    false
                }
            },
            Err(error) => {
                self.message = Some(format!("Open failed: {error}"));
                false
            }
        }
    }

    /// Applies the size typed into the Resize prompt.
    pub(super) fn resize_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        let Some(text) = self.resize.take_text() else {
            return;
        };
        let result = crate::model::parse_size(&text)
            .and_then(|(width, height)| self.model.resize(width, height).map(|()| (width, height)));
        self.message = Some(match result {
            Ok((width, height)) => format!("Resized to {width} x {height}"),
            Err(error) => format!("Resize failed: {error}"),
        });
    }
}
