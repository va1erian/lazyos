//! WebKit, the browser (docs/webkit-port-plan.md W5), in the disk image.
//!
//! The app is `webkit-app`, WebKit's UI process with its window (`webkit/port/app/` in
//! C++ and `xui-app/crates/webkit` in Rust, linked by `tools/webkit/app.py` in the WebKit
//! build container into `target/xui/xui-webkit.elf`). Like every desktop app it ships as a
//! core package, `org.lazy.webkit` (`xui-app/packages/webkit`, packed by
//! `tools/xui/core_packages.py`), which `xui_embed::embed_xui_apps` embeds in
//! `/system/packages` when [`enabled`] (`LAZYOS_WEBKIT=1`) and `pkgd` installs at boot. The two
//! processes it starts, `WebProcess` and `NetworkProcess`, are system programs in `/system/bin`
//! (`linuxapps_embed`), where an app of any label can start them.
//!
//! A browser needs the desktop and the network stack: the switch is refused without
//! `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` instead of producing an image whose browser cannot
//! open a window or a socket. The front ends set all of them (`python tools/run_demo.py
//! --webkit`, the launcher's "WebKit browser"); HTTPS needs nothing more from the image. Every
//! program is tens of MB and loaded whole into RAM, so the image wants the big-app profile and
//! a larger OS volume (`run_demo.py` picks both).
//!
//! `LAZYOS_WEBKIT_PORT=1` is the W4 proof instead: the three programs and no app, in a console
//! image (`tools/webkit/port_run.py`). With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/webkit`).
pub const PACKAGE_SHORT: &str = "webkit";

/// The built browser under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-webkit.elf";

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether the image ships the WebKit browser (`LAZYOS_WEBKIT=1`). Panics when the switch is
/// set without the profile it needs ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    let wanted = on("LAZYOS_WEBKIT");
    if let Err(reason) = requirements(wanted, desktop, on("LAZYOS_NETD")) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `webkit` can ship it: the switch needs the desktop profile and the
/// network stack.
pub fn requirements(webkit: bool, desktop: bool, netd: bool) -> Result<(), String> {
    if !webkit || (desktop && netd) {
        return Ok(());
    }
    let missing: Vec<&str> = [("LAZYOS_DESKTOP=1", desktop), ("LAZYOS_NETD=1", netd)]
        .into_iter()
        .filter(|(_, set)| !set)
        .map(|(name, _)| name)
        .collect();
    Err(format!(
        "LAZYOS_WEBKIT=1 needs {} (a browser needs the desktop and the network stack); \
         `python tools/run_demo.py --webkit` sets them (LAZYOS_WEBKIT_PORT=1 is the console proof)",
        missing.join(" and ")
    ))
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_WEBKIT=1 but core package {PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_WEBKIT=1 but target/xui/{ELF} is not built (Docker, then \
             `python tools/webkit/deps.py`, `python tools/webkit/port.py` and \
             `python tools/webkit/app.py`; then `python tools/xui/build.py` packages it)"
        )
    }
}
