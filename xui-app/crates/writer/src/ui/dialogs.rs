#![forbid(unsafe_code)]

//! The file pickers and modal dialogs, built once and held for the window's
//! lifetime (as the Installer's picker and the Editor's Save As are), so a
//! rebuild never destroys one while it is open.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Dialog, FileDialog, TaskDialog, TaskDialogIcon};

use crate::app::{Dialogs, Msg};
use crate::names::{EXTENSION, IMAGE_EXTENSIONS, MARKDOWN_EXTENSION, TEXT_EXTENSION};
use xui_host::AppHost;
use xui_host::dialogs::{open_dialog, preset, save_dialog};
use xui_host::error::ErrorDialog;

fn cancelled() -> Option<Msg> {
    Some(Msg::PickerClosed)
}

/// Builds every dialog, hidden.
pub fn build(ui: &Ui<Msg>, host: &dyn AppHost) -> Result<Dialogs> {
    let open = open_dialog(
        ui,
        host,
        "Open",
        &[
            ("LazyWriter documents (.lzw)", &[EXTENSION]),
            ("Text (.txt)", &[TEXT_EXTENSION]),
        ],
        cancelled,
    )?
    .on_accept(|p| Some(Msg::OpenChosen(p)));
    let save = save_dialog(
        ui,
        host,
        "Save As",
        &[("LazyWriter documents (.lzw)", &[EXTENSION])],
        cancelled,
    )?
    .on_accept(|p| Some(Msg::SaveChosen(p)));
    let export = save_dialog(
        ui,
        host,
        "Export as Markdown",
        &[("Markdown (.md)", &[MARKDOWN_EXTENSION])],
        cancelled,
    )?
    .on_accept(|p| Some(Msg::ExportChosen(p)));
    // The image picker offers pictures only, so it is not the preset's Open
    // (which ends with "All files").
    let image = preset(
        FileDialog::open_file(ui, "Insert image")?,
        host,
        None,
        cancelled,
    )
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
    let printing = TaskDialog::new(
        ui,
        "Stop printing?",
        "LazyWriter is still preparing the document for the printer. If you quit now, \
         nothing is printed.",
    )?
    .icon(TaskDialogIcon::Warning)
    .command("Stop printing")?
    .on_action(|action| Some(Msg::StopPrinting(action)));
    let link = Dialog::prompt(
        ui,
        "Link",
        "The address the selected text links to. Leave it empty to remove the link.",
        "https://",
    )?
    .accept_label("Link")
    .on_action(|action| Some(Msg::LinkChosen(action)));
    let message = ErrorDialog::new(ui, "LazyWriter", || Some(Msg::MessageClosed))?;

    Ok(Dialogs {
        page: super::page_menu::build(ui),
        table: super::table_menu::build(ui),
        open,
        save,
        export,
        image,
        unsaved,
        printing,
        link,
        message,
    })
}
