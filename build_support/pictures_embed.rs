//! The Picture Viewer (docs/lazyrad-pictures.md) in the disk image.
//!
//! The viewer is a LazyRAD project (`lazyrad-os/samples/pictures`) on the
//! LazyRAD player, packed by `tools/xui/core_packages.py` as the core package
//! `os.lazy.pictures` (`bin/pictures.elf` is `lrplay`, the project and its
//! sample pictures under `resources/project`, the layout File -> Make LazyOS
//! App gives every LazyRAD app). `xui_embed::embed_xui_apps` embeds it in
//! `/system/packages` when [`enabled`] (`LAZYOS_PICTURES=1`) and `pkgd`
//! installs it at boot, registering it for PNG, JPEG, BMP and GIF pictures.
//!
//! A viewer needs the desktop: the switch is refused without
//! `LAZYOS_DESKTOP=1` instead of producing an image whose viewer cannot open
//! a window. `python tools/run_demo.py --pictures` and the launcher's
//! "Picture Viewer" set both. With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/pictures`).
pub const PACKAGE_SHORT: &str = "pictures";

/// Whether the image ships the Picture Viewer (`LAZYOS_PICTURES=1`). Panics
/// when the switch is set without the desktop ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_PICTURES");
    let wanted = std::env::var_os("LAZYOS_PICTURES").as_deref() == Some(OsStr::new("1"));
    if let Err(reason) = requirements(wanted, desktop) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `pictures` can ship it: the switch needs the desktop.
pub fn requirements(pictures: bool, desktop: bool) -> Result<(), String> {
    if !pictures || desktop {
        return Ok(());
    }
    Err("LAZYOS_PICTURES=1 needs LAZYOS_DESKTOP=1 (the viewer is a desktop app); \
         `python tools/run_demo.py --pictures` sets both"
        .to_owned())
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built() -> String {
    format!(
        "LAZYOS_PICTURES=1 but core package {PACKAGE_SHORT} is not built; run \
         `python tools/lazyrad/build.py` and `python tools/xui/core_packages.py`"
    )
}
