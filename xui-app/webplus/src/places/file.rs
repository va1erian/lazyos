//! The bookmarks file's byte-level pieces: reading its head, appending one commit, cutting it back.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::log;
use super::model::{clean_url, Error};

/// How much of the file is read. A log this large holds far more than `MAX_ENTRIES` entries'
/// worth of live records; the rest is ignored like a corrupt tail.
const MAX_READ: u64 = 64 * 1024 * 1024;

/// A write failure to inject, so the torn-tail handling can be tested.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Fault {
    /// Write this many bytes of the next append, then fail.
    pub(super) tear_after: Option<usize>,
    /// Cutting the file back fails too.
    pub(super) truncate_fails: bool,
}

/// [`clean_url`], and only a web address: what the set keeps is what a load of the file keeps.
pub(super) fn clean_web_url(url: &str) -> Result<String, Error> {
    let url = clean_url(url)?;
    if crate::bookmarks::bookmarkable(&url) {
        Ok(url)
    } else {
        Err(Error::Unsupported)
    }
}

/// The first [`MAX_READ`] bytes of `file` (nothing when it cannot be read).
pub(super) fn read_head(file: &Path) -> Vec<u8> {
    let Ok(f) = fs::File::open(file) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let _ = f.take(MAX_READ).read_to_end(&mut bytes);
    bytes
}

/// Appends `bytes` to `file` (writing the header first when it is new or empty) and syncs. On a
/// failure after the file was opened it is cut back to its old length; the flag in the error says
/// that this failed too, leaving torn bytes at the end.
pub(super) fn append(file: &Path, bytes: &[u8], fault: Fault) -> Result<(), (io::Error, bool)> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| (e, false))?;
    }
    // Not `append(true)`: cutting the file back needs write access (Windows lacks it with append).
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(file)
        .map_err(|e| (e, false))?;
    let start = f.seek(SeekFrom::End(0)).map_err(|e| (e, false))?;
    let written = (|| {
        if start == 0 {
            f.write_all(log::MAGIC)?;
        }
        if let Some(cut) = fault.tear_after {
            f.write_all(&bytes[..cut.min(bytes.len())])?;
            return Err(io::Error::other("injected write failure"));
        }
        f.write_all(bytes)?;
        f.sync_data()
    })();
    let Err(error) = written else {
        return Ok(());
    };
    let cut = if fault.truncate_fails {
        Err(io::Error::other("injected truncate failure"))
    } else {
        f.set_len(start)
    };
    Err((error, cut.is_err()))
}

/// Cuts `file` back to its first `len` bytes.
pub(super) fn truncate(file: &Path, len: usize) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(file)?
        .set_len(len as u64)
}
