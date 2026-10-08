//! Embed the emusic package in the disk image (`docs/media-plan.md`).
//!
//! `tools/emusic/build.py` builds `target/pkg/emusic.lzp`: va1erian/emusic at
//! a pinned revision with its LazyOS audio backend (a build artifact of
//! fetched third-party code, never committed). With `LAZYOS_EMUSIC=1` it is
//! placed with the other samples as `/system/share/samples/emusic.lzp`
//! (`fhs::share::EMUSIC_LZP`): a *user* package, like Doom, so nothing is
//! pre-installed. With the switch unset the image is unchanged.

use std::ffi::OsStr;
use std::path::Path;

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/emusic.lzp";

/// Add [`fhs::share::EMUSIC_LZP`] when `LAZYOS_EMUSIC=1`. A missing package
/// fails the build, so an image that asked for emusic never silently comes
/// without it.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/emusic_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_EMUSIC");
    let path = manifest_dir.join(PACKAGE);
    // Watched even when missing: a package built later triggers an image rebuild.
    println!("cargo:rerun-if-changed={}", path.display());
    if std::env::var_os("LAZYOS_EMUSIC").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    if !path.is_file() {
        panic!(
            "LAZYOS_EMUSIC=1 but {} is missing; run `python tools/emusic/build.py`",
            path.display()
        );
    }
    println!(
        "cargo:warning=LAZYOS_EMUSIC embedded: {} as {}",
        path.display(),
        fhs::share::EMUSIC_LZP
    );
    sink.add_file(fhs::share::EMUSIC_LZP, path);
}
