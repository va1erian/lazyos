#![forbid(unsafe_code)]

//! The Edit menu commands and the Cmd forms of the editor's own shortcuts: each
//! changes the buffer and then refreshes the title and status.

use xui_core::app::Ui;

use crate::app::{Msg, Notepad};

/// Undo, then refresh.
pub fn undo(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.undo();
    super::edited(app, ui);
}

/// Redo, then refresh.
pub fn redo(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.redo();
    super::edited(app, ui);
}

/// Cut the selection, then refresh.
pub fn cut(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.cut();
    super::edited(app, ui);
}

/// Copy the selection; the text does not change.
pub fn copy(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.copy();
    super::refresh(app, ui);
}

/// Paste at the caret, then refresh.
pub fn paste(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.paste();
    super::edited(app, ui);
}

/// Select the whole buffer and refresh the selection readout.
pub fn select_all(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.editor.select_all();
    super::refresh(app, ui);
}
