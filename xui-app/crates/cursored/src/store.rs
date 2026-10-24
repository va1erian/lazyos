//! Where an applied theme lives: the `ui/cursor/theme` key of `confd`.
//!
//! The theme is the key's value (a `.lcur` text, at most `confd`'s value
//! size), shadowed per user like the other `ui` keys, so `xuid` follows it
//! live through the subscription it already holds and no file path is ever
//! read. An empty value is "the built-in shapes".

use confd::Value;
use cursorgfx::{format, Theme};

/// What the editor needs from `confd`; the bin implements it over the real
/// service, tests over memory.
pub trait ThemeStore {
    /// The uid this app runs as.
    fn uid(&self) -> Option<u32>;
    fn get(&self, key: &str) -> Option<Value>;
    fn set(&self, key: &str, value: Value) -> Result<(), String>;
}

/// The key the theme of `uid` is written to: its own `user/<uid>/ui/...`
/// copy, or the machine key for uid 0.
pub fn key(uid: u32) -> String {
    uitheme::user_key(uid, uitheme::KEY_CURSOR_THEME)
        .unwrap_or_else(|| uitheme::KEY_CURSOR_THEME.to_owned())
}

/// Why a theme cannot be applied.
pub fn check(theme: &Theme) -> Result<String, String> {
    if !cursorgfx::theme::valid_name(&theme.name) {
        return Err("The name must be 1-32 lower-case letters, digits or dashes.".into());
    }
    let text = format::serialize(theme);
    if text.len() > confd::MAX_VALUE_LEN {
        return Err(format!(
            "The theme is {} bytes; at most {} fit. Remove some points.",
            text.len(),
            confd::MAX_VALUE_LEN
        ));
    }
    match format::parse(&text) {
        Ok(read) if read == *theme => Ok(text),
        Ok(_) => Err("The theme does not survive saving; check every shape.".into()),
        Err(error) => Err(format!("Line {}: {}", error.line, error.what)),
    }
}

/// Write `theme` so `xuid` draws the pointer from it.
pub fn apply(store: &dyn ThemeStore, theme: &Theme) -> Result<(), String> {
    let text = check(theme)?;
    let uid = store.uid().ok_or("The user could not be read.")?;
    store.set(&key(uid), Value::Str(text))
}

/// Go back to the built-in shapes (an empty value shadows a machine theme).
pub fn revert(store: &dyn ThemeStore) -> Result<(), String> {
    let uid = store.uid().ok_or("The user could not be read.")?;
    store.set(&key(uid), Value::Str(String::new()))
}

/// The theme in effect for this user, when one is set and valid.
pub fn current(store: &dyn ThemeStore) -> Option<Theme> {
    let uid = store.uid()?;
    let own = store.get(&key(uid));
    let machine = store.get(uitheme::KEY_CURSOR_THEME);
    let text = uitheme::cursor_theme_from_value(uitheme::overlay(machine, own).as_ref())?;
    format::parse(&text).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    pub struct Mem {
        pub uid: Option<u32>,
        pub map: RefCell<HashMap<String, Value>>,
    }

    impl ThemeStore for Mem {
        fn uid(&self) -> Option<u32> {
            self.uid
        }
        fn get(&self, key: &str) -> Option<Value> {
            self.map.borrow().get(key).cloned()
        }
        fn set(&self, key: &str, value: Value) -> Result<(), String> {
            self.map.borrow_mut().insert(key.into(), value);
            Ok(())
        }
    }

    fn mem(uid: u32) -> Mem {
        Mem {
            uid: Some(uid),
            ..Mem::default()
        }
    }

    #[test]
    fn apply_writes_the_users_key_and_xuid_reads_it_back() {
        let store = mem(1000);
        let theme = Theme::builtin("mine");
        apply(&store, &theme).unwrap();
        assert!(store.get("user/1000/ui/cursor/theme").is_some());
        assert_eq!(current(&store), Some(theme));
    }

    #[test]
    fn the_administrator_writes_the_machine_key() {
        let store = mem(0);
        apply(&store, &Theme::builtin("mine")).unwrap();
        assert!(store.get("sys/ui/cursor/theme").is_some());
    }

    #[test]
    fn revert_shadows_a_machine_theme_with_the_builtin_shapes() {
        let store = mem(1000);
        let machine = format::serialize(&Theme::builtin("everyone"));
        store
            .set("sys/ui/cursor/theme", Value::Str(machine))
            .unwrap();
        assert_eq!(current(&store).unwrap().name, "everyone");
        revert(&store).unwrap();
        assert_eq!(current(&store), None);
    }

    #[test]
    fn a_bad_name_or_an_oversize_theme_is_refused_before_writing() {
        let store = mem(1000);
        let mut theme = Theme::builtin("Bad Name");
        assert!(apply(&store, &theme).is_err());
        theme.name = "big".into();
        let def = theme.shapes[0].as_mut().unwrap();
        for _ in 0..400 {
            def.layers[0]
                .polys
                .push(vec![(1, 2), (300, 400), (500, 12)]);
        }
        let error = apply(&store, &theme).unwrap_err();
        assert!(error.contains("at most 4096"), "{error}");
        assert!(store.map.borrow().is_empty());
    }

    #[test]
    fn without_a_uid_nothing_is_written() {
        let store = Mem::default();
        assert!(apply(&store, &Theme::builtin("mine")).is_err());
        assert!(revert(&store).is_err());
        assert_eq!(current(&store), None);
    }
}
