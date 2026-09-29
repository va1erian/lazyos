#![forbid(unsafe_code)]

//! The notepad's behaviour: new/open/save, the find/replace session and the
//! modal dialogs.
//!
//! File I/O is synchronous on the UI thread. The file model caps a load at
//! [`MAX_FILE_BYTES`](xui_code_editor::document::MAX_FILE_BYTES) (32 MiB) and an
//! atomic save is one write plus a rename, so the pause is bounded and a worker
//! thread would add cross-thread state for no gain.
//!
//! The editing commands and the find/replace session live in `edit` and `find`;
//! this module owns the file commands, the dialogs and the status readout.

mod edit;
mod find;

use std::path::{Path, PathBuf};

use xui_code_editor::document::Document;
use xui_core::app::Ui;
use xui_core::widget::DialogAction;

use crate::app::{After, Msg, Notepad, Pending};

pub use edit::{copy, cut, paste, redo, select_all, undo};
pub use find::{close_find, find_refresh, find_step, replace_all, replace_current, show_find};

/// The editor's text changed: refresh the title, status and find matches.
pub fn edited(app: &mut Notepad, ui: &mut Ui<Msg>) {
    refresh(app, ui);
    if app.find_open.get() {
        find_refresh(app, ui);
    }
}

/// Refreshes the window title and status bar from the editor and document.
pub fn refresh(app: &mut Notepad, ui: &mut Ui<Msg>) {
    let (line, col) = app.editor.caret_line_col();
    let selected = app
        .editor
        .selection()
        .map(|(start, end)| end - start)
        .unwrap_or(0);
    let dirty = dirty(app);
    let ending = match app.document.line_ending() {
        xui_code_editor::LineEnding::Lf => "LF",
        xui_code_editor::LineEnding::CrLf => "CRLF",
    };
    app.status
        .set_text(0, &format!("Ln {}, Col {}", line + 1, col + 1));
    app.status.set_text(1, &format!("Sel {selected}"));
    app.status.set_text(2, ending);
    app.status
        .set_text(3, if dirty { "Modified" } else { "Saved" });
    ui.set_window_title(&format!(
        "{}{} - xui notepad",
        if dirty { "*" } else { "" },
        app.document.display_name()
    ));
}

/// New: confirm discard when dirty, then start an untitled document.
pub fn new_document(app: &mut Notepad, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    if dirty(app) {
        confirm_discard(app, ui, After::New);
    } else {
        untitled(app, ui);
    }
}

/// Open: confirm discard when dirty, then ask for a path.
pub fn open(app: &mut Notepad, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    if dirty(app) {
        confirm_discard(app, ui, After::Open);
    } else {
        open_prompt(app, ui);
    }
}

/// Save: write to the current path, or ask for one when untitled.
pub fn save(app: &mut Notepad, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    match app.document.path().map(Path::to_path_buf) {
        Some(path) => save_to(app, ui, path),
        None => save_as_prompt(app, ui),
    }
}

/// Save As: ask for a path.
pub fn save_as_prompt(app: &mut Notepad, _ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    let suggested = app
        .document
        .path()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled.txt".to_string());
    app.save_dialog.set_suggested_name(&suggested);
    if let Some(dir) = app.document.path().and_then(Path::parent) {
        app.save_dialog.set_initial_dir(dir);
    }
    app.dialog_open.set(true);
    app.save_dialog.open();
}

/// Quit: confirm discard when dirty, otherwise end the loop.
///
/// While a dialog is open the close request is ignored, so the window's close
/// button cannot open a second dialog over the first.
pub fn quit(app: &mut Notepad, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    if dirty(app) {
        confirm_discard(app, ui, After::Quit);
    } else {
        ui.quit();
    }
}

/// Handle a dialog dismissal according to what the app is waiting for.
pub fn dialog_action(app: &mut Notepad, ui: &mut Ui<Msg>, action: DialogAction) {
    app.dialog_open.set(false);
    match std::mem::replace(&mut app.pending, Pending::None) {
        Pending::None => {}
        Pending::DiscardThen(after) => {
            if matches!(action, DialogAction::Accept(_)) {
                match after {
                    After::New => untitled(app, ui),
                    After::Open => open_prompt(app, ui),
                    After::Quit => ui.quit(),
                }
            } else {
                app.editor.focus();
            }
        }
    }
}

/// The Open picker returned a path: load it.
pub fn open_chosen(app: &mut Notepad, ui: &mut Ui<Msg>, path: PathBuf) {
    app.dialog_open.set(false);
    open_path(app, ui, path);
}

/// The Save As picker returned a path: write there.
pub fn save_chosen(app: &mut Notepad, ui: &mut Ui<Msg>, path: PathBuf) {
    app.dialog_open.set(false);
    save_to(app, ui, path);
}

/// Load `path` into the editor without touching the current document first.
pub fn open_path(app: &mut Notepad, ui: &mut Ui<Msg>, path: PathBuf) {
    match Document::load(&path) {
        Ok((document, text)) => {
            app.document = document;
            app.editor.set_text(&text);
            clear_find(app, ui);
            app.editor.focus();
            refresh(app, ui);
            println!("EDITOR:OPEN:PASS:{}", path.display());
        }
        Err(error) => {
            println!("EDITOR:OPEN:FAIL:{}", path.display());
            show_error(
                app,
                ui,
                &format!("Could not open {}: {error}", path.display()),
            );
        }
    }
}

/// Start an empty untitled document.
fn untitled(app: &mut Notepad, ui: &mut Ui<Msg>) {
    app.document = Document::untitled();
    app.editor.set_text("");
    clear_find(app, ui);
    app.editor.focus();
    refresh(app, ui);
}

/// Ask for a path to open through the portable picker.
fn open_prompt(app: &mut Notepad, _ui: &mut Ui<Msg>) {
    if let Some(dir) = app.document.path().and_then(Path::parent) {
        app.open_dialog.set_initial_dir(dir);
    }
    app.dialog_open.set(true);
    app.open_dialog.open();
}

/// Write the buffer to `path` and record the clean revision.
fn save_to(app: &mut Notepad, ui: &mut Ui<Msg>, path: PathBuf) {
    let text = app.editor.text();
    let revision = app.editor.revision();
    match app.document.save_as(&path, &text, revision) {
        Ok(()) => {
            refresh(app, ui);
            app.editor.focus();
            println!("EDITOR:SAVE:PASS:{}", path.display());
        }
        Err(error) => {
            println!("EDITOR:SAVE:FAIL:{}", path.display());
            show_error(
                app,
                ui,
                &format!("Could not save {}: {error}", path.display()),
            );
        }
    }
}

/// Show the discard confirmation before running `after`.
fn confirm_discard(app: &mut Notepad, _ui: &mut Ui<Msg>, after: After) {
    app.pending = Pending::DiscardThen(after);
    app.confirm.set_title("Discard unsaved changes?");
    app.confirm
        .set_message("This document has unsaved changes. Discard them?");
    app.confirm.open();
    app.dialog_open.set(true);
}

/// Reset the find session and hide the bar.
fn clear_find(app: &mut Notepad, ui: &mut Ui<Msg>) {
    use xui_core::widget::HasText;
    app.search = Default::default();
    app.find_open.set(false);
    app.find_bar.set_visible(ui, false);
    app.find_bar.query.set_text("");
    app.find_bar.replacement.set_text("");
    app.find_bar.status.set_text("");
    app.find_bar.regex.set_checked(false);
    app.find_bar.case.set_checked(false);
}

/// Show the error dialog.
fn show_error(app: &mut Notepad, _ui: &mut Ui<Msg>, message: &str) {
    app.message.set_title("Error");
    app.message.set_message(message);
    app.message.open();
    app.dialog_open.set(true);
}

/// Whether the buffer differs from the saved file.
fn dirty(app: &Notepad) -> bool {
    app.document.is_dirty(app.editor.revision())
}
