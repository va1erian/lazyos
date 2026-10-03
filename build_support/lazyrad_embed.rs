//! The LazyRAD IDE in the disk image: its core package and the sample projects.
//!
//! `lrplay` and `lazyrad` are static-musl `std` programs built by
//! `tools/lazyrad/build.py` (build artifacts, never committed). Like every
//! desktop app the IDE is a core package, `os.lazy.lazyrad`: `tools/xui/
//! core_packages.py` packs both programs (`bin/lazyrad.elf`, `bin/lrplay.elf`)
//! into `target/pkg/core`, and `xui_embed::embed_xui_apps` embeds it as
//! `/system/packages/os.lazy.lazyrad.lzp` for `pkgd` to install at boot, when
//! [`enabled`] (`LAZYOS_LAZYRAD=1`). Nothing of LazyRAD is in `/system/bin`
//! any more, and it is not an unlabelled exception (`docs/packages.md`, core
//! packages; `docs/lazyrad-package-plan.md`).
//!
//! This module copies the sample projects: `LAZYRAD_SAMPLES` is a platform path list (`;` on Windows, `:` elsewhere) of
//! sample project directories, each copied under
//! `/system/share/lazyrad/<directory>/` (`fhs::share::LAZYRAD_SAMPLES`; names
//! are kept exactly: ext2 is case-sensitive).
//!
//! With the switch unset nothing changes: the plain demo image is unchanged.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The core package's short id (`xui-app/packages/lazyrad`).
pub const PACKAGE_SHORT: &str = "lazyrad";

/// The built programs, relative to the manifest dir: the packager's inputs,
/// watched so a rebuilt IDE rebuilds the image once repackaged.
const ELFS: &[&str] = &["target/lazyrad/lrplay.elf", "target/lazyrad/lazyrad.elf"];

/// The image directory the sample projects are copied under.
const SAMPLES_ROOT: &str = fhs::share::LAZYRAD_SAMPLES;

/// Whether the image ships the IDE (`LAZYOS_LAZYRAD=1`).
pub fn enabled() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_LAZYRAD");
    std::env::var_os("LAZYOS_LAZYRAD").as_deref() == Some(OsStr::new("1"))
}

/// Add the sample projects when `LAZYOS_LAZYRAD=1`; the package itself is
/// added with the other core packages (`xui_embed::embed_xui_apps`).
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=LAZYRAD_SAMPLES");
    // Watched even when missing: a program built later triggers a rebuild.
    for relative in ELFS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(relative).display()
        );
    }
    if enabled() {
        embed_samples(sink, manifest_dir);
    }
}

/// Copy every directory in `LAZYRAD_SAMPLES` under [`SAMPLES_ROOT`]`/<directory>/`.
///
/// Relative entries resolve against the manifest dir. A duplicate directory
/// name is a warning: the first wins, so two projects can never silently
/// overwrite each other in the image.
fn embed_samples(sink: &mut dyn Sink, manifest_dir: &Path) {
    let Some(list) = std::env::var_os("LAZYRAD_SAMPLES") else {
        return;
    };
    let mut seen: Vec<String> = Vec::new();
    for entry in std::env::split_paths(&list) {
        let source = if entry.is_absolute() {
            entry
        } else {
            manifest_dir.join(entry)
        };
        // Tracked even when missing, so a sample added later is picked up.
        println!("cargo:rerun-if-changed={}", source.display());
        if !source.is_dir() {
            println!(
                "cargo:warning=LAZYRAD_SAMPLES entry is not a directory: {}",
                source.display()
            );
            continue;
        }
        let Some(name) = safe_component(&source) else {
            println!(
                "cargo:warning=LAZYRAD_SAMPLES entry has no usable name: {}",
                source.display()
            );
            continue;
        };
        if seen.contains(&name) {
            println!("cargo:warning=LAZYRAD_SAMPLES duplicate project {name}; keeping the first");
            continue;
        }
        seen.push(name.clone());
        copy_dir(sink, &source, &format!("{SAMPLES_ROOT}/{name}"));
    }
}

/// The destination directory name for a sample project, or `None` when the
/// path has no normal final component (so it cannot escape [`SAMPLES_ROOT`]).
fn safe_component(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name.to_string())
}

/// Recursively add every regular file under `source` to the image as
/// `destination/<name>`. Symlinks are skipped so a sample tree cannot pull in
/// files outside itself.
fn copy_dir(sink: &mut dyn Sink, source: &Path, destination: &str) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
                .path()
        })
        .collect();
    entries.sort();
    for path in entries {
        let metadata = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()));
        if metadata.file_type().is_symlink() {
            println!("cargo:warning=skipping symlink {}", path.display());
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            println!(
                "cargo:warning=skipping {}: name is not UTF-8",
                path.display()
            );
            continue;
        };
        let target = format!("{destination}/{name}");
        if metadata.is_dir() {
            copy_dir(sink, &path, &target);
        } else if metadata.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            sink.add_file(&target, path);
        }
    }
}
