#![forbid(unsafe_code)]

//! New, Open, Save, Export, Insert image and Quit, with the unsaved-changes
//! prompt, ported from the wordpad example's `commands.rs`.
//!
//! Serial evidence for the QEMU sessions, one line each:
//! `WRITER:OPEN:PASS|FAIL:<path>`, `WRITER:SAVE:PASS|FAIL:<path>`,
//! `WRITER:EXPORT:PASS|FAIL:<path>` and `WRITER:IMAGE:PASS|FAIL:<path>`.
//!
//! File I/O is synchronous on the UI thread: reads are capped and a save is
//! one write plus a rename, so the pause is bounded.

use std::path::{Path, PathBuf};

use xui_core::app::Ui;
use xui_core::widget::TaskDialogAction;
use xui_rich_text::edit::Command;
use xui_rich_text::model::{Document, Selection};

use crate::app::{After, Msg, Writer};
use crate::commands::{format, refresh_table, refresh_title, selection, words_label};
use crate::files;
use crate::names;

/// Runs `after` (New, Open or Quit), first asking Save / Discard / Cancel
/// when the document is modified.
pub fn guard(app: &mut Writer, ui: &mut Ui<Msg>, after: After) {
    if app.dialog_open.get() {
        return;
    }
    if app.dirty {
        app.after = Some(after);
        app.dialog_open.set(true);
        app.dialogs.unsaved.open();
    } else {
        proceed(app, ui, after);
    }
}

fn proceed(app: &mut Writer, ui: &mut Ui<Msg>, after: After) {
    match after {
        After::New => load(app, ui, Document::new(), None),
        After::Open => {
            app.dialogs.open.set_initial_dir(start_dir(app));
            app.dialog_open.set(true);
            app.dialogs.open.open();
        }
        After::Quit => ui.quit(),
    }
}

/// The unsaved-changes prompt was dismissed: 0 is Save, 1 is Discard.
pub fn unsaved(app: &mut Writer, ui: &mut Ui<Msg>, action: TaskDialogAction) {
    app.dialog_open.set(false);
    match action {
        TaskDialogAction::Command(0) => match saved_path(app) {
            Some(path) => {
                if save_to(app, ui, path) {
                    finish(app, ui);
                }
            }
            // Untitled: Save As runs `after` once it has saved.
            None => save_as(app),
        },
        TaskDialogAction::Command(_) => finish(app, ui),
        TaskDialogAction::Cancel => {
            app.after = None;
            app.editor.focus();
        }
    }
}

/// Runs the pending `after`, if any.
fn finish(app: &mut Writer, ui: &mut Ui<Msg>) {
    match app.after.take() {
        Some(after) => proceed(app, ui, after),
        None => app.editor.focus(),
    }
}

/// A picker was cancelled or a message dismissed. A cancelled Save As inside
/// the unsaved prompt cancels what it was for too.
pub fn dialog_closed(app: &mut Writer, _ui: &mut Ui<Msg>) {
    app.dialog_open.set(false);
    app.after = None;
    app.editor.focus();
}

/// The folder the pickers open in: the document's, else the host's start.
fn start_dir(app: &Writer) -> PathBuf {
    app.path
        .as_deref()
        .and_then(Path::parent)
        .filter(|dir| dir.is_absolute())
        .map_or_else(|| app.host.start_dir.clone(), Path::to_path_buf)
}

/// The path Save writes to without asking: the document's, unless it was
/// opened from plain text (saving JSON over a `.txt` would destroy it).
fn saved_path(app: &Writer) -> Option<PathBuf> {
    app.path.clone().filter(|path| !names::is_plain_text(path))
}

/// Replaces the document, as New and Open do.
fn load(app: &mut Writer, ui: &mut Ui<Msg>, doc: Document, path: Option<PathBuf>) {
    let words = files::word_count(&doc);
    app.editor.set_document(doc);
    refresh_table(app);
    app.path = path;
    app.dirty = false;
    app.summary = None;
    app.status.set_text(2, &words_label(words));
    refresh_title(app, ui);
    app.editor.focus();
    let summary = app
        .editor
        .with_document(|d| d.style_summary(&Selection::default()));
    selection(app, summary);
}

/// Shows an error message.
fn fail(app: &mut Writer, what: &str, path: &Path, error: impl std::fmt::Display) {
    app.dialogs
        .message
        .set_message(&format!("{what} {}: {error}", path.display()));
    app.dialog_open.set(true);
    app.dialogs.message.open();
}

pub fn open_chosen(app: &mut Writer, ui: &mut Ui<Msg>, path: PathBuf) {
    app.dialog_open.set(false);
    open_path(app, ui, path);
}

/// Opens `path` in place of the current document (the caller has dealt with
/// unsaved changes). Also used for the file named on the command line.
pub fn open_path(app: &mut Writer, ui: &mut Ui<Msg>, path: PathBuf) {
    match files::open_document(&path) {
        Ok(doc) => {
            println!("WRITER:OPEN:PASS:{}", path.display());
            load(app, ui, doc, Some(path));
        }
        Err(error) => {
            println!("WRITER:OPEN:FAIL:{}", path.display());
            fail(app, "Could not open", &path, error);
        }
    }
}

pub fn save(app: &mut Writer, ui: &mut Ui<Msg>) {
    if app.dialog_open.get() {
        return;
    }
    match saved_path(app) {
        Some(path) => {
            save_to(app, ui, path);
        }
        None => save_as(app),
    }
}

pub fn save_as(app: &mut Writer) {
    if app.dialog_open.get() {
        return;
    }
    let dialog = &app.dialogs.save;
    dialog.set_suggested_name(&names::suggested_document_name(app.path.as_deref()));
    dialog.set_initial_dir(start_dir(app));
    app.dialog_open.set(true);
    dialog.open();
}

pub fn save_chosen(app: &mut Writer, ui: &mut Ui<Msg>, path: PathBuf) {
    app.dialog_open.set(false);
    let path = names::with_document_extension(path);
    if save_to(app, ui, path) {
        finish(app, ui);
    } else {
        app.after = None;
    }
}

/// Saves to `path`; returns whether it worked.
fn save_to(app: &mut Writer, ui: &mut Ui<Msg>, path: PathBuf) -> bool {
    let host = app.host.clone();
    match app
        .editor
        .with_document(|d| files::save_document(&host, &path, d))
    {
        Ok(()) => {
            println!("WRITER:SAVE:PASS:{}", path.display());
            app.path = Some(path);
            app.dirty = false;
            refresh_title(app, ui);
            app.editor.focus();
            true
        }
        Err(error) => {
            println!("WRITER:SAVE:FAIL:{}", path.display());
            fail(app, "Could not save", &path, error);
            false
        }
    }
}

pub fn export(app: &mut Writer) {
    if app.dialog_open.get() {
        return;
    }
    let dialog = &app.dialogs.export;
    dialog.set_suggested_name(&names::suggested_export_name(app.path.as_deref()));
    dialog.set_initial_dir(start_dir(app));
    app.dialog_open.set(true);
    dialog.open();
}

pub fn export_chosen(app: &mut Writer, path: PathBuf) {
    app.dialog_open.set(false);
    let path = names::with_markdown_extension(path);
    let host = app.host.clone();
    match app
        .editor
        .with_document(|d| files::export_markdown(&host, &path, d))
    {
        Ok(()) => {
            println!("WRITER:EXPORT:PASS:{}", path.display());
            app.status
                .set_text(0, &format!("Exported {}", names::display_name(Some(&path))));
            app.editor.focus();
        }
        Err(error) => {
            println!("WRITER:EXPORT:FAIL:{}", path.display());
            fail(app, "Could not export", &path, error);
        }
    }
}

pub fn insert_image(app: &mut Writer) {
    if app.dialog_open.get() {
        return;
    }
    app.dialogs.image.set_initial_dir(start_dir(app));
    app.dialog_open.set(true);
    app.dialogs.image.open();
}

pub fn image_chosen(app: &mut Writer, path: PathBuf) {
    app.dialog_open.set(false);
    match files::load_image(&path) {
        Ok(image) => {
            println!("WRITER:IMAGE:PASS:{}", path.display());
            format(app, Command::InsertImage(image));
        }
        Err(error) => {
            println!("WRITER:IMAGE:FAIL:{}", path.display());
            fail(app, "Could not insert", &path, error);
        }
    }
}
