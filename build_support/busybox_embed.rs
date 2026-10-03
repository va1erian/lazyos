//! Embed BusyBox, the system shell (issue #254), as `/system/bin/busybox`.

use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// Add BusyBox to `/system/bin` when one is available (see the body).
pub fn embed(files: &mut dyn Sink, manifest_dir: &Path) {
    // BusyBox is the system shell (issue #254). It is a fetched/built artifact
    // (see `tools/abi/busybox.py`), so embed it automatically whenever it is
    // available — `LAZYOS_BUSYBOX` overrides the search. The ABI bench embeds a
    // fixture as `abi-init` instead (`LAZYOS_INIT`); skipping BusyBox then
    // keeps those images small and lets the fixture own the boot. Without a
    // BusyBox the image still boots, just without a console shell,
    // and this warns so the reason is visible in the build log.
    println!("cargo:rerun-if-env-changed=LAZYOS_BUSYBOX");
    println!("cargo:rerun-if-env-changed=LAZYOS_BUSYBOX_TEST");
    let explicit = std::env::var_os("LAZYOS_BUSYBOX")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let busybox = explicit.or_else(|| {
        if std::env::var_os("LAZYOS_INIT").is_some() {
            None
        } else {
            // Watch every candidate, not just the pick: a BusyBox built or
            // dropped in later (or outranking the pick) must rebuild the image.
            for path in busybox_candidates(manifest_dir) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            find_busybox(manifest_dir)
        }
    });
    match busybox {
        Some(path) => {
            println!("cargo:warning=LAZYOS_BUSYBOX embedded: {}", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
            files.add_file(fhs::bin::BUSYBOX, path);
        }
        None => println!(
            "cargo:warning=LAZYOS_BUSYBOX unavailable; the image will have no console shell \
             (run tools/abi/busybox.py for build/supply instructions)"
        ),
    }
}

/// Locate a BusyBox built by `tools/abi/busybox.py`, whether it was dropped by
/// hand (`tools/abi/busybox`) or built into the ABI cache
/// (`target/abi/busybox/busybox`). Returns `None` when the host could not build
/// one, which makes the image boot without a console shell.
fn find_busybox(manifest_dir: &Path) -> Option<PathBuf> {
    busybox_candidates(manifest_dir)
        .into_iter()
        .find(|path| path.is_file())
}

/// Where a BusyBox may appear, highest priority first.
fn busybox_candidates(manifest_dir: &Path) -> Vec<PathBuf> {
    [
        "tools/abi/busybox",
        "target/abi/busybox/busybox",
        "target/abi/fixtures/busybox.elf",
    ]
    .iter()
    .map(|relative| manifest_dir.join(relative))
    .collect()
}
