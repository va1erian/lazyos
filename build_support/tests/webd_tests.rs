//! `LAZYOS_WEBD=1` (docs/webd.md): refused without the desktop, a clear message when the daemon is not
//! built, and the client packages that only an image with webd carries.

use crate::core_packages::{is_core_stem, parse_autostart};
use crate::webd_embed::{
    client_tree_present, not_built, parse_switch, requirements, shipped, Switch, CLIENT_PACKAGES,
    ELF, PACKAGE_SHORT,
};
use std::ffi::OsStr;

#[test]
fn unset_needs_nothing() {
    assert_eq!(requirements(false, false), Ok(()));
}

#[test]
fn the_desktop_alone_ships_it() {
    // A light view has no network: the stack is not asked for.
    assert_eq!(requirements(true, true), Ok(()));
}

#[test]
fn a_missing_desktop_is_named() {
    let reason = requirements(true, false).unwrap_err();
    assert!(reason.contains("LAZYOS_DESKTOP=1"), "{reason}");
    assert!(!reason.contains("NETD"), "{reason}");
    assert!(reason.contains("run_demo.py --webd"), "{reason}");
}

#[test]
fn not_built_says_what_to_run() {
    assert!(not_built(false).contains("tools/webkit/webd.py"));
    assert!(not_built(false).contains(ELF));
    assert!(not_built(true).contains("tools/xui/core_packages.py"));
}

#[test]
fn the_package_is_os_lazy_webd() {
    assert_eq!(PACKAGE_SHORT, "webd");
    assert_eq!(ELF, "xui-webd.elf");
    assert!(is_core_stem("webd"));
}

#[test]
fn webd_is_not_in_the_default_autostart() {
    // The switch adds it (`autostart_shorts`); the plain list never names it.
    assert!(!parse_autostart(None).iter().any(|short| short == "webd"));
}

#[test]
fn the_switch_is_on_off_or_left_to_the_desktop() {
    assert_eq!(parse_switch(Some(OsStr::new("1"))), Switch::On);
    assert_eq!(parse_switch(Some(OsStr::new("0"))), Switch::Off);
    assert_eq!(parse_switch(None), Switch::Auto);
    assert_eq!(parse_switch(Some(OsStr::new(""))), Switch::Auto);
    assert_eq!(parse_switch(Some(OsStr::new("yes"))), Switch::Auto);
}

#[test]
fn a_desktop_ships_it_once_built_and_a_forced_switch_does_not_look() {
    // Docs and Mail are in every desktop and need it: unset, a desktop that has it ships it.
    assert!(shipped(Switch::Auto, true, true));
    // A checkout that cannot build WebKit still gets a desktop.
    assert!(!shipped(Switch::Auto, true, false));
    // Not a desktop: nothing changes.
    assert!(!shipped(Switch::Auto, false, true));
    // `0` leaves it out even when built; `1` asks for it (a missing program then fails the build).
    assert!(!shipped(Switch::Off, true, true));
    assert!(shipped(Switch::On, true, false));
}

#[test]
fn a_client_without_a_tree_is_skipped_and_the_daemon_builds_alone() {
    let nowhere = std::env::temp_dir().join("lazyos-webd-no-such-checkout");
    for (short, _) in CLIENT_PACKAGES {
        assert!(!client_tree_present(&nowhere, short));
    }
}

#[test]
fn the_clients_are_named_by_short_id_and_program() {
    for (short, elf) in CLIENT_PACKAGES {
        assert_eq!(*elf, format!("xui-{short}.elf"));
    }
}
