//! The name table's contract: it covers `Lucide::ALL` exactly, the names are
//! well formed and unique, and lookups are exact.

use std::collections::HashSet;
use xui_core::Lucide;

#[test]
fn covers_every_outline_exactly_once() {
    let listed: Vec<Lucide> = lazyicons::all().map(|(_, icon)| icon).collect();
    assert_eq!(listed.len(), Lucide::ALL.len(), "one entry per variant");
    let unique: HashSet<Lucide> = listed.iter().copied().collect();
    assert_eq!(unique.len(), listed.len(), "no variant listed twice");
    for icon in Lucide::ALL {
        assert!(unique.contains(icon), "{icon:?} has no name");
    }
}

#[test]
fn names_are_unique() {
    let mut seen = HashSet::new();
    for (name, _) in lazyicons::all() {
        assert!(seen.insert(name), "duplicate name {name:?}");
    }
}

#[test]
fn names_are_kebab_case_and_short() {
    for (name, _) in lazyicons::all() {
        assert!(
            !name.is_empty() && name.len() <= lazyicons::MAX_NAME_LEN,
            "{name:?}"
        );
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{name:?} is not [a-z0-9-]"
        );
        assert!(
            !name.starts_with('-') && !name.ends_with('-') && !name.contains("--"),
            "{name:?}"
        );
    }
}

#[test]
fn round_trips() {
    for &icon in Lucide::ALL {
        assert_eq!(lazyicons::from_name(lazyicons::name(icon)), Some(icon));
    }
    for (name, icon) in lazyicons::all() {
        assert_eq!(lazyicons::name(icon), name);
    }
}

#[test]
fn known_names_from_the_plan() {
    assert_eq!(lazyicons::from_name("app-window"), Some(Lucide::AppWindow));
    assert_eq!(lazyicons::from_name("volume-2"), Some(Lucide::Volume2));
    assert_eq!(lazyicons::from_name("heading-1"), Some(Lucide::Heading1));
    assert_eq!(
        lazyicons::from_name("mouse-pointer-2"),
        Some(Lucide::MousePointer2)
    );
    assert_eq!(lazyicons::from_name("refresh-cw"), Some(Lucide::RefreshCw));
}

#[test]
fn unknown_names_are_none() {
    for bad in [
        "", "Save", "SAVE", " save", "save ", "save\0", "volume2", "Volume2", "wifi", "-", "x-",
    ] {
        assert_eq!(lazyicons::from_name(bad), None, "{bad:?}");
    }
}

#[test]
fn oversized_input_is_refused() {
    let long = "a".repeat(lazyicons::MAX_NAME_LEN + 1);
    assert_eq!(lazyicons::from_name(&long), None);
    let huge = "save".repeat(1 << 16);
    assert_eq!(lazyicons::from_name(&huge), None);
}
