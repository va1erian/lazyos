#![forbid(unsafe_code)]

//! The unsaved-changes guard: New, Open and Quit ask "Save changes?" first
//! when the picture was edited since it was opened or saved (LazyWriter's
//! `files::guard` pattern).
//!
//! The pending action waits in `PaintApp::after` while the card, and then the
//! Save As picker or the "Flatten image?" card, are answered. It runs when the
//! save succeeded or the user chose Don't Save, and is dropped on Cancel, a
//! cancelled picker or a failed write.

use xui_core::app::Ui;
use xui_core::widget::TaskDialogAction;

use super::commands::PasteKind;
use super::{Msg, PaintApp};
use crate::{DEFAULT_HEIGHT, DEFAULT_WIDTH};

/// What waits for the unsaved changes to be dealt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum After {
    /// A new white picture.
    New,
    /// A new transparent picture.
    NewTransparent,
    /// The Open picker.
    Open,
    /// Quit the application.
    Quit,
    /// Paste the clipboard picture as a new image.
    PasteNewImage,
}

impl PaintApp {
    /// Whether the picture has changes the file (or a fresh start) lacks.
    pub(super) fn dirty(&self) -> bool {
        self.model.is_floating()
            || self.model.is_editing_text()
            || self.saved.is_dirty(self.model.selection())
    }

    /// Records that the picture is as saved (or opened, or new).
    pub(super) fn mark_clean(&mut self) {
        self.saved
            .mark_clean(self.model.history(), self.model.selection());
    }

    /// Runs `after`, first asking Save / Don't Save / Cancel when there are
    /// unsaved changes.
    pub(super) fn guard(&mut self, ui: &Ui<Msg>, after: After) {
        if !self.dirty() {
            self.proceed(ui, after);
            return;
        }
        self.after = Some(after);
        if let Err(error) = self.unsaved.show(ui, &self.doc_name()) {
            self.after = None;
            self.message = Some(format!("Could not ask about unsaved changes: {error}"));
        }
    }

    /// Does what the guard was holding.
    fn proceed(&mut self, ui: &Ui<Msg>, after: After) {
        match after {
            After::New => self.new_picture(None),
            After::NewTransparent => self.new_picture(Some([0, 0, 0, 0])),
            After::Open => self.open_requested(),
            After::Quit => ui.quit(),
            After::PasteNewImage => self.finish_paste(PasteKind::NewImage),
        }
    }

    /// A fresh picture of the default size, white or with `fill`.
    fn new_picture(&mut self, fill: Option<crate::model::Pixel>) {
        let result = match fill {
            Some(fill) => self.model.reset_with(DEFAULT_WIDTH, DEFAULT_HEIGHT, fill),
            None => self.model.reset(DEFAULT_WIDTH, DEFAULT_HEIGHT),
        };
        match result {
            Ok(()) => {
                self.cursor = None;
                self.current_path = None;
                self.mark_clean();
            }
            Err(error) => self.message = Some(format!("New failed: {error}")),
        }
    }

    /// The "Save changes?" card was answered: 0 is Save, 1 is Don't Save.
    pub(super) fn unsaved_answer(&mut self, ui: &Ui<Msg>, action: TaskDialogAction) {
        self.unsaved.close();
        ui.focus(self.canvas.id());
        match action {
            // The save runs `after` itself once it has written the file, which
            // may take a picker and the Flatten card.
            TaskDialogAction::Command(0) => self.save_requested(ui),
            TaskDialogAction::Command(_) => self.finish_after(ui),
            TaskDialogAction::Cancel => self.after = None,
        }
    }

    /// Runs the pending `after`, if any.
    pub(super) fn finish_after(&mut self, ui: &Ui<Msg>) {
        if let Some(after) = self.after.take() {
            self.proceed(ui, after);
        }
    }
}
