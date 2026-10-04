//! `LAZYOS_LAZYWEB=1` (docs/lazyweb.md): refused without the desktop and the
//! network stack, and a clear message when the browser is not built.

use crate::lazyweb_embed::{not_built, requirements};

#[test]
fn unset_needs_nothing() {
    assert_eq!(requirements(false, false, false), Ok(()));
}

#[test]
fn the_desktop_with_the_stack_ships_it() {
    assert_eq!(requirements(true, true, true), Ok(()));
}

#[test]
fn a_missing_profile_is_named() {
    let both = requirements(true, false, false).unwrap_err();
    assert!(
        both.contains("LAZYOS_DESKTOP=1 and LAZYOS_NETD=1"),
        "{both}"
    );
    let netd = requirements(true, true, false).unwrap_err();
    assert!(
        netd.contains("LAZYOS_NETD=1") && !netd.contains("DESKTOP"),
        "{netd}"
    );
    let desktop = requirements(true, false, true).unwrap_err();
    assert!(
        desktop.contains("LAZYOS_DESKTOP=1") && !desktop.contains("NETD"),
        "{desktop}"
    );
    assert!(both.contains("run_demo.py --lazyweb"));
}

#[test]
fn not_built_says_what_to_run() {
    assert!(not_built(false).contains("tools/xui/build.py"));
    assert!(not_built(false).contains("xui-lazyweb.elf"));
    assert!(not_built(true).contains("tools/xui/core_packages.py"));
}
