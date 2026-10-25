//! `LAZYOS_RUSTC=1` (docs/self-hosting-plan.md): the staged compiler lands with `/system` as its
//! sysroot, its tools are executable, the loader is left to `LAZYOS_DYNLINK` when both are on, and
//! the cargo sample lands without a host build's `target/`.

use std::path::{Path, PathBuf};

use crate::os_image::OsFiles;
use crate::rustc_embed::{place, place_sample};

const RUSTLIB_TOOL: &str = "rustlib/x86_64-unknown-linux-musl/bin/rust-lld";
const RLIB: &str = "rustlib/x86_64-unknown-linux-musl/lib/libstd-0.rlib";

/// A staged tree with the compiler, cargo, rustdoc, the driver, one rlib, the linker and the loader.
fn staged(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustc-embed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for name in [
        "bin/rustc",
        "bin/cargo",
        "bin/rustdoc",
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
    assert_eq!(mode(fhs::bin::CARGO), Some(0o755));
    assert_eq!(mode(fhs::bin::RUSTDOC), Some(0o755));
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
    assert_eq!(files.len(), 7);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynlink_keeps_its_own_loader() {
    let dir = staged("dynlink");
    let files = placed(&dir, true);
    assert!(!files
        .iter()
        .any(|(path, _)| path == "/system/lib/ld-musl-x86_64.so.1"));
    assert_eq!(files.len(), 6);
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

#[test]
fn the_cargo_sample_lands_without_a_host_target_dir() {
    let dir = std::env::temp_dir().join(format!("cargo-sample-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for name in ["Cargo.toml", "hello/src/main.rs", "target/debug/hello"] {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, name.as_bytes()).unwrap();
    }
    let mut files = OsFiles::default();
    place_sample(&mut files, &dir);
    let mut paths: Vec<String> = files.files().into_iter().map(|file| file.path).collect();
    paths.sort();
    let sample = fhs::share::CARGO_SAMPLE;
    assert_eq!(
        paths,
        [
            format!("{sample}/Cargo.toml"),
            format!("{sample}/hello/src/main.rs")
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shipped_sample_is_a_workspace_with_its_lock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/selfhost/cargo-hello");
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "greet/src/lib.rs",
        "hello/src/main.rs",
    ] {
        assert!(root.join(name).is_file(), "{name}");
    }
}
