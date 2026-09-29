//! `init`'s app registry (issue #158): what the start menu lists and what
//! `Launch` resolves an app id to, plus which rows the image actually ships
//! and which of them the desktop session opens at boot (issues #215/#216).
//!
//! Split out of `init.rs`, which is far past the file-size budget.
//!
//! # Availability
//!
//! A row is [`Ship::Always`] when its ELF is part of every services image
//! (`SH.ELF`, `MSGCTL.ELF`, `TOP.ELF`) and [`Ship::Manifest`] when it exists
//! only if the image builder embedded it. The builder lists what it embedded
//! in `XAPPS.LST` (one 8.3 name per line, optionally followed by `autostart`);
//! [`load_manifest`] reads it once at boot. A row whose ELF is not shipped is
//! *unavailable*: `ListApps` omits it and `Launch` refuses it with `-ENOENT`
//! without logging a failure, so the registry never advertises (or noisily
//! fails to start) a program the image does not carry.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use user::messenger::services;
use user::sys;

use super::Restart;

/// The image builder's list of shipped optional apps.
const MANIFEST_FILE: &str = "XAPPS.LST\0";
/// The most bytes of the manifest read (a few 8.3 names per line).
const MANIFEST_BYTES: usize = 1024;

/// Whether a row's ELF is always in the image.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ship {
    /// Part of every services image.
    Always,
    /// Present only when listed in `XAPPS.LST`.
    Manifest,
}

/// One app-registry row: what the start menu enumerates and what `Launch`
/// resolves an app id to.
pub struct AppSpec {
    /// Lowercase program stem (`top`, `editor`); `mimed` registers these ids.
    pub id: &'static str,
    /// Display name for menus.
    pub name: &'static str,
    /// On-disk ELF path (8.3 on the FAT boot image).
    pub path: &'static str,
    /// Default restart policy for launches.
    pub restart: Restart,
    /// MIME verbs the app handles.
    pub verbs: &'static [&'static str],
    /// Whether the app is a static Linux-ABI (musl) program: the kernel's
    /// spawn needs the `linux:` personality prefix for those.
    pub linux: bool,
    /// Arguments every launch passes first (the desktop apps are `xuid`
    /// clients: `--client` keeps them off the display grant).
    pub args: &'static str,
    /// Whether the image is guaranteed to carry the ELF.
    pub ship: Ship,
}

/// A desktop (xui) app: Linux ABI, `xuid` client, listed in `XAPPS.LST`.
const fn xui_app(id: &'static str, name: &'static str, path: &'static str) -> AppSpec {
    AppSpec {
        id,
        name,
        path,
        restart: Restart::OnFailure,
        verbs: &["open"],
        linux: true,
        args: "--client",
        ship: Ship::Manifest,
    }
}

/// A native program the image may ship (`Manifest`) or always ships.
const fn native_app(
    id: &'static str,
    name: &'static str,
    path: &'static str,
    restart: Restart,
    verbs: &'static [&'static str],
    ship: Ship,
) -> AppSpec {
    AppSpec {
        id,
        name,
        path,
        restart,
        verbs,
        linux: false,
        args: "",
        ship,
    }
}

/// The built-in app registry. The first four ids are exactly the ones
/// `mimed`'s open-with defaults register (`editor`, `files`, `viewer`,
/// `runner`), so an `Open` resolution names an app the supervisor knows; no
/// image ships their ELFs yet, so they are unavailable until one does. The
/// rest are launchable system programs (`top` proves the path end to end in a
/// headless boot) and the desktop's xui apps.
pub const APPS: &[AppSpec] = &[
    native_app(
        "editor",
        "Editor",
        "EDITOR.ELF",
        Restart::OnFailure,
        &["open", "edit"],
        Ship::Manifest,
    ),
    native_app(
        "files",
        "Files",
        "FILES.ELF",
        Restart::OnFailure,
        &["open", "reveal"],
        Ship::Manifest,
    ),
    native_app(
        "viewer",
        "Image Viewer",
        "VIEW.ELF",
        Restart::OnFailure,
        &["open", "reveal"],
        Ship::Manifest,
    ),
    native_app(
        "runner",
        "Program Runner",
        "RUNNER.ELF",
        Restart::Once,
        &["open"],
        Ship::Manifest,
    ),
    // The desktop Terminal hosts the shell in a `xuid` window; `shell` is the
    // console `sh` (it draws in `init`'s mux window, so it is only useful in
    // a console session).
    xui_app("terminal", "Terminal", "XTERM.ELF"),
    native_app(
        "shell",
        "Console Shell",
        "SH.ELF",
        Restart::OnFailure,
        &["open"],
        Ship::Always,
    ),
    xui_app("sysmon", "System Monitor", "XSYSMON.ELF"),
    xui_app("fabricmon", "Fabric Monitor", "XFABMON.ELF"),
    xui_app("counter", "Counter", "XCOUNTR.ELF"),
    native_app(
        "top",
        "System Monitor (text)",
        "TOP.ELF",
        Restart::Once,
        &["open"],
        Ship::Always,
    ),
    native_app(
        "messengerctl",
        "Messenger Console",
        "MSGCTL.ELF",
        Restart::OnFailure,
        &[],
        Ship::Always,
    ),
];

/// Bit `i` is set when `APPS[i]` is shipped (see [`load_manifest`]).
static AVAILABLE: AtomicU64 = AtomicU64::new(0);
/// Bit `i` is set when `APPS[i]` opens at boot.
static AUTOSTART: AtomicU64 = AtomicU64::new(0);

// The masks are 64 bits wide.
const _: () = assert!(APPS.len() <= 64);

/// The app registry row for `id` (case-insensitive), if any.
pub fn find_app(id: &str) -> Option<&'static AppSpec> {
    APPS.iter()
        .find(|app| app.id.eq_ignore_ascii_case(id.trim()))
}

/// Whether `app`'s ELF is shipped in this image.
pub fn is_available(app: &AppSpec) -> bool {
    APPS.iter()
        .position(|row| core::ptr::eq(row, app))
        .is_some_and(|index| AVAILABLE.load(Ordering::Relaxed) & (1 << index) != 0)
}

/// The ids of the shipped apps that open at boot, in registry order.
pub fn autostart_ids() -> Vec<&'static str> {
    let mask = AUTOSTART.load(Ordering::Relaxed);
    APPS.iter()
        .enumerate()
        .filter(|(index, _)| mask & (1 << index) != 0)
        .map(|(_, app)| app.id)
        .collect()
}

/// Record what the image ships, from the `XAPPS.LST` text (`NAME.ELF
/// [autostart]` per line). `Always` rows are shipped regardless; a name the
/// registry does not know is ignored.
pub fn apply_manifest(text: &str) {
    let mut available = 0u64;
    let mut autostart = 0u64;
    for (index, app) in APPS.iter().enumerate() {
        if app.ship == Ship::Always {
            available |= 1 << index;
        }
    }
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { continue };
        let Some(index) = APPS
            .iter()
            .position(|app| app.path.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        available |= 1 << index;
        if words.any(|word| word == "autostart") {
            autostart |= 1 << index;
        }
    }
    AVAILABLE.store(available, Ordering::Relaxed);
    AUTOSTART.store(autostart, Ordering::Relaxed);
}

/// Read `XAPPS.LST` from the boot volume and apply it. An image without the
/// file ships no optional apps.
pub fn load_manifest() {
    let mut buffer = [0u8; MANIFEST_BYTES];
    let text = match sys::read_file(MANIFEST_FILE.as_bytes(), &mut buffer) {
        Some(count) => core::str::from_utf8(&buffer[..count.min(MANIFEST_BYTES)]).unwrap_or(""),
        None => "",
    };
    apply_manifest(text);
}

/// The registry as wire rows for `ListApps`: shipped apps only.
pub fn app_infos() -> Vec<services::AppInfo> {
    APPS.iter()
        .filter(|app| is_available(app))
        .map(|app| services::AppInfo {
            id: app.id.to_string(),
            name: app.name.to_string(),
            path: app.path.to_string(),
            restart: app.restart.label().to_string(),
            verbs: app.verbs.iter().map(|verb| verb.to_string()).collect(),
        })
        .collect()
}

/// The registry self-test: every row is well formed and the ids `mimed`
/// registers are present. Prints `INIT:APPS:PASS` (the count is the whole
/// registry) and, once the manifest is applied, `INIT:APPS:SHIPPED` with the
/// number of rows this image can launch.
pub fn selftest_apps() -> String {
    let mut ok = !APPS.is_empty();
    for app in APPS {
        let verbs = app.verbs.len();
        ok &= !app.id.is_empty()
            && !app.name.is_empty()
            && app.path.ends_with(".ELF")
            && (verbs == 0 || verbs <= 4);
    }
    let has_editor = APPS
        .iter()
        .find(|app| app.id == "editor")
        .map(|app| app.verbs.contains(&"open") && app.verbs.contains(&"edit"))
        .unwrap_or(false);
    let has_top = APPS
        .iter()
        .any(|app| app.id == "top" && app.path == "TOP.ELF");
    // The desktop rows must be launchable as `xuid` clients.
    let desktop_ok = APPS
        .iter()
        .filter(|app| app.linux)
        .all(|app| app.args == "--client" && app.ship == Ship::Manifest);
    ok &= has_editor && has_top && desktop_ok;
    let shipped = APPS.iter().filter(|app| is_available(app)).count();
    if ok {
        alloc::format!(
            "INIT:APPS:PASS count={}\nINIT:APPS:SHIPPED count={shipped}\n",
            APPS.len()
        )
    } else {
        String::from("INIT:APPS:FAIL registry is malformed\n")
    }
}
