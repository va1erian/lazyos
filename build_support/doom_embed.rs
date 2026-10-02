//! Embed the Doom package in the disk image (`docs/doom-port-plan.md`).
//!
//! `tools/doom/build.py` builds `target/pkg/DOOM.LZP`: the doomgeneric engine
//! and its LazyOS platform layer, with the Freedoom IWAD inside (a build
//! artifact holding fetched third-party code and data, never committed). With
//! `LAZYOS_DOOM=1` it is placed at the OS volume root as `/DOOM.LZP`, ready for
//! `pkgctl install /DOOM.LZP` or the Installer; once installed, `init` lists it
//! with the other installed apps. Nothing is pre-installed: installing goes
//! through `pkgd` like any package. With the switch unset the image is
//! unchanged (the package adds about 10 MiB).

use std::ffi::OsStr;
use std::path::Path;

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/DOOM.LZP";
/// Its name at the OS volume root.
const DISK_NAME: &str = "DOOM.LZP";

/// Add `/DOOM.LZP` when `LAZYOS_DOOM=1`. A missing package fails the build, so
/// an image that asked for Doom never silently comes without it.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/doom_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_DOOM");
    let path = manifest_dir.join(PACKAGE);
    // Watched even when missing: a package built later triggers an image rebuild.
    println!("cargo:rerun-if-changed={}", path.display());
    if std::env::var_os("LAZYOS_DOOM").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    if !path.is_file() {
        panic!(
            "LAZYOS_DOOM=1 but {} is missing; run `python tools/doom/build.py`",
            path.display()
        );
    }
    println!("cargo:warning=LAZYOS_DOOM embedded: {} as /{DISK_NAME}", path.display());
    sink.add_file(DISK_NAME, path);
}
