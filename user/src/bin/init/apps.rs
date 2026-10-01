//! `init`'s app registry (issue #158): what the start menu lists and what
//! `Launch` resolves an app id to, plus which rows the image actually ships
//! and which of them the desktop session opens at boot (issues #215/#216).
//!
//! Split out of `init.rs`, which is far past the file-size budget.
//!
//! # Availability
//!
//! A row is [`Ship::Always`] when its ELF (or, for the shell, the `BUSYBOX`
//! applet alias) is part of every services image (`sh`, `MSGCTL.ELF`,
//! `TOP.ELF`) and [`Ship::Manifest`] when it exists
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

use super::state::Restart;

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

/// An [`xui_app`] with its own MIME verbs (the document apps `mimed` opens).
const fn xui_app_verbs(
    id: &'static str,
    name: &'static str,
    path: &'static str,
    verbs: &'static [&'static str],
) -> AppSpec {
    AppSpec {
        verbs,
        ..xui_app(id, name, path)
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

/// A Linux-ABI console program the services image always ships: BusyBox's
/// `sh` (a bare applet name the kernel's Linux loader aliases to `BUSYBOX`,
/// issue #254), which draws in `init`'s mux window. Unlike [`xui_app`] it is
/// not a `xuid` client and takes no arguments.
const fn linux_console_app(id: &'static str, name: &'static str, path: &'static str) -> AppSpec {
    AppSpec {
        id,
        name,
        path,
        restart: Restart::OnFailure,
        verbs: &["open"],
        linux: true,
        args: "",
        ship: Ship::Always,
    }
}

/// The built-in app registry. `editor`, `files`, `paint`, `viewer` and
/// `runner` are the ids `mimed`'s open-with defaults register, so an `Open`
/// resolution names an app the supervisor knows; `editor`, `files` and `paint`
/// are xui desktop apps (shipped with the desktop image, launched on demand),
/// `viewer` and `runner` have no ELF yet and stay unavailable. The
/// rest are launchable system programs (`top` proves the path end to end in a
/// headless boot) and the desktop's xui apps.
///
/// A `static`, not a `const`: [`is_available`] identifies a row by address, so
/// the table must have one stable storage location.
pub static APPS: &[AppSpec] = &[
    xui_app_verbs("editor", "Editor", "XEDITOR.ELF", &["open", "edit"]),
    xui_app_verbs("files", "Files", "XFILES.ELF", &["open", "reveal"]),
    xui_app_verbs("paint", "Paint", "XPAINT.ELF", &["open", "edit"]),
    xui_app("settings", "Settings", "XSETTNG.ELF"),
    xui_app("confd", "Config", "XCONFD.ELF"),
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
    // console shell (BusyBox `sh`; it draws in `init`'s mux window, so it is
    // only useful in a console session).
    xui_app("terminal", "Terminal", "XTERM.ELF"),
    linux_console_app("shell", "Console Shell", "sh"),
    xui_app("sysmon", "System Monitor", "XSYSMON.ELF"),
    xui_app("fabricmon", "Fabric Monitor", "XFABMON.ELF"),
    xui_app("widget", "CPU & Memory", "XWIDGET.ELF"),
    xui_app("counter", "Counter", "XCOUNTR.ELF"),
    xui_app_verbs("docs", "Docs", "XDOCS.ELF", &["open", "view"]),
    // The LazyRAD IDE (`LAZYOS_LAZYRAD=1` embeds it and lists it in
    // `XAPPS.LST`); apps it builds are installed packages, not rows here.
    xui_app("lazyrad", "LazyRAD", "LAZYRAD.ELF"),
    // The package installer (docs/packages.md section 8); `mimed` routes
    // `application/x-lazyos-package` to it, so opening a `.lzp` shows consent.
    xui_app_verbs(
        "installer",
        "Package Installer",
        "XINSTALL.ELF",
        &["open", "install"],
    ),
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
        // `top` is the boot launch self-test's target: normally always shipped,
        // but the desktop profile (`LAZYOS_DESKTOP=1`) leaves its ELF out, so
        // its row must not advertise a program the image does not carry.
        let desktop_skip = cfg!(lazyos_desktop) && app.path == "TOP.ELF";
        if app.ship == Ship::Always && !desktop_skip {
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
            installed: false,
        })
        .collect()
}

/// The console shell row: a bare BusyBox applet name (no `.ELF` suffix) that
/// the Linux loader resolves, drawn in `init`'s mux window rather than a
/// `xuid` client window.
fn is_console_alias(app: &AppSpec) -> bool {
    app.linux && app.ship == Ship::Always && !app.path.contains('.')
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
            && (app.path.ends_with(".ELF") || is_console_alias(app))
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
        .filter(|app| app.linux && !is_console_alias(app))
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
