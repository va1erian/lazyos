//! Embed the Rust compiler (`LAZYOS_RUSTC=1`, docs/self-hosting-plan.md).
//!
//! `tools/selfhost/build.py` cross-compiles rustc for `x86_64-unknown-linux-musl` (the ABI
//! LazyOS runs) and stages it as a `/system`-shaped tree in `target/selfhost/system`:
//! `bin/rustc`, and in `lib/` the driver library, `rustlib/` (the standard library and
//! `rust-lld`), musl's dynamic linker and `libgcc_s`. Placed at `/system`, the compiler's
//! sysroot is `/system` itself: rustc finds it from where its driver library was loaded.
//! It is hundreds of MB, so it wants a larger OS volume (`LAZYOS_OS_SIZE`).

use std::path::Path;

use crate::os_image::Sink;

/// Where `tools/selfhost/build.py stage` writes the tree.
const STAGED: &str = "target/selfhost/system";

/// The dynamic linker, which `LAZYOS_DYNLINK=1` also places (the same musl program).
const LOADER: &str = "ld-musl-x86_64.so.1";

/// Add the staged compiler when `LAZYOS_RUSTC=1`.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/rustc_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_RUSTC");
    if std::env::var("LAZYOS_RUSTC").as_deref() != Ok("1") {
        return;
    }
    let staged = manifest_dir.join(STAGED);
    crate::os_image::rerun_if_changed(&staged.join("bin").join("rustc"));
    let dynlink = std::env::var("LAZYOS_DYNLINK").as_deref() == Ok("1");
    if place(sink, &staged, dynlink) {
        println!("cargo:warning=LAZYOS_RUSTC embedded: {}", staged.display());
    } else {
        println!(
            "cargo:warning=LAZYOS_RUSTC: {} not staged, no compiler (run tools/selfhost/build.py)",
            staged.display()
        );
    }
}

/// Place the tree `staged`: `bin/rustc` at [`fhs::bin::RUSTC`], everything under `lib/` at the
/// same path under [`fhs::SYSTEM_LIB`], without the loader when `dynlink` places it already.
/// False (nothing placed) when there is no `bin/rustc`.
pub fn place(sink: &mut dyn Sink, staged: &Path, dynlink: bool) -> bool {
    let rustc = staged.join("bin").join("rustc");
    if !rustc.is_file() {
        return false;
    }
    sink.add_file(fhs::bin::RUSTC, rustc);
    add_tree(sink, &staged.join("lib"), fhs::SYSTEM_LIB, &|name| {
        !(dynlink && name == LOADER)
    });
    true
}

/// Store every regular file under `dir`, recursively, at the same relative path under
/// `image_dir`, skipping top-level names `keep` refuses.
fn add_tree(sink: &mut dyn Sink, dir: &Path, image_dir: &str, keep: &dyn Fn(&str) -> bool) {
    crate::os_image::rerun_if_changed(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| keep(name)) else {
            continue;
        };
        let path = format!("{image_dir}/{name}");
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => add_tree(sink, &entry.path(), &path, &|_| true),
            Ok(kind) if kind.is_file() => sink.add_file(&path, entry.path()),
            _ => {}
        }
    }
}
