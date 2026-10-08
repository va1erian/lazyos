#![forbid(unsafe_code)]

//! Moving around: opening a folder in place, Back, Forward, Up, the address
//! bar, refreshing (and climbing out of a folder that was deleted), opening
//! a file or a new window, and the item context menu.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::geometry::Point;
use xui_core::widget::{HasText, MenuId};

use super::chrome::{
    MENU_COPY, MENU_DELETE, MENU_OPEN, MENU_OPEN_WINDOW, MENU_PASTE, MENU_PROPERTIES, MENU_REFRESH,
};
use super::{ExplorerWindow, Msg};
use crate::model::{Listing, resolve_address, title};
use crate::platform::Kind;

impl ExplorerWindow {
    /// Shows `listing` (of a folder the window now shows) with the entries
    /// named `select` selected, and updates the title, the address bar, the
    /// navigation buttons and the status bar.
    pub(super) fn show_listing(
        &mut self,
        ui: &mut Ui<Msg>,
        listing: Rc<Listing>,
        select: &[OsString],
    ) {
        self.dir = listing.dir.clone();
        self.listing = listing;
        self.set_models(ui);
        let rows = self.listing.indices_of(select);
        self.set_selection(&rows);
        if let Some(first) = rows.first() {
            self.ensure_visible(*first);
        }
        self.title = title(&self.dir);
        ui.set_window_title(&self.title);
        self.explorer.publish_title(ui.window(), &self.title);
        self.restore_address();
        self.chrome.back.set_enabled(self.history.can_back());
        self.chrome.forward.set_enabled(self.history.can_forward());
        self.chrome.up.set_enabled(self.dir.parent().is_some());
        self.selected = self.selection();
        self.previous.clear();
        self.publish_view();
        self.update_status();
    }

    /// Opens folder `dir` in this window, remembering the current one for
    /// Back. A folder that cannot be listed is not entered: the window stays
    /// and says why. Returns whether the window moved.
    pub(super) fn navigate(&mut self, ui: &mut Ui<Msg>, dir: PathBuf, select: &[OsString]) -> bool {
        if dir == self.dir {
            self.refresh(ui);
            return true;
        }
        let listing = Listing::load(self.explorer.platform(), &dir, self.options.sort);
        if let Some(error) = &listing.error {
            self.chrome
                .status
                .set_parts(&[&format!("Cannot open {}: {error}", dir.display())]);
            return false;
        }
        self.history.visit(&self.dir);
        self.show_listing(ui, Rc::new(listing), select);
        true
    }

    /// Back to the previous folder.
    pub(super) fn back(&mut self, ui: &mut Ui<Msg>) {
        if let Some(dir) = self.history.back(&self.dir) {
            self.revisit(ui, dir);
        }
    }

    /// Forward to the next folder.
    pub(super) fn forward(&mut self, ui: &mut Ui<Msg>) {
        if let Some(dir) = self.history.forward(&self.dir) {
            self.revisit(ui, dir);
        }
    }

    /// Shows a folder from the history (which has already moved), climbing
    /// to its nearest existing ancestor if it is gone.
    fn revisit(&mut self, ui: &mut Ui<Msg>, dir: PathBuf) {
        let dir = self.nearest_existing(dir);
        let listing = Listing::load(self.explorer.platform(), &dir, self.options.sort);
        self.show_listing(ui, Rc::new(listing), &[]);
    }

    /// Up to the parent folder, with the folder we came from selected.
    pub(super) fn up(&mut self, ui: &mut Ui<Msg>) {
        let (Some(parent), Some(name)) = (self.dir.parent(), self.dir.file_name()) else {
            return;
        };
        let (parent, name) = (parent.to_path_buf(), name.to_os_string());
        self.navigate(ui, parent, &[name]);
    }

    /// Opens what the address bar holds: a folder in place, a file with its
    /// application. Anything else is reported and left in the bar to fix.
    pub(super) fn go_address(&mut self, ui: &mut Ui<Msg>) {
        let text = self.chrome.address.text();
        let home = self.explorer.home();
        let Some(path) = resolve_address(&text, &self.dir, home.as_deref()) else {
            self.restore_address();
            return;
        };
        match self.explorer.platform().metadata(&path) {
            Ok(meta) if meta.kind == Kind::Dir => {
                if self.navigate(ui, path, &[]) {
                    ui.focus(self.active_view());
                }
            }
            Ok(_) => {
                self.open_file(&path);
                self.restore_address();
            }
            Err(error) => self
                .chrome
                .status
                .set_parts(&[&format!("Cannot find {}: {error}", path.display())]),
        }
    }

    /// Empties the address bar and gives it the keyboard, ready for a path.
    pub(super) fn focus_address(&mut self) {
        self.chrome.address.set_text("");
        self.address_dirty = true;
        self.chrome.address.focus();
    }

    /// Puts the current folder back in the address bar, discarding an edit.
    pub(super) fn restore_address(&mut self) {
        self.chrome
            .address
            .set_text(&self.dir.display().to_string());
        self.address_dirty = false;
    }

    /// Re-lists the folder, keeps the selection where the items still exist,
    /// and updates the status bar. A folder that no longer exists makes the
    /// window climb to the nearest one that does.
    pub(super) fn refresh(&mut self, ui: &mut Ui<Msg>) {
        let dir = self.nearest_existing(self.dir.clone());
        let keep = if dir == self.dir {
            self.listing.names_of(&self.selection())
        } else {
            Vec::new()
        };
        let listing = Listing::load(self.explorer.platform(), &dir, self.options.sort);
        self.show_listing(ui, Rc::new(listing), &keep);
    }

    /// `dir`, or its nearest ancestor the platform does not report missing.
    fn nearest_existing(&self, mut dir: PathBuf) -> PathBuf {
        let missing = |dir: &Path| matches!(self.explorer.platform().metadata(dir), Err(error) if error.kind() == ErrorKind::NotFound);
        while missing(&dir) && dir.pop() {}
        dir
    }

    /// Opens a folder in place or hands a file to the launcher.
    pub(super) fn activate(&mut self, index: usize, ui: &mut Ui<Msg>) {
        let Some(entry) = self.listing.entries.get(index).cloned() else {
            return;
        };
        let path = self.dir.join(&entry.name);
        match entry.kind {
            Kind::Dir => {
                self.navigate(ui, path, &[]);
            }
            Kind::File | Kind::Symlink => self.open_file(&path),
        }
    }

    /// Hands a file to the launcher, reporting a refusal in the status bar.
    fn open_file(&self, path: &Path) {
        if let Err(error) = self.explorer.launcher().open(path) {
            let name = title(path);
            self.chrome
                .status
                .set_parts(&[&format!("Cannot open {name}: {error}")]);
        }
    }

    /// Opens a folder entry in a new window with this window's view and sort.
    pub(super) fn open_in_new_window(&mut self, index: usize, ui: &mut Ui<Msg>) {
        let Some(entry) = self.listing.entries.get(index) else {
            return;
        };
        if entry.kind == Kind::Dir {
            let path = self.dir.join(&entry.name);
            // As in place, a folder that cannot be listed is not opened.
            if let Err(error) = self.explorer.platform().list(&path) {
                let text = format!("Cannot open {}: {error}", entry.display);
                self.chrome.status.set_parts(&[&text]);
            } else if !self.explorer.open_window(ui, path, self.options) {
                self.chrome
                    .status
                    .set_parts(&[&format!("Cannot open a window for {}", entry.display)]);
            }
        }
    }

    /// Shows the context menu for the right-clicked item (which the view has
    /// made part of the selection), or for the folder on empty space.
    pub(super) fn context(&mut self, item: Option<usize>, at: Point, ui: &mut Ui<Msg>) {
        self.context_item = item;
        let has_item = item.is_some();
        let is_dir = item
            .and_then(|index| self.listing.entries.get(index))
            .is_some_and(|entry| entry.kind == Kind::Dir);
        self.menu.set_enabled(MENU_OPEN, has_item);
        self.menu.set_enabled(MENU_OPEN_WINDOW, is_dir);
        self.menu.set_enabled(MENU_COPY, has_item);
        self.menu.set_enabled(MENU_PASTE, true);
        self.menu.set_enabled(MENU_DELETE, has_item);
        self.menu.set_enabled(MENU_PROPERTIES, true);
        self.menu.set_enabled(MENU_REFRESH, true);
        // The right click may have changed the selection (XP selects the
        // unselected tile under the pointer), so refresh the status line.
        self.selection_changed();
        let bounds = ui.bounds(self.active_view());
        self.menu
            .show_context(bounds.left + at.x, bounds.top + at.y);
    }

    /// Runs a context-menu command against the right-clicked item (the
    /// folder itself on empty space).
    pub(super) fn menu_command(&mut self, id: MenuId, ui: &mut Ui<Msg>) {
        let item = self.context_item;
        if id == MENU_OPEN {
            if let Some(index) = item {
                self.activate(index, ui);
            }
        } else if id == MENU_OPEN_WINDOW {
            if let Some(index) = item {
                self.open_in_new_window(index, ui);
            }
        } else if id == MENU_COPY {
            self.copy_selection();
        } else if id == MENU_PASTE {
            self.paste(ui);
        } else if id == MENU_DELETE {
            self.begin_delete(ui);
        } else if id == MENU_PROPERTIES {
            if item.is_some() {
                self.show_selection_properties(ui);
            } else {
                self.show_folder_properties(ui);
            }
        } else if id == MENU_REFRESH {
            self.refresh(ui);
        }
    }
}
