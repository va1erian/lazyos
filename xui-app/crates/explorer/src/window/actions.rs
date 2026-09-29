#![forbid(unsafe_code)]

//! Deletion and the Properties dialog: the effects [`ExplorerWindow`] runs from
//! `update`.

use std::ffi::OsString;
use std::path::PathBuf;

use xui_core::app::Ui;
use xui_core::widget::{Dialog, TaskDialog, TaskDialogAction, TaskDialogIcon};

use super::{ExplorerWindow, Msg};
use crate::model::{Entry, Listing, deletion_refused, describe};
use crate::platform::Kind;

impl ExplorerWindow {
    /// Opens the delete confirmation for the current selection, naming what
    /// will go. Does nothing when the selection is empty.
    pub(super) fn begin_delete(&mut self, ui: &mut Ui<Msg>) {
        let selection = self.view.selection();
        if selection.is_empty() {
            return;
        }
        let selected: Vec<Entry> = self
            .listing
            .selected(&selection)
            .into_iter()
            .cloned()
            .collect();
        self.pending_delete = self.listing.names_of(&selection);
        let prompt = delete_prompt(&selected);
        let dialog = TaskDialog::new(ui, "Delete", &prompt)
            .expect("the delete dialog built")
            .icon(TaskDialogIcon::Warning)
            .command("Delete")
            .expect("the delete command built")
            .on_action(|action| Some(Msg::Confirm(action)));
        self.confirm = Some(dialog);
        if let Some(dialog) = &self.confirm {
            dialog.open();
        }
    }

    /// Acts on the confirmation. The dialog is dropped before the filesystem is
    /// touched, so no widget borrow is held across the delete.
    pub(super) fn resolve_delete(&mut self, action: TaskDialogAction, ui: &mut Ui<Msg>) {
        let names = std::mem::take(&mut self.pending_delete);
        self.confirm = None;
        if action == TaskDialogAction::Command(0) {
            self.perform_delete(&names, ui);
        }
    }

    /// Deletes each still-present target, collects failures, closes the windows
    /// below any deleted folder, refreshes the parent windows and finally shows
    /// what could not be deleted.
    ///
    /// Targets are re-resolved by name against a freshly read listing, so a
    /// confirm opened before a change deletes only what really exists now and
    /// quietly skips something that already vanished.
    fn perform_delete(&mut self, names: &[OsString], ui: &mut Ui<Msg>) {
        let current = Listing::load(self.explorer.platform(), &self.dir);
        let mut failures: Vec<String> = Vec::new();
        let mut deleted_dirs: Vec<PathBuf> = Vec::new();
        for name in names {
            let Some(entry) = current
                .entries
                .iter()
                .find(|entry| &entry.name == name)
                .cloned()
            else {
                // It vanished between the confirm and now: already gone.
                continue;
            };
            let path = self.dir.join(&entry.name);
            if let Some(reason) = deletion_refused(&path) {
                failures.push(format!("{}: {reason}", entry.display));
                continue;
            }
            let recursive = entry.kind == Kind::Dir;
            match self.explorer.platform().remove(&path, recursive) {
                Ok(()) => {
                    if recursive {
                        deleted_dirs.push(path);
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", entry.display)),
            }
        }

        // Close descendants and refresh siblings with no registry borrow held.
        for dir in deleted_dirs {
            self.explorer.close_under(&dir);
        }
        self.refresh(ui);
        self.explorer
            .refresh_windows_showing(&self.dir, ui.window());
        if !failures.is_empty() {
            self.status
                .set_parts(&[&format!("Could not delete: {}", failures.join("; "))]);
        }
    }

    /// Shows the first selected item's properties. A folder reports its direct
    /// entry count, never a recursive size.
    pub(super) fn show_properties(&mut self, ui: &mut Ui<Msg>) {
        let Some(index) = self.view.selection().first().copied() else {
            return;
        };
        let Some(entry) = self.listing.entries.get(index).cloned() else {
            return;
        };
        let path = self.dir.join(&entry.name);
        let meta = match self.explorer.platform().metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                self.status
                    .set_parts(&[&format!("Cannot read {}: {error}", entry.display)]);
                return;
            }
        };
        let rows = describe(&meta);
        let title = format!("Properties — {}", entry.display);
        let message = rows
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let dialog = Dialog::message(ui, &title, &message)
            .expect("the properties dialog built")
            .on_action(|_| Some(Msg::PropertiesClosed));
        self.properties = Some(dialog);
        if let Some(dialog) = &self.properties {
            dialog.open();
        }
    }
}

/// The confirmation text: names a single item, counts several, and always says
/// a folder's contents go with it.
pub(super) fn delete_prompt(selected: &[Entry]) -> String {
    match selected {
        [] => "Nothing to delete.".to_string(),
        [entry] => match entry.kind {
            Kind::Dir => format!(
                "Delete the folder \"{}\" and everything inside it?",
                entry.display
            ),
            Kind::File | Kind::Symlink => {
                format!("Delete \"{}\"? This cannot be undone.", entry.display)
            }
        },
        many => format!(
            "Delete these {} items? Folders and everything inside them will be deleted.",
            many.len()
        ),
    }
}
