//! The user's notification settings in `confd` (docs/notify-plan.md,
//! "Policy"): the keys, and how their values become the presenter's
//! [`Policy`] and [`Motion`]. The shell reads them; senders never see them.
//!
//! * `sys/ui/motion`: `full` (default), `reduced` or `none`.
//! * `sys/ui/notify/dnd`: do not disturb; only Error toasts show.
//! * `sys/ui/notify/mute`: app ids (or `<terminal>`) whose toasts never
//!   show, separated by spaces or commas.
//!
//! Each key also has the user's own `user/<uid>/ui/...` shadow
//! (`uitheme::user_key`), like the rest of `sys/ui`.

use confd::Value;
use lazymotion::Motion;

use super::policy::Policy;

/// The motion setting's key.
pub const KEY_MOTION: &str = "sys/ui/motion";
/// The do-not-disturb key.
pub const KEY_DND: &str = "sys/ui/notify/dnd";
/// The per-app mute list's key.
pub const KEY_MUTE: &str = "sys/ui/notify/mute";
/// Every key the feed reads.
pub const KEYS: [&str; 3] = [KEY_MOTION, KEY_DND, KEY_MUTE];

/// The values in effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub motion: Motion,
    pub policy: Policy,
}

impl Settings {
    /// Settings from the three values (`None`: the key is absent).
    pub fn from_values(
        motion: Option<&Value>,
        dnd: Option<&Value>,
        mute: Option<&Value>,
    ) -> Settings {
        Settings {
            motion: match motion {
                Some(Value::Str(value)) => Motion::parse(value),
                _ => Motion::Full,
            },
            policy: Policy {
                dnd: dnd.is_some_and(truthy),
                muted: match mute {
                    Some(Value::Str(list)) => Policy::parse_muted(list),
                    _ => Vec::new(),
                },
            },
        }
    }
}

/// A switch: a boolean, a non-zero number, or `1`/`true`/`on`/`yes`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(on) => *on,
        Value::I64(n) => *n != 0,
        Value::U64(n) => *n != 0,
        Value::Str(text) => matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Value::Bytes(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Str(value.into())
    }

    #[test]
    fn absent_keys_are_the_defaults() {
        let settings = Settings::from_values(None, None, None);
        assert_eq!(settings.motion, Motion::Full);
        assert!(!settings.policy.dnd && settings.policy.muted.is_empty());
    }

    #[test]
    fn values_are_read_in_their_usual_spellings() {
        let settings = Settings::from_values(
            Some(&text("reduced")),
            Some(&Value::Bool(true)),
            Some(&text("org.a, org.b <terminal>")),
        );
        assert_eq!(settings.motion, Motion::Reduced);
        assert!(settings.policy.dnd);
        assert_eq!(settings.policy.muted, ["org.a", "org.b", "<terminal>"]);
        for on in [text("1"), text("true"), text(" ON "), Value::I64(2)] {
            assert!(Settings::from_values(None, Some(&on), None).policy.dnd);
        }
        for off in [text("0"), text("no"), Value::Bool(false), Value::U64(0)] {
            assert!(!Settings::from_values(None, Some(&off), None).policy.dnd);
        }
    }

    #[test]
    fn a_value_of_the_wrong_kind_means_the_default() {
        let settings = Settings::from_values(
            Some(&Value::I64(1)),
            Some(&Value::Bytes(vec![1])),
            Some(&Value::Bool(true)),
        );
        assert_eq!(settings, Settings::default());
    }
}
