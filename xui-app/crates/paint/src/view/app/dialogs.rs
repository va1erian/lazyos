#![forbid(unsafe_code)]

//! The modal dialogs Paint opens: the Open/Save As pickers and the Resize
//! prompt.
//!
//! [`Msg`] is `Copy`, so a dialog's payload (a path, the typed size) never rides
//! in the message: the dialog's mapper parks it in a shared slot and raises a
//! payload-free message, and `update` takes it out of the slot.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Dialog, DialogAction, FileDialog, FileSystem};

use crate::view::Msg;

/// A one-value mailbox between a dialog's mapper and `update`.
pub(super) type Slot<T> = Rc<RefCell<Option<T>>>;

/// The Open and Save As pickers over one [`FileSystem`].
pub(super) struct FileDialogs {
    open: FileDialog<Msg>,
    save: FileDialog<Msg>,
    fs: Rc<dyn FileSystem>,
    chosen: Slot<PathBuf>,
}

impl FileDialogs {
    pub(super) fn new(ui: &Ui<Msg>, fs: Rc<dyn FileSystem>) -> Result<FileDialogs> {
        let chosen: Slot<PathBuf> = Rc::new(RefCell::new(None));
        let open = FileDialog::open_file(ui, "Open")?
            .file_system(Rc::clone(&fs))
            .require_existing(true)
            .filter("PNG images", &["png"])
            .filter("All files", &[])
            .on_accept(accept(&chosen, Msg::OpenChosen))
            .on_cancel(|| Some(Msg::DialogClosed));
        let save = FileDialog::save_file(ui, "Save As")?
            .file_system(Rc::clone(&fs))
            .filter("PNG images", &["png"])
            .filter("All files", &[])
            .on_accept(accept(&chosen, Msg::SaveChosen))
            .on_cancel(|| Some(Msg::DialogClosed));
        Ok(FileDialogs {
            open,
            save,
            fs,
            chosen,
        })
    }

    /// Whether either picker is showing.
    pub(super) fn is_open(&self) -> bool {
        self.open.is_open() || self.save.is_open()
    }

    pub(super) fn set_start_dir(&self, dir: &Path) {
        self.open.set_initial_dir(dir);
        self.save.set_initial_dir(dir);
    }

    pub(super) fn show_open(&self, dir: Option<&Path>) {
        if let Some(dir) = dir {
            self.open.set_initial_dir(dir);
        }
        self.open.open();
    }

    pub(super) fn show_save(&self, name: &str, dir: Option<&Path>) {
        self.save.set_suggested_name(name);
        if let Some(dir) = dir {
            self.save.set_initial_dir(dir);
        }
        self.save.open();
    }

    /// The path the last accepted picker returned, once.
    pub(super) fn take_chosen(&self) -> Option<PathBuf> {
        self.chosen.borrow_mut().take()
    }

    pub(super) fn exists(&self, path: &Path) -> bool {
        self.fs.exists(path)
    }
}

/// A picker's accept mapper: park the path, raise `msg`.
fn accept(chosen: &Slot<PathBuf>, msg: Msg) -> impl Fn(PathBuf) -> Option<Msg> + 'static {
    let chosen = Rc::clone(chosen);
    move |path| {
        *chosen.borrow_mut() = Some(path);
        Some(msg)
    }
}

/// The Resize prompt. `Dialog::prompt` seeds its field once at build time, so a
/// fresh dialog is built per open to show the current size; the old one (closed
/// by then) is dropped, which removes its nodes.
#[derive(Default)]
pub(super) struct ResizePrompt {
    dialog: Option<Dialog<Msg>>,
    text: Slot<String>,
}

impl ResizePrompt {
    pub(super) fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(Dialog::is_open)
    }

    pub(super) fn show(&mut self, ui: &Ui<Msg>, (width, height): (u32, u32)) -> Result<()> {
        let text = Rc::clone(&self.text);
        let dialog = Dialog::prompt(
            ui,
            "Resize canvas",
            "New size as WIDTHxHEIGHT (1-1024 pixels)",
            &format!("{width}x{height}"),
        )?
        .on_action(move |action| match action {
            DialogAction::Accept(entered) => {
                *text.borrow_mut() = Some(entered);
                Some(Msg::ResizeChosen)
            }
            DialogAction::Cancel => Some(Msg::DialogClosed),
        });
        dialog.open();
        self.dialog = Some(dialog);
        Ok(())
    }

    /// The text the last accepted prompt returned, once.
    pub(super) fn take_text(&mut self) -> Option<String> {
        self.text.borrow_mut().take()
    }
}
