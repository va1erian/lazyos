//! The popups a right click on the page opens: the page's own commands (Back, Forward, Reload)
//! when no link is under the pointer, and the link's commands when one is.
//!
//! A link menu replaces the page's own context menu (`Hooks::swallow_right`: the page does not see
//! the click); a click on bare page is forwarded to the page as well, as it always was.

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::widget::Menu;

use crate::address;
use crate::app::Msg;
use crate::chrome::{self, Command};
use crate::pages;

/// What the link menu offers for a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkItems {
    /// "Open link in another app": only for a scheme the engine does not load (`mailto:`, `tel:`).
    /// A web link is the page's to open; opening it in a new tab will be an item of its own
    /// (tabs come later: add it beside this one).
    pub open_in_app: bool,
}

/// The items of the link menu for `url` (what `Hover` last reported), or `None` when there is no
/// link menu: nothing is under the pointer, the link would run a script in the page
/// (`javascript:`), or it is one of the browser's own page commands, which only a click runs.
pub fn link_items(url: &str) -> Option<LinkItems> {
    let url = url.trim();
    if url.is_empty() || url.starts_with(pages::SCHEME) {
        return None;
    }
    match address::scheme(url).as_deref() {
        Some("javascript") => None,
        // No scheme: not an address anything could open.
        None => None,
        Some(_) => Some(LinkItems {
            open_in_app: !address::engine_loads(url),
        }),
    }
}

/// The right-click menus and what the last right click was on.
pub struct Context {
    page_menu: Menu<Msg>,
    link_menu: Menu<Msg>,
    link: Option<String>,
}

impl Context {
    pub fn new(ui: &Ui<Msg>) -> Context {
        Context {
            page_menu: chrome::context_menu(ui),
            link_menu: chrome::link_menu(ui),
            link: None,
        }
    }

    /// Opens the menu at the view-relative point `(x, y)` of a view placed at `origin`: the link
    /// menu when `link` has one, else the page's.
    pub fn show(
        &mut self,
        origin: Rect,
        (x, y): (i32, i32),
        (back, forward): (bool, bool),
        link: Option<String>,
    ) {
        let (x, y) = (origin.left + x, origin.top + y);
        match link.as_deref().and_then(link_items) {
            Some(items) => {
                self.link_menu
                    .set_enabled(Command::OpenLinkInApp.id(), items.open_in_app);
                self.link = link;
                self.link_menu.show_context(x, y);
            }
            None => {
                self.link = None;
                self.page_menu.set_enabled(Command::Back.id(), back);
                self.page_menu.set_enabled(Command::Forward.id(), forward);
                self.page_menu.show_context(x, y);
            }
        }
    }

    /// The link the menu was opened on.
    pub fn link(&self) -> Option<&str> {
        self.link.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(url: &str) -> Option<bool> {
        link_items(url).map(|i| i.open_in_app)
    }

    #[test]
    fn a_web_link_can_be_copied_but_is_not_handed_to_another_app() {
        assert_eq!(items("http://example.com/a"), Some(false));
        assert_eq!(items("https://example.com/?q=1#f"), Some(false));
        assert_eq!(items("HTTPS://EXAMPLE.COM/"), Some(false));
        assert_eq!(items("file:///tmp/x.html"), Some(false));
    }

    #[test]
    fn a_link_for_another_app_offers_to_open_there() {
        assert_eq!(items("mailto:a@b.test"), Some(true));
        assert_eq!(items("tel:+15551234"), Some(true));
        assert_eq!(items("magnet:?xt=urn:btih:1"), Some(true));
    }

    #[test]
    fn nothing_under_the_pointer_or_a_script_has_no_link_menu() {
        assert_eq!(items(""), None);
        assert_eq!(items("   "), None);
        assert_eq!(items("javascript:alert(1)"), None);
        assert_eq!(items("JavaScript:void(0)"), None);
        assert_eq!(items("  javascript:x"), None);
        assert_eq!(items("not a url"), None);
    }

    #[test]
    fn the_browsers_own_page_commands_have_no_link_menu() {
        assert_eq!(items("x-lazyweb:clear-history"), None);
    }
}
