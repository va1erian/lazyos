//! webd, the daemon that owns WebKit for the session and serves light views (docs/webd.md,
//! docs/webd-plan.md), in the disk image.
//!
//! The program is `webd-app`: WebKit's engine (`webkit/port/app/`, `webkit/port/webd/`) in C++ with the
//! service in a Rust static library (`xui-app/webd`), linked by `tools/webkit/webd.py` in the WebKit build
//! container into `target/xui/xui-webd.elf`. Like every resident app it ships as a core package,
//! `os.lazy.webd` (`xui-app/packages/webd`, packed by `tools/xui/core_packages.py`), which
//! `xui_embed::embed_xui_apps` embeds in `/system/packages` when [`enabled`] (`LAZYOS_WEBD=1`) and `pkgd`
//! installs at boot; it opens with every session (`core_packages::autostart_shorts`, in the background).
//! The two processes it starts, `WebProcess` and `NetworkProcess`, are system programs in `/system/bin`
//! (`linuxapps_embed`, the same files LazyWeb+ uses), where an app of any label can start them.
//!
//! The first clients are the demo (`os.lazy.webddemo`, [`CLIENT_PACKAGES`]) and, later, Mail, Docs and
//! LazyWeb+. A client is a window, so webd wants the desktop; it needs no network stack (a light view
//! has no network at all): the switch is refused without `LAZYOS_DESKTOP=1` and nothing else is asked.
//! The program is tens of MB and loaded whole into RAM, so the image wants the big-app profile and a
//! larger OS volume (`run_demo.py` picks both).
//!
//! With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/webd`).
pub const PACKAGE_SHORT: &str = "webd";

/// The built daemon under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-webd.elf";

/// The packages that are clients of webd and ship only with it: `(short id, packaged program)`. A
/// client lives in its own tree (`xui-app/packages/<short>`) and is listed in
/// `tools/xui/core_packages.py` like any app; this table only says which of them an image with
/// `LAZYOS_WEBD=1` should carry. A client whose tree is absent (a branch that does not have it
/// yet) is skipped, so the daemon builds alone ([`client_tree_present`]).
pub const CLIENT_PACKAGES: &[(&str, &str)] = &[("webddemo", "xui-webddemo.elf")];

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether the image ships webd (`LAZYOS_WEBD=1`). Panics when the switch is set without the profile
/// it needs ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    let wanted = on("LAZYOS_WEBD");
    if let Err(reason) = requirements(wanted, desktop) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `webd` can ship it: the switch needs the desktop profile (the clients are
/// windows, and `init` runs a resident app in a session).
pub fn requirements(webd: bool, desktop: bool) -> Result<(), String> {
    if !webd || desktop {
        return Ok(());
    }
    Err(String::from(
        "LAZYOS_WEBD=1 needs LAZYOS_DESKTOP=1 (webd is a resident app of a login session and its \
         clients are windows); `python tools/run_demo.py --webd` sets it",
    ))
}

/// Whether the client package `short` has a tree in this checkout (`xui-app/packages/<short>`).
pub fn client_tree_present(manifest_dir: &std::path::Path, short: &str) -> bool {
    manifest_dir
        .join("xui-app")
        .join("packages")
        .join(short)
        .is_dir()
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_WEBD=1 but core package {PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_WEBD=1 but target/xui/{ELF} is not built (Docker, then \
             `python tools/webkit/deps.py`, `python tools/webkit/port.py` and \
             `python tools/webkit/webd.py`; then `python tools/xui/build.py` packages it)"
        )
    }
}
