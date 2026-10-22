//! The window's side of bookmarks (the store is [`crate::places`]): the star in the toolbar, the
//! small editor over the page, and the list the Bookmarks menu shows.
//!
//! Serial evidence, after `WEBPLUS:BOOKMARK:` and naming no page: `ADD` when the star bookmarked
//! the page, `EDIT` when the editor's Done changed it, `REMOVE` when it was removed.
//!
//! What xui's menu bar cannot do is change its entries after it is built, so the bar's Bookmarks
//! menu holds the commands (bookmark this page, edit it, show the list) and "Show Bookmarks" opens
//! the list itself as a popup built from the store at that moment ([`menu`]).

pub mod editor;
pub mod menu;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use xui_core::widget::ToggleButton;

use crate::address;
use crate::places::Bookmarks;

pub use editor::Editor;

/// What the bookmarks UI raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookmarkMsg {
    /// The star, or Ctrl+D: bookmark the page, or edit its bookmark.
    Star,
    /// A row of the list was chosen (an index into the list's targets).
    Pick(usize),
    /// The editor's Done, Remove or Cancel.
    Done,
    Remove,
    Cancel,
}

/// The widgets the window builds for bookmarks.
#[derive(Default)]
pub struct Widgets {
    pub star: xui_core::arrange::Handle<ToggleButton<crate::app::Msg>>,
    pub editor: editor::Handles,
}

/// Whether the page at `url` can be bookmarked: a web page. The browser's own pages (`data:` URLs
/// the engine shows, `about:`), files and other schemes are not kept.
pub fn bookmarkable(url: &str) -> bool {
    address::is_network(url)
}

/// What the window keeps for bookmarks: the store and the widgets that show it.
pub struct State {
    pub store: Bookmarks,
    pub star: Rc<ToggleButton<crate::app::Msg>>,
    pub editor: Editor,
    /// The list popup last built, kept until the next (a popup's windows belong to it), and the
    /// bookmark each of its rows opens.
    pub list: RefCell<Option<xui_core::widget::Menu<crate::app::Msg>>>,
    pub targets: RefCell<Vec<crate::places::Id>>,
    /// The page and store version the star was last drawn for, and whether it showed "bookmarked".
    pub shown: RefCell<(String, u64, bool)>,
}

impl State {
    /// The bookmarks kept in the profile folder (`bookmarks.log`, beside the history), or in memory
    /// without one.
    pub fn open(profile: Option<&Path>, widgets: &Widgets) -> State {
        let store = match profile {
            Some(dir) => Bookmarks::open(dir.join(crate::BOOKMARKS_FILE)),
            None => Bookmarks::in_memory(),
        };
        State {
            store,
            star: widgets.star.get(),
            editor: Editor::new(&widgets.editor),
            list: RefCell::new(None),
            targets: RefCell::new(Vec::new()),
            shown: RefCell::new((String::new(), u64::MAX, false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_are_bookmarked() {
        assert!(bookmarkable("https://example.com/a?b#c"));
        assert!(bookmarkable("http://example.com/"));
        assert!(!bookmarkable("data:text/html,<p>history"));
        assert!(!bookmarkable("about:blank"));
        assert!(!bookmarkable("file:///tmp/x.html"));
        assert!(!bookmarkable("javascript:alert(1)"));
        assert!(!bookmarkable("mailto:a@b.test"));
        assert!(!bookmarkable(""));
    }
}
