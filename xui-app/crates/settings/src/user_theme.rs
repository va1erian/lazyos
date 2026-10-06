//! The per-user theme seam (issue #407): a [`ConfigStore`] that redirects the
//! theme keys (`sys/ui/*`, the desktop picture included) to the caller's own
//! `user/<uid>/ui/*` and reads them back through the machine default.
//!
//! The Appearance and Windows pages then edit the user's theme without
//! knowing it: a write lands on `user/<uid>/ui/<name>`, a read shows the
//! user's value when it has one and the machine's otherwise, and "Reset to
//! defaults" deletes the user's keys, which brings the machine theme back.
//! The administrator (uid 0) has no personal theme ([`uitheme::personal`]):
//! for it the store is passed through and edits the machine default, as
//! before. Every other key goes straight through.

use std::rc::Rc;

use crate::store::{AppChoice, ConfigStore, StoreError, Value};

/// A store whose theme keys are `uid`'s own.
pub struct UserTheme {
    inner: Rc<dyn ConfigStore>,
    uid: u32,
}

impl UserTheme {
    /// `store` with the theme keys of its caller redirected, or `store`
    /// itself when the caller is the administrator or its uid is unknown.
    pub fn scoped(store: Rc<dyn ConfigStore>) -> Rc<dyn ConfigStore> {
        match store.uid().filter(|uid| uitheme::personal(*uid)) {
            Some(uid) => Rc::new(UserTheme { inner: store, uid }),
            None => store,
        }
    }

    /// The user key shadowing `key`, when `key` is a theme key.
    fn own(&self, key: &str) -> Option<String> {
        is_theme_key(key)
            .then(|| uitheme::user_key(self.uid, key))
            .flatten()
    }
}

/// The keys a user may make its own: the palette, the animations switch and
/// the desktop picture. The UI scale stays machine-wide (the compositor fixes
/// it at start-up).
fn is_theme_key(key: &str) -> bool {
    uitheme::ALL_KEYS.contains(&key) || key == uitheme::KEY_WALLPAPER
}

impl ConfigStore for UserTheme {
    fn get(&self, key: &str) -> Option<Value> {
        let machine = self.inner.get(key);
        match self.own(key) {
            Some(own) => uitheme::overlay(machine, self.inner.get(&own)),
            None => machine,
        }
    }

    fn set(&self, key: &str, value: Value) -> Result<(), StoreError> {
        match self.own(key) {
            Some(own) => self.inner.set(&own, value),
            None => self.inner.set(key, value),
        }
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        match self.own(key) {
            Some(own) => self.inner.delete(&own),
            None => self.inner.delete(key),
        }
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        self.inner.list(prefix)
    }

    fn uid(&self) -> Option<u32> {
        Some(self.uid)
    }

    fn apps(&self) -> Vec<AppChoice> {
        self.inner.apps()
    }

    fn persistent(&self) -> bool {
        self.inner.persistent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use crate::theme_ops;
    use uitheme::Mode;

    fn user_store(uid: Option<u32>) -> (Rc<MemStore>, Rc<dyn ConfigStore>) {
        let mem = Rc::new(MemStore::new());
        *mem.uid.borrow_mut() = uid;
        let scoped = UserTheme::scoped(mem.clone());
        (mem, scoped)
    }

    #[test]
    fn a_user_edits_its_own_keys_over_the_machine_default() {
        let (mem, store) = user_store(Some(1000));
        mem.set(uitheme::KEY_ACCENT, Value::U64(0x336699)).unwrap();
        theme_ops::set_mode(store.as_ref(), Mode::Light).unwrap();
        assert_eq!(
            mem.get("user/1000/ui/mode"),
            Some(Value::Str("light".into()))
        );
        assert_eq!(mem.get(uitheme::KEY_MODE), None, "machine key touched");
        let seen = theme_ops::load(store.as_ref());
        assert_eq!(seen.mode, Mode::Light);
        // A machine colour the user never set still shows through.
        assert_eq!(seen.accent, Some(0x336699));
        theme_ops::set_color(store.as_ref(), uitheme::KEY_ACCENT, Some(0xAA3232)).unwrap();
        assert_eq!(theme_ops::load(store.as_ref()).accent, Some(0xAA3232));
        assert_eq!(mem.get(uitheme::KEY_ACCENT), Some(Value::U64(0x336699)));
    }

    #[test]
    fn reset_brings_the_machine_theme_back() {
        let (mem, store) = user_store(Some(1000));
        mem.set(uitheme::KEY_MODE, Value::Str("light".into()))
            .unwrap();
        theme_ops::set_mode(store.as_ref(), Mode::Dark).unwrap();
        crate::wallpaper_ops::set(store.as_ref(), Some("/home/user/a.png")).unwrap();
        assert_eq!(theme_ops::load(store.as_ref()).mode, Mode::Dark);
        theme_ops::reset(store.as_ref()).unwrap();
        assert_eq!(theme_ops::load(store.as_ref()).mode, Mode::Light);
        assert!(mem.list("user/1000").is_empty(), "user keys left behind");
        assert_eq!(
            mem.get(uitheme::KEY_MODE),
            Some(Value::Str("light".into()))
        );
    }

    #[test]
    fn the_administrator_edits_the_machine_default() {
        let (mem, store) = user_store(Some(0));
        theme_ops::set_animations(store.as_ref(), false).unwrap();
        assert_eq!(mem.get(uitheme::KEY_ANIM), Some(Value::Bool(false)));
        assert!(mem.list("user").is_empty());
        // An unknown uid behaves the same: no personal keys are guessed.
        let (mem, store) = user_store(None);
        theme_ops::set_animations(store.as_ref(), false).unwrap();
        assert_eq!(mem.get(uitheme::KEY_ANIM), Some(Value::Bool(false)));
    }

    #[test]
    fn other_keys_pass_through() {
        let (mem, store) = user_store(Some(1000));
        store
            .set("sys/input/layout", Value::Str("fr".into()))
            .unwrap();
        store.set(uitheme::KEY_SCALE, Value::U64(2)).unwrap();
        assert_eq!(
            mem.get("sys/input/layout"),
            Some(Value::Str("fr".into()))
        );
        assert_eq!(mem.get(uitheme::KEY_SCALE), Some(Value::U64(2)));
        assert_eq!(store.uid(), Some(1000));
    }
}
