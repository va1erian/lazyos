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

use std::collections::HashSet;
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

/// Place every file of `stage` at the same relative path under [`fhs::LAZYVM`], following
/// links. An error names what is wrong: no stage, a stage without `bin/lazyvm`, or an entry
/// that cannot be placed (a broken link, a name that is not UTF-8, a directory reached a
/// second time through a link).
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
    let mut seen = HashSet::new();
    add_tree(sink, stage, fhs::LAZYVM, false, &mut seen)
}

/// Store the files under `dir`, recursively, at the same relative path under `image_dir`;
/// `program` (set below the stage's top-level `bin/`) makes them 0755. A link is followed
/// (`fs::metadata`), so the image holds what it points at. `seen` holds every directory
/// walked so far, by its real path: a directory met twice (a link cycle, or two links to
/// one tree) is an error, so the walk ends after visiting each real directory once.
fn add_tree(
    sink: &mut dyn Sink,
    dir: &Path,
    image_dir: &str,
    program: bool,
    seen: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    let real = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !seen.insert(real.clone()) {
        return Err(format!(
            "{}: the directory {} is reached a second time (a link cycle, or two links to it)",
            dir.display(),
            real.display()
        ));
    }
    crate::os_image::rerun_if_changed(dir);
    let unreadable = |e: std::io::Error| format!("{}: {e}", dir.display());
    let mut entries = std::fs::read_dir(dir)
        .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        .map_err(unreadable)?;
    entries.sort_by_key(|entry| entry.file_name());
    let top = image_dir == fhs::LAZYVM;
    for entry in entries {
        let source = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("{}: the name is not UTF-8", source.display()))?;
        let path = format!("{image_dir}/{name}");
        let kind = std::fs::metadata(&source)
            .map_err(|e| format!("{}: {e} (a broken link?)", source.display()))?;
        if kind.is_dir() {
            let below_bin = program || (top && name == "bin");
            add_tree(sink, &source, &path, below_bin, seen)?;
        } else if kind.is_file() {
            let mode = if program { 0o755 } else { 0o644 };
            sink.add(&path, Source::Path(source), mode);
        } else {
            return Err(format!("{}: not a file or a directory", source.display()));
        }
    }
    Ok(())
}
