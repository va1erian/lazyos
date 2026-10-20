//! `LAZYOS_INTELGFX=1` (docs/kabylake-gpu-plan.md section 8): refused without
//! a driver stack or without `devd`, accepted otherwise, and always off
//! when not asked for.

use crate::intelgfx_embed::requirements;

#[test]
fn off_is_always_accepted() {
    for driver_stack in [false, true] {
        for devd_off in [false, true] {
            assert_eq!(requirements(false, driver_stack, devd_off), Ok(()));
        }
    }
}

#[test]
fn the_switch_needs_a_driver_stack_and_devd() {
    assert_eq!(requirements(true, true, false), Ok(()));
    let stack = requirements(true, false, false).unwrap_err();
    assert!(
        stack.contains("LAZYOS_SOUND=1") && stack.contains("--intelgfx"),
        "{stack}"
    );
    let devd = requirements(true, true, true).unwrap_err();
    assert!(devd.contains("LAZYOS_DEVD=0"), "{devd}");
}
