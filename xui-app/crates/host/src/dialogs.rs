//! The file dialog preset: the host's file system, the start folder, "All
//! files" last on an Open dialog, and the app's own cancel message.
//!
//! One chain instead of the eight an app wrote by hand. The caller adds what
//! is its own (`on_accept`, a suggested name).

use std::path::PathBuf;

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::FileDialog;

use crate::AppHost;

/// One extension filter: a label and the extensions (without the dot) it
/// admits.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

/// An Open dialog over the host's file system, starting in the host's start
/// folder, listing `filters` and then "All files", and refusing a path that
/// does not exist. `on_cancel` is the app's message for a dismissed dialog.
pub fn open_dialog<M: 'static>(
    ui: &Ui<M>,
    host: &dyn AppHost,
    title: &str,
    filters: &[Filter<'_>],
    on_cancel: impl Fn() -> Option<M> + 'static,
) -> Result<FileDialog<M>> {
    let dialog = preset(FileDialog::open_file(ui, title)?, host, None, on_cancel);
    let dialog = with_filters(dialog.require_existing(true), filters);
    Ok(dialog.filter("All files", &[]))
}

/// A Save dialog over the host's file system. `filters` is used as given (a
/// save has one right format; the caller adds "All files" when the app
/// writes anything).
pub fn save_dialog<M: 'static>(
    ui: &Ui<M>,
    host: &dyn AppHost,
    title: &str,
    filters: &[Filter<'_>],
    on_cancel: impl Fn() -> Option<M> + 'static,
) -> Result<FileDialog<M>> {
    let dialog = preset(FileDialog::save_file(ui, title)?, host, None, on_cancel);
    Ok(with_filters(dialog, filters))
}

/// The shared part: file system, start folder (`start`, else the host's)
/// and cancel.
pub fn preset<M: 'static>(
    dialog: FileDialog<M>,
    host: &dyn AppHost,
    start: Option<PathBuf>,
    on_cancel: impl Fn() -> Option<M> + 'static,
) -> FileDialog<M> {
    dialog
        .file_system(host.file_system())
        .initial_dir(start.unwrap_or_else(|| host.start_dir()))
        .on_cancel(on_cancel)
}

fn with_filters<M: 'static>(mut dialog: FileDialog<M>, filters: &[Filter<'_>]) -> FileDialog<M> {
    for (label, extensions) in filters {
        dialog = dialog.filter(label, extensions);
    }
    dialog
}
