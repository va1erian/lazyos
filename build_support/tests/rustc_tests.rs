//! `LAZYOS_RUSTC=1` (docs/self-hosting-plan.md): the staged compiler lands with `/system` as its
//! sysroot, its tools are executable, and the loader is left to `LAZYOS_DYNLINK` when both are on.

use std::path::{Path, PathBuf};

use crate::os_image::OsFiles;
use crate::rustc_embed::place;

const RUSTLIB_TOOL: &str = "rustlib/x86_64-unknown-linux-musl/bin/rust-lld";
const RLIB: &str = "rustlib/x86_64-unknown-linux-musl/lib/libstd-0.rlib";

/// A staged tree with the compiler, its driver, one rlib, the linker and the loader.
fn staged(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustc-embed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for name in [
        "bin/rustc",
        "lib/librustc_driver-0.so",
        "lib/ld-musl-x86_64.so.1",
        &format!("lib/{RUSTLIB_TOOL}"),
        &format!("lib/{RLIB}"),
    ] {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, name.as_bytes()).unwrap();
    }
    dir
}

fn placed(dir: &Path, dynlink: bool) -> Vec<(String, u16)> {
    let mut files = OsFiles::default();
    assert!(place(&mut files, dir, dynlink));
    files
        .files()
        .into_iter()
        .map(|file| (file.path, file.mode))
        .collect()
}

#[test]
fn the_compiler_lands_with_system_as_its_sysroot() {
    let dir = staged("layout");
    let files = placed(&dir, false);
    let mode = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, m)| *m);
    assert_eq!(mode(fhs::bin::RUSTC), Some(0o755));
    assert_eq!(mode("/system/lib/librustc_driver-0.so"), Some(0o755));
    // The linker is a program rustc runs: executable, under the sysroot's rustlib.
    assert_eq!(
        mode(&format!(
            "{}/x86_64-unknown-linux-musl/bin/rust-lld",
            fhs::RUSTLIB
        )),
        Some(0o755)
    );
    assert!(mode(&format!("/system/lib/{RLIB}")).is_some());
    assert!(mode("/system/lib/ld-musl-x86_64.so.1").is_some());
    assert_eq!(files.len(), 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynlink_keeps_its_own_loader() {
    let dir = staged("dynlink");
    let files = placed(&dir, true);
    assert!(!files
        .iter()
        .any(|(path, _)| path == "/system/lib/ld-musl-x86_64.so.1"));
    assert_eq!(files.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_is_placed_without_a_staged_compiler() {
    let mut files = OsFiles::default();
    assert!(!place(
        &mut files,
        Path::new("/nonexistent/selfhost/system"),
        false
    ));
    assert_eq!(files.len(), 0);
}
