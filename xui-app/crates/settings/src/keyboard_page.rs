//! The Keyboard page: the layout `inputd` follows ([`keyboard`]) and a field
//! to try it in.
//!
//! The layout is a machine setting (`sys/input/layout`), so every write asks
//! an administrator. Moving through the list only chooses; **Use this
//! layout** writes the choice, once. The page names as active only what the
//! store holds: after a cancelled or refused prompt the list goes back to the
//! stored layout.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::keyboard;
use crate::store::ConfigStore;

/// The widest the list and the test field get.
const FIELD_W: i32 = 300;

/// Messages the Keyboard page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardMsg {
    /// A layout row was selected (nothing is written).
    Select(usize),
    /// Write the selected layout.
    Apply,
}

/// The page's widgets.
pub struct KeyboardPage {
    layout: Rc<ListView<Msg>>,
    hint: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl KeyboardPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<KeyboardPage> {
        let (layout, hint) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(8).children((
                label("Keyboard layout"),
                choice_list(&keyboard::LAYOUTS.map(|(_, name)| name))
                    .on_select(|i| Msg::Keyboard(KeyboardMsg::Select(i)))
                    .bind(&layout)
                    .height(60)
                    .max_width(FIELD_W),
                row().child(button("Use this layout").on_click(Msg::Keyboard(KeyboardMsg::Apply))),
                label("").bind(&hint),
                label("Try it"),
                edit()
                    .placeholder("Type here to test the layout")
                    .max_width(FIELD_W),
            )),
        )?;
        Ok(KeyboardPage {
            layout: layout.get(),
            hint: hint.get(),
            _mounted: mounted,
        })
    }

    /// Select and name the stored layout (no event is raised).
    pub fn load(&self, store: &dyn ConfigStore) {
        match keyboard::current(store) {
            Some(index) => {
                self.layout.select(Some(index));
                if let Some((_, name)) = keyboard::LAYOUTS.get(index) {
                    self.hint.set_text(&format!("Active: {name}"));
                }
            }
            None => {
                self.layout.select(None);
                self.hint
                    .set_text("No layout chosen yet (using the boot default).");
            }
        }
    }

    /// Handle one message; returns the status line text.
    pub fn update(&self, msg: KeyboardMsg, store: &dyn ConfigStore) -> String {
        match msg {
            KeyboardMsg::Select(row) => {
                // Only chosen, not written: the row the message names.
                self.layout
                    .select(Some(row).filter(|row| *row < keyboard::LAYOUTS.len()));
                String::new()
            }
            KeyboardMsg::Apply => {
                let text = apply(store, self.layout.selected());
                // Show what is stored, whatever the outcome.
                self.load(store);
                text
            }
        }
    }
}

/// Write layout `selected` unless it is already the stored one; the status
/// text.
pub fn apply(store: &dyn ConfigStore, selected: Option<usize>) -> String {
    let Some(index) = selected else {
        return String::from("Select a layout first.");
    };
    if keyboard::current(store) == Some(index) {
        return String::from("That layout is already in use.");
    }
    match keyboard::set(store, index) {
        Ok(()) => {
            println!("SETTINGS:LAYOUT:APPLIED:{}", keyboard::LAYOUTS[index].0);
            String::from("Keyboard layout changed.")
        }
        Err(error) => format!("Keyboard layout not changed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemStore, Value};

    #[test]
    fn apply_writes_once_and_only_a_change() {
        let store = MemStore::new();
        assert_eq!(apply(&store, None), "Select a layout first.");
        assert!(store.is_empty());
        assert_eq!(apply(&store, Some(1)), "Keyboard layout changed.");
        assert_eq!(keyboard::current(&store), Some(1));
        // The same layout again is not a second request.
        *store.fail_writes.borrow_mut() = Some("would prompt".into());
        assert_eq!(apply(&store, Some(1)), "That layout is already in use.");
    }

    #[test]
    fn a_refused_apply_keeps_the_stored_layout() {
        let store = MemStore::new();
        store
            .set(keyboard::KEY_LAYOUT, Value::Str("us".into()))
            .unwrap();
        *store.fail_writes.borrow_mut() = Some("cancelled".into());
        let text = apply(&store, Some(1));
        assert!(text.contains("cancelled"), "{text}");
        assert_eq!(keyboard::current(&store), Some(0));
    }
}
