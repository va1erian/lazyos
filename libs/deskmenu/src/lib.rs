//! Desktop context-menu schema (`sys/ui/menu` in confd).
//!
//! One definition of the stored list, shared by `xuid` (which paints and
//! launches from it) and the Settings app (which edits it), so neither
//! hand-copies the format. Pure `no_std` + `alloc` logic with host tests.
//!
//! The value is one string, one entry per line: `<app id>\t<label>`. Storage
//! is untrusted (any uid-0 writer, or a corrupt file), so [`parse`] validates
//! everything: app ids must be well formed *and* known to the caller (the
//! `init` registry), labels are cleaned and capped, the count is capped, and
//! duplicates are dropped. [`from_value`] never fails: a missing, mistyped or
//! all-invalid value yields the built-in [`defaults`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use confd::Value;

/// The confd key. Under `sys/ui/`, so `xuid`'s existing
/// `system/confd/changed/sys/ui/#` subscription already reports changes.
pub const KEY: &str = "sys/ui/menu";
/// Most entries kept (a longer list is truncated).
pub const MAX_ENTRIES: usize = 24;
/// Longest label, in characters.
pub const MAX_LABEL: usize = 32;
/// Longest app id, in bytes.
pub const MAX_APP: usize = 32;

/// One menu row: which registry app it launches and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub app: String,
    pub label: String,
}

impl Entry {
    /// An entry with a cleaned label; `None` when `app` is not a valid id.
    /// An empty label falls back to the app id.
    pub fn new(app: &str, label: &str) -> Option<Entry> {
        if !valid_app_id(app) {
            return None;
        }
        let mut label = clean_label(label);
        if label.is_empty() {
            label = String::from(app);
        }
        Some(Entry {
            app: String::from(app),
            label,
        })
    }
}

/// The built-in list: what the menu shows when confd has nothing usable.
/// Terminal first.
pub fn defaults() -> Vec<Entry> {
    const ITEMS: [(&str, &str); 13] = [
        ("terminal", "Terminal"),
        ("sysmon", "System Monitor"),
        ("fabricmon", "Fabric Monitor"),
        ("counter", "Counter"),
        ("editor", "Editor"),
        ("paint", "Paint"),
        ("files", "Files"),
        ("settings", "Settings"),
        // Shipped only when the build had the zig toolchain; an image
        // without it answers the launch as unavailable.
        ("docs", "Docs"),
        // Last, so the rows above keep the positions the screenshot sessions
        // click by coordinate.
        ("widget", "CPU & Memory"),
        ("confd", "Config"),
        ("installer", "Package Installer"),
        ("devices", "Devices"),
    ];
    ITEMS
        .iter()
        .filter_map(|(app, label)| Entry::new(app, label))
        .collect()
}

/// Whether `app` looks like an `init` registry id: the lowercase program
/// stem, `[a-z0-9_-]`, bounded.
pub fn valid_app_id(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= MAX_APP
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A label with control characters removed, edges trimmed, capped at
/// [`MAX_LABEL`] characters.
pub fn clean_label(label: &str) -> String {
    let kept: String = label.chars().filter(|c| !c.is_control()).collect();
    kept.trim().chars().take(MAX_LABEL).collect()
}

/// Serialize `entries` (invalid and repeated ones, and anything past
/// [`MAX_ENTRIES`], are left out, so the result always round-trips through
/// [`parse`]).
pub fn encode(entries: &[Entry]) -> String {
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        if seen.len() == MAX_ENTRIES {
            break;
        }
        let Some(clean) = Entry::new(&entry.app, &entry.label) else {
            continue;
        };
        if seen.contains(&entry.app.as_str()) {
            continue;
        }
        seen.push(&entry.app);
        out.push_str(&clean.app);
        out.push('\t');
        out.push_str(&clean.label);
        out.push('\n');
    }
    out
}

/// Parse untrusted stored text. Lines that are malformed, name an app for
/// which `known` is false, or repeat an earlier app are skipped; at most
/// [`MAX_ENTRIES`] entries are returned. May be empty.
pub fn parse(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if out.len() == MAX_ENTRIES {
            break;
        }
        let (app, label) = line.split_once('\t').unwrap_or((line, ""));
        let Some(entry) = Entry::new(app, label) else {
            continue;
        };
        if !known(&entry.app) || out.iter().any(|e| e.app == entry.app) {
            continue;
        }
        out.push(entry);
    }
    out
}

/// The list a stored value stands for: [`parse`] of a string value, or the
/// [`defaults`] when the key is absent, not a string, or yields no entry.
pub fn from_value(value: Option<&Value>, known: &dyn Fn(&str) -> bool) -> Vec<Entry> {
    if let Some(Value::Str(text)) = value {
        let entries = parse(text, known);
        if !entries.is_empty() {
            return entries;
        }
    }
    defaults()
}

/// The confd value to store for `entries`.
pub fn to_value(entries: &[Entry]) -> Value {
    Value::Str(encode(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any(_: &str) -> bool {
        true
    }

    #[test]
    fn defaults_are_valid_and_start_with_terminal() {
        let list = defaults();
        assert_eq!(list.len(), 13);
        assert_eq!(list[0].app, "terminal");
        assert_eq!(parse(&encode(&list), &any), list);
    }

    #[test]
    fn round_trip_preserves_order_and_labels() {
        let list = alloc::vec![
            Entry::new("paint", "Draw").unwrap(),
            Entry::new("files", "Files").unwrap(),
        ];
        assert_eq!(parse(&encode(&list), &any), list);
    }

    #[test]
    fn unknown_apps_are_skipped() {
        let known = |app: &str| app == "files";
        let got = parse("terminal\tTerm\nfiles\tF\n", &known);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].app, "files");
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let got = parse("\nBad Id\tx\n../etc\tx\nok\tfine\n\t\n", &any);
        assert_eq!(got, alloc::vec![Entry::new("ok", "fine").unwrap()]);
    }

    #[test]
    fn labels_are_cleaned_capped_and_default_to_the_id() {
        let long: String = core::iter::repeat('x').take(200).collect();
        let got = parse(
            &alloc::format!("a\t{long}\nb\t  \x07 \nc\tHi\x1b there\n"),
            &any,
        );
        assert_eq!(got[0].label.chars().count(), MAX_LABEL);
        assert_eq!(got[1].label, "b");
        assert_eq!(got[2].label, "Hi there");
    }

    #[test]
    fn a_line_without_a_tab_uses_the_id_as_label() {
        assert_eq!(parse("files", &any)[0].label, "files");
    }

    #[test]
    fn count_is_capped_and_duplicates_dropped() {
        let mut text = String::new();
        for i in 0..(MAX_ENTRIES + 10) {
            text.push_str(&alloc::format!("app{i}\tL\n"));
        }
        assert_eq!(parse(&text, &any).len(), MAX_ENTRIES);
        assert_eq!(parse("a\tx\na\ty\n", &any).len(), 1);
        let many: Vec<Entry> = (0..40)
            .map(|i| Entry::new(&alloc::format!("a{i}"), "x").unwrap())
            .collect();
        assert_eq!(parse(&encode(&many), &any).len(), MAX_ENTRIES);
    }

    #[test]
    fn encode_drops_invalid_entries_and_tabs_in_labels() {
        let list = alloc::vec![
            Entry {
                app: "Bad!".into(),
                label: "x".into()
            },
            Entry {
                app: "ok".into(),
                label: "a\tb\nc".into(),
            },
        ];
        assert_eq!(encode(&list), "ok\tabc\n");
    }

    #[test]
    fn overlong_app_ids_are_rejected() {
        let id: String = core::iter::repeat('a').take(MAX_APP + 1).collect();
        assert!(!valid_app_id(&id));
        assert!(valid_app_id("fabricmon"));
        assert!(!valid_app_id("Term"));
    }

    #[test]
    fn from_value_falls_back_to_defaults() {
        assert_eq!(from_value(None, &any), defaults());
        assert_eq!(from_value(Some(&Value::Bool(true)), &any), defaults());
        assert_eq!(
            from_value(Some(&Value::Bytes(alloc::vec![1])), &any),
            defaults()
        );
        assert_eq!(from_value(Some(&Value::Str("".into())), &any), defaults());
        // Nothing valid (no app is known) is also "unusable".
        assert_eq!(
            from_value(Some(&Value::Str("files\tF\n".into())), &|_| false),
            defaults()
        );
        let custom = from_value(Some(&Value::Str("files\tF\n".into())), &any);
        assert_eq!(custom, alloc::vec![Entry::new("files", "F").unwrap()]);
    }

    #[test]
    fn a_full_list_fits_a_confd_value() {
        let many: Vec<Entry> = (0..MAX_ENTRIES)
            .map(|i| {
                let id = alloc::format!("{:0>width$}", i, width = MAX_APP);
                let label: String = core::iter::repeat('m').take(MAX_LABEL * 4).collect();
                Entry::new(&id, &label).unwrap()
            })
            .collect();
        assert!(encode(&many).len() <= confd::MAX_VALUE_LEN);
    }
}
