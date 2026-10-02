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

/// The autostart apps when `LAZYOS_XUI_AUTOSTART` is unset: the Terminal
/// (a built-in program; `user/build.rs` reads the same switch for it).
const DEFAULT_AUTOSTART: &str = "terminal";

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
    println!("cargo:rerun-if-changed={}", list.display());
    let Ok(text) = std::fs::read_to_string(&list) else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let [short, system_name, version, plain, plain_digest, auto, auto_digest] = words[..] else {
            panic!("{}: malformed line {line:?}; rerun `python tools/xui/build.py`", list.display());
        };
        let package = CorePackage {
            short: short.into(),
            system_name: system_name.into(),
            version: version.into(),
            plain: (dir.join(plain), plain_digest.into()),
            autostart: (dir.join(auto), auto_digest.into()),
        };
        for (path, _) in [&package.plain, &package.autostart] {
            println!("cargo:rerun-if-changed={}", path.display());
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

/// The short ids `LAZYOS_XUI_AUTOSTART` names: comma-separated, each a short
/// id (`terminal`), an xui binary stem (`term`) or a `system_name`
/// (`os.lazy.terminal`); `none` opens nothing; unset means the Terminal.
pub fn autostart_shorts() -> Vec<String> {
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_AUTOSTART");
    parse_autostart(std::env::var("LAZYOS_XUI_AUTOSTART").ok().as_deref())
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
/// `CORE_APPS`): whether `stem`'s app ships as a package, built or not.
pub fn is_core_stem(stem: &str) -> bool {
    const CORE: &[&str] = &[
        "sysmon", "fabricmon", "widget", "counter", "editor", "files", "paint", "settings",
        "confd", "docs",
    ];
    CORE.contains(&short_of(stem))
}

/// Embed `packages` in `/system/packages`, each in its autostart variant when
/// `autostart` names it, and write the index.
pub fn embed(sink: &mut dyn Sink, packages: &[&CorePackage], autostart: &[String]) {
    let mut index =
        String::from("# <system_name> <version> <sha256> [autostart], written by build.rs\n");
    for package in packages {
        let starts = autostart.iter().any(|short| *short == package.short);
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
