#![forbid(unsafe_code)]

//! Open, Save and Save As: the file-dialog flows and the document's path.
//!
//! Every write goes through `Storage::save_to` and every read through
//! `Storage::load_from`, so the app never names a filesystem. A failed write or
//! a corrupt file only sets the status message: the document is untouched.

use std::path::{Path, PathBuf};

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::model::Bitmap;

/// The name shown for `path`.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `path` with `.png` appended unless it already ends in it, since PNG bytes
/// under another extension would not reopen as a picture.
fn with_png_extension(path: PathBuf) -> PathBuf {
    let is_png = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
    if is_png {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(".png");
    PathBuf::from(name)
}

impl PaintApp {
    /// Whether any modal dialog is showing.
    pub(super) fn modal_open(&self) -> bool {
        self.resize.is_open() || self.files.as_ref().is_some_and(|files| files.is_open())
    }

    /// The window title for the current document.
    pub(super) fn title(&self) -> String {
        let name = self
            .current_path
            .as_deref()
            .map(display_name)
            .unwrap_or_else(|| "untitled".to_string());
        format!("{name} - Paint")
    }

    /// Open: the picker when wired, else the storage's own file.
    pub(super) fn open_requested(&mut self) {
        match &self.files {
            Some(files) => files.show_open(self.current_path.as_deref().and_then(Path::parent)),
            None => {
                self.open();
            }
        }
    }

    /// Save: to the current path when known, else through Save As.
    pub(super) fn save_requested(&mut self) {
        if self.files.is_none() {
            self.save();
        } else if let Some(path) = self.current_path.clone() {
            self.save_to(&path);
        } else {
            self.save_as_requested();
        }
    }

    fn save_as_requested(&mut self) {
        let Some(files) = &self.files else { return };
        let name = self
            .current_path
            .as_deref()
            .map(display_name)
            .unwrap_or_else(|| "untitled.png".to_string());
        files.show_save(&name, self.current_path.as_deref().and_then(Path::parent));
    }

    /// The Open picker returned a path.
    pub(super) fn open_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        if let Some(path) = self.files.as_ref().and_then(|files| files.take_chosen()) {
            self.open_path(&path);
        }
    }

    /// The Save As picker returned a path.
    pub(super) fn save_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        let Some(chosen) = self.files.as_ref().and_then(|files| files.take_chosen()) else {
            return;
        };
        let path = with_png_extension(chosen.clone());
        // The picker confirmed overwriting `chosen`; a path it did not see must
        // not be replaced silently.
        let unseen = path != chosen && self.files.as_ref().is_some_and(|f| f.exists(&path));
        if unseen {
            self.message = Some(format!("Not saved: {} already exists", display_name(&path)));
            return;
        }
        self.save_to(&path);
    }

    /// Decodes `path` first, then swaps, so a failed load leaves the canvas.
    fn open_path(&mut self, path: &Path) {
        let name = display_name(path);
        let Some(bytes) = self.storage.load_from(path) else {
            self.message = Some(format!("Open failed: cannot read {name}"));
            return;
        };
        match Bitmap::decode(&bytes) {
            Ok(bitmap) => {
                self.model.load(bitmap);
                self.cursor = None;
                self.current_path = Some(path.to_path_buf());
                self.message = Some(format!("Opened {name}"));
            }
            Err(error) => self.message = Some(format!("Open failed: {name}: {error}")),
        }
    }

    /// Encodes the bitmap and writes it to `path`, keeping the document as is
    /// (and the previous path) on failure.
    fn save_to(&mut self, path: &Path) {
        let name = display_name(path);
        let result = self
            .model
            .bitmap()
            .encode_png()
            .map_err(|error| error.to_string())
            .and_then(|bytes| self.storage.save_to(path, &bytes));
        self.message = Some(match result {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                format!("Saved {name}")
            }
            Err(error) => format!("Save failed: {name}: {error}"),
        });
    }

    /// The start-up file: loaded through the storage's own path, no dialog.
    pub(super) fn open_startup(&mut self) {
        if self.open() {
            self.current_path = self.storage.default_path();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_appended_only_when_missing() {
        let png = |p: &str| with_png_extension(PathBuf::from(p));
        assert_eq!(png("/a/cat"), PathBuf::from("/a/cat.png"));
        assert_eq!(png("/a/cat.PNG"), PathBuf::from("/a/cat.PNG"));
        assert_eq!(png("/a/cat.jpg"), PathBuf::from("/a/cat.jpg.png"));
        assert_eq!(png("/a/cat.png"), PathBuf::from("/a/cat.png"));
    }
}
