//! The bookmark editor: a small frame over the top right of the page with the name, the folder
//! (a list of the three roots) and Done, Remove and Cancel.
//!
//! xui's `Dialog` holds text and one entry only, so a form is a frame in the window's layout that
//! is shown and hidden (the way Paint does its Resize dialog). It is not modal: the page below
//! stays usable, and a click on the star again or on Cancel closes it.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, combo_box, edit, group, label, row, Handle, LayoutExt};
use xui_core::layout::Align;
use xui_core::widget::{Button, ComboBox, Edit, GroupBox, HasText};

use super::BookmarkMsg;
use crate::app::Msg;
use crate::places::{Id, BAR, OTHER, UNSORTED};

/// The folder list's rows: the three roots, then "stay where it is" for a bookmark in a folder
/// the list cannot name.
const FOLDERS: [&str; 4] = [
    "Bookmarks bar",
    "Other bookmarks",
    "Unsorted",
    "Keep in its folder",
];
const ROOT_IDS: [Id; 3] = [BAR, OTHER, UNSORTED];
const KEEP: usize = 3;

/// The size of the frame, in design units.
const WIDTH: f32 = 380.0;
const HEIGHT: f32 = 142.0;

/// The editor's widgets, bound while the window is built.
#[derive(Default)]
pub struct Handles {
    frame: Handle<GroupBox<Msg>>,
    name: Handle<Edit<Msg>>,
    folder: Handle<ComboBox<Msg>>,
    done: Handle<Button<Msg>>,
    remove: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
}

/// The frame for the window's layout, over the page.
pub fn layout(h: &Handles) -> impl LayoutExt<Msg> {
    let content = column().gap(8).padding(10).children((
        row()
            .gap(8)
            .align(Align::Center)
            .children((label("Name:").width(60), edit().bind(&h.name).fill(1)))
            .fixed(30),
        row()
            .gap(8)
            .align(Align::Center)
            .children((
                label("Folder:").width(60),
                combo_box(&FOLDERS).bind(&h.folder).fill(1),
            ))
            .fixed(30),
        row()
            .gap(8)
            .align(Align::Center)
            .children((
                button("Remove")
                    .on_click(Msg::Bookmarks(BookmarkMsg::Remove))
                    .bind(&h.remove)
                    .width(80),
                label("").fill(1),
                button("Done")
                    .on_click(Msg::Bookmarks(BookmarkMsg::Done))
                    .primary()
                    .bind(&h.done)
                    .width(80),
                button("Cancel")
                    .on_click(Msg::Bookmarks(BookmarkMsg::Cancel))
                    .bind(&h.cancel)
                    .width(80),
            ))
            .fixed(34),
    ));
    group("Bookmark", content)
        .bind(&h.frame)
        .size(WIDTH, HEIGHT)
        .align_x(Align::End)
        .align_y(Align::Start)
}

/// The editor and the bookmark it is editing.
pub struct Editor {
    frame: Rc<GroupBox<Msg>>,
    name: Rc<Edit<Msg>>,
    folder: Rc<ComboBox<Msg>>,
    done: Rc<Button<Msg>>,
    remove: Rc<Button<Msg>>,
    cancel: Rc<Button<Msg>>,
    editing: Cell<Option<Id>>,
}

impl Editor {
    pub fn new(h: &Handles) -> Editor {
        Editor {
            frame: h.frame.get(),
            name: h.name.get(),
            folder: h.folder.get(),
            done: h.done.get(),
            remove: h.remove.get(),
            cancel: h.cancel.get(),
            editing: Cell::new(None),
        }
    }

    pub fn hide(&self, ui: &Ui<Msg>) {
        self.editing.set(None);
        ui.set_visible(self.frame.id(), false);
    }

    /// The bookmark being edited, while the frame is up.
    pub fn editing(&self) -> Option<Id> {
        self.editing.get()
    }

    /// Shows the frame for bookmark `id`, titled `title`, in `parent`.
    pub fn show(&self, ui: &Ui<Msg>, id: Id, title: &str, parent: Id) {
        self.editing.set(Some(id));
        self.name.set_text(title);
        self.folder
            .select(ROOT_IDS.iter().position(|r| *r == parent).unwrap_or(KEEP));
        ui.set_visible(self.frame.id(), true);
        ui.relayout();
        self.name.focus();
    }

    /// The name typed and the root chosen (`None`: stay in the folder it is in).
    pub fn read(&self) -> (String, Option<Id>) {
        let chosen = self.folder.selected();
        (self.name.text(), ROOT_IDS.get(chosen).copied())
    }

    /// Where the frame and its buttons are, by the names a session script clicks.
    pub fn controls(&self, ui: &Ui<Msg>) -> Vec<(&'static str, xui_core::geometry::Rect)> {
        vec![
            ("bookmark_editor", ui.bounds(self.frame.id())),
            ("bookmark_name", ui.bounds(self.name.id())),
            ("bookmark_folder", ui.bounds(self.folder.id())),
            ("bookmark_done", ui.bounds(self.done.id())),
            ("bookmark_remove", ui.bounds(self.remove.id())),
            ("bookmark_cancel", ui.bounds(self.cancel.id())),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_list_names_the_roots_in_order() {
        assert_eq!(FOLDERS.len(), ROOT_IDS.len() + 1);
        assert_eq!(FOLDERS[KEEP], "Keep in its folder");
        for (name, id) in FOLDERS.iter().zip(ROOT_IDS) {
            assert_eq!(Some(*name), crate::places::model::root_title(id));
        }
    }
}
