//! What the star, Ctrl+D, the Bookmarks menu and the bookmark editor do (`crate::bookmarks`).

use xui_core::app::Ui;
use xui_core::widget::Placeable;

use super::{now, Browser, Msg};
use crate::bookmarks::{self, menu, BookmarkMsg};
use crate::chrome::Command;
use crate::places::{Kind, UNSORTED};

impl Browser {
    /// Puts away the frames that cover the page (the bookmark editor, the report form) and the
    /// suggestion dropdown, so that only the one about to open is up.
    pub(super) fn hide_overlays(&mut self, ui: &Ui<Msg>) {
        self.bookmarks.editor.hide(ui);
        self.report.form.hide(ui);
        self.hide_suggest(ui);
    }

    /// The page the window shows, as the engine last said.
    fn current_url(&self) -> &str {
        if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        }
    }

    pub(super) fn bookmark_msg(&mut self, ui: &Ui<Msg>, msg: BookmarkMsg) {
        self.forget_star();
        match msg {
            BookmarkMsg::Star => self.star(ui),
            BookmarkMsg::Pick(n) => self.open_bookmark(n),
            BookmarkMsg::Done => self.editor_done(ui),
            BookmarkMsg::Remove => self.editor_remove(ui),
            BookmarkMsg::Cancel => {
                self.bookmarks.editor.hide(ui);
                self.page.focus();
            }
        }
        self.update_star();
    }

    pub(super) fn bookmark_command(&mut self, ui: &Ui<Msg>, command: Command) {
        self.forget_star();
        match command {
            Command::BookmarkPage => self.star(ui),
            Command::EditBookmark => self.edit_bookmark(ui),
            Command::ShowBookmarks => self.show_bookmarks(ui),
            _ => {}
        }
        self.update_star();
    }

    /// The star: bookmark the page into Unsorted, or, when it is bookmarked already, edit it.
    fn star(&mut self, ui: &Ui<Msg>) {
        self.hide_overlays(ui);
        let url = self.current_url().to_owned();
        if !bookmarks::bookmarkable(&url) {
            self.set_status("Only web pages can be bookmarked");
            return;
        }
        if self.bookmarks.store.find_url(&url).is_some() {
            self.edit_bookmark(ui);
            return;
        }
        let title = self.state.title.clone();
        let result = self.bookmarks.store.add_link(UNSORTED, &title, &url, now());
        if result.is_ok() {
            crate::marker("BOOKMARK:ADD");
        }
        self.set_status(&match result {
            Ok(_) => String::from("Bookmarked in Unsorted. Click the star again to edit it."),
            Err(e) => format!("Not bookmarked: {e}"),
        });
    }

    /// Opens the editor for the page's bookmark.
    fn edit_bookmark(&mut self, ui: &Ui<Msg>) {
        self.hide_overlays(ui);
        let url = self.current_url().to_owned();
        let Some(entry) = self.bookmarks.store.find_url(&url).cloned() else {
            self.set_status("This page is not bookmarked");
            return;
        };
        let parent = entry.parent;
        self.bookmarks
            .editor
            .show(ui, entry.id, &entry.title, parent);
        self.announce(ui);
    }

    fn editor_done(&mut self, ui: &Ui<Msg>) {
        let Some(id) = self.bookmarks.editor.editing() else {
            return;
        };
        let (name, folder) = self.bookmarks.editor.read();
        self.bookmarks.editor.hide(ui);
        self.page.focus();
        let Some(entry) = self.bookmarks.store.get(id).cloned() else {
            return;
        };
        let store = &mut self.bookmarks.store;
        let mut result = store.edit(id, &name, &entry.url, now());
        if let Some(folder) = folder.filter(|f| *f != entry.parent) {
            let moved = store.move_to(id, folder, usize::MAX, now());
            if result.is_ok() {
                result = moved;
            }
        }
        crate::marker("BOOKMARK:EDIT");
        match result {
            Ok(()) => self.set_status("Bookmark saved"),
            Err(e) => self.set_status(&format!("Bookmark not saved: {e}")),
        }
    }

    fn editor_remove(&mut self, ui: &Ui<Msg>) {
        let Some(id) = self.bookmarks.editor.editing() else {
            return;
        };
        self.bookmarks.editor.hide(ui);
        self.page.focus();
        match self.bookmarks.store.remove(id) {
            Ok(_) => {
                crate::marker("BOOKMARK:REMOVE");
                self.set_status("Bookmark removed");
            }
            Err(e) => self.set_status(&format!("Bookmark not removed: {e}")),
        }
    }

    /// Shows the list of bookmarks as a popup under the menu bar.
    fn show_bookmarks(&mut self, ui: &Ui<Msg>) {
        let (popup, targets) = menu::build(ui, &menu::plan(&self.bookmarks.store));
        let bar = ui.bounds(self.menu.id());
        popup.show_context(bar.left + 8, bar.bottom);
        *self.bookmarks.targets.borrow_mut() = targets;
        // The previous popup is dropped now that its replacement is up.
        *self.bookmarks.list.borrow_mut() = Some(popup);
    }

    /// A row of the list was chosen: open that bookmark, as a click on a link does.
    fn open_bookmark(&mut self, n: usize) {
        let id = self.bookmarks.targets.borrow().get(n).copied();
        let entry = id.and_then(|id| self.bookmarks.store.get(id));
        // Only web pages: a bookmark must never start another program (`open` hands such schemes
        // over), whatever a hand-edited file holds.
        if let Some(entry) =
            entry.filter(|e| e.kind == Kind::Link && bookmarks::bookmarkable(&e.url))
        {
            let url = entry.url.clone();
            self.open(&url);
        }
    }

    /// The star was clicked, so it flipped by itself: draw it again whatever the store says.
    fn forget_star(&self) {
        self.bookmarks.shown.borrow_mut().1 = u64::MAX;
    }

    /// Draws the star as "bookmarked" or not for the page on show, and enables Edit in the menu.
    /// Cheap when nothing changed: the page and the store's version are compared first.
    pub(super) fn update_star(&self) {
        let url = self.current_url();
        let version = self.bookmarks.store.version();
        {
            let shown = self.bookmarks.shown.borrow();
            if shown.0 == url && shown.1 == version {
                return;
            }
        }
        let marked = bookmarks::bookmarkable(url) && self.bookmarks.store.find_url(url).is_some();
        *self.bookmarks.shown.borrow_mut() = (url.to_owned(), version, marked);
        self.bookmarks.star.set_checked(marked);
        let _ = self.bookmarks.star.set_tooltip(if marked {
            "Edit this bookmark (Ctrl+D)"
        } else {
            "Bookmark this page (Ctrl+D)"
        });
        self.menu.set_enabled(Command::EditBookmark.id(), marked);
        self.menu
            .set_enabled(Command::BookmarkPage.id(), bookmarks::bookmarkable(url));
    }
}
