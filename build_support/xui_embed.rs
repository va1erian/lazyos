//! Embed the desktop's xui apps (issues #215/#216) and the LazyShell desktop
//! shell (issue #157) in the disk image, and write `XAPPS.LST`, the list of
//! shipped apps `init`'s registry reads at boot.
//!
//! Split out of `build.rs`, which is past the file-size budget.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::lazyrad_embed;

/// LazyShell's binary under `target/xui/` (`tools/xui/build.py` builds it from
/// `xui-app/src/bin/lazyshell.rs`); stored as `XSHELL.ELF`.
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

/// Embed `xui-shell.elf` as `XSHELL.ELF` and return its `XAPPS.LST` line. The
/// line always carries `autostart`, whatever `LAZYOS_XUI_AUTOSTART` says
/// (that switch lists the *apps*): `init` opens it first, as the desktop's
/// shell, and restarts it when it dies. A missing binary fails the build: a
/// desktop that asked for its shell must not boot without one.
fn embed_shell(builder: &mut bootloader::DiskImageBuilder) -> String {
    let path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("xui")
        .join(SHELL_XUI_APP);
    println!("cargo:rerun-if-changed={}", path.display());
    if !path.is_file() {
        panic!(
            "LazyShell ({}) is not built; run `python tools/xui/build.py`, \
             or opt out with LAZYOS_SHELL=0",
            path.display()
        );
    }
    let (_, disk) = xui_disk_name(&path);
    println!(
        "cargo:warning=LazyShell embedded: {} as {disk}",
        path.display()
    );
    builder.set_file(disk.clone(), path);
    format!("{disk} autostart\n")
}

/// The 8.3 on-disk name for an xui app binary (`xui-sysmon.elf` ->
/// `XSYSMON.ELF`): the kernel's FAT reader only resolves short names, and
/// `init`'s app registry (`user/src/bin/init/apps.rs`) refers to these names.
/// Returns `(stem, disk_name)`.
pub fn xui_disk_name(path: &std::path::Path) -> (String, String) {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("app")
        .to_ascii_lowercase();
    let stem = stem.strip_prefix("xui-").unwrap_or(&stem).to_string();
    let base = match stem.as_str() {
        "sysmon" => "XSYSMON".to_string(),
        "fabricmon" => "XFABMON".to_string(),
        "counter" => "XCOUNTR".to_string(),
        "term" => "XTERM".to_string(),
        "editor" => "XEDITOR".to_string(),
        "paint" => "XPAINT".to_string(),
        "files" => "XFILES".to_string(),
        "settings" => "XSETTNG".to_string(),
        "confd" => "XCONFD".to_string(),
        "client" => "XCLIENT".to_string(),
        other => {
            let short: String = other
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(7)
                .collect();
            format!("X{}", short.to_ascii_uppercase())
        }
    };
    (stem, format!("{base}.ELF"))
}

/// The desktop profile's default xui app set, in the order `init` opens them
/// (the Terminal first, so it takes the focus). `LAZYOS_DESKTOP=1` embeds
/// these from `target/xui/` unless `LAZYOS_XUI_APPS` overrides the list; the
/// names match `tools/xui/build.py`'s outputs and `lazygui`'s `DESKTOP_APPS`.
const DESKTOP_XUI_APPS: &[&str] = &[
    "xui-term.elf",
    "xui-sysmon.elf",
    "xui-fabricmon.elf",
    "xui-widget.elf",
    "xui-counter.elf",
];

/// The document apps (Editor, Paint, Files): always-shipped desktop apps.
/// `python tools/xui/build.py` produces all three under `target/xui/`; a
/// missing one fails the desktop build on purpose. They are on-demand (never
/// autostarted at boot), opened from the Start menu or by open-with.
const SHIP_DOCUMENT_APPS: bool = true;

/// The document apps' binaries, appended to [`DESKTOP_XUI_APPS`] when
/// [`SHIP_DOCUMENT_APPS`] is on.
const DOCUMENT_XUI_APPS: &[&str] = &[
    "xui-editor.elf",
    "xui-files.elf",
    "xui-paint.elf",
    "xui-settings.elf",
    "xui-confd.elf",
    // The package installer (docs/packages.md section 8): the consent screen
    // for `.lzp` packages, opened from the menu or by open-with.
    "xui-installer.elf",
];

/// Desktop apps embedded when their ELF exists, and skipped (with a build
/// warning) when it does not. The Docs app is C++ (litehtml) and needs the zig
/// toolchain (`tools/xui/zig.py`), which a developer machine may lack; a
/// missing one leaves a smaller desktop, not a broken one, so it is not a
/// required default like [`DOCUMENT_XUI_APPS`].
const OPTIONAL_XUI_APPS: &[&str] = &["xui-docs.elf"];

/// The one app the desktop opens at boot when `LAZYOS_XUI_AUTOSTART` is unset:
/// the Terminal. Every other embedded app (viewers, Editor, Files, Paint) is
/// launched on demand from the Start menu, the right-click menu or open-with.
const DEFAULT_AUTOSTART_STEM: &str = "term";

/// Embed the desktop's xui apps (issues #215/#216).
///
/// `LAZYOS_XUI_APPS` is a platform path list (`;` on Windows, `:` elsewhere)
/// of binaries built by `tools/xui/build.py`. With `LAZYOS_DESKTOP=1` and no
/// explicit list, the [`DESKTOP_XUI_APPS`] defaults under `target/xui/` are
/// used, so one switch is enough. Each is stored under its 8.3 name, and
/// `XAPPS.LST` lists the shipped ones so `init` marks every other registry row
/// unavailable instead of failing to launch it. Rows named in
/// `LAZYOS_XUI_AUTOSTART` (comma-separated stems such as `term,sysmon`; the
/// default is the Terminal only, `none` disables it) are tagged `autostart`,
/// and `init` launches them at boot as `xuid` clients. With `shell`, LazyShell
/// is embedded too and listed first (see [`embed_shell`]).
pub fn embed_xui_apps(builder: &mut bootloader::DiskImageBuilder, desktop: bool, shell: bool) {
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APPS");
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_AUTOSTART");
    let explicit = std::env::var_os("LAZYOS_XUI_APPS");
    // The default set is part of the `LAZYOS_DESKTOP=1` profile: a desktop with
    // one of its apps missing is a broken profile, not a smaller one, so a
    // missing default fails the build. An explicit `LAZYOS_XUI_APPS` list only
    // warns, since it may name apps the caller knows are optional.
    let default_desktop_apps = desktop && explicit.is_none();
    let apps: Vec<PathBuf> = match explicit {
        Some(list) => std::env::split_paths(&list).collect(),
        None if desktop => {
            let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
                .join("target")
                .join("xui");
            let document: &[&str] = if SHIP_DOCUMENT_APPS {
                DOCUMENT_XUI_APPS
            } else {
                &[]
            };
            let optional = OPTIONAL_XUI_APPS.iter().filter_map(|name| {
                let path = dir.join(name);
                // Tracked even when missing, so building it later is picked up.
                println!("cargo:rerun-if-changed={}", path.display());
                if path.is_file() {
                    Some(path)
                } else {
                    println!(
                        "cargo:warning=optional xui app {name} not built (needs zig:                          `pip install ziglang==0.16.0`, then `python tools/xui/build.py`)"
                    );
                    None
                }
            });
            DESKTOP_XUI_APPS
                .iter()
                .chain(document)
                .map(|name| dir.join(name))
                .chain(optional)
                .collect()
        }
        // No xui apps requested, but the shell or the IDE still needs its line.
        None if shell || !lazyrad_embed::manifest_lines().is_empty() => Vec::new(),
        None => return,
    };
    let autostart = std::env::var("LAZYOS_XUI_AUTOSTART").ok();
    let wanted = |stem: &str| match autostart.as_deref() {
        None => stem == DEFAULT_AUTOSTART_STEM,
        Some("none") => false,
        Some(list) => list.split(',').any(|item| item.trim() == stem),
    };
    let mut manifest = if shell {
        embed_shell(builder)
    } else {
        String::new()
    };
    for app in apps {
        // Tracked even when missing: Cargo reruns while a listed path does not
        // exist, so an app built later is picked up without changing the env.
        println!("cargo:rerun-if-changed={}", app.display());
        if !app.is_file() {
            if default_desktop_apps {
                panic!(
                    "LAZYOS_DESKTOP=1 is missing its default xui app {}; \
                     run `python tools/xui/build.py`, or set LAZYOS_XUI_APPS \
                     to the apps you built",
                    app.display()
                );
            }
            println!(
                "cargo:warning=LAZYOS_XUI_APPS entry not found: {}",
                app.display()
            );
            continue;
        }
        let (stem, disk) = xui_disk_name(&app);
        println!(
            "cargo:warning=LAZYOS_XUI_APPS embedded: {} as {disk}",
            app.display()
        );
        let suffix = if wanted(&stem) { " autostart" } else { "" };
        manifest.push_str(&format!("{disk}{suffix}\n"));
        builder.set_file(disk, app);
    }
    // The IDE is embedded by `lazyrad_embed` under its own 8.3 name, not as an
    // `xui-*` app, so its manifest line is added here.
    manifest.push_str(lazyrad_embed::manifest_lines());
    builder.set_file_contents(String::from("XAPPS.LST"), manifest.into_bytes());
}
