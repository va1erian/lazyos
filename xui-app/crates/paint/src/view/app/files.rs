#![forbid(unsafe_code)]

//! Open, the document's path and name, and the window title. Saving is in
//! [`save_flow`](super::save_flow), the unsaved-changes guard in
//! [`guard`](super::guard).
//!
//! Every read goes through `Storage::load_from`, so the app never names a
//! filesystem. A corrupt file only sets the status message: the document is
//! untouched.

use std::path::{Path, PathBuf};

use xui_core::app::Ui;

use super::{Msg, PaintApp};
use crate::view::Report;

/// The name shown for `path`.
pub(super) fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl PaintApp {
    /// Whether a modal dialog, a prompt or the colour editor has the window.
    pub(super) fn modal_open(&self, ui: &Ui<Msg>) -> bool {
        self.paste_size.is_open()
            || self.about.is_open()
            || self.unsaved.is_open()
            || self.flatten_prompt.is_open()
            || self.editor.is_open(ui)
            || self.props.is_open(ui)
            || self.frames.is_open(ui)
            || self.files.as_ref().is_some_and(|files| files.is_open())
    }

    /// The document's file name, or `untitled`.
    pub(super) fn doc_name(&self) -> String {
        self.current_path
            .as_deref()
            .map(display_name)
            .unwrap_or_else(|| "untitled".to_string())
    }

    /// The window title for the current document: `name.ext* - Paint`, the
    /// star while there are unsaved changes.
    pub(super) fn title(&self) -> String {
        let star = if self.dirty() { "*" } else { "" };
        format!("{}{star} - Paint", self.doc_name())
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

    /// The Open picker returned a path.
    pub(super) fn open_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        if let Some(path) = self.files.as_ref().and_then(|files| files.take_chosen()) {
            self.open_path(&path);
        }
    }

    /// Decodes `path` first, then swaps, so a failed load leaves the canvas.
    fn open_path(&mut self, path: &Path) {
        let name = display_name(path);
        let Some(bytes) = self.storage.load_from(path) else {
            self.message = Some(format!("Open failed: cannot read {name}"));
            return;
        };
        match self.model.open_bytes(&bytes) {
            Ok(_) => {
                self.cursor = None;
                self.current_path = Some(path.to_path_buf());
                self.mark_clean();
                self.message = Some(format!("Opened {name}"));
                self.announce_open(Some(path.to_path_buf()));
            }
            Err(error) => self.message = Some(format!("Open failed: {name}: {error}")),
        }
    }

    /// Tells the host's reporter a picture was opened.
    pub(super) fn announce_open(&self, path: Option<PathBuf>) {
        let doc = self.model.document();
        self.announce(Report::Opened {
            path,
            size: doc.size(),
            layers: doc.layers().len(),
        });
    }

    /// The start-up file: loaded through the storage's own path, no dialog.
    pub(super) fn open_startup(&mut self) {
        if self.open() {
            self.current_path = self.storage.default_path();
        }
    }
}
