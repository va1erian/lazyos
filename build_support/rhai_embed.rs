//! Embed the `rhai` command in the disk image (issue #319).
//!
//! `rhai` is a static-musl `std` program built by `tools/rhai/build.py` (a
//! build artifact, never committed), so like BusyBox it is embedded whenever it
//! is available. It is stored as `RHAI.ELF`: the kernel's Linux loader maps
//! `rhai` typed at the `sh` prompt (or `/usr/local/bin/rhai`) to the image-root
//! `RHAI.ELF` (`kernel/src/process/linux/path.rs`), and the 8.3 name is what
//! the kernel's short-name FAT reader resolves.

use std::path::{Path, PathBuf};

/// Where the built command may appear.
const BUILT: &str = "target/rhai/rhai.elf";

/// The binary to embed: `LAZYOS_RHAI` when it names a file, else the output of
/// `tools/rhai/build.py`. Returns `None` when neither exists.
pub fn find(manifest_dir: &Path) -> Option<PathBuf> {
    let explicit = std::env::var_os("LAZYOS_RHAI")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    explicit.or_else(|| Some(manifest_dir.join(BUILT)).filter(|path| path.is_file()))
}

/// Add `RHAI.ELF` to the image when the command is available. The ABI bench
/// (`LAZYOS_INIT`) keeps its baseline image size and boot time, so it skips it.
pub fn embed(builder: &mut bootloader::DiskImageBuilder, manifest_dir: &Path) {
    println!("cargo:rerun-if-env-changed=LAZYOS_RHAI");
    // Watched even when missing: an ELF built later triggers an image rebuild.
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(BUILT).display()
    );
    if std::env::var_os("LAZYOS_INIT").is_some() {
        return;
    }
    match find(manifest_dir) {
        Some(path) => {
            println!("cargo:warning=LAZYOS_RHAI embedded: {}", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
            builder.set_file(String::from("RHAI.ELF"), path);
        }
        None => println!(
            "cargo:warning=rhai unavailable; the image will have no `rhai` command \
             (build it with tools/rhai/build.py)"
        ),
    }
}
