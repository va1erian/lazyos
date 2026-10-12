#![forbid(unsafe_code)]

//! The modal dialogs Paint opens: the Open/Save As pickers, the About box and
//! the oversized-paste question.
//!
//! [`Msg`] is `Copy`, so a dialog's payload (a path, the typed size) never rides
//! in the message: the dialog's mapper parks it in a shared slot and raises a
//! payload-free message, and `update` takes it out of the slot.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{
    Dialog, FileDialog, FileSystem, TaskDialog, TaskDialogAction, TaskDialogIcon,
};

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
            .filter("Pictures", &["png", "jpg", "jpeg", "bmp", "ora"])
            .filter("PNG images", &["png"])
            .filter("JPEG images", &["jpg", "jpeg"])
            .filter("Bitmaps", &["bmp"])
            .filter("OpenRaster images", &["ora"])
            .filter("All files", &[])
            .on_accept(accept(&chosen, Msg::OpenChosen))
            .on_cancel(|| Some(Msg::DialogClosed));
        let save = FileDialog::save_file(ui, "Save As")?
            .file_system(Rc::clone(&fs))
            .filter("OpenRaster images (layers)", &["ora"])
            .filter("PNG images", &["png"])
            .filter("Bitmaps", &["bmp"])
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

    /// Hides both pickers without an answer.
    pub(super) fn close(&self) {
        self.open.close();
        self.save.close();
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

/// The About box: one message dialog, built when first shown.
#[derive(Default)]
pub(super) struct AboutBox {
    dialog: Option<Dialog<Msg>>,
}

impl AboutBox {
    pub(super) fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(Dialog::is_open)
    }

    /// Hides the box.
    pub(super) fn close(&self) {
        if let Some(dialog) = &self.dialog {
            dialog.close();
        }
    }

    pub(super) fn show(&mut self, ui: &Ui<Msg>) -> Result<()> {
        let dialog = match self.dialog.take() {
            Some(dialog) => dialog,
            None => Dialog::message(
                ui,
                "About Paint",
                "Paint, an image editor for LazyOS, built on xui.",
            )?
            .on_action(|_| Some(Msg::DialogClosed)),
        };
        dialog.open();
        self.dialog = Some(dialog);
        Ok(())
    }
}

/// The answer to "the pasted picture is larger than the canvas".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PasteSize {
    /// Grow the canvas to hold the picture.
    Expand,
    /// Keep the canvas; the picture hangs over the edge.
    Keep,
}

/// The oversized-paste question: expand the canvas (command 0), keep its size
/// (command 1) or cancel the paste. Built per open so the message can name the
/// two sizes.
#[derive(Default)]
pub(super) struct PasteSizePrompt {
    dialog: Option<TaskDialog<Msg>>,
}

impl PasteSizePrompt {
    pub(super) fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(TaskDialog::is_open)
    }

    /// Hides the card without an answer.
    pub(super) fn close(&self) {
        if let Some(dialog) = &self.dialog {
            dialog.close();
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &Ui<Msg>,
        picture: (u32, u32),
        canvas: (u32, u32),
    ) -> Result<()> {
        let text = format!(
            "The picture ({} x {}) is larger than the canvas ({} x {}). Expand the canvas to fit it?",
            picture.0, picture.1, canvas.0, canvas.1
        );
        let dialog = TaskDialog::new(ui, "Paste", &text)?
            .icon(TaskDialogIcon::Info)
            .command("Expand canvas")?
            .command("Keep size")?
            .on_action(|action| match action {
                TaskDialogAction::Command(0) => Some(Msg::PasteExpand),
                TaskDialogAction::Command(_) => Some(Msg::PasteKeep),
                TaskDialogAction::Cancel => Some(Msg::PasteCancel),
            });
        dialog.open();
        self.dialog = Some(dialog);
        Ok(())
    }
}
