//! The core packages (issue #509): every desktop xui app ships as an `.lzp` in
//! `/system/packages`, and `pkgd` installs it into `/apps` at boot.
//!
//! `tools/xui/build.py` (`tools/xui/core_packages.py`) builds two archives per
//! app under `target/pkg/core/`, one with `autostart = false` and one with
//! `autostart = true`, and lists them in `core.lst`. This module picks one per
//! wanted app as `LAZYOS_XUI_AUTOSTART` says, embeds it as
//! `/system/packages/<sn>.lzp` and writes the index (`fhs::system::
//! PACKAGES_INDEX`) that lets `pkgd` skip reading the archives on a boot whose
//! set did not change. Every path goes through the `Sink`, so it is in
//! `/system/.image-manifest` and an in-place update replaces a changed package
//! and deletes a dropped one.

use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// `core.lst`, relative to the core package directory.
const LIST: &str = "core.lst";

/// The autostart apps when `LAZYOS_XUI_AUTOSTART` is unset: none, so the
/// desktop opens on its own (`user/build.rs` reads the same switch for the
/// built-in programs; `term` opens the Terminal, as the sessions that type
/// into it ask for).
const DEFAULT_AUTOSTART: &str = "none";

/// One built core package, as `core.lst` lists it.
pub struct CorePackage {
    /// The short id (`terminal`).
    pub short: String,
    /// `os.lazy.<short>`.
    pub system_name: String,
    pub version: String,
    /// The archive with `autostart = false`, and its SHA-256.
    plain: (PathBuf, String),
    /// The archive with `autostart = true`, and its SHA-256.
    autostart: (PathBuf, String),
}

/// The core package directory, `target/pkg/core`.
pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("pkg")
        .join("core")
}

/// The packages `tools/xui/build.py` built, from `core.lst`; empty when it has
/// not run. A malformed line fails the build: the list is generated.
pub fn built(dir: &Path) -> Vec<CorePackage> {
    let list = dir.join(LIST);
    // Tracked even when missing, so building the packages later is picked up.
    crate::os_image::rerun_if_changed(&list);
    let Ok(text) = std::fs::read_to_string(&list) else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let [short, system_name, version, plain, plain_digest, auto, auto_digest] = words[..]
        else {
            panic!(
                "{}: malformed line {line:?}; rerun `python tools/xui/build.py`",
                list.display()
            );
        };
        let package = CorePackage {
            short: short.into(),
            system_name: system_name.into(),
            version: version.into(),
            plain: (dir.join(plain), plain_digest.into()),
            autostart: (dir.join(auto), auto_digest.into()),
        };
        for (path, _) in [&package.plain, &package.autostart] {
            crate::os_image::rerun_if_changed(path);
            if !path.is_file() {
                panic!(
                    "{} lists {}, which is missing; rerun `python tools/xui/build.py`",
                    list.display(),
                    path.display()
                );
            }
        }
        packages.push(package);
    }
    packages
}

/// The resident tray applets (docs/tray-plan.md T3): they open with every
/// session of an image that ships them, whatever `LAZYOS_XUI_AUTOSTART`
/// says, as the taskbar's own volume and network icons.
pub const APPLETS: &[&str] = &["volume", "netstatus", "removable"];

/// The short ids to open at boot: what `LAZYOS_XUI_AUTOSTART` names
/// (comma-separated, each a short id (`terminal`), an xui binary stem
/// (`term`) or a `system_name` (`os.lazy.terminal`); `none` or unset opens
/// nothing), plus the [`APPLETS`].
pub fn autostart_shorts() -> Vec<String> {
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_AUTOSTART");
    let mut shorts = parse_autostart(std::env::var("LAZYOS_XUI_AUTOSTART").ok().as_deref());
    // webd is resident and windowless: an image that ships it opens it with every session (in the
    // background), whatever `LAZYOS_XUI_AUTOSTART` says, so its clients find the service.
    let webd = crate::webd_embed::shipped_env();
    for resident in APPLETS.iter().chain(webd.then_some(&"webd")) {
        if !shorts.iter().any(|short| short == resident) {
            shorts.push((*resident).to_string());
        }
    }
    shorts
}

/// [`autostart_shorts`] of a `LAZYOS_XUI_AUTOSTART` value (`None`: unset).
pub fn parse_autostart(value: Option<&str>) -> Vec<String> {
    let value = value.unwrap_or(DEFAULT_AUTOSTART);
    if value.trim() == "none" {
        return Vec::new();
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| short_of(item.strip_prefix("os.lazy.").unwrap_or(item)).to_string())
        .collect()
}

/// The core short id of an xui binary stem: `term` is the Terminal, every
/// other stem is its own short id.
pub fn short_of(stem: &str) -> &str {
    match stem {
        "term" => "terminal",
        other => other,
    }
}

/// The xui binary stems that are core packages (`tools/xui/core_packages.py`
/// `CORE_APPS`, minus the LazyRAD IDE, which is not an xui binary and is added
/// by `lazyrad_embed`): whether `stem`'s app ships as a package, built or not.
pub fn is_core_stem(stem: &str) -> bool {
    const CORE: &[&str] = &[
        // The Terminal, a developer tool that is the user's shell; every desktop
        // image ships it (`devtools_embed`, `always`).
        "terminal",
        "sysmon",
        "fabricmon",
        "widget",
        "counter",
        "editor",
        "files",
        "paint",
        "writer",
        "archiver",
        "settings",
        "confd",
        "docs",
        "mail",
        "network",
        "nettools",
        // LazySky, only with `LAZYOS_LAZYSKY=1` (`lazysky_embed`).
        "lazysky",
        // LazyWeb, the web browser on WebKit, only with `LAZYOS_LAZYWEB=1`
        // (`webd_embed::lazyweb_enabled`).
        "lazyweb",
        // webd, the daemon that serves WebKit views, on a desktop that has built it or with
        // `LAZYOS_WEBD=1` (`webd_embed`).
        "webd",
        // The test clients of webd, only with `LAZYOS_WEBDDEMO=1` (`webd_embed::CLIENT_PACKAGES`):
        // the light view's demo, Web View Test (full views through every scope) and Web Attack Probe
        // (the malicious client of the attack harness: it reaches for scopes it was not granted).
        "webddemo",
        "webdbrowse",
        "webdattack",
        // Calculator: A basic calculator.
        "calc",
        // PDF Viewer: Read PDF documents.
        "pdf",
        // Tray Demo, the tray sample app (docs/tray-plan.md T1); only
        // `LAZYOS_TRAYDEMO=1` images ship it (`xui_embed`).
        "traydemo",
        // Volume: Sound volume in the taskbar tray.
        "volume",
        // Network Status: Network status in the taskbar tray.
        "netstatus",
        // Network Drives: Mount FTP servers as folders under /mnt.
        "netdrives",
        // LazyGolf: Fly over a procedurally generated golf course; only
        // `LAZYOS_GAMES=1` images ship it (`xui_embed`).
        "golf",
        // Video Player: Play video files.
        "video",
        // About LazyOS: About LazyOS, with a real-time 3D renderer.
        "about",
        // Removable Drives: the USB sticks' tray applet, shipped with the
        // USB stack (`LAZYOS_USB=1`, `xui_embed`).
        "removable",
        // NES: Play NES game ROMs, with sound; only `LAZYOS_GAMES=1` images
        // ship it (`xui_embed`).
        "nes",
        // Messenger Playground, a developer tool; only `LAZYOS_DEVTOOLS=1` images ship it
        // (`devtools_embed`).
        "playground",
        // Cursor Studio: Draw the mouse pointer.
        "cursored",
    ];
    CORE.contains(&short_of(stem))
}

/// Embed `packages` in `/system/packages`, each in its autostart variant when
/// `autostart` names it, and write the index.
pub fn embed(sink: &mut dyn Sink, packages: &[&CorePackage], autostart: &[String]) {
    let mut index =
        String::from("# <system_name> <version> <sha256> [autostart], written by build.rs\n");
    for package in packages {
        let starts = autostart.contains(&package.short);
        let (path, digest) = if starts {
            &package.autostart
        } else {
            &package.plain
        };
        let destination = format!("{}/{}.lzp", fhs::SYSTEM_PACKAGES, package.system_name);
        println!(
            "cargo:warning=core package embedded: {} as {destination}{}",
            path.display(),
            if starts { " (autostart)" } else { "" }
        );
        sink.add_file(&destination, path.clone());
        // `autostart` lets `pkgd` provision it first (`pkgstore::provision`).
        index.push_str(&format!(
            "{} {} {digest}{}\n",
            package.system_name,
            package.version,
            if starts { " autostart" } else { "" }
        ));
    }
    if !packages.is_empty() {
        sink.add_bytes(fhs::system::PACKAGES_INDEX, index.into_bytes());
    }
}
