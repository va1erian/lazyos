//! Embed the LazyRAD MOD player package in the disk image
//! (`docs/lazyrad-modplay.md`).
//!
//! `tools/lazyrad/package.py` builds `target/pkg/modplayer.lzp`: the
//! `lazyrad-os/samples/modplayer` project with the `lrplay` player, packaged
//! as `org.lazy.modplayer` the way the IDE's Make LazyOS App packages a
//! project. With `LAZYOS_MODPLAYER=1` it is placed with the other samples as
//! `/system/share/samples/modplayer.lzp` (`fhs::share::MODPLAYER_LZP`): a
//! *user* package, like Doom's, so nothing is pre-installed. A user copies it
//! to their home and installs it with `pkgctl install` or the Installer;
//! `init` then lists it with the other installed apps. With the switch unset
//! the image is unchanged.

use std::ffi::OsStr;
use std::path::Path;

use crate::os_image::Sink;

/// The package, relative to the manifest dir.
const PACKAGE: &str = "target/pkg/modplayer.lzp";

/// Add [`fhs::share::MODPLAYER_LZP`] when `LAZYOS_MODPLAYER=1`. A missing
/// package fails the build, so an image that asked for the player never
/// silently lacks it.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/modplayer_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_MODPLAYER");
    embed_test_packages(sink);
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
        "cargo:warning=LAZYOS_MODPLAYER embedded: {} as {}",
        path.display(),
        fhs::share::MODPLAYER_LZP
    );
    sink.add_file(fhs::share::MODPLAYER_LZP, path);
}

/// Test packages a session installs (`LAZYOS_TEST_PACKAGES`, a platform path
/// list of `.lzp` files): each is placed in [`fhs::share::SAMPLES`] under its
/// own file name, a user package nothing pre-installs. Only test images set
/// it (`tools/screenshot/examples/app_crash_notice.json` installs
/// `crashload.lzp`, issue #549); a missing file fails the build.
fn embed_test_packages(sink: &mut dyn Sink) {
    println!("cargo:rerun-if-env-changed=LAZYOS_TEST_PACKAGES");
    let Some(list) = std::env::var_os("LAZYOS_TEST_PACKAGES") else {
        return;
    };
    for path in std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()) {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|name| name.ends_with(".lzp"))
            .unwrap_or_else(|| panic!("LAZYOS_TEST_PACKAGES: {} is not a .lzp", path.display()))
            .to_owned();
        assert!(
            path.is_file(),
            "LAZYOS_TEST_PACKAGES: {} is missing",
            path.display()
        );
        let destination = format!("{}/{name}", fhs::share::SAMPLES);
        println!(
            "cargo:warning=LAZYOS_TEST_PACKAGES embedded: {} as {destination}",
            path.display()
        );
        sink.add_file(&destination, path);
    }
}
