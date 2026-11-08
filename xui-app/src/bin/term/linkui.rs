//! Links under the pointer: with Ctrl held the one under it is underlined and
//! the pointer is a hand; Ctrl+click opens it. A plain click never opens
//! anything. An OSC 8 link whose text is not its target asks first (the bar
//! shows the target); the menu's Open Link always asks.

use lazyterm::LinkHit;
use xui_core::app::Ui;
use xui_core::backend::Cursor;

use super::geometry::{point_at, Edge};
use super::helpers::sanitize_url;
use super::msg::Msg;
use super::view::first_row;
use super::Terminal;

impl Terminal {
    /// The link under node pixel (`x`, `y`), if the pixel is on the grid.
    pub(super) fn link_at_pixel(&self, x: i32, y: i32) -> Option<LinkHit> {
        let layout = self.layout.get()?;
        let term = self.term.borrow();
        let (at, edge) = point_at(&layout, &term, self.offset.get(), x, y);
        if edge != Edge::Inside {
            return None;
        }
        term.link_hit_at(at.line, at.col)
    }

    /// The pointer moved (or Ctrl changed): underline the link under it while
    /// Ctrl is held, and show the hand over it.
    pub(super) fn link_hover(&mut self, ui: &mut Ui<Msg>, x: i32, y: i32, ctrl: bool) {
        let hit = if ctrl && !self.modal() { self.link_at_pixel(x, y) } else { None };
        if hit == self.hover {
            return;
        }
        self.chrome.borrow_mut().hover = hit.as_ref().map(|h| (h.start, h.end));
        ui.set_cursor(self.root.id(), if hit.is_some() { Cursor::Hand } else { Cursor::Text });
        self.hover = hit;
        self.redraw(ui);
    }

    /// Ctrl+click at (`x`, `y`): open the link there. Returns whether there
    /// was one.
    pub(super) fn ctrl_click(&mut self, ui: &mut Ui<Msg>, x: i32, y: i32) -> bool {
        let Some(hit) = self.link_at_pixel(x, y) else {
            return false;
        };
        if hit.explicit && hit.text != hit.uri {
            // What the screen shows is not where it goes: show the target.
            self.ask_open(ui, &hit.uri);
        } else {
            self.open_url(&hit.uri);
        }
        true
    }

    /// Right click: the context menu, with the link under the pointer (its
    /// target only if it is one that may open).
    pub(super) fn menu_at_pointer(&mut self, ui: &mut Ui<Msg>, x: i32, y: i32) {
        let link = self.link_at_pixel(x, y).and_then(|h| sanitize_url(&h.uri));
        self.open_menu(ui, (x, y), link);
    }

    /// Shift+F10 / the Menu key: the context menu at the text cursor.
    pub(super) fn menu_at_cursor(&mut self, ui: &mut Ui<Msg>) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let (at, link) = {
            let term = self.term.borrow();
            let (row, col) = term.cursor();
            let visible = layout.visible.clamp(1, term.rows());
            let first = first_row(row, visible, term.rows());
            let slot = (row + self.offset.get()).saturating_sub(first);
            let at = (layout.pad + col as i32 * layout.cell_w, layout.pad + (slot as i32 + 1) * layout.line_h);
            let link = term.link_hit_at(term.screen_top() + row as u64, col).and_then(|h| sanitize_url(&h.uri));
            (at, link)
        };
        self.open_menu(ui, at, link);
    }
}
