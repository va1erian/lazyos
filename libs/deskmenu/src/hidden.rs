//! Hidden apps (issue #509 §5): which apps the start menu leaves out.
//!
//! Hiding is a menu matter only: a hidden app still launches, autostarts and
//! opens files. Two confd keys decide it, both a `bool` per app id:
//!
//! * `user/<uid>/menu/hidden/<id>`: the user's choice, writable by that uid;
//! * `sys/menu/hidden/<id>`: the machine default, writable by uid 0 only.
//!
//! The user's key wins when present, so a user can show an app the machine
//! hides by storing `false`. A missing key, or a value that is not a `bool`,
//! counts as absent: storage is untrusted, and a corrupt value must not hide
//! an app nobody chose to hide.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use confd::Value;

use crate::valid_system_name;

/// The machine defaults' prefix.
pub const SYS_PREFIX: &str = "sys/menu/hidden";

/// The prefix of `uid`'s own choices: `user/<uid>/menu/hidden`.
pub fn user_prefix(uid: u32) -> String {
    format!("user/{uid}/menu/hidden")
}

/// `uid`'s key for `app`; `None` when `app` is not a well-formed id.
pub fn user_key(uid: u32, app: &str) -> Option<String> {
    valid_system_name(app).then(|| format!("{}/{app}", user_prefix(uid)))
}

/// The machine default's key for `app`; `None` when `app` is not a
/// well-formed id.
pub fn sys_key(app: &str) -> Option<String> {
    valid_system_name(app).then(|| format!("{SYS_PREFIX}/{app}"))
}

/// The flag a stored value stands for: only a `bool` counts.
pub fn flag(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(on)) => Some(*on),
        _ => None,
    }
}

/// Whether `app` is hidden, given the user's value and the machine default:
/// the user's wins when present, nothing set means shown, and an id that is
/// not well formed is never hidden (it has no key to hide it by).
pub fn is_hidden(app: &str, user: Option<bool>, machine: Option<bool>) -> bool {
    valid_system_name(app) && user.or(machine).unwrap_or(false)
}

/// The app id a listed key names under `prefix` (`<prefix>/<id>`); `None`
/// for anything else (a deeper path, or a malformed id).
pub fn app_of<'a>(key: &'a str, prefix: &str) -> Option<&'a str> {
    let app = key.strip_prefix(prefix)?.strip_prefix('/')?;
    valid_system_name(app).then_some(app)
}

/// Both layers read at once, for a caller that resolves many apps (the start
/// menu): the user's and the machine's flags, by app id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    user: Vec<(String, bool)>,
    machine: Vec<(String, bool)>,
}

impl Hidden {
    /// From `(key, value)` pairs as confd lists them: entries under
    /// [`user_prefix`]`(uid)` are the user's, entries under [`SYS_PREFIX`] the
    /// machine's, and everything else (other users, other subtrees,
    /// non-`bool` values) is ignored.
    pub fn from_pairs<'a>(
        uid: u32,
        pairs: impl IntoIterator<Item = (&'a str, &'a Value)>,
    ) -> Hidden {
        let user_prefix = user_prefix(uid);
        let mut hidden = Hidden::default();
        for (key, value) in pairs {
            let Some(on) = flag(Some(value)) else {
                continue;
            };
            if let Some(app) = app_of(key, &user_prefix) {
                hidden.user.push((String::from(app), on));
            } else if let Some(app) = app_of(key, SYS_PREFIX) {
                hidden.machine.push((String::from(app), on));
            }
        }
        hidden
    }

    /// Whether the menu leaves `app` out.
    pub fn hides(&self, app: &str) -> bool {
        let find =
            |layer: &[(String, bool)]| layer.iter().find(|(id, _)| id == app).map(|(_, on)| *on);
        is_hidden(app, find(&self.user), find(&self.machine))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_valid_confd_paths() {
        for app in ["terminal", "os.lazy.terminal", "org.lazy.counter"] {
            let user = user_key(1000, app).unwrap();
            let sys = sys_key(app).unwrap();
            assert_eq!(confd::validate_path(&user), Ok(()), "{user}");
            assert_eq!(confd::validate_path(&sys), Ok(()), "{sys}");
        }
        assert_eq!(
            user_key(7, "os.lazy.paint").as_deref(),
            Some("user/7/menu/hidden/os.lazy.paint")
        );
        assert_eq!(
            sys_key("os.lazy.paint").as_deref(),
            Some("sys/menu/hidden/os.lazy.paint")
        );
        let longest = "a".repeat(crate::MAX_SYSTEM_NAME);
        let key = user_key(u32::MAX, &longest).unwrap();
        assert_eq!(confd::validate_path(&key), Ok(()));
    }

    #[test]
    fn malformed_ids_have_no_key() {
        for bad in ["", "..", ".", "a/b", "Paint", "a..b", ".a", "a."] {
            assert_eq!(user_key(1, bad), None, "{bad}");
            assert_eq!(sys_key(bad), None, "{bad}");
            assert!(!is_hidden(bad, Some(true), Some(true)), "{bad}");
        }
    }

    #[test]
    fn the_user_wins_over_the_machine_default() {
        let app = "os.lazy.paint";
        assert!(!is_hidden(app, None, None));
        assert!(is_hidden(app, None, Some(true)));
        assert!(!is_hidden(app, Some(false), Some(true)));
        assert!(is_hidden(app, Some(true), Some(false)));
        assert!(is_hidden(app, Some(true), None));
        assert!(!is_hidden(app, None, Some(false)));
    }

    #[test]
    fn only_bools_count() {
        assert_eq!(flag(Some(&Value::Bool(true))), Some(true));
        assert_eq!(flag(Some(&Value::Bool(false))), Some(false));
        assert_eq!(flag(Some(&Value::U64(1))), None);
        assert_eq!(flag(Some(&Value::Str("true".into()))), None);
        assert_eq!(flag(None), None);
    }

    #[test]
    fn app_of_reads_back_only_direct_children() {
        let prefix = user_prefix(5);
        assert_eq!(
            app_of("user/5/menu/hidden/os.lazy.files", &prefix),
            Some("os.lazy.files")
        );
        assert_eq!(app_of("user/5/menu/hidden", &prefix), None);
        assert_eq!(app_of("user/5/menu/hiddenx/a", &prefix), None);
        assert_eq!(app_of("user/5/menu/hidden/a/b", &prefix), None);
        assert_eq!(app_of("user/50/menu/hidden/a", &prefix), None);
    }

    #[test]
    fn hidden_resolves_both_layers_from_listed_pairs() {
        let t = Value::Bool(true);
        let f = Value::Bool(false);
        let junk = Value::Str("yes".into());
        let pairs = [
            ("sys/menu/hidden/os.lazy.paint", &t),
            ("sys/menu/hidden/os.lazy.files", &t),
            ("user/5/menu/hidden/os.lazy.files", &f),
            ("user/5/menu/hidden/os.lazy.editor", &t),
            ("user/6/menu/hidden/os.lazy.terminal", &t),
            ("sys/menu/hidden/os.lazy.sysmon", &junk),
            ("sys/ui/menu", &t),
        ];
        let hidden = Hidden::from_pairs(5, pairs);
        assert!(hidden.hides("os.lazy.paint"), "machine default");
        assert!(!hidden.hides("os.lazy.files"), "user un-hid it");
        assert!(hidden.hides("os.lazy.editor"), "user hid it");
        assert!(!hidden.hides("os.lazy.terminal"), "another user's choice");
        assert!(!hidden.hides("os.lazy.sysmon"), "not a bool");
        assert!(!hidden.hides("anything"));
        assert!(!Hidden::default().hides("os.lazy.paint"));
    }
}
