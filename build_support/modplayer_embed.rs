//! Embed the LazyRAD MOD player package in the disk image
//! (`docs/lazyrad-modplay.md`).
//!
//! `tools/lazyrad/package.py` builds `target/pkg/MODPLAY.LZP`: the
//! `lazyrad-os/samples/modplayer` project with the `lrplay` player, packaged
//! as `org.lazy.modplayer` the way the IDE's Make LazyOS App packages a
//! project. With `LAZYOS_MODPLAYER=1` it is placed at the OS volume root as
//! `/MODPLAY.LZP`, ready for `pkgctl install /MODPLAY.LZP` or the Installer;
//! once installed, `init` lists it with the other installed apps. With the
//! switch unset the image is unchanged.

use std::ffi::OsStr;
use std::path::Path;

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/MODPLAY.LZP";
/// Its name at the OS volume root.
const DISK_NAME: &str = "MODPLAY.LZP";

/// Add `/MODPLAY.LZP` when `LAZYOS_MODPLAYER=1`. A missing package fails the
/// build, so an image that asked for the player never silently lacks it.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/modplayer_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_MODPLAYER");
    let path = manifest_dir.join(PACKAGE);
    // Watched even when missing: a package built later triggers an image rebuild.
    println!("cargo:rerun-if-changed={}", path.display());
    if std::env::var_os("LAZYOS_MODPLAYER").as_deref() != Some(OsStr::new("1")) {
        return;
    }
    if !path.is_file() {
        panic!(
            "LAZYOS_MODPLAYER=1 but {} is missing; run `python tools/lazyrad/package.py`",
            path.display()
        );
    }
    println!(
        "cargo:warning=LAZYOS_MODPLAYER embedded: {} as /{DISK_NAME}",
        path.display()
    );
    sink.add_file(DISK_NAME, path);
}
