//! The Intel GPU service in the disk image (`LAZYOS_INTELGFX=1`;
//! docs/kabylake-gpu-plan.md stage G1, section 7 and 8): `gpud`, which
//! `devd` starts under `init`'s `gpud` row when it finds the HD Graphics 620
//! (`8086:5916`, `libs/devmatch`).
//!
//! Off by default and nothing else changes with it: no file, no `init` row
//! and no `devd` match without the switch (`user/build.rs` sets the matching
//! `cfg(lazyos_intelgfx)`). It needs `devd`, because only `devd` starts a
//! driver for the device it found, so a desktop (which ships the sound
//! stack), `LAZYOS_SOUND=1` or the network stack, and no `LAZYOS_DEVD=0`.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::os_image::Sink;

/// Whether a build with `intelgfx` can ship it: `devd` must be in the image
/// (`driver_stack`: a sound or network stack is there to start it for) and
/// not switched off.
pub fn requirements(intelgfx: bool, driver_stack: bool, devd_off: bool) -> Result<(), String> {
    if intelgfx && !driver_stack {
        return Err(
            "LAZYOS_INTELGFX=1 needs a driver stack for devd to start gpud from: a desktop, \
             LAZYOS_SOUND=1 or LAZYOS_NETD=1; `python tools/run_demo.py --intelgfx` sets it"
                .into(),
        );
    }
    if intelgfx && devd_off {
        return Err(
            "LAZYOS_INTELGFX=1 needs devd (drop LAZYOS_DEVD=0): only devd starts gpud, for \
             the device it found"
                .into(),
        );
    }
    Ok(())
}

/// Whether the image ships `gpud` (`LAZYOS_INTELGFX=1`); panics on a build
/// that cannot ([`requirements`]).
pub fn enabled(driver_stack: bool) -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_INTELGFX");
    println!("cargo:rerun-if-env-changed=LAZYOS_DEVD");
    let on = std::env::var_os("LAZYOS_INTELGFX").as_deref() == Some(OsStr::new("1"));
    let devd_off = std::env::var_os("LAZYOS_DEVD").as_deref() == Some(OsStr::new("0"));
    if let Err(reason) = requirements(on, driver_stack, devd_off) {
        panic!("{reason}");
    }
    on
}

/// Add `gpud` when [`enabled`].
pub fn embed(sink: &mut dyn Sink, driver_stack: bool) {
    if !enabled(driver_stack) {
        return;
    }
    let artifact = std::env::var_os("CARGO_BIN_FILE_USER_gpud")
        .unwrap_or_else(|| panic!("user gpud artifact not found"));
    sink.add_file(fhs::bin::GPUD, PathBuf::from(artifact));
}
