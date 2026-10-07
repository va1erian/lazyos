//! The per-user theme seam (issue #407): a [`ConfigStore`] that redirects the
//! theme keys (`sys/ui/*`, the desktop picture included) to the caller's own
//! `user/<uid>/ui/*` and reads them back through the machine default.
//!
//! The Appearance and Windows pages then edit the user's theme without
//! knowing it: a write lands on `user/<uid>/ui/<name>`, a read shows the
//! user's value when it has one and the machine's otherwise, and "Reset to
//! defaults" deletes the user's keys, which brings the machine theme back.
//! Every account but uid 0 has a personal theme, administrators included
//! ([`uitheme::personal`]; uid 0, which no session runs as, edits the
//! machine keys directly). The machine default (`sys/ui/*`, what the login
//! screen and every account without its own value show) changes only
//! through [`make_default`], whose writes go to `elevd` and so ask an
//! administrator (docs/accounts-plan.md U2). Every other key goes straight
//! through.

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

/// What [`make_default`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Published {
    /// Machine keys written (each one an administrator approved).
    pub written: usize,
    /// The user's desktop picture was left out: it lives in the user's
    /// home, which nobody else can read.
    pub kept_picture: bool,
}

/// Make `uid`'s theme the machine default: every theme key the user set to
/// something other than the machine value is written to `sys/ui/*` through
/// `machine` (one request, so one administrator approval, per key that
/// differs), then the user's own copy is dropped so the account follows the
/// default it just set. A desktop picture under `/home` is not published.
/// The first refusal stops it: what was written stays written, and the
/// user's keys are kept so nothing they chose is lost.
pub fn make_default(machine: &dyn ConfigStore, uid: u32) -> Result<Published, StoreError> {
    let mut published = Published::default();
    let mut adopted = Vec::new();
    for key in uitheme::ALL_KEYS
        .into_iter()
        .chain([uitheme::KEY_WALLPAPER])
    {
        let Some(own) = uitheme::user_key(uid, key) else {
            continue;
        };
        let Some(value) = machine.get(&own) else {
            continue;
        };
        if key == uitheme::KEY_WALLPAPER && in_home(&value) {
            published.kept_picture = true;
            continue;
        }
        if machine.get(key).as_ref() != Some(&value) {
            machine.set(key, value)?;
            published.written += 1;
        }
        adopted.push(own);
    }
    for own in adopted {
        machine.delete(&own)?;
    }
    Ok(published)
}

/// Whether a desktop picture value names a file under `/home`.
fn in_home(value: &Value) -> bool {
    uitheme::wallpaper_path(Some(value)).is_some_and(|path| {
        path.strip_prefix(fhs::mount::HOME)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
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
        assert_eq!(mem.get(uitheme::KEY_MODE), Some(Value::Str("light".into())));
    }

    #[test]
    fn make_default_publishes_what_differs_and_follows_it() {
        let (mem, store) = user_store(Some(1001));
        mem.set(uitheme::KEY_MODE, Value::Str("light".into()))
            .unwrap();
        mem.set(uitheme::KEY_ACCENT, Value::U64(0x336699)).unwrap();
        // The user's mode equals the machine's: not a write.
        store
            .set(uitheme::KEY_MODE, Value::Str("light".into()))
            .unwrap();
        theme_ops::set_color(store.as_ref(), uitheme::KEY_ACCENT, Some(0xAA3232)).unwrap();
        theme_ops::set_animations(store.as_ref(), false).unwrap();
        crate::wallpaper_ops::set(store.as_ref(), Some("/home/admin/me.png")).unwrap();
        let before = theme_ops::load(store.as_ref());
        let published = make_default(mem.as_ref(), 1001).unwrap();
        assert_eq!(
            published,
            Published {
                written: 2,
                kept_picture: true
            }
        );
        assert_eq!(mem.get(uitheme::KEY_ACCENT), Some(Value::U64(0xAA3232)));
        assert_eq!(mem.get(uitheme::KEY_ANIM), Some(Value::Bool(false)));
        assert_eq!(
            mem.get(uitheme::KEY_WALLPAPER),
            None,
            "a home picture leaked"
        );
        // The account now follows the default, and looks the same.
        assert_eq!(mem.list("user/1001"), ["user/1001/ui/wallpaper"]);
        assert_eq!(theme_ops::load(store.as_ref()), before);
    }

    #[test]
    fn a_refused_make_default_keeps_the_user_theme() {
        let (mem, store) = user_store(Some(1000));
        theme_ops::set_animations(store.as_ref(), false).unwrap();
        *mem.fail_writes.borrow_mut() = Some("cancelled".into());
        assert_eq!(make_default(mem.as_ref(), 1000), Err("cancelled".into()));
        *mem.fail_writes.borrow_mut() = None;
        assert_eq!(mem.get("user/1000/ui/anim"), Some(Value::Bool(false)));
        assert_eq!(mem.get(uitheme::KEY_ANIM), None);
    }

    #[test]
    fn an_administrator_has_a_personal_theme_too() {
        let (mem, store) = user_store(Some(1001));
        theme_ops::set_animations(store.as_ref(), false).unwrap();
        assert_eq!(mem.get(uitheme::KEY_ANIM), None, "the machine key moved");
        assert_eq!(mem.get("user/1001/ui/anim"), Some(Value::Bool(false)));
    }

    #[test]
    fn uid_0_edits_the_machine_default() {
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
        assert_eq!(mem.get("sys/input/layout"), Some(Value::Str("fr".into())));
        assert_eq!(mem.get(uitheme::KEY_SCALE), Some(Value::U64(2)));
        assert_eq!(store.uid(), Some(1000));
    }
}
