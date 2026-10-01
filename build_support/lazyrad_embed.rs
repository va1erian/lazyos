//! Embed the `lazyrad` runtime and its sample projects in the disk image.
//!
//! `lrplay` and `lazyrad` are static-musl `std` programs built by
//! `tools/lazyrad/build.py` (build artifacts, never committed), so they are
//! embedded only when `LAZYOS_LAZYRAD=1` is set. They are stored under the 8.3
//! names `LRPLAY.ELF` and `LAZYRAD.ELF` because the kernel's FAT reader
//! resolves short names. `LAZYRAD_SAMPLES` is a platform path list (`;` on
//! Windows, `:` elsewhere) of sample project directories, each copied under
//! `/LAZYRAD/<directory>/` (long names are kept: the kernel's LFN driver reads
//! them).
//!
//! With the switch unset nothing changes: the plain demo image is unchanged.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The built ELFs, as (8.3 on-disk name, path relative to the manifest dir).
const ELFS: &[(&str, &str)] = &[
    ("LRPLAY.ELF", "target/lazyrad/lrplay.elf"),
    ("LAZYRAD.ELF", "target/lazyrad/lazyrad.elf"),
];

/// The image directory the sample projects are copied under.
const SAMPLES_ROOT: &str = "LAZYRAD";

/// Add the runtime and samples when `LAZYOS_LAZYRAD=1`.
pub fn embed(builder: &mut bootloader::DiskImageBuilder, manifest_dir: &Path) {
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
    embed_elfs(builder, manifest_dir);
    embed_samples(builder, manifest_dir);
}

/// Embed `LRPLAY.ELF` and `LAZYRAD.ELF`. A missing one fails the build, so the
/// image is never silently built without the runtime it asked for.
fn embed_elfs(builder: &mut bootloader::DiskImageBuilder, manifest_dir: &Path) {
    for (disk_name, relative) in ELFS {
        let path = manifest_dir.join(relative);
        if !path.is_file() {
            panic!(
                "LAZYOS_LAZYRAD=1 but {} is missing; run `python tools/lazyrad/build.py`",
                path.display()
            );
        }
        println!(
            "cargo:warning=LAZYOS_LAZYRAD embedded: {} as {disk_name}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
        builder.set_file(String::from(*disk_name), path);
    }
}

/// Copy every directory in `LAZYRAD_SAMPLES` under `/LAZYRAD/<directory>/`.
///
/// Relative entries resolve against the manifest dir. A duplicate directory
/// name is a warning: the first wins, so two projects can never silently
/// overwrite each other in the image.
fn embed_samples(builder: &mut bootloader::DiskImageBuilder, manifest_dir: &Path) {
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
        copy_dir(builder, &source, &format!("{SAMPLES_ROOT}/{name}"));
    }
}

/// The destination directory name for a sample project, or `None` when the
/// path has no normal final component (so it cannot escape `/LAZYRAD/`).
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
fn copy_dir(builder: &mut bootloader::DiskImageBuilder, source: &Path, destination: &str) {
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
            copy_dir(builder, &path, &target);
        } else if metadata.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            builder.set_file(target, path);
        }
    }
}
