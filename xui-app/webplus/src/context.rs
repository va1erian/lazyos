//! The popup a right click on the page opens: the page's own commands, then
//! the ones for the link under the pointer.

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::Menu;

use crate::app::Msg;
use crate::chrome::{self, Command};

/// The right-click menu and what the last right click was on.
pub struct Context {
    menu: Menu<Msg>,
    link: Option<String>,
}

impl Context {
    pub fn new(ui: &Ui<Msg>) -> Context {
        Context {
            menu: chrome::context_menu(ui),
            link: None,
        }
    }

    /// Opens the menu at the view-relative point `(x, y)` of a view placed at
    /// `origin`, with the link commands enabled when a link is there.
    pub fn show(
        &mut self,
        origin: Rect,
        (x, y): (i32, i32),
        (back, forward): (bool, bool),
        link: Option<String>,
    ) {
        self.menu.set_enabled(Command::Back.id(), back);
        self.menu.set_enabled(Command::Forward.id(), forward);
        self.menu
            .set_enabled(Command::OpenLink.id(), link.is_some());
        self.menu
            .set_enabled(Command::CopyLink.id(), link.is_some());
        self.link = link;
        self.menu.show_context(origin.left + x, origin.top + y);
    }

    /// The link the menu was opened on.
    pub fn link(&self) -> Option<&str> {
        self.link.as_deref()
    }
}
