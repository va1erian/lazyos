//! The xui desktop apps, the LazyShell desktop shell (issue #157) and the
//! sample packages embedded in the OS volume (issues #215/#216): which apps
//! ship, as core packages in `/system/packages` (issue #509,
//! [`core_packages`](crate::core_packages)) or, for the shell and the
//! Installer, as programs in `/system/bin`. Split out of `build.rs`.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::core_packages;
use crate::os_image::Sink;

/// LazyShell's binary under `target/xui/` (`tools/xui/build.py` builds it from
/// `xui-app/src/bin/lazyshell.rs`); stored as `fhs::bin::LAZYSHELL`.
const SHELL_XUI_APP: &str = "xui-shell.elf";

/// Whether the image ships LazyShell (issue #157). `LAZYOS_SHELL` defaults to
/// on for the desktop profile (`LAZYOS_DESKTOP=1`); `LAZYOS_SHELL=0` opts out
/// (the compositor then paints background and windows only). `LAZYOS_SHELL=1`
/// also adds it to a hand-assembled `LAZYOS_SERVICES=1 LAZYOS_XUID=1` image;
/// without both, nothing could start or host it, so the switch only warns.
pub fn shell_enabled(desktop: bool, services: bool, xuid: bool) -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_SHELL");
    match std::env::var_os("LAZYOS_SHELL").as_deref() {
        Some(value) if value == OsStr::new("0") => false,
        Some(value) if value == OsStr::new("1") => {
            if !(services && xuid) {
                println!(
                    "cargo:warning=LAZYOS_SHELL=1 needs LAZYOS_SERVICES=1 and LAZYOS_XUID=1 \
                     (or LAZYOS_DESKTOP=1); LazyShell is not embedded"
                );
            }
            services && xuid
        }
        _ => desktop,
    }
}

/// The graphical login screen's binary under `target/xui/` (issue #623,
/// `xui-app/src/bin/greeter.rs`); stored as `fhs::bin::GREETER`.
const GREETER_XUI_APP: &str = "xui-greeter.elf";

/// Embed `xui-shell.elf` as `lazyshell` and the login screen as `greeter`.
/// `logind` opens the login screen (or logs straight in) and has `init`
/// launch the shell into each desktop session; `init` restarts both when
/// they die. A missing binary fails the build: a desktop that asked for its
/// shell must not boot without one, or without a way to log in.
fn embed_shell(sink: &mut dyn Sink) {
    for (file, destination, what) in [
        (SHELL_XUI_APP, fhs::bin::LAZYSHELL, "LazyShell"),
        (GREETER_XUI_APP, fhs::bin::GREETER, "The login screen"),
    ] {
        let path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
            .join("target")
            .join("xui")
            .join(file);
        crate::os_image::rerun_if_changed(&path);
        if !path.is_file() {
            panic!(
                "{what} ({}) is not built; run `python tools/xui/build.py`, \
                 or opt out of the shell with LAZYOS_SHELL=0",
                path.display()
            );
        }
        println!(
            "cargo:warning={what} embedded: {} as {destination}",
            path.display()
        );
        sink.add_file(destination, path);
    }
}

/// The xui programs that stay unlabelled in `/system/bin` (issue #509), by the
/// stem of their binary (`xui-<stem>.elf`): the desktop shell, the Installer
/// (`pkgd`'s trusted UI, which refuses every labelled caller) and Devices (it
/// reads the kernel's device inspection calls, `os.kernel.dev`, which no
/// package permission can name). Every other desktop app is a core package,
/// the Terminal too (a developer tool that is the user's shell,
/// `devtools_embed`).
const XUI_DESTINATIONS: &[(&str, &str)] = &[
    ("devices", fhs::bin::DEVICES),
    ("installer", fhs::bin::INSTALLER),
    ("shell", fhs::bin::LAZYSHELL),
    ("greeter", fhs::bin::GREETER),
];

/// The stem of an xui app binary (`xui-sysmon.elf` -> `sysmon`).
fn xui_stem(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("app")
        .to_ascii_lowercase();
    stem.strip_prefix("xui-").unwrap_or(&stem).to_string()
}

/// Where an xui program that is not a core package goes: its `fhs::bin`
/// constant, or `/system/bin/<stem>` for a hand-built one (`xui-client.elf`).
/// `None` when that fallback would be empty or would replace another program
/// of the image (`xui-init.elf` must never become `/system/bin/init`).
fn xui_destination(stem: &str) -> Option<String> {
    if let Some((_, path)) = XUI_DESTINATIONS.iter().find(|(name, _)| *name == stem) {
        return Some((*path).to_string());
    }
    let name: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let path = format!("{}/{name}", fhs::SYSTEM_BIN);
    (!name.is_empty() && !fhs::bin::ALL.contains(&path.as_str())).then_some(path)
}

/// The desktop profile's default xui app set, in the order the Terminal-first
/// desktop lists them; the names match `tools/xui/build.py`'s outputs and
/// `lazygui`'s `DESKTOP_APPS`.
const DESKTOP_XUI_APPS: &[&str] = &[
    // The Terminal, `xui-term.elf`, is a developer tool that every desktop ships
    // (`devtools_embed::TOOLS`, `always`), listed there and not here.
    "xui-sysmon.elf",
    "xui-fabricmon.elf",
    "xui-widget.elf",
    "xui-counter.elf",
];

/// The document apps (Editor, Paint, Files, LazyWriter, Archiver) and the other always-shipped
/// desktop apps: `python tools/xui/build.py` produces all of them under
/// `target/xui/`; a missing one fails the desktop build on purpose.
const DOCUMENT_XUI_APPS: &[&str] = &[
    "xui-editor.elf",
    "xui-files.elf",
    "xui-paint.elf",
    // LazyWriter, the word processor (issue #533).
    "xui-writer.elf",
    // The Archiver, a 7-Zip-style archive manager (docs/archiver-plan.md).
    "xui-archiver.elf",
    "xui-settings.elf",
    "xui-confd.elf",
    // The package installer (docs/packages.md section 8): the consent screen
    // for `.lzp` packages, opened from the menu or by open-with. Not a package.
    "xui-installer.elf",
    // The Devices app (issue #481): devices, their owners and the driver
    // class rules, read-only; opened from the menu.
    "xui-devices.elf",
    // Calculator: A basic calculator.
    "xui-calc.elf",
    // PDF Viewer: Read PDF documents.
    "xui-pdf.elf",
    // Volume, the tray applet over the mixer (docs/tray-plan.md T3): every
    // desktop image has the sound stack (`drivers::embed`).
    "xui-volume.elf",
    // Video Player: Play video files.
    "xui-video.elf",
    // About LazyOS: About LazyOS, with a real-time 3D renderer.
    "xui-about.elf",
];

/// Desktop apps shipped when they were built, and skipped (with a build
/// warning) when they were not. Docs is built with the other apps (pure Rust,
/// its pages drawn by webd), but a build that skipped it leaves a smaller desktop, not a broken
/// one.
const OPTIONAL_XUI_APPS: &[&str] = &["xui-docs.elf"];

/// The network apps, shipped by a desktop image that has the network stack
/// (`LAZYOS_NETD=1`, docs/networking-host-access.md): Network (status and
/// configuration), Net Tools (ping, lookups, a web fetch and a web server
/// the host can reach), Network Status (its tray icon) and Network Drives
/// (FTP servers mounted under `/mnt` through `mountd`). Without the stack
/// they would have nothing to show.
const NETWORK_XUI_APPS: &[&str] = &[
    "xui-network.elf",
    "xui-nettools.elf",
    "xui-netstatus.elf",
    "xui-netdrives.elf",
];

/// The removable-drives tray applet, shipped by a desktop image that has the
/// USB stack (`LAZYOS_USB=1`, docs/usb-hotplug-plan.md G): without `usbd`
/// there is nothing for it to show.
const USB_XUI_APPS: &[&str] = &["xui-removable.elf"];

/// Mail (esMail, docs/mail.md), shipped by `LAZYOS_MAIL=1` desktop images:
/// `python tools/xui/build.py --mail` builds it (`run_demo.py --mail` does
/// both), and a missing one fails such a build.
const MAIL_XUI_APPS: &[&str] = &["xui-mail.elf"];

/// Whether this build asks for Mail.
fn mail_app() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_MAIL");
    std::env::var_os("LAZYOS_MAIL").as_deref() == Some(OsStr::new("1"))
}

/// The tray sample app (`os.lazy.traydemo`, docs/tray-plan.md T1), shipped
/// only by `LAZYOS_TRAYDEMO=1` desktop images (`run_demo.py --traydemo`).
/// `tools/xui/build.py` always builds it, so no extra step is needed.
const TRAYDEMO_XUI_APPS: &[&str] = &["xui-traydemo.elf"];

/// Whether this build asks for the tray sample app.
fn traydemo_app() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_TRAYDEMO");
    std::env::var_os("LAZYOS_TRAYDEMO").as_deref() == Some(OsStr::new("1"))
}

/// The games (LazyGolf and the NES Player), shipped only by `LAZYOS_GAMES=1`
/// desktop images (`run_demo.py --games`, the launcher's Games group).
/// `tools/xui/build.py` always builds them, so no extra step is needed.
const GAMES_XUI_APPS: &[&str] = &[
    // LazyGolf: Fly over a procedurally generated golf course.
    "xui-golf.elf",
    // NES: Play NES game ROMs, with sound.
    "xui-nes.elf",
];

/// Whether this build asks for the games.
fn games_apps() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_GAMES");
    std::env::var_os("LAZYOS_GAMES").as_deref() == Some(OsStr::new("1"))
}

/// The print spooler (docs/printing-plan.md P6), an xui-app program with no
/// window: `init` starts it on every desktop image with the network stack.
const PRINTD_ELF: &str = "xui-printd.elf";

/// Rerun the build script when a module of the apps' embedding changes (`#[path]` modules are
/// not tracked by cargo).
pub fn rerun_if_changed() {
    for module in [
        "xui_embed",
        "core_packages",
        "devtools_embed",
        "webd_embed",
        "lazysky_embed",
        "pictures_embed",
    ] {
        println!("cargo:rerun-if-changed=build_support/{module}.rs");
    }
}

/// Whether this build has the USB stack, which brings the removable-drives
/// applet.
fn usb_stack() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_USB");
    std::env::var_os("LAZYOS_USB").as_deref() == Some(OsStr::new("1"))
}

/// Whether this build has the network stack, which brings the network apps.
fn network_stack() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_NETD");
    std::env::var_os("LAZYOS_NETD").as_deref() == Some(OsStr::new("1"))
}

/// Embed the desktop's apps (issues #215/#216/#509).
///
/// `LAZYOS_XUI_APPS` is a platform path list (`;` on Windows, `:` elsewhere)
/// of binaries built by `tools/xui/build.py`; with `LAZYOS_DESKTOP=1` and no
/// explicit list, [`DESKTOP_XUI_APPS`], [`DOCUMENT_XUI_APPS`] and the built
/// [`OPTIONAL_XUI_APPS`] (plus the switched network, Mail, tray-demo and games apps)
/// are used, so one switch is enough. An app that is a core package
/// (`core_packages`) ships as `/system/packages/<sn>.lzp`, the rest (the
/// Installer, a hand-built client) as an ELF in `/system/bin`
/// ([`xui_destination`]). `LAZYOS_XUI_AUTOSTART` picks which packages open at
/// boot (`core_packages::autostart_shorts`). With `shell`, LazyShell is
/// embedded too.
pub fn embed_xui_apps(sink: &mut dyn Sink, desktop: bool, shell: bool) {
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APPS");
    let explicit = std::env::var_os("LAZYOS_XUI_APPS");
    // The default set is part of the `LAZYOS_DESKTOP=1` profile: a desktop with
    // one of its apps missing is a broken profile, not a smaller one, so a
    // missing default fails the build. An explicit `LAZYOS_XUI_APPS` list only
    // warns, since it may name apps the caller knows are optional.
    let default_desktop_apps = desktop && explicit.is_none();
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("xui");
    let apps: Vec<PathBuf> = match explicit {
        Some(list) => std::env::split_paths(&list).collect(),
        None if desktop => DESKTOP_XUI_APPS
            .iter()
            .chain(DOCUMENT_XUI_APPS)
            .chain(OPTIONAL_XUI_APPS)
            .chain(NETWORK_XUI_APPS.iter().filter(|_| network_stack()))
            .chain(USB_XUI_APPS.iter().filter(|_| usb_stack()))
            .chain(MAIL_XUI_APPS.iter().filter(|_| mail_app()))
            .chain(TRAYDEMO_XUI_APPS.iter().filter(|_| traydemo_app()))
            .chain(GAMES_XUI_APPS.iter().filter(|_| games_apps()))
            .map(|name| dir.join(name))
            .collect(),
        None => Vec::new(),
    };
    if shell {
        embed_shell(sink);
    }
    let core_dir = core_packages::dir();
    let built = core_packages::built(&core_dir);
    let mut packages: Vec<&core_packages::CorePackage> = Vec::new();
    for app in apps {
        let stem = xui_stem(&app);
        let optional = OPTIONAL_XUI_APPS.iter().any(|name| dir.join(name) == app);
        let short = core_packages::short_of(&stem);
        if let Some(package) = built.iter().find(|p| p.short == short) {
            if !packages.iter().any(|p| p.short == short) {
                packages.push(package);
            }
            continue;
        }
        // Tracked even when missing: Cargo reruns while a listed path does not
        // exist, so an app built later is picked up without changing the env.
        crate::os_image::rerun_if_changed(&app);
        let is_core = crate::core_packages::is_core_stem(&stem);
        if is_core || !app.is_file() {
            let what = if is_core {
                format!("core package {short}")
            } else {
                app.display().to_string()
            };
            if optional {
                println!(
                    "cargo:warning=optional xui app {what} not built (needs zig: \
                     `pip install ziglang==0.16.0`, then `python tools/xui/build.py`)"
                );
            } else if default_desktop_apps {
                panic!(
                    "LAZYOS_DESKTOP=1 is missing {what}; run `python tools/xui/build.py`, \
                     or set LAZYOS_XUI_APPS to the apps you built"
                );
            } else {
                println!("cargo:warning=LAZYOS_XUI_APPS entry not built: {what}");
            }
            continue;
        }
        let Some(destination) = xui_destination(&stem) else {
            println!(
                "cargo:warning=LAZYOS_XUI_APPS entry {} has no usable name; skipped",
                app.display()
            );
            continue;
        };
        println!(
            "cargo:warning=LAZYOS_XUI_APPS embedded: {} as {destination}",
            app.display()
        );
        sink.add_file(&destination, app);
    }
    // The LazyRAD IDE, LazySky, LazyWeb and the Picture Viewer are core packages
    // shipped only on request (`LAZYOS_LAZYRAD=1`, `LAZYOS_LAZYSKY=1`,
    // `LAZYOS_LAZYWEB=1`, `LAZYOS_PICTURES=1`, and the developer tools as a class,
    // `LAZYOS_DEVTOOLS=1`); asking for one without having built it is an error.
    let lazysky_elf = dir.join(crate::lazysky_embed::ELF);
    crate::os_image::rerun_if_changed(&lazysky_elf);
    let lazyweb_elf = dir.join(crate::webd_embed::LAZYWEB_ELF);
    crate::os_image::rerun_if_changed(&lazyweb_elf);
    let webd_elf = dir.join(crate::webd_embed::ELF);
    crate::os_image::rerun_if_changed(&webd_elf);
    // LazyWeb first: its requirements (webd, the desktop, the network) are named before webd's own.
    let lazyweb_wanted = crate::webd_embed::lazyweb_enabled(desktop);
    let webd_wanted = crate::webd_embed::enabled(desktop);
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let devtools =
        crate::devtools_embed::enabled(desktop, crate::rhai_embed::find(&manifest_dir).is_some());
    let mut requested = vec![
        (
            crate::lazyrad_embed::enabled(),
            crate::lazyrad_embed::PACKAGE_SHORT,
            format!(
                "LAZYOS_LAZYRAD=1 but core package {} is not built; run \
                 `python tools/lazyrad/build.py` and `python tools/xui/core_packages.py`",
                crate::lazyrad_embed::PACKAGE_SHORT
            ),
        ),
        (
            crate::lazysky_embed::enabled(desktop),
            crate::lazysky_embed::PACKAGE_SHORT,
            crate::lazysky_embed::not_built(lazysky_elf.is_file()),
        ),
        (
            lazyweb_wanted,
            crate::webd_embed::LAZYWEB_PACKAGE_SHORT,
            crate::webd_embed::lazyweb_not_built(lazyweb_elf.is_file()),
        ),
        (
            webd_wanted,
            crate::webd_embed::PACKAGE_SHORT,
            crate::webd_embed::not_built(webd_elf.is_file()),
        ),
        (
            crate::pictures_embed::enabled(desktop),
            crate::pictures_embed::PACKAGE_SHORT,
            crate::pictures_embed::not_built(),
        ),
    ];
    for tool in crate::devtools_embed::TOOLS {
        let elf = dir.join(tool.elf);
        crate::os_image::rerun_if_changed(&elf);
        requested.push((
            crate::devtools_embed::ships(tool, devtools, desktop),
            tool.short,
            crate::devtools_embed::not_built(tool, elf.is_file()),
        ));
    }
    for (wanted, short, missing) in requested {
        if !wanted || packages.iter().any(|p| p.short == short) {
            continue;
        }
        match built.iter().find(|p| p.short == short) {
            Some(package) => packages.push(package),
            None => panic!("{missing}"),
        }
    }
    if webd_wanted && crate::webd_embed::demo_clients() {
        embed_webd_clients(&built, &mut packages);
    }
    core_packages::embed(sink, &packages, &core_packages::autostart_shorts());
    if desktop && network_stack() {
        embed_printd(sink, &dir);
    }
}

/// The packages that are clients of webd (`webd_embed::CLIENT_PACKAGES`), added to an image that ships
/// it. A client whose tree is not in this checkout is skipped; one that has a tree but was not built
/// is an error, like the daemon.
fn embed_webd_clients<'a>(
    built: &'a [core_packages::CorePackage],
    packages: &mut Vec<&'a core_packages::CorePackage>,
) {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    for (short, elf) in crate::webd_embed::CLIENT_PACKAGES {
        if !crate::webd_embed::client_tree_present(&root, short)
            || packages.iter().any(|p| p.short == *short)
        {
            continue;
        }
        match built.iter().find(|p| p.short == *short) {
            Some(package) => packages.push(package),
            None => panic!(
                "LAZYOS_WEBDDEMO=1 but core package {short} is not built (target/xui/{elf}); run \
                 `python tools/xui/build.py`"
            ),
        }
    }
}

/// Embed `printd` as `fhs::bin::PRINTD`, whatever `LAZYOS_XUI_APPS` lists:
/// `init`'s manifest has its row on such an image, so a missing binary fails
/// the build rather than the boot.
fn embed_printd(sink: &mut dyn Sink, dir: &std::path::Path) {
    let path = dir.join(PRINTD_ELF);
    crate::os_image::rerun_if_changed(&path);
    if !path.is_file() {
        panic!(
            "LAZYOS_DESKTOP=1 with LAZYOS_NETD=1 needs the print spooler ({}); \
             run `python tools/xui/build.py`",
            path.display()
        );
    }
    sink.add_file(fhs::bin::PRINTD, path);
}

/// Embed the sample `.lzp` packages in `/system/share/samples`, when
/// `tools/pkg/build_samples.py` produced them (`tools/xui/build.py` runs it
/// after building the xui apps): `pkgdemo.lzp` is the Counter demo as an
/// installable package, installed with `pkgctl install
/// /system/share/samples/pkgdemo.lzp`. A missing sample only means a smaller
/// image, so it warns instead of failing.
pub fn embed_sample_packages(sink: &mut dyn Sink) {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("pkg");
    // (what `build_samples.py` writes, where the image keeps it)
    for (built, destination) in [("pkgdemo.lzp", fhs::share::PKGDEMO)] {
        let path = dir.join(built);
        // Tracked even when missing, so building it later is picked up.
        crate::os_image::rerun_if_changed(&path);
        if path.is_file() {
            println!(
                "cargo:warning=sample package embedded: {} as {destination}",
                path.display()
            );
            sink.add_file(destination, path);
        } else {
            println!(
                "cargo:warning=sample package {built} not built \
                 (`python tools/xui/build.py` or `python tools/pkg/build_samples.py`)"
            );
        }
    }
}
