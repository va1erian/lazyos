//! The Keyboard page: the layout `inputd` follows ([`keyboard`]) and a field
//! to try it in.
//!
//! The layout is the account's own: through the per-user store
//! ([`crate::user_theme`]) **Use this layout** writes
//! `user/<uid>/input/layout`, once, and asks nobody. **Make it the default
//! for everyone** writes the machine layout (`sys/input/layout`: the login
//! screen, the console, every account without its own), so it asks an
//! administrator, and then drops the account's own copy so it follows the
//! default it set. Moving through the list only chooses. The page names as
//! active only what the store holds: after a cancelled or refused prompt the
//! list goes back to the stored layout.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::keyboard;
use crate::store::ConfigStore;
use inputmap::session_layout::user_layout_key;

/// The widest the list and the test field get.
const FIELD_W: i32 = 300;

/// Messages the Keyboard page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardMsg {
    /// A layout row was selected (nothing is written).
    Select(usize),
    /// Write the selected layout as the account's own.
    Apply,
    /// Write the selected layout as the machine default.
    MakeDefault,
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
                row().gap(8).children((
                    button("Use this layout").on_click(Msg::Keyboard(KeyboardMsg::Apply)),
                    button("Make it the default for everyone")
                        .on_click(Msg::Keyboard(KeyboardMsg::MakeDefault)),
                )),
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

    /// Select and name the stored layout (no event is raised). `store` is
    /// the account's view, `machine` the machine's.
    pub fn load(&self, store: &dyn ConfigStore, machine: &dyn ConfigStore) {
        let current = keyboard::current(store);
        self.layout.select(current);
        let default = keyboard::current(machine).map_or("the boot default", keyboard::name);
        self.hint.set_text(&match current.map(keyboard::name) {
            Some(name) => format!("Active: {name}. Default for everyone: {default}."),
            None => format!("No layout chosen yet (using {default})."),
        });
    }

    /// The layout row chosen in the list, not yet written.
    pub fn selected(&self) -> Option<usize> {
        self.layout.selected()
    }

    /// Handle one message; returns the status line text.
    pub fn update(
        &self,
        msg: KeyboardMsg,
        store: &dyn ConfigStore,
        machine: &dyn ConfigStore,
    ) -> String {
        match msg {
            KeyboardMsg::Select(row) => {
                // Only chosen, not written: the row the message names.
                self.layout
                    .select(Some(row).filter(|row| *row < keyboard::LAYOUTS.len()));
                String::new()
            }
            KeyboardMsg::Apply | KeyboardMsg::MakeDefault => {
                let text = if msg == KeyboardMsg::Apply {
                    apply(store, self.layout.selected())
                } else {
                    make_default(store, machine, self.layout.selected())
                };
                // Show what is stored, whatever the outcome.
                self.load(store, machine);
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

/// Write layout `selected` as the machine default through `machine` (one
/// administrator approval, only when it differs), then drop the account's own
/// choice from `store` so it follows that default; the status text. A
/// refusal keeps the account's own layout.
pub fn make_default(
    store: &dyn ConfigStore,
    machine: &dyn ConfigStore,
    selected: Option<usize>,
) -> String {
    let Some(index) = selected else {
        return String::from("Select a layout first.");
    };
    if keyboard::current(machine) != Some(index) {
        if let Err(error) = keyboard::set(machine, index) {
            return format!("The default keyboard layout was not changed: {error}");
        }
        println!("SETTINGS:LAYOUT:DEFAULT:{}", keyboard::LAYOUTS[index].0);
    }
    // uid 0 (and an unknown uid) has no layout of its own: `store` is the
    // machine's, and the key just written must stay.
    if store.uid().and_then(user_layout_key).is_some() {
        if let Err(error) = store.delete(keyboard::KEY_LAYOUT) {
            return format!("The default changed, but your own layout stays: {error}");
        }
    }
    format!("{} is now the default for everyone.", keyboard::name(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemStore, Value};
    use crate::user_theme::UserTheme;
    use std::rc::Rc;

    /// The machine's store and an account's view of it.
    fn account(uid: u32) -> (Rc<MemStore>, Rc<dyn ConfigStore>) {
        let mem = Rc::new(MemStore::new());
        *mem.uid.borrow_mut() = Some(uid);
        let own = UserTheme::scoped(mem.clone());
        (mem, own)
    }

    fn stored(mem: &MemStore, key: &str) -> Option<Value> {
        mem.get(key)
    }

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

    #[test]
    fn a_user_layout_is_its_own_and_asks_nobody() {
        let (mem, own) = account(1000);
        mem.set(keyboard::KEY_LAYOUT, Value::Str("us".into()))
            .unwrap();
        assert_eq!(apply(own.as_ref(), Some(1)), "Keyboard layout changed.");
        assert_eq!(
            stored(&mem, "user/1000/input/layout"),
            Some(Value::Str("fr".into()))
        );
        assert_eq!(
            stored(&mem, keyboard::KEY_LAYOUT),
            Some(Value::Str("us".into()))
        );
        assert_eq!(keyboard::current(own.as_ref()), Some(1));
    }

    #[test]
    fn make_default_writes_the_machine_layout_and_follows_it() {
        let (mem, own) = account(1000);
        apply(own.as_ref(), Some(1));
        let text = make_default(own.as_ref(), mem.as_ref(), Some(1));
        assert_eq!(text, "Français (AZERTY) is now the default for everyone.");
        assert_eq!(
            stored(&mem, keyboard::KEY_LAYOUT),
            Some(Value::Str("fr".into()))
        );
        assert_eq!(
            stored(&mem, "user/1000/input/layout"),
            None,
            "own copy kept"
        );
        assert_eq!(keyboard::current(own.as_ref()), Some(1));
    }

    #[test]
    fn a_refused_make_default_keeps_the_users_layout() {
        let (mem, own) = account(1000);
        apply(own.as_ref(), Some(1));
        *mem.fail_writes.borrow_mut() = Some("cancelled".into());
        let text = make_default(own.as_ref(), mem.as_ref(), Some(1));
        assert!(text.contains("cancelled"), "{text}");
        assert_eq!(stored(&mem, keyboard::KEY_LAYOUT), None);
        assert_eq!(
            stored(&mem, "user/1000/input/layout"),
            Some(Value::Str("fr".into()))
        );
    }

    #[test]
    fn uid_0_make_default_keeps_the_key_it_wrote() {
        let (mem, own) = account(0);
        make_default(own.as_ref(), mem.as_ref(), Some(1));
        assert_eq!(
            stored(&mem, keyboard::KEY_LAYOUT),
            Some(Value::Str("fr".into()))
        );
        assert_eq!(
            make_default(own.as_ref(), mem.as_ref(), None),
            "Select a layout first."
        );
    }
}
