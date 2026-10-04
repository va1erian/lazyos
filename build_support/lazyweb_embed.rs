//! LazyWeb, the web browser (docs/lazyweb.md), in the disk image.
//!
//! LazyWeb is a desktop xui app on the NetSurf engine (`xui-app/web`, built
//! with zig by `tools/xui/build.py` into `target/xui/xui-lazyweb.elf`). Like
//! every desktop app it ships as a core package, `os.lazy.lazyweb`
//! (`xui-app/packages/lazyweb`, packed by `tools/xui/core_packages.py`), which
//! `xui_embed::embed_xui_apps` embeds in `/system/packages` when [`enabled`]
//! (`LAZYOS_LAZYWEB=1`) and `pkgd` installs at boot.
//!
//! A browser needs the desktop and the network stack: the switch is refused
//! without `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` instead of producing an
//! image whose browser cannot open a window or a socket. The front ends set
//! all of them (`python tools/run_demo.py --lazyweb`, the launcher's
//! "LazyWeb browser"), plus `LAZYOS_TLS=1` for `curl` beside it. HTTPS needs
//! nothing more from the image: the CA bundle, `/etc/hosts` and
//! `/etc/resolv.conf` are in every image (`ca_bundle`, `hosts_embed`).
//!
//! With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/lazyweb`).
pub const PACKAGE_SHORT: &str = "lazyweb";

/// The built browser under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-lazyweb.elf";

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether the image ships LazyWeb (`LAZYOS_LAZYWEB=1`). Panics when the
/// switch is set without the profile it needs ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    let wanted = on("LAZYOS_LAZYWEB");
    if let Err(reason) = requirements(wanted, desktop, on("LAZYOS_NETD")) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `lazyweb` can ship it: the switch needs the desktop
/// profile and the network stack.
pub fn requirements(lazyweb: bool, desktop: bool, netd: bool) -> Result<(), String> {
    if !lazyweb || (desktop && netd) {
        return Ok(());
    }
    let missing: Vec<&str> = [("LAZYOS_DESKTOP=1", desktop), ("LAZYOS_NETD=1", netd)]
        .into_iter()
        .filter(|(_, set)| !set)
        .map(|(name, _)| name)
        .collect();
    Err(format!(
        "LAZYOS_LAZYWEB=1 needs {} (a browser needs the desktop and the network stack); \
         `python tools/run_demo.py --lazyweb` sets them",
        missing.join(" and ")
    ))
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_LAZYWEB=1 but core package {PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_LAZYWEB=1 but target/xui/{ELF} is not built (NetSurf is C, compiled \
             with zig: `pip install ziglang==0.16.0`, then `python tools/xui/build.py`)"
        )
    }
}
