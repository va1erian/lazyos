//! Embed the Quake package in the disk image (`docs/quake-port-plan.md`).
//!
//! `tools/quake/build.py` builds `target/pkg/quake.lzp`: the quake-srp
//! engine and its LazyOS record bridge, with id's freely redistributable
//! shareware pak inside (a build artifact holding fetched third-party
//! code and data, never committed). With `LAZYOS_QUAKE=1` it is placed
//! with the other samples as `/system/share/samples/quake.lzp`
//! (`fhs::share::QUAKE_LZP`): a *user* package, not a core one, so
//! nothing is pre-installed. A user copies it to their home and installs
//! it with `pkgctl install` or the Installer; `init` then lists it with
//! the other installed apps. With the switch unset the image is unchanged
//! (the package adds about 18 MiB).

use std::ffi::OsStr;
use std::path::Path;

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/quake.lzp";

/// Add [`fhs::share::QUAKE_LZP`] when `LAZYOS_QUAKE=1`. A missing package
/// fails the build, so an image that asked for Quake never silently comes
/// without it.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/quake_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_QUAKE");
    let path = manifest_dir.join(PACKAGE);
    // Watched even when missing: a package built later triggers an image
    // rebuild.
    println!("cargo:rerun-if-changed={}", path.display());
    if std::env::var_os("LAZYOS_QUAKE").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    if !path.is_file() {
        panic!(
            "LAZYOS_QUAKE=1 but {} is missing; run `python tools/quake/build.py`",
            path.display()
        );
    }
    println!(
        "cargo:warning=LAZYOS_QUAKE embedded: {} as {}",
        path.display(),
        fhs::share::QUAKE_LZP
    );
    sink.add_file(fhs::share::QUAKE_LZP, path);
}
