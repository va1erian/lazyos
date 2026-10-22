//! The window's side of the address bar's suggestions (`crate::suggest` ranks them and keeps the
//! rows): typing fills the dropdown from the history, Up and Down move through it, Enter opens
//! what is highlighted (or the typed text), Escape puts it away, Shift+Delete forgets the
//! highlighted page and End finishes the best match's host. Nothing here asks the network.
//!
//! Serial evidence: `WEBPLUS:SUGGEST:SHOW:<rows>` when the dropdown has rows, `SUGGEST:OPEN:visit|search`
//! when one is opened, `SUGGEST:REMOVE` when a page is forgotten, `SUGGEST:HIDE`. In a UI-probe image
//! `UI:WIDGET` names the box `suggest` and its rows `suggest:<n>`.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::widget::{HasText, Placeable};
use xui_core::{Key, Modifiers};

use super::{now, Browser, Msg};
use crate::address;
use crate::chrome::Widgets;
use crate::suggest::dropdown::{Act, Dropdown, Row};
use crate::suggest::widget::SuggestBox;
use crate::suggest::{Index, MAX_ROWS};

/// The dropdown's widget and state.
pub(super) struct Panel {
    widget: Rc<SuggestBox>,
    /// Whether rows are showing: the window's key mapper reads it to claim the arrow keys.
    open: Rc<Cell<bool>>,
    state: Dropdown,
}

impl Panel {
    pub(super) fn new(widgets: &Widgets, open: Rc<Cell<bool>>) -> Panel {
        Panel {
            widget: widgets.suggest.get(),
            open,
            state: Dropdown::default(),
        }
    }

    /// The row to open instead of the typed text, when the person moved onto one.
    pub(super) fn chosen(&self) -> Option<Row> {
        self.state.chosen().cloned()
    }

    pub(super) fn is_open(&self) -> bool {
        self.state.is_open()
    }
}

/// The keys the dropdown claims from the address field while it has rows. Enter is the field's
/// (`Msg::Go` reads the highlighted row); the field gets every one of these keys too, but none does
/// anything to a single line of text.
pub fn key(key: Key, mods: Modifiers) -> Option<Msg> {
    let act = |act| Some(Msg::Suggest(act));
    match key {
        Key::DOWN => act(Act::Move(1)),
        Key::UP => act(Act::Move(-1)),
        Key::ESCAPE => act(Act::Close),
        Key::END => act(Act::Accept),
        Key::DELETE if mods.shift => act(Act::Remove),
        _ => None,
    }
}

/// Messages after which the dropdown is put away (the person is going elsewhere).
pub(super) fn closes(msg: &Msg) -> bool {
    matches!(
        msg,
        Msg::Go
            | Msg::FocusAddress
            | Msg::RestoreAddress
            | Msg::PageClicked
            | Msg::PageMenu(..)
            | Msg::Menu(_)
            | Msg::ReloadOrStop
    )
}

impl Browser {
    /// The address field's text changed by typing: rank the history for it.
    pub(super) fn address_edited(&mut self, ui: &Ui<Msg>) {
        let text = self.address.text();
        let index = Index::from_visits(self.visits.entries());
        let matches = index.rank(&text, now(), MAX_ROWS);
        self.suggest.state.set(&text, &matches);
        self.show_suggestions(ui);
    }

    /// Puts the state's rows in the box.
    fn show_suggestions(&mut self, ui: &Ui<Msg>) {
        let state = &self.suggest.state;
        self.suggest.widget.set(state.labels(), state.selected());
        let was = self.suggest.open.replace(state.is_open());
        ui.relayout();
        if state.is_open() {
            crate::marker(&format!("SUGGEST:SHOW:{}", state.rows().len()));
            self.announce_suggestions(ui);
        } else if was {
            crate::marker("SUGGEST:HIDE");
        }
    }

    /// Where the box and its rows are, for a session script.
    fn announce_suggestions(&self, ui: &Ui<Msg>) {
        let bounds = ui.bounds(self.suggest.widget.id());
        (self.host.probe)(&self.caption, "suggest", bounds);
        for n in 0..self.suggest.state.rows().len() {
            let rect = self.suggest.widget.row_rect(n, bounds, ui.dpi());
            (self.host.probe)(&self.caption, &format!("suggest:{n}"), rect);
        }
    }

    /// Puts the dropdown away.
    pub(super) fn hide_suggest(&mut self, ui: &Ui<Msg>) {
        if self.suggest.is_open() {
            self.suggest.state.clear();
            self.show_suggestions(ui);
        }
    }

    /// The person acted on the dropdown.
    pub(super) fn suggest_act(&mut self, ui: &Ui<Msg>, act: Act) {
        match act {
            Act::Move(delta) => {
                self.suggest.state.step(delta);
                self.suggest.widget.select(self.suggest.state.selected());
            }
            Act::Close => self.hide_suggest(ui),
            Act::Accept => {
                if let Some(text) = self.suggest.state.completion().map(str::to_owned) {
                    self.address.set_text(&text);
                    self.address_edited(ui);
                }
            }
            Act::Remove => {
                let Some(url) = self.suggest.state.remove_chosen() else {
                    return;
                };
                crate::marker("SUGGEST:REMOVE");
                if let Err(e) = self.visits.remove_url(&url) {
                    self.set_status(&format!("History not saved: {e}"));
                }
                // The page may be recorded again by its next visit.
                self.recorded = None;
                self.show_suggestions(ui);
            }
            Act::Pick(n) => {
                self.suggest.state.select(n);
                let row = self.suggest.chosen();
                self.hide_suggest(ui);
                if let Some(row) = row {
                    self.open_row(row);
                }
            }
        }
    }

    /// Opens a row of the dropdown: the page, or the search for the typed words.
    pub(super) fn open_row(&mut self, row: Row) {
        let url = match row {
            Row::Visit { url, .. } => {
                crate::marker("SUGGEST:OPEN:visit");
                url
            }
            Row::Search(query) => {
                crate::marker("SUGGEST:OPEN:search");
                address::search_url(&self.host.prefs.search, &query)
            }
        };
        self.address.set_text(&url);
        self.open(&url);
        self.page.focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(key: Key, mods: Modifiers) -> Option<Act> {
        match super::key(key, mods) {
            Some(Msg::Suggest(act)) => Some(act),
            _ => None,
        }
    }

    #[test]
    fn the_dropdown_claims_arrows_escape_end_and_shift_delete_only() {
        let none = Modifiers::NONE;
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(act(Key::DOWN, none), Some(Act::Move(1)));
        assert_eq!(act(Key::UP, none), Some(Act::Move(-1)));
        assert_eq!(act(Key::ESCAPE, none), Some(Act::Close));
        assert_eq!(act(Key::END, none), Some(Act::Accept));
        assert_eq!(act(Key::DELETE, shift), Some(Act::Remove));
        assert_eq!(act(Key::DELETE, none), None, "plain Delete edits the text");
        assert_eq!(act(Key::RETURN, none), None, "Enter is the field's");
        assert_eq!(act(Key::A, none), None);
    }
}
