//! `init`'s built-in app registry (issue #158): the programs that are not
//! packages. Since F5 (issue #509) every desktop app is a core package that
//! `pkgd` installs into `/apps`, and [`super::installed`] is the source of
//! desktop apps; what stays here is what must not be a package:
//!
//! * LazyShell, the desktop itself (started by `logind`, `Restart::Always`);
//! * the Installer, `pkgd`'s trusted UI, which a labelled app may never be
//!   (`pkgstore::access` refuses every labelled caller);
//! * the Terminal: its shell and every command typed in it are its children,
//!   and a child inherits its parent's label, so a packaged Terminal would run
//!   `pkgctl`, `powerctl` and `messengerctl` sandboxed as an app;
//! * Devices, which reads the kernel's device inspection calls
//!   (`os.kernel.dev`), something no package permission can name;
//! * the console programs (`top`, `messengerctl`, BusyBox `sh`) and the
//!   `runner` placeholder `mimed` names for `application/x-elf`.
//!
//! The LazyRAD IDE is not here: it is a core package like the other desktop
//! apps (`LAZYOS_LAZYRAD=1` ships `os.lazy.lazyrad`). Neither is Files: its
//! `reveal` verb (issue #488; `mimed` seeds `os.lazy.files` for it) is in its
//! package manifest (`xui-app/packages/files/manifest.toml`), which
//! [`super::installed`] serves with its verbs; a row here would shadow the
//! package ([`selftest_builtins`] refuses one). `Launch` hands Files the
//! item's path, and Files opens the folder holding it with the item selected.
//!
//! Split out of `init.rs`, which is far past the file-size budget.
//!
//! # Availability
//!
//! A row is *available* when its program is in `/system/bin` ([`load`]
//! checks each once at boot; the console shell is a BusyBox applet alias and
//! always is). An unavailable row is omitted from `ListApps` and refused by
//! `Launch` with `-ENOENT` without logging a failure, so the registry never
//! advertises (or noisily fails to start) a program the image does not carry.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use user::messenger::services;

use super::state::Restart;

/// One built-in registry row: what the start menu enumerates and what
/// `Launch` resolves an app id to.
pub struct AppSpec {
    /// Lowercase program stem (`top`, `installer`).
    pub id: &'static str,
    /// Display name for menus.
    pub name: &'static str,
    /// The program's path in `/system/bin` (`fhs::bin`), or a console alias.
    pub path: &'static str,
    /// Default restart policy for launches.
    pub restart: Restart,
    /// MIME verbs the app handles.
    pub verbs: &'static [&'static str],
    /// Whether the app is a static Linux-ABI (musl) program: it is spawned
    /// under the Linux personality.
    pub linux: bool,
    /// Arguments every launch passes first (an xui program is an `xuid`
    /// client: `--client` keeps it off the display grant).
    pub args: &'static [&'static str],
    /// Whether `ListApps` reports the row. Only the desktop shell is unlisted:
    /// it is the start menu, not an entry in it. `Launch` still resolves it.
    pub listed: bool,
    /// The start-menu group (`lazypkg::Category` spelling) of a desktop
    /// program; empty for a console one, which the menu leaves out.
    pub category: &'static str,
}

/// An xui program that is not a package: Linux ABI, `xuid` client.
const fn xui_app(
    id: &'static str,
    name: &'static str,
    path: &'static str,
    verbs: &'static [&'static str],
) -> AppSpec {
    AppSpec {
        id,
        name,
        path,
        restart: Restart::OnFailure,
        verbs,
        linux: true,
        args: &["--client"],
        listed: true,
        category: "system",
    }
}

/// A native console program.
const fn native_app(
    id: &'static str,
    name: &'static str,
    path: &'static str,
    restart: Restart,
    verbs: &'static [&'static str],
) -> AppSpec {
    AppSpec {
        id,
        name,
        path,
        restart,
        verbs,
        linux: false,
        args: &[],
        listed: true,
        category: "",
    }
}

/// The desktop shell's registry id (`Launch("lazyshell", ...)`).
pub const SHELL_APP_ID: &str = "lazyshell";
/// The Installer started for the `develop` verb (issue #529).
pub const DEVELOP_APP_ID: &str = "installer-develop";

/// The built-in registry.
///
/// A `static`, not a `const`: [`is_available`] identifies a row by address, so
/// the table must have one stable storage location.
pub static APPS: &[AppSpec] = &[
    // First: autostart opens it before the apps, so their windows land on its
    // taskbar from the start. A graphical login asks for it by id (`logind`).
    AppSpec {
        restart: Restart::Always,
        listed: false,
        ..xui_app(SHELL_APP_ID, "LazyShell", fhs::bin::LAZYSHELL, &[])
    },
    // The package installer (docs/packages.md section 8); `mimed` routes
    // `application/x-lazyos-package` to it, so opening a `.lzp` shows consent.
    xui_app(
        "installer",
        "Package Installer",
        fhs::bin::INSTALLER,
        &["open", "install"],
    ),
    // The Installer for the `develop` verb (issue #529): an IDE asks `mimed`
    // to open its project's package with `develop`, and `--develop` shows only
    // the development consent (`pkgd.Develop`). Reached through `mimed` alone,
    // so it is not a menu entry.
    AppSpec {
        listed: false,
        args: &["--client", "--develop"],
        ..xui_app(
            DEVELOP_APP_ID,
            "Development Approval",
            fhs::bin::INSTALLER,
            &["develop"],
        )
    },
    // The desktop Terminal hosts the shell in a `xuid` window (`shell` below
    // is the console shell, drawn in `init`'s mux window).
    xui_app("terminal", "Terminal", fhs::bin::TERMINAL, &["open"]),
    // Devices, owners, rights and the driver class rules (issue #481).
    xui_app("devices", "Devices", fhs::bin::DEVICES, &["open"]),
    // `mimed`'s handler for `application/x-elf`; no image ships it yet.
    native_app(
        "runner",
        "Program Runner",
        fhs::bin::RUNNER,
        Restart::Once,
        &["open"],
    ),
    // The console shell (BusyBox `sh`, a bare applet name the kernel's Linux
    // loader aliases to BusyBox); it draws in `init`'s mux window.
    AppSpec {
        args: &[],
        category: "",
        ..xui_app("shell", "Console Shell", "sh", &["open"])
    },
    native_app(
        "top",
        "System Monitor (text)",
        fhs::bin::TOP,
        Restart::Once,
        &["open"],
    ),
    native_app(
        "messengerctl",
        "Messenger Console",
        fhs::bin::MESSENGERCTL,
        Restart::OnFailure,
        &[],
    ),
];

/// Bit `i` is set when `APPS[i]`'s program is in the image (see [`load`]).
static AVAILABLE: AtomicU64 = AtomicU64::new(0);

// The mask is 64 bits wide.
const _: () = assert!(APPS.len() <= 64);

/// The built-in row for `id` (case-insensitive), if any.
pub fn find_app(id: &str) -> Option<&'static AppSpec> {
    APPS.iter()
        .find(|app| app.id.eq_ignore_ascii_case(id.trim()))
}

/// Whether `app`'s program is in this image.
pub fn is_available(app: &AppSpec) -> bool {
    APPS.iter()
        .position(|row| core::ptr::eq(row, app))
        .is_some_and(|index| AVAILABLE.load(Ordering::Relaxed) & (1 << index) != 0)
}

/// The built-in rows the image build asked to open at boot, comma-separated
/// registry ids (`LAZYOS_XUI_AUTOSTART`, read by `user/build.rs`): the
/// Terminal by default, Devices on request.
const BUILTIN_AUTOSTART: &str = env!("LAZYOS_BUILTIN_AUTOSTART");

/// The built-in rows that open at boot: the desktop shell first, then those
/// the build asked for, each when the image ships it. The other apps that
/// autostart are packages ([`super::installed`]).
pub fn autostart_ids() -> Vec<&'static str> {
    let wanted = |id: &str| BUILTIN_AUTOSTART.split(',').any(|want| want == id);
    let shell = APPS.iter().filter(|app| app.id == SHELL_APP_ID);
    let others = BUILTIN_AUTOSTART.split(',').filter_map(|id| {
        APPS.iter()
            .find(|app| app.id == id && app.id != SHELL_APP_ID)
    });
    shell
        .chain(others)
        .filter(|app| is_available(app) && (app.id == SHELL_APP_ID || wanted(app.id)))
        .map(|app| app.id)
        .collect()
}

/// Record which built-in programs the image carries: each `/system/bin` path
/// is looked up once; the console alias is always there.
pub fn load() {
    let mut available = 0u64;
    for (index, app) in APPS.iter().enumerate() {
        if is_console_alias(app) || user::files::stat(app.path).is_ok() {
            available |= 1 << index;
        }
    }
    AVAILABLE.store(available, Ordering::Relaxed);
}

/// The registry as wire rows for `ListApps`: available, listed rows only (the
/// desktop shell is launchable but never offered as a menu entry).
pub fn app_infos() -> Vec<services::AppInfo> {
    APPS.iter()
        .filter(|app| app.listed && is_available(app))
        .map(|app| services::AppInfo {
            id: app.id.to_string(),
            name: app.name.to_string(),
            path: app.path.to_string(),
            restart: app.restart.label().to_string(),
            verbs: app.verbs.iter().map(|verb| verb.to_string()).collect(),
            installed: false,
            origin: String::from("system"),
            category: app.category.to_string(),
            // Set per caller by `InstalledApps::infos`.
            hidden: false,
            autostart: autostart_ids().contains(&app.id),
            icon: String::new(),
            // Built-ins are never resident apps.
            resident: false,
        })
        .collect()
}

/// The console shell row: a bare BusyBox applet name (not a path) that the
/// Linux loader resolves, drawn in `init`'s mux window.
fn is_console_alias(app: &AppSpec) -> bool {
    app.linux && !app.path.contains('/')
}

/// Whether `path` is a program directly in `/system/bin` (`fhs::SYSTEM_BIN`).
fn in_system_bin(path: &str) -> bool {
    path.strip_prefix(fhs::SYSTEM_BIN)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// The built-in registry's self-test: every row is well formed, the shell is
/// the first row, unlisted and always restarted, the xui programs are `xuid`
/// clients, and no desktop app that is now a package is still built in.
/// Returns whether it holds; [`super::installed::selftest`] adds the packages
/// and prints the markers.
pub fn selftest_builtins() -> bool {
    let rows_ok = APPS.iter().all(|app| {
        !app.id.is_empty()
            && !app.name.is_empty()
            && (in_system_bin(app.path) || is_console_alias(app))
            && app.verbs.len() <= 4
    });
    let clients_ok = APPS
        .iter()
        .filter(|app| app.linux && !is_console_alias(app))
        .all(|app| app.args.first() == Some(&"--client"));
    let shell_ok = APPS
        .first()
        .is_some_and(|app| app.id == SHELL_APP_ID && !app.listed && app.restart == Restart::Always)
        && APPS
            .iter()
            .skip(1)
            .all(|app| app.listed || app.id == DEVELOP_APP_ID);
    // The desktop apps are packages now; a row for one would shadow it.
    let no_packaged = ["editor", "files", "paint", "settings", "sysmon"]
        .iter()
        .all(|id| find_app(id).is_none());
    let has_top = APPS
        .iter()
        .any(|app| app.id == "top" && app.path == fhs::bin::TOP);
    rows_ok && clients_ok && shell_ok && no_packaged && has_top
}

/// How many built-in rows this image can launch.
pub fn available_count() -> usize {
    APPS.iter().filter(|app| is_available(app)).count()
}
