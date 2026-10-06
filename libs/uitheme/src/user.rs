//! Per-user theme (issue #407): `user/<uid>/ui/<name>` overrides the machine
//! default `sys/ui/<name>` for that uid, key by key (a user key that is
//! present wins; an absent one falls back to the machine value).
//!
//! The administrator (uid 0) has no personal theme: what it sets *is* the
//! machine default, so [`user_key`] is `None` for uid 0 and its session
//! paints from `sys/ui/*` alone. `confd` lets only the owner (and root) read
//! or write `user/<uid>`, and announces its changes on
//! `user/<uid>/confd/changed/ui/...`, which only that uid and root may
//! subscribe to (the kernel's per-uid topic namespace).

extern crate alloc;

use alloc::format;
use alloc::string::String;

use confd::Value;

/// The path below `user/<uid>/` holding the per-user theme keys.
pub const USER_SUBTREE: &str = "ui";
/// The change-topic `path...` (below `user/<uid>/confd/changed/`) that
/// covers every per-user theme key.
pub const USER_FILTER_PATH: &str = "ui/#";

/// Whether `uid` has a personal theme (everyone but the administrator).
pub const fn personal(uid: u32) -> bool {
    uid != 0
}

/// The per-user key shadowing the machine key `key` (`sys/ui/<name>`) for
/// `uid`: `user/<uid>/ui/<name>`. `None` for uid 0 and for a key outside
/// `sys/ui/`.
pub fn user_key(uid: u32, key: &str) -> Option<String> {
    let name = key.strip_prefix(crate::PREFIX)?.strip_prefix('/')?;
    (personal(uid) && !name.is_empty()).then(|| format!("user/{uid}/{USER_SUBTREE}/{name}"))
}

/// The value in effect: the user's own when it has one, else the machine's.
pub fn overlay(machine: Option<Value>, user: Option<Value>) -> Option<Value> {
    user.or(machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_keys_mirror_the_machine_keys() {
        assert_eq!(
            user_key(1000, crate::KEY_MODE).as_deref(),
            Some("user/1000/ui/mode")
        );
        assert_eq!(
            user_key(7, crate::KEY_WALLPAPER).as_deref(),
            Some("user/7/ui/wallpaper")
        );
        for key in crate::ALL_KEYS {
            let user = user_key(1000, key).unwrap();
            assert!(confd::validate_path(&user).is_ok(), "{user}");
        }
    }

    #[test]
    fn the_administrator_and_foreign_keys_have_none() {
        assert_eq!(user_key(0, crate::KEY_MODE), None);
        assert_eq!(user_key(1000, "sys/time/clock24"), None);
        assert_eq!(user_key(1000, "sys/ui"), None);
        assert_eq!(user_key(1000, "sys/uix/mode"), None);
    }

    #[test]
    fn the_user_value_wins_when_present() {
        let light = Some(Value::Str("light".into()));
        let dark = Some(Value::Str("dark".into()));
        assert_eq!(overlay(dark.clone(), light.clone()), light);
        assert_eq!(overlay(dark.clone(), None), dark);
        assert_eq!(overlay(None, None), None);
    }
}
