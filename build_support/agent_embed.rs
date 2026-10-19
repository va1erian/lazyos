//! LazyAgent, the AI chat and diagnostics agent (docs/agent.md), in the disk image.
//!
//! LazyAgent is a desktop xui app (`xui-app/crates/agent-core` and
//! `agent-ui`, launched by `xui-app/src/bin/agent.rs`, built by
//! `tools/xui/build.py` into `target/xui/xui-agent.elf`). Like every
//! desktop app it ships as a core package, `os.lazy.agent`
//! (`xui-app/packages/agent`, packed by `tools/xui/core_packages.py`),
//! which `xui_embed::embed_xui_apps` embeds in `/system/packages` when
//! [`enabled`] (`LAZYOS_AGENT=1`) and `pkgd` installs at boot.
//!
//! An AI chat client needs the desktop and the network stack: the switch is
//! refused without `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` instead of
//! producing an image whose client cannot open a window or a socket. The
//! front ends set both (`python tools/run_demo.py --agent`, the launcher's
//! "AI Agent"). HTTPS needs nothing more from the image: the CA
//! bundle, `/etc/hosts` and `/etc/resolv.conf` are in every image
//! (`ca_bundle`, `hosts_embed`).
//!
//! With the switch unset nothing changes.

use std::ffi::OsStr;

/// The core package's short id (`xui-app/packages/agent`).
pub const PACKAGE_SHORT: &str = "agent";

/// The built client under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-agent.elf";

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether the image ships LazyAgent (`LAZYOS_AGENT=1`). Panics when the
/// switch is set without the profile it needs ([`requirements`]).
pub fn enabled(desktop: bool) -> bool {
    let wanted = on("LAZYOS_AGENT");
    if let Err(reason) = requirements(wanted, desktop, on("LAZYOS_NETD")) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `agent` can ship it: the switch needs the desktop
/// profile and the network stack.
pub fn requirements(agent: bool, desktop: bool, netd: bool) -> Result<(), String> {
    if !agent || (desktop && netd) {
        return Ok(());
    }
    let missing: Vec<&str> = [("LAZYOS_DESKTOP=1", desktop), ("LAZYOS_NETD=1", netd)]
        .into_iter()
        .filter(|(_, set)| !set)
        .map(|(name, _)| name)
        .collect();
    Err(format!(
        "LAZYOS_AGENT=1 needs {} (an AI chat client needs the desktop and the network stack); \
         `python tools/run_demo.py --agent` sets them",
        missing.join(" and ")
    ))
}

/// The build failure for a wanted but unbuilt package, naming what to run.
pub fn not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_AGENT=1 but core package {PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_AGENT=1 but target/xui/{ELF} is not built; run \
             `python tools/xui/build.py`"
        )
    }
}
