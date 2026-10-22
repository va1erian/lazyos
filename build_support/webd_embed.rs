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
//! (`linuxapps_embed`), where an app of any label can start them. They are webd's alone: LazyWeb+
//! (`os.lazy.lazywebplus`, `LAZYOS_LAZYWEBPLUS=1`, [`lazywebplus_enabled`]) is a plain xui app, a client of
//! webd, so its switch needs webd shipped (an unset `LAZYOS_WEBD` is `1` with it, `0` is refused).
//!
//! Its clients are Docs and Mail (their pages are light views, docs/webd-plan.md P3), so **a desktop image
//! ships it**: `LAZYOS_WEBD` is `1` (required: a missing program fails the build), `0` (left out; Docs and Mail
//! then say the web engine is not running) or unset, which means "on a desktop whenever `target/xui/xui-webd.elf`
//! has been built" ([`Switch::Auto`]), so a checkout without Docker and WebKit still builds a desktop and
//! says so in a warning. The test clients (`os.lazy.webddemo` and `os.lazy.webdbrowse`, [`CLIENT_PACKAGES`])
//! are test fixtures and ship only with `LAZYOS_WEBDDEMO=1`. A client is a window, so webd wants the desktop; it needs no network stack
//! (a light view has no network at all): the switch is refused without `LAZYOS_DESKTOP=1` and nothing else is
//! asked. The program is tens of MB and loaded whole into RAM, so the image wants a larger OS volume
//! (`os_disk::default_size`).
//!
//! On a non-desktop image nothing changes.

use std::ffi::OsStr;
use std::path::PathBuf;

/// The core package's short id (`xui-app/packages/webd`).
pub const PACKAGE_SHORT: &str = "webd";

/// The built daemon under `target/xui/`, the packager's input.
pub const ELF: &str = "xui-webd.elf";

/// LazyWeb+'s core package (`xui-app/packages/lazywebplus`) and its program, built with the other apps by
/// `tools/xui/build.py`.
pub const LAZYWEBPLUS_PACKAGE_SHORT: &str = "lazywebplus";
pub const LAZYWEBPLUS_ELF: &str = "xui-lazywebplus.elf";

/// The packages that are test clients of webd and ship only with `LAZYOS_WEBDDEMO=1`: `(short id,
/// packaged program)`. A client lives in its own tree (`xui-app/packages/<short>`) and is listed in
/// `tools/xui/core_packages.py` like any app; this table only says which of them an image with the
/// switch should carry. A client whose tree is absent (a branch that does not have it yet) is skipped,
/// so the daemon builds alone ([`client_tree_present`]).
/// `webddemo` shows a light view; `webdbrowse` (Web View Test) drives full views through every scope
/// against the harness server.
pub const CLIENT_PACKAGES: &[(&str, &str)] = &[
    ("webddemo", "xui-webddemo.elf"),
    ("webdbrowse", "xui-webdbrowse.elf"),
];

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// What `LAZYOS_WEBD` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// `0`: leave it out.
    Off,
    /// Unset (or anything else): a desktop ships it when the program has been built.
    Auto,
    /// `1`: ship it; a missing program is an error.
    On,
}

/// [`Switch`] of a `LAZYOS_WEBD` value (`None`: unset).
pub fn parse_switch(value: Option<&OsStr>) -> Switch {
    match value.and_then(OsStr::to_str) {
        Some("1") => Switch::On,
        Some("0") => Switch::Off,
        _ => Switch::Auto,
    }
}

/// The switch an image is built with: `LAZYOS_WEBD`, but a LazyWeb+ image (`lazywebplus`) cannot leave it to
/// chance, so an unset one is `1` there (a missing program then fails the build with [`not_built`]).
pub fn effective_switch(switch: Switch, lazywebplus: bool) -> Switch {
    if lazywebplus && switch == Switch::Auto {
        Switch::On
    } else {
        switch
    }
}

/// Whether an image with this `switch` ships webd.
pub fn shipped(switch: Switch, desktop: bool, built: bool) -> bool {
    match switch {
        Switch::On => true,
        Switch::Off => false,
        Switch::Auto => desktop && built,
    }
}

/// The built daemon, `target/xui/xui-webd.elf` of this checkout.
fn built_elf() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("xui")
        .join(ELF)
}

/// The engine processes the daemon starts, `target/webkit/bin/` (`linuxapps_embed` puts them in `/system/bin`).
const ENGINE_PROCESSES: [&str; 2] = ["WebProcess", "NetworkProcess"];

/// Whether the daemon and the two processes it starts are all built: a daemon without them could not start a
/// view, so an unset switch does not ship it.
fn built() -> bool {
    // A missing file is watched through its directory, never by name (cargo takes that for "always changed").
    println!("cargo:rerun-if-changed=target/webkit/bin");
    let dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("target");
    built_elf().is_file()
        && ENGINE_PROCESSES
            .iter()
            .all(|name| dir.join("webkit").join("bin").join(name).is_file())
}

/// [`effective_switch`] of `LAZYOS_WEBD` and `LAZYOS_LAZYWEBPLUS`.
fn env_switch() -> Switch {
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBD");
    let switch = parse_switch(std::env::var_os("LAZYOS_WEBD").as_deref());
    effective_switch(switch, on("LAZYOS_LAZYWEBPLUS"))
}

/// Whether the image ships webd ([`Switch`]), from the environment: what the build modules that size
/// the volume, pick the engine processes and the autostart list ask. No panic.
pub fn shipped_env() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBD");
    println!("cargo:rerun-if-env-changed=LAZYOS_DESKTOP");
    let desktop = on("LAZYOS_DESKTOP");
    shipped(env_switch(), desktop, built())
}

/// Whether the image ships webd (see [`Switch`]). Panics when `LAZYOS_WEBD=1` is set without the
/// profile it needs ([`requirements`]); warns when a desktop was left without it.
pub fn enabled(desktop: bool) -> bool {
    let switch = env_switch();
    if let Err(reason) = requirements(switch == Switch::On, desktop) {
        panic!("{reason}");
    }
    let built = built();
    if desktop && !built && switch == Switch::Auto {
        println!(
            "cargo:warning=this desktop ships without webd (target/xui/{ELF} or the engine processes in target/webkit/bin are not built): Docs and Mail will \
             say the web engine is not running; build it with `python tools/run_demo.py --webd` or \
             `python tools/webkit/webd.py` (Docker), or silence this with LAZYOS_WEBD=0"
        );
    }
    shipped(switch, desktop, built)
}

/// Whether the image carries the test clients ([`CLIENT_PACKAGES`]): `LAZYOS_WEBDDEMO=1`.
pub fn demo_clients() -> bool {
    on("LAZYOS_WEBDDEMO")
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

/// Whether the image ships LazyWeb+ (`LAZYOS_LAZYWEBPLUS=1`). Panics when the switch is set without the profile
/// it needs ([`lazywebplus_requirements`]).
pub fn lazywebplus_enabled(desktop: bool) -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBD");
    let wanted = on("LAZYOS_LAZYWEBPLUS");
    let webd = parse_switch(std::env::var_os("LAZYOS_WEBD").as_deref());
    if let Err(reason) = lazywebplus_requirements(wanted, desktop, on("LAZYOS_NETD"), webd) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `lazywebplus` can ship it: the switch needs the desktop profile, the network stack
/// (the pages come over webd's NetworkProcess) and webd, which every desktop ships unless `LAZYOS_WEBD=0`
/// (`webd`, the switch as set: unset is fine, the browser makes it required).
pub fn lazywebplus_requirements(
    lazywebplus: bool,
    desktop: bool,
    netd: bool,
    webd: Switch,
) -> Result<(), String> {
    if !lazywebplus {
        return Ok(());
    }
    if webd == Switch::Off {
        return Err(String::from(
            "LAZYOS_LAZYWEBPLUS=1 needs webd (LAZYOS_WEBD=0 leaves it out): the browser draws the pages \
             webd renders; unset LAZYOS_WEBD or set it to 1",
        ));
    }
    if desktop && netd {
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

/// Where webd's two engine processes come from, and what builds them: the WebKit port
/// (`tools/webkit/port.py`), or with `attack` (`LAZYOS_WEBKIT_ATTACK=1`) the stand-ins of the sandbox
/// attack harness (`tools/webkit/attack_build.py`, `tools/webkit/attack_run.py`): one test program,
/// under the two programs' names, that attacks from inside their labels.
pub fn engine_source(attack: bool) -> (&'static str, &'static str) {
    if attack {
        ("target/webkit/attack", "tools/webkit/attack_build.py")
    } else {
        ("target/webkit/bin", "tools/webkit/port.py")
    }
}

/// Whether the client package `short` has a tree in this checkout (`xui-app/packages/<short>`).
pub fn client_tree_present(manifest_dir: &std::path::Path, short: &str) -> bool {
    manifest_dir
        .join("xui-app")
        .join("packages")
        .join(short)
        .is_dir()
}

/// The build failure for a wanted but unbuilt LazyWeb+, naming what to run.
pub fn lazywebplus_not_built(elf_present: bool) -> String {
    if elf_present {
        format!(
            "LAZYOS_LAZYWEBPLUS=1 but core package {LAZYWEBPLUS_PACKAGE_SHORT} is not packaged; run \
             `python tools/xui/core_packages.py`"
        )
    } else {
        format!(
            "LAZYOS_LAZYWEBPLUS=1 but target/xui/{LAZYWEBPLUS_ELF} is not built; run \
             `python tools/xui/build.py` (it builds the browser with the other apps, webd's own \
             program is `python tools/webkit/webd.py`)"
        )
    }
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
