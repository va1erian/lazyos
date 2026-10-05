//! Shared helpers for the integration tests: scratch folders, a sample tree
//! on disk, and readers for what an archive holds.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lazyarc::{Archive, EntryKind, Progress};

/// A unique folder under the temp dir, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lazyarc-it-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn progress() -> Arc<Progress> {
    Arc::new(Progress::new())
}

/// `project/` with a nested folder, an empty file and a compressible file.
pub fn sample_tree(root: &Path) -> PathBuf {
    let project = root.join("project");
    fs::create_dir_all(project.join("src/deep")).unwrap();
    fs::write(project.join("README.md"), "# Project\n").unwrap();
    fs::write(project.join("empty"), "").unwrap();
    let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
    fs::write(project.join("src/main.rs"), body).unwrap();
    fs::write(
        project.join("src/deep/data.bin"),
        (0..=255u8).cycle().take(70_000).collect::<Vec<_>>(),
    )
    .unwrap();
    project
}

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Every file's `(path, data)`, sorted.
pub fn files(archive: &Archive) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    archive
        .visit(&|_| true, &progress(), &mut |entry, data| {
            let mut buf = Vec::new();
            data.read_to_end(&mut buf)?;
            if matches!(entry.kind, EntryKind::File) {
                out.push((entry.path.clone(), buf));
            }
            Ok(())
        })
        .unwrap();
    out.sort();
    out
}

/// Every regular file under `root`, as `(relative path, data)`, sorted.
pub fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            walk(root, &path, out);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}
