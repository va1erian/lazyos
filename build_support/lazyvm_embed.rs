//! Embed LazyVM's staging tree (`LAZYOS_LAZYVM=1`, docs/agents/lazyvm.md).
//!
//! LazyVM (its own repository) stages what a LazyOS boot needs in one directory:
//! `bin/lazyvm` (a static `x86_64-unknown-linux-musl` program, run through the Linux ABI),
//! its class library (`lib/lazyrt.jar` or the class tree `lib/lazyrt/`) and its tests
//! (`tests/`). The tree is `target/lazyvm/stage`, or the directory `LAZYOS_LAZYVM_STAGE`
//! names, and lands as it is under [`fhs::LAZYVM`]; everything under its `bin/` is
//! executable. With the switch on, a missing stage fails the build: the image would
//! otherwise boot without the VM the caller asked for.
//!
//! `LAZYOS_LAZYVM_COMMANDS=<file>` (written by `tools/lazyvm/run.py`) adds the command list
//! the `lazyvm` ABI fixture runs at [`fhs::LAZYVM_COMMANDS`].

use std::path::{Path, PathBuf};

use crate::os_image::{Sink, Source};

/// The default stage, relative to the LazyOS checkout.
const DEFAULT_STAGE: &str = "target/lazyvm/stage";

/// Add the stage (and the command list) when `LAZYOS_LAZYVM=1`.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/lazyvm_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_LAZYVM");
    println!("cargo:rerun-if-env-changed=LAZYOS_LAZYVM_STAGE");
    println!("cargo:rerun-if-env-changed=LAZYOS_LAZYVM_COMMANDS");
    if std::env::var("LAZYOS_LAZYVM").as_deref() != Ok("1") {
        return;
    }
    let stage = stage_dir(manifest_dir, std::env::var_os("LAZYOS_LAZYVM_STAGE"));
    if let Err(reason) = place(sink, &stage) {
        panic!("LAZYOS_LAZYVM: {reason}");
    }
    if let Some(commands) = std::env::var_os("LAZYOS_LAZYVM_COMMANDS") {
        let commands = PathBuf::from(commands);
        crate::os_image::rerun_if_changed(&commands);
        if !commands.is_file() {
            panic!(
                "LAZYOS_LAZYVM_COMMANDS: {} is not a file",
                commands.display()
            );
        }
        sink.add_file(fhs::LAZYVM_COMMANDS, commands);
    }
    println!("cargo:warning=LAZYOS_LAZYVM embedded: {}", stage.display());
}

/// The stage directory: `LAZYOS_LAZYVM_STAGE` when set (relative to the checkout when not
/// absolute), else [`DEFAULT_STAGE`].
pub fn stage_dir(manifest_dir: &Path, chosen: Option<std::ffi::OsString>) -> PathBuf {
    match chosen {
        Some(dir) if !dir.is_empty() => manifest_dir.join(dir),
        _ => manifest_dir.join(DEFAULT_STAGE),
    }
}

/// Place every regular file of `stage` at the same relative path under [`fhs::LAZYVM`]. An
/// error names what is missing: no stage, or a stage without `bin/lazyvm`.
pub fn place(sink: &mut dyn Sink, stage: &Path) -> Result<(), String> {
    if !stage.is_dir() {
        return Err(format!(
            "no LazyVM stage at {} (stage LazyVM there, or point LAZYOS_LAZYVM_STAGE at one)",
            stage.display()
        ));
    }
    if !stage.join("bin").join("lazyvm").is_file() {
        return Err(format!("{} has no bin/lazyvm", stage.display()));
    }
    add_tree(sink, stage, fhs::LAZYVM, false);
    Ok(())
}

/// Store the regular files under `dir`, recursively, at the same relative path under
/// `image_dir`; `program` (set below the stage's top-level `bin/`) makes them 0755.
fn add_tree(sink: &mut dyn Sink, dir: &Path, image_dir: &str, program: bool) {
    crate::os_image::rerun_if_changed(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    let top = image_dir == fhs::LAZYVM;
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let path = format!("{image_dir}/{name}");
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => add_tree(
                sink,
                &entry.path(),
                &path,
                program || (top && name == "bin"),
            ),
            Ok(kind) if kind.is_file() => {
                let mode = if program { 0o755 } else { 0o644 };
                sink.add(&path, Source::Path(entry.path()), mode);
            }
            _ => {}
        }
    }
}
