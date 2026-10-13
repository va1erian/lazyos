//! LazyWebPlus, LazyWeb's browser window on WebKit (docs/lazywebplus.md), in the disk image.
//!
//! The app is `lazywebplus-app`, WebKit's UI process with LazyWeb's window (`webkit/port/app/` in
//! C++ and `xui-app/webplus` in Rust, linked by `tools/webkit/app.py --plus` in the WebKit build
//! container into `target/xui/xui-lazywebplus.elf`). Like every desktop app it ships as a core
//! package, `os.lazy.lazywebplus` (`xui-app/packages/lazywebplus`, packed by
//! `tools/xui/core_packages.py`), which `xui_embed::embed_xui_apps` embeds in `/system/packages`
//! when [`enabled`] (`LAZYOS_LAZYWEBPLUS=1`) and `pkgd` installs at boot. The two processes it
//! starts, `WebProcess` and `NetworkProcess`, are system programs in `/system/bin`
//! (`linuxapps_embed`), where an app of any label can start them.
//!
//! A browser needs the desktop and the network stack: the switch is refused without
//! `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` instead of producing an image whose browser cannot open
//! a window or a socket. The front ends set all of them (`python tools/run_demo.py
//! --lazywebplus`, the launcher's "LazyWeb+ browser"); HTTPS needs nothing more from the image.
//! Every program is tens of MB and loaded whole into RAM, so the image wants the big-app profile
//! and a larger OS volume (`run_demo.py` picks both).
//!
//! With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/lazywebplus`).
pub const PACKAGE_SHORT: &str = "lazywebplus";

/// The built browser under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-lazywebplus.elf";

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether the image ships LazyWebPlus (`LAZYOS_LAZYWEBPLUS=1`). Panics when the switch is set
/// without the profile it needs ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    let wanted = on("LAZYOS_LAZYWEBPLUS");
    if let Err(reason) = requirements(wanted, desktop, on("LAZYOS_NETD")) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `lazywebplus` can ship it: the switch needs the desktop profile and the
/// network stack.
pub fn requirements(lazywebplus: bool, desktop: bool, netd: bool) -> Result<(), String> {
    if !lazywebplus || (desktop && netd) {
        return Ok(());
    }
    let missing: Vec<&str> = [("LAZYOS_DESKTOP=1", desktop), ("LAZYOS_NETD=1", netd)]
        .into_iter()
        .filter(|(_, set)| !set)
        .map(|(name, _)| name)
        .collect();
    Err(format!(
        "LAZYOS_LAZYWEBPLUS=1 needs {} (a browser needs the desktop and the network stack); \
         `python tools/run_demo.py --lazywebplus` sets them",
        missing.join(" and ")
    ))
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_LAZYWEBPLUS=1 but core package {PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_LAZYWEBPLUS=1 but target/xui/{ELF} is not built (Docker, then \
             `python tools/webkit/deps.py`, `python tools/webkit/port.py` and \
             `python tools/webkit/app.py --plus`; then `python tools/xui/build.py` packages it)"
        )
    }
}
