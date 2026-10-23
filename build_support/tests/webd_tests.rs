//! `LAZYOS_WEBD=1` (docs/webd.md): refused without the desktop, a clear message when the daemon is not
//! built, and the client packages that only an image with webd carries. Also `LAZYOS_LAZYWEB=1`
//! (docs/lazyweb.md), a client of webd: refused without the desktop, the network stack and webd.

use crate::core_packages::{is_core_stem, parse_autostart};
use crate::webd_embed::{
    client_tree_present, effective_switch, engine_source, lazyweb_not_built, lazyweb_requirements,
    not_built, parse_switch, requirements, shipped, Switch, CLIENT_PACKAGES, ELF, LAZYWEB_ELF,
    LAZYWEB_PACKAGE_SHORT, PACKAGE_SHORT,
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

#[test]
fn lazyweb_unset_needs_nothing() {
    for webd in [Switch::Off, Switch::Auto, Switch::On] {
        assert_eq!(lazyweb_requirements(false, false, false, webd), Ok(()));
    }
}

#[test]
fn lazyweb_on_the_desktop_with_the_stack_ships_with_webd_left_to_it_or_forced() {
    assert_eq!(lazyweb_requirements(true, true, true, Switch::Auto), Ok(()));
    assert_eq!(lazyweb_requirements(true, true, true, Switch::On), Ok(()));
}

#[test]
fn lazyweb_without_webd_is_refused_and_says_so() {
    let reason = lazyweb_requirements(true, true, true, Switch::Off).unwrap_err();
    assert!(
        reason.contains("LAZYOS_LAZYWEB=1") && reason.contains("LAZYOS_WEBD=0"),
        "{reason}"
    );
}

#[test]
fn lazyweb_a_missing_profile_is_named() {
    let both = lazyweb_requirements(true, false, false, Switch::Auto).unwrap_err();
    assert!(
        both.contains("LAZYOS_DESKTOP=1 and LAZYOS_NETD=1"),
        "{both}"
    );
    let netd = lazyweb_requirements(true, true, false, Switch::Auto).unwrap_err();
    assert!(
        netd.contains("LAZYOS_NETD=1") && !netd.contains("DESKTOP=1 and"),
        "{netd}"
    );
    let desktop = lazyweb_requirements(true, false, true, Switch::Auto).unwrap_err();
    assert!(
        desktop.contains("LAZYOS_DESKTOP=1") && !desktop.contains("NETD=1 ("),
        "{desktop}"
    );
    assert!(both.contains("run_demo.py --lazyweb"));
}

#[test]
fn the_browser_makes_an_unset_webd_switch_required_and_leaves_the_others() {
    assert_eq!(effective_switch(Switch::Auto, true), Switch::On);
    assert_eq!(effective_switch(Switch::Auto, false), Switch::Auto);
    assert_eq!(effective_switch(Switch::On, true), Switch::On);
    // `0` stays `0`: lazyweb_requirements refuses that combination by name.
    assert_eq!(effective_switch(Switch::Off, true), Switch::Off);
    // A forced switch ships webd even where the program has not been looked for.
    assert!(shipped(effective_switch(Switch::Auto, true), true, false));
}

#[test]
fn lazyweb_not_built_says_what_to_run() {
    assert!(lazyweb_not_built(false).contains("tools/xui/build.py"));
    assert!(!lazyweb_not_built(false).contains("tools/webkit/app.py"));
    assert!(lazyweb_not_built(false).contains(LAZYWEB_ELF));
    assert!(lazyweb_not_built(true).contains("tools/xui/core_packages.py"));
}

#[test]
fn the_browser_package_is_os_lazy_lazyweb() {
    assert_eq!(LAZYWEB_PACKAGE_SHORT, "lazyweb");
    assert_eq!(LAZYWEB_ELF, "xui-lazyweb.elf");
    assert!(is_core_stem("lazyweb"));
}

#[test]
fn webds_engine_processes_come_from_the_port_unless_the_attack_harness_asks() {
    assert_eq!(
        engine_source(false),
        ("target/webkit/bin", "tools/webkit/port.py")
    );
    let (dir, builder) = engine_source(true);
    assert_eq!(dir, "target/webkit/attack");
    assert!(builder.contains("attack_build.py"));
}
