//! Embed the SRB2Kart package and its game data in the disk image
//! (`docs/srb2kart-port-plan.md`).
//!
//! With `LAZYOS_SRB2KART=1` the image gets:
//!
//! - `target/pkg/srb2kart.lzp` (engine only, built by
//!   `tools/srb2kart/build.py`) as `/system/share/samples/srb2kart.lzp`
//!   (`fhs::share::SRB2KART_LZP`), a *user* package installed with
//!   `pkgctl install`;
//! - every file of `target/srb2kart/data/` (fetched by the same tool from
//!   Kart Krew's own release, never committed) under
//!   `/system/share/srb2kart/` (`fhs::share::SRB2KART_DATA`), one copy,
//!   read-only, outside `assets/` (whose manifest requires a free licence);
//! - `srb2kart/CREDITS.txt` beside the data as `CREDITS.txt`.
//!
//! The data is not free and not ours: an image holding it is never
//! published (`srb2kart/README.md`). A missing input is a
//! `cargo:warning=`, not a failure, so a plain `cargo build` with the switch
//! left over in the environment still produces an image. The files are
//! large (`music.kart` is about 105 MB) and are read one at a time while the
//! volume is written, so the build holds one file in memory at a time.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/srb2kart.lzp";
/// The fetched data, relative to the manifest dir.
const DATA: &str = "target/srb2kart/data";
/// The credits text, relative to the manifest dir.
const CREDITS: &str = "srb2kart/CREDITS.txt";
/// The credits file's name beside the data.
const CREDITS_NAME: &str = "CREDITS.txt";

/// Where the data file at `relative` (path under the data directory, with
/// `/` separators) lands in the image.
pub fn data_destination(relative: &str) -> String {
    format!("{}/{}", fhs::share::SRB2KART_DATA, relative)
}

/// Every regular file under `dir` as (path relative to `dir` with `/`
/// separators, full path), sorted by name so the image is reproducible.
pub fn data_files(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &relative, out);
            } else if path.is_file() {
                out.push((relative, path));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out
}

/// Add the package, the data and the credits when `LAZYOS_SRB2KART=1`.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/srb2kart_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_SRB2KART");
    let package = manifest_dir.join(PACKAGE);
    let data = manifest_dir.join(DATA);
    let credits = manifest_dir.join(CREDITS);
    // Watched even when missing: an artifact built later triggers an image rebuild.
    for path in [&package, &data, &credits] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if std::env::var_os("LAZYOS_SRB2KART").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    if package.is_file() {
        println!(
            "cargo:warning=LAZYOS_SRB2KART embedded: {} as {}",
            package.display(),
            fhs::share::SRB2KART_LZP
        );
        sink.add_file(fhs::share::SRB2KART_LZP, package);
    } else {
        println!(
            "cargo:warning=LAZYOS_SRB2KART=1 but {} is missing; run `python tools/srb2kart/build.py`",
            package.display()
        );
    }
    let files = data_files(&data);
    if files.is_empty() {
        println!(
            "cargo:warning=LAZYOS_SRB2KART=1 but {} has no game data; run `python tools/srb2kart/build.py`",
            data.display()
        );
    }
    for (relative, path) in files {
        sink.add_file(&data_destination(&relative), path);
    }
    if credits.is_file() {
        sink.add_file(&data_destination(CREDITS_NAME), credits);
    } else {
        println!("cargo:warning={} is missing", credits.display());
    }
}
