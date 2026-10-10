//! The popup a right click on the page opens: the page's own commands, then
//! the ones for the link or picture under the pointer.

use lazyweb::address;
use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::Menu;

use crate::app::Msg;
use crate::chrome::{self, Command};

/// The right-click menu and what the last right click was on.
pub struct Context {
    menu: Menu<Msg>,
    link: Option<String>,
    image: Option<String>,
}

impl Context {
    pub fn new(ui: &Ui<Msg>) -> Context {
        Context {
            menu: chrome::context_menu(ui),
            link: None,
            image: None,
        }
    }

    /// Opens the menu at the view-relative point `(x, y)` of a view placed at
    /// `origin`, with the link and picture commands enabled for what is
    /// there. Only network pictures can be saved: a `data:` picture has no
    /// download to start.
    pub fn show(
        &mut self,
        origin: Rect,
        (x, y): (i32, i32),
        (back, forward): (bool, bool),
        link: Option<String>,
        image: Option<String>,
    ) {
        let saveable = image.as_deref().is_some_and(address::is_network);
        self.menu.set_enabled(Command::Back.id(), back);
        self.menu.set_enabled(Command::Forward.id(), forward);
        self.menu
            .set_enabled(Command::OpenLink.id(), link.is_some());
        self.menu
            .set_enabled(Command::CopyLink.id(), link.is_some());
        self.menu.set_enabled(Command::SaveImage.id(), saveable);
        self.link = link;
        self.image = image;
        self.menu.show_context(origin.left + x, origin.top + y);
    }

    /// The link the menu was opened on.
    pub fn link(&self) -> Option<&str> {
        self.link.as_deref()
    }

    /// The picture the menu was opened on.
    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }
}
