//! Embed the HTTPS client (`LAZYOS_TLS=1`; docs/tls-plan.md §7).
//!
//! `fetch` is a static-musl `std` program built by `tools/nettls/build.py`
//! (a build artifact, never committed). It picks its option syntax from
//! `argv[0]`, so the same bytes are stored three times: `fhs::bin::FETCH`,
//! `fhs::bin::CURL` and `fhs::bin::WGET` (the ext2 populator has no hard
//! links; the copies cost a few MiB). The kernel's Linux loader maps `curl`,
//! `/usr/bin/curl`, `/bin/wget` and the like to those files before any BusyBox
//! applet alias, so BusyBox's `wget`, which does not verify certificates, is
//! out of reach once `/system/bin/wget` exists
//! (`kernel/src/process/linux/path.rs`). Without `LAZYOS_TLS=1` nothing is
//! embedded and `wget` stays BusyBox's.

use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// Where the built client appears.
const BUILT: &str = "target/nettls/fetch.elf";

/// The names the client is stored under.
pub const NAMES: [&str; 3] = [fhs::bin::FETCH, fhs::bin::CURL, fhs::bin::WGET];

/// Whether `LAZYOS_TLS=1`.
pub fn enabled() -> bool {
    std::env::var_os("LAZYOS_TLS").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// The binary to embed: `LAZYOS_FETCH` when it names a file, else the output
/// of `tools/nettls/build.py`. `None` when neither exists.
pub fn find(manifest_dir: &Path) -> Option<PathBuf> {
    let explicit = std::env::var_os("LAZYOS_FETCH")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    explicit.or_else(|| Some(manifest_dir.join(BUILT)).filter(|path| path.is_file()))
}

/// Add `fetch`, `curl` and `wget` to `/system/bin` when `LAZYOS_TLS=1` and the
/// client is available; warn and continue when it is not.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=LAZYOS_TLS");
    println!("cargo:rerun-if-env-changed=LAZYOS_FETCH");
    // Watched even when missing: a client built later rebuilds the image.
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(BUILT).display()
    );
    if !enabled() {
        return;
    }
    let Some(path) = find(manifest_dir) else {
        println!(
            "cargo:warning=LAZYOS_TLS=1 but no HTTPS client was found; the image will have no \
             fetch/curl/wget beyond BusyBox's (build it with tools/nettls/build.py)"
        );
        return;
    };
    println!(
        "cargo:warning=LAZYOS_TLS embedded: {} as fetch, curl, wget",
        path.display()
    );
    println!("cargo:rerun-if-changed={}", path.display());
    for name in NAMES {
        sink.add_file(name, path.clone());
    }
}
