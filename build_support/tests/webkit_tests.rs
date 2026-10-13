//! `LAZYOS_WEBKIT=1` (docs/webkit-port-plan.md W5): refused without the desktop and the
//! network stack, and a clear message when the browser is not built.

use crate::webkit_embed::{not_built, requirements};

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
        netd.contains("LAZYOS_NETD=1") && !netd.contains("DESKTOP=1 and"),
        "{netd}"
    );
    let desktop = requirements(true, false, true).unwrap_err();
    assert!(
        desktop.contains("LAZYOS_DESKTOP=1") && !desktop.contains("NETD=1 ("),
        "{desktop}"
    );
    // The way out for a console proof is named too.
    assert!(both.contains("run_demo.py --webkit") && both.contains("LAZYOS_WEBKIT_PORT=1"));
}

#[test]
fn the_engine_processes_come_from_the_port_unless_the_attack_harness_asks() {
    use crate::webkit_embed::engine_source;
    assert_eq!(
        engine_source(false),
        ("target/webkit/bin", "tools/webkit/port.py")
    );
    let (dir, builder) = engine_source(true);
    assert_eq!(dir, "target/webkit/attack");
    assert!(builder.contains("attack_build.py"));
}

#[test]
fn not_built_says_what_to_run() {
    assert!(not_built(false).contains("tools/webkit/app.py"));
    assert!(not_built(false).contains("xui-webkit.elf"));
    assert!(not_built(true).contains("tools/xui/core_packages.py"));
}
