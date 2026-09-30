//! Integration tests for [`StdPlatform`] against an isolated temp directory.
//! The directory is created fresh and removed at the end.
#![cfg(feature = "std-platform")]
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xui_explorer::platform::{Kind, Platform};
use xui_explorer::std_platform::StdPlatform;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A unique temp directory that is removed by [`TempDir`]'s `Drop`.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("xui-explorer-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn lists_files_and_folders_with_their_kinds_and_sizes() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), b"hello").expect("write");
    fs::create_dir(dir.path().join("sub")).expect("mkdir");
    fs::write(dir.path().join("sub/inner.bin"), b"0123456789").expect("write inner");

    let mut entries = StdPlatform.list(dir.path()).expect("list");
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(entries.len(), 2);

    assert_eq!(entries[0].name, "a.txt");
    assert_eq!(entries[0].meta.kind, Kind::File);
    assert_eq!(entries[0].meta.size, Some(5));

    assert_eq!(entries[1].name, "sub");
    assert_eq!(entries[1].meta.kind, Kind::Dir);

    let sub = StdPlatform
        .metadata(&dir.path().join("sub"))
        .expect("metadata");
    assert_eq!(
        sub.entries,
        Some(1),
        "a folder reports its direct entries only"
    );
}

#[test]
fn metadata_does_not_follow_a_symlink_and_remove_removes_only_the_link() {
    let dir = TempDir::new();
    let target = dir.path().join("target.txt");
    fs::write(&target, b"content").expect("write target");
    let link = dir.path().join("link.txt");
    if !create_file_symlink(&target, &link) {
        // No privilege (Windows without developer mode) or no support: skip.
        return;
    }

    let meta = StdPlatform.metadata(&link).expect("metadata");
    assert_eq!(meta.kind, Kind::Symlink, "a link is not its target");

    StdPlatform.remove(&link, false).expect("remove link");
    assert!(!link.exists(), "the link is gone");
    assert!(target.exists(), "the target survived");
}

#[test]
fn remove_directory_is_recursive_only_when_asked() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).expect("mkdir");
    fs::write(sub.join("inner.txt"), b"x").expect("write");

    assert!(
        StdPlatform.remove(&sub, false).is_err(),
        "a non-empty folder needs recursive"
    );
    StdPlatform.remove(&sub, true).expect("recursive remove");
    assert!(!sub.exists());
}

/// Creates a file symlink, returning `false` when the platform refuses.
#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

/// On Windows a file symlink needs developer mode or elevation; a failure is
/// treated as "skip".
#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

#[cfg(not(any(unix, windows)))]
fn create_file_symlink(_target: &Path, _link: &Path) -> bool {
    false
}
