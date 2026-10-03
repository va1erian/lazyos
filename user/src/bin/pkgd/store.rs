//! `pkgd`'s filesystem: whether its store is writable, reading a package, and
//! the binding of `pkgstore::tree` (extraction under `/apps`, documentation
//! under `/docs/apps`, removal) to the file syscalls.
//!
//! Everything here is a root write, so every path is composed through
//! `pkgstore::layout`/`pkgstore::docs` and every removal is confined to
//! `/apps` and `/docs/apps` (`pkgstore::tree::deletable`). The ext2 volume has
//! no symlinks, so a lexically safe path is a physically safe one.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use pkgstore::layout;
use pkgstore::tree::{Node, TreeError, TreeFs};
use user::files::{self, Kind};

/// Largest package file `pkgd` reads (256 MiB). The package is streamed into
/// `pkgd`'s own heap one range at a time (`files::read_large`), so the kernel
/// never holds it whole and this only bounds `pkgd`'s buffer; what a package
/// may *expand* to is `lazypkg::MAX_TOTAL_UNCOMPRESSED`.
pub(crate) const MAX_PACKAGE_FILE: usize = 256 * 1024 * 1024;

/// The errno `files` reports for an absent path.
pub(crate) const ENOENT: i64 = 2;

/// The file syscalls, as `pkgd` (root) performs them.
pub(crate) struct SysFs;

impl TreeFs for SysFs {
    type Error = i64;

    fn stat(&mut self, path: &str) -> Result<Option<Node>, i64> {
        match files::stat(path) {
            Ok((_, Kind::Dir)) => Ok(Some(Node::Dir)),
            Ok((size, Kind::File)) => Ok(Some(Node::File(size))),
            Err(ENOENT) => Ok(None),
            Err(code) => Err(code),
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), i64> {
        files::mkdir(path)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), i64> {
        files::write_large(path, data)
    }

    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), i64> {
        files::chmod(path, mode)
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, i64> {
        let mut names = Vec::new();
        for entry in files::list(path)? {
            if entry.name != "." && entry.name != ".." {
                names.push(entry.name);
            }
        }
        Ok(names)
    }

    fn remove(&mut self, path: &str) -> Result<(), i64> {
        files::remove(path)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        files::rename(from, to)
    }
}

/// A tree operation's failure as one line of text: the step and, for a
/// filesystem error, what the errno means.
pub(crate) fn describe(error: &TreeError<i64>) -> String {
    match error {
        TreeError::Fs { step, error } => format!("{step}: {}", files::describe(*error)),
        TreeError::Bad(step) => step.clone(),
    }
}

/// Whether `path` exists.
pub(crate) fn exists(path: &str) -> bool {
    files::stat(path).is_ok()
}

/// Why `pkgd` cannot keep installed apps, if it cannot: each of `/apps`,
/// `/docs/apps` and `/logs` must be a directory (created when absent) in
/// which a probe file can be written and removed, the way `confd` probes
/// `/conf`. `Ok` means installs are possible.
pub(crate) fn probe_store() -> Result<(), String> {
    for dir in [
        layout::APPS_ROOT,
        fhs::docs::DOCS_ROOT,
        layout::DOCS_ROOT,
        layout::LOG_DIR,
    ] {
        pkgstore::tree::ensure_dir(&mut SysFs, dir).map_err(|error| describe(&error))?;
    }
    for dir in [layout::APPS_ROOT, layout::DOCS_ROOT, layout::LOG_DIR] {
        let probe = format!("{dir}/.pkgd-probe");
        files::write_file(&probe, b"ok")
            .map_err(|code| format!("{dir} is not writable: {}", files::describe(code)))?;
        let _ = files::remove(&probe);
    }
    Ok(())
}

/// Read the package at `path` into `buffer`, reusing the buffer's allocation
/// (the user heap never returns blocks this large, so a service that read every
/// package into a fresh `Vec` would grow by the package size each time).
/// `Err` is an errno; a file over [`MAX_PACKAGE_FILE`] is `EFBIG`, and a file
/// that changed size during the read is `EINVAL`. A same-size rewrite during
/// the read is not detected here; it fails validation instead (`lazypkg`
/// checks every entry's CRC-32 as it is read).
pub(crate) fn read_package(buffer: &mut Vec<u8>, path: &str) -> Result<(), i64> {
    files::read_large(path, MAX_PACKAGE_FILE, buffer)
}
