//! `pkgd`'s writes to the data volume: reading a package, extracting it under
//! `/data/apps`, and deleting an install directory again.
//!
//! Everything here is a root write, so every path is composed through
//! `pkgstore::layout` (which refuses anything that could leave the install
//! directory) and every removal is confined to `/data/apps`. The ext2 volume has
//! no symlinks, so a lexically safe path is a physically safe one.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Package;
use pkgstore::{access, layout};
use user::files::{self, Kind};

/// Largest package file `pkgd` reads (256 MiB). The package is streamed into
/// `pkgd`'s own heap one range at a time (`files::read_large`), so the kernel
/// never holds it whole and this only bounds `pkgd`'s buffer; what a package
/// may *expand* to is `lazypkg::MAX_TOTAL_UNCOMPRESSED`.
pub(crate) const MAX_PACKAGE_FILE: usize = 256 * 1024 * 1024;
/// Deepest directory tree `remove_tree` walks (a package holds at most 255
/// bytes of name, so this is generous).
const MAX_DEPTH: usize = 24;
/// Listings per directory before `remove_tree` gives up (a listing holds a
/// bounded number of entries, so a huge directory needs several rounds).
const MAX_ROUNDS: usize = 64;

/// The errno `files` reports for an absent path.
pub(crate) const ENOENT: i64 = 2;
const EEXIST: i64 = 17;
const EINVAL: i64 = 22;
const ELOOP: i64 = 40;

/// Whether the data volume is mounted (`/data` is a directory).
pub(crate) fn data_mounted() -> bool {
    matches!(files::stat(layout::DATA_ROOT), Ok((_, Kind::Dir)))
}

/// Create `path` if it is not already a directory.
pub(crate) fn ensure_dir(path: &str) -> Result<(), i64> {
    match files::mkdir(path) {
        Ok(()) => Ok(()),
        Err(EEXIST) => match files::stat(path)? {
            (_, Kind::Dir) => Ok(()),
            _ => Err(EEXIST),
        },
        Err(code) => Err(code),
    }
}

/// Whether `path` exists.
pub(crate) fn exists(path: &str) -> bool {
    files::stat(path).is_ok()
}

/// Make `/data/apps` (and `/data/log`) exist; `Err` when the volume cannot be
/// written, which `pkgd` reports as "no usable data volume".
pub(crate) fn prepare_volume() -> Result<(), i64> {
    ensure_dir(layout::APPS_ROOT)?;
    ensure_dir(layout::LOG_DIR)
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

/// What went wrong while extracting: the step, for the failure text.
pub(crate) struct ExtractError {
    pub(crate) step: String,
}

fn failed(step: String, code: i64) -> ExtractError {
    ExtractError {
        step: format!("{step}: {}", files::describe(code)),
    }
}

/// Extract every entry of `package` under `install_path`: directories first
/// (parents before children), then each file, verified by size after the write.
/// Returns the number of files written. The caller removes `install_path` when
/// this fails part-way.
pub(crate) fn extract(package: &Package<'_>, install_path: &str) -> Result<usize, ExtractError> {
    let app_dir = install_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or(install_path);
    ensure_dir(app_dir).map_err(|code| failed(format!("creating {app_dir}"), code))?;
    ensure_dir(install_path).map_err(|code| failed(format!("creating {install_path}"), code))?;
    let dirs = layout::directories(package.entries().map(|entry| (entry.name, entry.is_dir)))
        .map_err(|error| ExtractError {
            step: format!("planning the extraction: {error}"),
        })?;
    for dir in &dirs {
        let path = layout::entry_path(install_path, dir).map_err(|error| ExtractError {
            step: format!("creating {dir}: {error}"),
        })?;
        ensure_dir(&path).map_err(|code| failed(format!("creating {dir}"), code))?;
    }
    let mut written = 0;
    for entry in package.entries().filter(|entry| !entry.is_dir) {
        let path = layout::entry_path(install_path, entry.name).map_err(|error| ExtractError {
            step: format!("writing {}: {error}", entry.name),
        })?;
        let data = package.read(entry.name).map_err(|error| ExtractError {
            step: format!("unpacking {}: {error}", entry.name),
        })?;
        files::write_large(&path, &data)
            .map_err(|code| failed(format!("writing {}", entry.name), code))?;
        match files::stat(&path) {
            Ok((size, Kind::File)) if size == data.len() as u64 => {}
            Ok(_) => {
                return Err(ExtractError {
                    step: format!(
                        "writing {}: the file on disk has the wrong size",
                        entry.name
                    ),
                })
            }
            Err(code) => return Err(failed(format!("checking {}", entry.name), code)),
        }
        written += 1;
    }
    Ok(written)
}

/// Whether `path` is an install directory or app directory `pkgd` may delete:
/// strictly below `/data/apps`, well formed.
fn deletable(path: &str) -> bool {
    access::well_formed(path)
        && path
            .strip_prefix(layout::APPS_ROOT)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// Delete `path` and everything under it: files first, then directories,
/// deepest first, never following anything (there are no symlinks). A path
/// that is already gone is success. Refuses anything outside `/data/apps`.
pub(crate) fn remove_tree(path: &str) -> Result<(), i64> {
    if !deletable(path) {
        return Err(EINVAL);
    }
    remove_at(path, 0)
}

fn remove_at(path: &str, depth: usize) -> Result<(), i64> {
    if depth > MAX_DEPTH {
        return Err(ELOOP);
    }
    match files::stat(path) {
        Err(ENOENT) => return Ok(()),
        Err(code) => return Err(code),
        Ok((_, Kind::File)) => return files::remove(path),
        Ok((_, Kind::Dir)) => {}
    }
    for _ in 0..MAX_ROUNDS {
        let entries = files::list(path)?;
        let mut progressed = false;
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            if entry.name.is_empty() || entry.name.contains('/') {
                return Err(EINVAL);
            }
            remove_at(&format!("{path}/{}", entry.name), depth + 1)?;
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    files::remove(path)
}

/// Remove `path` when it is an empty directory; anything else is left alone.
pub(crate) fn remove_if_empty(path: &str) {
    if !deletable(path) {
        return;
    }
    let empty = files::list(path)
        .map(|entries| entries.iter().all(|e| e.name == "." || e.name == ".."))
        .unwrap_or(false);
    if empty {
        let _ = files::remove(path);
    }
}
