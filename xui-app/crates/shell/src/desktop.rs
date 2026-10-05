//! The desktop: the icons of the user's desktop folder ([`folder`], laid out
//! by [`grid`]), and the launchers (`sys/ui/desktop` in confd) that seed it.
//!
//! The launchers are what a new desktop folder starts with (one shortcut
//! each), and what the desktop shows when there is no folder to show (no
//! `$HOME`). The stored value uses the start menu's `deskmenu` format (one
//! `<app id>\t<label>` line per icon) and the same validation, so a corrupt or
//! hostile value can only produce well-formed rows. An absent, mistyped or
//! all-invalid value yields [`defaults`].

use confd::Value;
use deskmenu::Entry;

pub mod folder;
pub mod grid;

/// The confd key, under `sys/ui/` like the theme and the menu.
pub const KEY: &str = "sys/ui/desktop";

/// The built-in launchers: core packages by `system_name` (issue #509), and
/// the built-in Terminal.
pub fn defaults() -> Vec<Entry> {
    const ITEMS: [(&str, &str); 6] = [
        ("os.lazy.files", "Files"),
        ("terminal", "Terminal"),
        ("os.lazy.editor", "Editor"),
        ("os.lazy.settings", "Settings"),
        ("os.lazy.sysmon", "System Monitor"),
        ("os.lazy.paint", "Paint"),
    ];
    ITEMS
        .iter()
        .filter_map(|(app, label)| Entry::new(app, label))
        .collect()
}

/// The launchers a stored value stands for.
pub fn from_value(value: Option<&Value>) -> Vec<Entry> {
    if let Some(Value::Str(text)) = value {
        let entries = deskmenu::parse(text, &|_| true);
        if !entries.is_empty() {
            return entries;
        }
    }
    defaults()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_contract_six() {
        let apps: Vec<String> = defaults().into_iter().map(|e| e.app).collect();
        assert_eq!(
            apps,
            [
                "os.lazy.files",
                "terminal",
                "os.lazy.editor",
                "os.lazy.settings",
                "os.lazy.sysmon",
                "os.lazy.paint"
            ]
        );
    }

    #[test]
    fn a_stored_list_replaces_the_defaults() {
        let value = Value::Str("paint\tDraw\nfiles\tMy Files\n".into());
        let entries = from_value(Some(&value));
        assert_eq!(
            entries,
            vec![
                Entry::new("paint", "Draw").unwrap(),
                Entry::new("files", "My Files").unwrap()
            ]
        );
    }

    #[test]
    fn missing_mistyped_or_invalid_values_fall_back() {
        assert_eq!(from_value(None), defaults());
        assert_eq!(from_value(Some(&Value::U64(3))), defaults());
        assert_eq!(
            from_value(Some(&Value::Str("BAD ID\tx\n\n".into()))),
            defaults()
        );
    }
}
