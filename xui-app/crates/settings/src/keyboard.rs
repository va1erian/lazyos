//! Keyboard layout: which layouts exist and where the choice is stored.
//!
//! [`KEY_LAYOUT`] is the machine default `inputd` follows. Through the
//! per-user store ([`crate::user_theme`]) the same key is the account's own
//! `user/<uid>/input/layout`, which `xuid` hands `inputd` for the session;
//! both apply live, so this module only reads and writes the key.

use crate::store::{ConfigStore, StoreError, Value};

/// The machine layout's confd key (an account's own shadows it).
pub const KEY_LAYOUT: &str = inputmap::LAYOUT_KEY;

/// `(confd value, display name)`; the value is what `inputd` matches on.
pub const LAYOUTS: [(&str, &str); 2] = [("us", "English (US)"), ("fr", "Français (AZERTY)")];

/// Index into [`LAYOUTS`] of the stored layout, `None` when unset or unknown.
pub fn current(store: &dyn ConfigStore) -> Option<usize> {
    match store.get(KEY_LAYOUT)? {
        Value::Str(name) => LAYOUTS.iter().position(|(value, _)| *value == name),
        _ => None,
    }
}

/// The display name of `LAYOUTS[index]` (`""` when out of range).
pub fn name(index: usize) -> &'static str {
    LAYOUTS.get(index).map_or("", |(_, name)| name)
}

/// Store `LAYOUTS[index]`; an out-of-range index is refused.
pub fn set(store: &dyn ConfigStore, index: usize) -> Result<(), StoreError> {
    let (value, _) = LAYOUTS
        .get(index)
        .ok_or_else(|| String::from("unknown keyboard layout"))?;
    store.set(KEY_LAYOUT, Value::Str((*value).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    #[test]
    fn unset_reads_as_none_and_set_round_trips() {
        let store = MemStore::new();
        assert_eq!(current(&store), None);
        set(&store, 1).unwrap();
        assert_eq!(current(&store), Some(1));
        assert_eq!(store.get(KEY_LAYOUT), Some(Value::Str("fr".into())));
    }

    #[test]
    fn out_of_range_and_garbage_are_rejected() {
        let store = MemStore::new();
        assert!(set(&store, 2).is_err());
        assert!(store.is_empty());
        store.set(KEY_LAYOUT, Value::Str("klingon".into())).unwrap();
        assert_eq!(current(&store), None);
        store.set(KEY_LAYOUT, Value::Bool(true)).unwrap();
        assert_eq!(current(&store), None);
    }

    #[test]
    fn write_failure_is_reported() {
        let store = MemStore::new();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert_eq!(set(&store, 0), Err("denied".into()));
    }
}
