//! `LAZYOS_LAZYWEBPLUS=1` (docs/lazywebplus.md): refused without the desktop and the network
//! stack, and a clear message when the browser is not built.

use crate::lazywebplus_embed::{engine_source, not_built, requirements, ELF, PACKAGE_SHORT};

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
    assert!(both.contains("run_demo.py --lazywebplus"));
}

#[test]
fn not_built_says_what_to_run() {
    assert!(not_built(false).contains("tools/webkit/app.py"));
    assert!(not_built(false).contains(ELF));
    assert!(not_built(true).contains("tools/xui/core_packages.py"));
}

#[test]
fn the_package_is_os_lazy_lazywebplus() {
    assert_eq!(PACKAGE_SHORT, "lazywebplus");
    assert_eq!(ELF, "xui-lazywebplus.elf");
}

#[test]
fn the_engine_processes_come_from_the_port_unless_the_attack_harness_asks() {
    assert_eq!(
        engine_source(false),
        ("target/webkit/bin", "tools/webkit/port.py")
    );
    let (dir, builder) = engine_source(true);
    assert_eq!(dir, "target/webkit/attack");
    assert!(builder.contains("attack_build.py"));
}
