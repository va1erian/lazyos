//! `LAZYOS_PICTURES=1` (docs/lazyrad-pictures.md): refused without the
//! desktop, and a clear message when the package is not built.

use crate::pictures_embed::{not_built, requirements, PACKAGE_SHORT};

#[test]
fn unset_needs_nothing() {
    assert_eq!(requirements(false, false), Ok(()));
}

#[test]
fn the_desktop_ships_it() {
    assert_eq!(requirements(true, true), Ok(()));
}

#[test]
fn without_the_desktop_it_is_refused_with_the_fix() {
    let error = requirements(true, false).unwrap_err();
    assert!(error.contains("LAZYOS_DESKTOP=1"), "{error}");
    assert!(error.contains("run_demo.py --pictures"), "{error}");
}

#[test]
fn not_built_says_what_to_run() {
    let message = not_built();
    assert!(message.contains("tools/lazyrad/build.py"), "{message}");
    assert!(message.contains("tools/xui/core_packages.py"), "{message}");
    assert_eq!(PACKAGE_SHORT, "pictures");
}
