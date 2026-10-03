//! Embed the `lazyrad` runtime and its sample projects in the disk image.
//!
//! `lrplay` and `lazyrad` are static-musl `std` programs built by
//! `tools/lazyrad/build.py` (build artifacts, never committed), so they are
//! embedded only when `LAZYOS_LAZYRAD=1` is set. They are stored as
//! `/system/bin/lrplay` and `/system/bin/lazyrad` (`fhs::bin`).
//! `LAZYRAD_SAMPLES` is a platform path list (`;` on Windows, `:` elsewhere) of
//! sample project directories, each copied under
//! `/system/share/lazyrad/<directory>/` (`fhs::share::LAZYRAD_SAMPLES`; names
//! are kept exactly: ext2 is case-sensitive).
//!
//! When `tools/lazyrad/devtest.py` built `target/pkg/lrdev-test.lzp` (the
//! development-run test package, issue #529), it is added as
//! `/system/share/samples/lrdev-test.lzp` (`fhs::share::LRDEV_TEST_LZP`) for
//! the `lazyrad_devplay.json` session; an image without it is unchanged.
//!
//! With the switch unset nothing changes: the plain demo image is unchanged.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The built ELFs, as (image path, path relative to the manifest dir).
const ELFS: &[(&str, &str)] = &[
    (fhs::bin::LRPLAY, "target/lazyrad/lrplay.elf"),
    (fhs::bin::LAZYRAD, "target/lazyrad/lazyrad.elf"),
];

/// The development-run test package, when built (path relative to the
/// manifest dir).
const DEVTEST_LZP: &str = "target/pkg/lrdev-test.lzp";

/// The image directory the sample projects are copied under.
const SAMPLES_ROOT: &str = fhs::share::LAZYRAD_SAMPLES;

/// Add the runtime and samples when `LAZYOS_LAZYRAD=1`.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=LAZYOS_LAZYRAD");
    println!("cargo:rerun-if-env-changed=LAZYRAD_SAMPLES");
    // Watched even when missing: an ELF built later triggers an image rebuild.
    for (_, relative) in ELFS {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(relative).display()
        );
    }
    if std::env::var_os("LAZYOS_LAZYRAD").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    embed_elfs(sink, manifest_dir);
    embed_samples(sink, manifest_dir);
    embed_devtest(sink, manifest_dir);
}

/// Add the development-run test package when it was built.
fn embed_devtest(sink: &mut dyn Sink, manifest_dir: &Path) {
    let path = manifest_dir.join(DEVTEST_LZP);
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_file() {
        println!(
            "cargo:warning=LAZYOS_LAZYRAD embedded: {} as {}",
            path.display(),
            fhs::share::LRDEV_TEST_LZP
        );
        sink.add_file(fhs::share::LRDEV_TEST_LZP, path);
    }
}

/// Embed `lrplay` and `lazyrad`. A missing one fails the build, so the
/// image is never silently built without the runtime it asked for.
fn embed_elfs(sink: &mut dyn Sink, manifest_dir: &Path) {
    for (image_path, relative) in ELFS {
        let path = manifest_dir.join(relative);
        if !path.is_file() {
            panic!(
                "LAZYOS_LAZYRAD=1 but {} is missing; run `python tools/lazyrad/build.py`",
                path.display()
            );
        }
        println!(
            "cargo:warning=LAZYOS_LAZYRAD embedded: {} as {image_path}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
        sink.add_file(image_path, path);
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
