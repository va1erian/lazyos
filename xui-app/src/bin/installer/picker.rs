//! The Choose step's file picker: xui's portable [`FileDialog`], filtered to
//! `.lzp` packages.
//!
//! The dialog lives for the whole window, not in a screen's view, so a view
//! rebuild never destroys it while it is open. Whether it is open is shared
//! with the window's key hook ([`Picker::gate`]): the hook runs before the
//! dialog sees a key, so without the gate `Esc` would close the dialog *and*
//! cancel the wizard.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::widget::{FileDialog, StdFileSystem};

use crate::msg::Msg;
use crate::view::fail;

/// The package extension the picker shows by default (matched without regard
/// to case, so `DOOM.LZP` is listed).
const EXTENSION: &str = "lzp";

/// The window's package picker.
pub struct Picker {
    dialog: FileDialog<Msg>,
    open: Rc<Cell<bool>>,
}

impl Picker {
    /// Builds the (hidden) picker.
    pub fn build(ui: &Ui<Msg>) -> Result<Picker, String> {
        let open = Rc::new(Cell::new(false));
        let accepted = Rc::clone(&open);
        let cancelled = Rc::clone(&open);
        let dialog = FileDialog::open_file(ui, "Choose a package")
            .map_err(fail)?
            .file_system(Rc::new(StdFileSystem))
            .initial_dir(fhs::mount::ROOT)
            .filter("Packages (.lzp)", &[EXTENSION])
            .filter("All files", &[])
            .require_existing(true)
            .on_accept(move |path| {
                accepted.set(false);
                Some(Msg::Picked(path))
            })
            .on_cancel(move || {
                cancelled.set(false);
                Some(Msg::PickCancelled)
            });
        Ok(Picker { dialog, open })
    }

    /// Shows the picker in the folder of `current` (the path in the field)
    /// when that is an absolute path, else at the root. A second call while it
    /// is open does nothing.
    pub fn show(&self, current: &str) {
        if self.open.replace(true) {
            return;
        }
        let folder = Path::new(current.trim())
            .parent()
            .filter(|dir| dir.is_absolute() && dir.is_dir());
        match folder {
            Some(dir) => self.dialog.set_initial_dir(dir),
            None => self.dialog.set_initial_dir(fhs::mount::ROOT),
        }
        self.dialog.open();
    }

    /// Whether the picker is showing.
    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// The shared open flag, for the window's key hook.
    pub fn gate(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.open)
    }
}
