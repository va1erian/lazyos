#![forbid(unsafe_code)]

//! The file pickers and modal dialogs, built once and held for the window's
//! lifetime (as the Installer's picker and the Editor's Save As are), so a
//! rebuild never destroys one while it is open.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Dialog, FileDialog, TaskDialog, TaskDialogIcon};

use crate::app::{Dialogs, Msg};
use crate::host::Host;
use crate::names::{EXTENSION, IMAGE_EXTENSIONS, MARKDOWN_EXTENSION, TEXT_EXTENSION};

/// A picker over the host's filesystem, starting in its start folder.
fn picker(dialog: FileDialog<Msg>, host: &Host) -> FileDialog<Msg> {
    dialog
        .file_system(Rc::clone(&host.file_system))
        .initial_dir(host.start_dir.clone())
        .on_cancel(|| Some(Msg::PickerClosed))
}

/// Builds every dialog, hidden.
pub fn build(ui: &Ui<Msg>, host: &Host) -> Result<Dialogs> {
    let open = picker(FileDialog::open_file(ui, "Open")?, host)
        .require_existing(true)
        .filter("LazyWriter documents (.lzw)", &[EXTENSION])
        .filter("Text (.txt)", &[TEXT_EXTENSION])
        .filter("All files", &[])
        .on_accept(|p| Some(Msg::OpenChosen(p)));
    let save = picker(FileDialog::save_file(ui, "Save As")?, host)
        .filter("LazyWriter documents (.lzw)", &[EXTENSION])
        .on_accept(|p| Some(Msg::SaveChosen(p)));
    let export = picker(FileDialog::save_file(ui, "Export as Markdown")?, host)
        .filter("Markdown (.md)", &[MARKDOWN_EXTENSION])
        .on_accept(|p| Some(Msg::ExportChosen(p)));
    let image = picker(FileDialog::open_file(ui, "Insert image")?, host)
        .require_existing(true)
        .filter("Images (.png, .jpg, .jpeg)", &IMAGE_EXTENSIONS)
        .on_accept(|p| Some(Msg::ImageChosen(p)));

    let unsaved = TaskDialog::new(
        ui,
        "Save changes?",
        "This document has unsaved changes. Save them before you go on?",
    )?
    .icon(TaskDialogIcon::Warning)
    .command("Save")?
    .command("Discard")?
    .on_action(|action| Some(Msg::Unsaved(action)));
    let link = Dialog::prompt(
        ui,
        "Link",
        "The address the selected text links to. Leave it empty to remove the link.",
        "https://",
    )?
    .accept_label("Link")
    .on_action(|action| Some(Msg::LinkChosen(action)));
    let message = Dialog::message(ui, "LazyWriter", "")?.on_action(|_| Some(Msg::MessageClosed));

    Ok(Dialogs {
        page: super::page_menu::build(ui),
        table: super::table_menu::build(ui),
        open,
        save,
        export,
        image,
        unsaved,
        link,
        message,
    })
}
