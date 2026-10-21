//! `LAZYOS_WEBD=1` (docs/webd.md): refused without the desktop, a clear message when the daemon is not
//! built, and the client packages that only an image with webd carries.

use crate::core_packages::{is_core_stem, parse_autostart};
use crate::webd_embed::{
    client_tree_present, not_built, requirements, CLIENT_PACKAGES, ELF, PACKAGE_SHORT,
};

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
