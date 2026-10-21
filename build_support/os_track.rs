//! What the image build asks cargo to watch.
//!
//! Cargo takes a `rerun-if-changed` path that does not exist for "changed
//! since the last run", always: the build script, the crate it belongs to (and
//! here the kernel too) were then built again by every `cargo build`, and the
//! whole image rewritten, for as long as one optional output was missing (the
//! LazyWeb+ browser's `target/xui/xui-lazywebplus.elf` in an image that does not
//! have it, a sample package not built yet).

use std::path::{Component, Path, PathBuf};

use crate::os_image::{format_uuid, Action, Plan};

/// Print how long the stage that just ended took and restart the clock: the
/// launcher shows these lines, so a slow rebuild says where its time went.
pub fn lap(stage: &str, clock: &mut std::time::Instant) {
    println!(
        "cargo:warning=image build: {stage} {:.1}s",
        clock.elapsed().as_secs_f64()
    );
    *clock = std::time::Instant::now();
}

/// The one line that says which OS volume the build made or updated.
pub fn report(plan: &Plan, files: usize, os_size: u64) {
    println!(
        "cargo:warning=OS volume {} ({files} files, {} MiB): {}",
        format_uuid(plan.uuid),
        os_size >> 20,
        match plan.action {
            Action::Create => "created",
            Action::Update { .. } => "updated in place",
        }
    );
}

/// Ask cargo to run the build script again when `path` changes.
///
/// A `path` that does not exist (yet) inside the repository is watched through
/// the directory that will hold it: the directory's time changes when the file
/// appears, and the build notices that, as it noticed the file. A directory
/// below `target/` is made when it is missing, since there is nothing else
/// small enough to watch. Never `target/` itself (its time moves with every
/// artifact cargo writes) or the repository root.
///
/// A missing path anywhere else (one a variable names) is passed on as it is,
/// and cargo runs the build again, which is the point of naming a path that
/// is not there.
pub fn rerun_if_changed(path: &Path) {
    let root = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let directory = match root {
        Some(root) if !path.exists() => watch_for_missing(path, &root),
        _ => None,
    };
    let watched = match directory {
        Some(directory) => {
            // Below `target/` and not there yet: made, so that it can be watched.
            let _ = std::fs::create_dir_all(&directory);
            directory
        }
        None => path.to_path_buf(),
    };
    println!("cargo:rerun-if-changed={}", watched.display());
}

/// The directory to watch in place of the missing `path`, or `None` to watch
/// the path itself. `root` is the repository.
fn watch_for_missing(path: &Path, root: &Path) -> Option<PathBuf> {
    // `a/../../b` starts like a path of ours and is not: a lexical check
    // cannot tell, so such a path is never rewritten.
    if path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    // Relative paths are the repository's own (the build runs from its root).
    let parent = root.join(path.parent()?);
    let output = root.join("target");
    if !parent.starts_with(root) || parent == root || parent == output {
        return None;
    }
    // A directory that is there is watched as it is; below `target/` one that
    // is not there is made.
    (parent.is_dir() || parent.starts_with(&output)).then_some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir().join("lazyos-os-track-root")
    }

    #[test]
    fn a_missing_file_is_watched_through_its_directory() {
        let root = root();
        let xui = root.join("target/xui");
        assert_eq!(
            watch_for_missing(&xui.join("xui-lazywebplus.elf"), &root),
            Some(xui)
        );
        // A path of the repository that is relative.
        assert_eq!(
            watch_for_missing(Path::new("target/pkg/pkgdemo.lzp"), &root),
            Some(root.join("target/pkg"))
        );
    }

    #[test]
    fn never_target_itself_or_the_repository_root() {
        let root = root();
        assert_eq!(
            watch_for_missing(&root.join("target/firmware"), &root),
            None
        );
        assert_eq!(watch_for_missing(&root.join("Cargo.lock.new"), &root), None);
        assert_eq!(watch_for_missing(Path::new("target"), &root), None);
    }

    #[test]
    fn nothing_outside_the_repository_or_with_a_parent_component() {
        let root = root();
        let elsewhere = std::env::temp_dir().join("elsewhere/x.elf");
        assert_eq!(watch_for_missing(&elsewhere, &root), None);
        assert_eq!(
            watch_for_missing(&root.join("target/../../x/y.bin"), &root),
            None
        );
        assert_eq!(watch_for_missing(Path::new("../x/y.bin"), &root), None);
    }

    #[test]
    fn a_missing_directory_outside_target_is_not_made() {
        let root = root();
        // `tools/nothing-here` is not a directory and not below `target/`.
        assert_eq!(
            watch_for_missing(&root.join("tools/nothing-here/busybox"), &root),
            None
        );
    }
}
