#![forbid(unsafe_code)]

//! Copy, paste and reveal: the effects [`ExplorerWindow`] runs through the
//! shell's [`Session`](crate::platform::Session).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use xui_core::app::Ui;

use super::{ExplorerWindow, Msg};
use crate::platform::Pasted;

impl ExplorerWindow {
    /// The selection as absolute paths, in view order.
    fn selected_paths(&self) -> Vec<PathBuf> {
        self.listing
            .names_of(&self.selection())
            .into_iter()
            .map(|name| self.dir.join(name))
            .collect()
    }

    /// Puts the selection on the clipboard. Does nothing when it is empty.
    pub(super) fn copy_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let text = match self.explorer.session().copy(&paths) {
            Ok(()) => format!("Copied {}", count(paths.len())),
            Err(error) => format!("Cannot copy: {error}"),
        };
        self.chrome.status.set_parts(&[&text]);
    }

    /// Copies the clipboard's files into this folder, refreshes every window
    /// showing it, and says what happened.
    ///
    /// A session that can be rebuilt on a worker thread
    /// ([`Session::detached`]) pastes there, so a large copy leaves the
    /// window painting; the answer returns as [`Msg::PasteDone`].
    pub(super) fn paste(&mut self, ui: &mut Ui<Msg>) {
        if self.pasting {
            self.chrome.status.set_parts(&["A paste is still running"]);
            return;
        }
        let Some(detached) = self.explorer.session().detached() else {
            let result = self.explorer.session().paste_into(&self.dir);
            let dir = self.dir.clone();
            return self.show_pasted(&dir, result, ui);
        };
        let dir = self.dir.clone();
        // A panic or a thread that cannot start still answers (`Failed`), so
        // the window never waits for a paste that cannot finish.
        xui_work::spawn(
            &self.paste_to,
            "files-paste",
            move || {
                let result = detached().paste_into(&dir);
                (dir, result)
            },
            |answer| answer,
        );
        self.pasting = true;
        self.chrome.status.set_parts(&["Pasting..."]);
    }

    /// The worker's answer.
    pub(super) fn paste_done(&mut self, ui: &mut Ui<Msg>) {
        for answer in self.pasted.drain() {
            self.pasting = false;
            match answer {
                Ok((dir, result)) => self.show_pasted(&dir, result, ui),
                Err(failed) => {
                    let text = format!("Cannot paste: {failed}");
                    self.chrome.status.set_parts(&[&text]);
                }
            }
        }
    }

    /// Refresh what a paste into `dir` changed, and say what it did.
    fn show_pasted(&mut self, dir: &Path, result: io::Result<Pasted>, ui: &mut Ui<Msg>) {
        if result.as_ref().is_ok_and(|pasted| pasted.copied > 0) {
            // The window may have moved on while the copy ran.
            if self.dir == dir {
                self.refresh(ui);
            }
            self.explorer.refresh_under(dir, Some(ui.window()));
        }
        self.chrome.status.set_parts(&[&paste_summary(&result)]);
    }

    /// Selects the entry called `name` (a reveal), when the folder holds it;
    /// returns whether it did.
    pub fn select_name(&mut self, name: &OsStr) -> bool {
        let rows = self.listing.indices_of(&[name.to_os_string()]);
        if rows.is_empty() {
            return false;
        }
        self.set_selection(&rows);
        self.ensure_visible(rows[0]);
        self.selection_changed();
        true
    }
}

/// "1 item" or "`n` items".
fn count(n: usize) -> String {
    if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    }
}

/// The status line after a paste.
pub(super) fn paste_summary(result: &std::io::Result<Pasted>) -> String {
    match result {
        Err(error) => format!("Cannot paste: {error}"),
        Ok(Pasted { copied: 0, failed }) if failed.is_empty() => {
            "Nothing to paste: copy files first".to_string()
        }
        Ok(Pasted { copied, failed }) if failed.is_empty() => format!("Pasted {}", count(*copied)),
        Ok(Pasted { copied, failed }) => {
            let reasons: Vec<String> = failed
                .iter()
                .map(|(path, why)| {
                    let name = path.file_name().unwrap_or(path.as_os_str());
                    format!("{}: {why}", name.to_string_lossy())
                })
                .collect();
            format!(
                "Pasted {}; could not paste {}",
                count(*copied),
                reasons.join("; ")
            )
        }
    }
}
