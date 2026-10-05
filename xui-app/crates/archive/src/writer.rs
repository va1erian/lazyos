//! The interface every archive writer implements, so creating and rewriting
//! are written once over all writable formats.

use std::fs::File;
use std::io::{self, BufWriter, Read};
use std::path::Path;

use crate::codec::{self, Finish};
use crate::error::{Error, Result};
use crate::format::{Format, Level};

/// What a writer records about a member besides its name and data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// Modification time, Unix seconds.
    pub modified: Option<i64>,
    /// Unix permission bits.
    pub mode: Option<u32>,
}

/// Writes members into a new archive. Paths are normalised archive paths
/// (`a/b/c`, no trailing slash).
pub trait EntryWriter {
    /// A directory.
    fn dir(&mut self, path: &str, meta: Meta) -> Result<()>;
    /// A regular file of `size` bytes read from `data`; fails if `data` holds
    /// a different amount.
    fn file(&mut self, path: &str, meta: Meta, size: u64, data: &mut dyn Read) -> Result<()>;
    /// A symbolic link to `target`.
    fn symlink(&mut self, path: &str, meta: Meta, target: &str) -> Result<()>;
    /// A hard link to the earlier member `target` (tar only).
    fn hardlink(&mut self, path: &str, _meta: Meta, _target: &str) -> Result<()> {
        Err(Error::unsupported(format!(
            "a hard link ({path}) in this format"
        )))
    }
    /// Complete the archive (trailers, central directory, compressor tail).
    fn finish(self: Box<Self>) -> Result<()>;
}

/// A writer for a new multi-member archive of `format` at `path`.
pub fn open(format: Format, level: Level, path: &Path) -> Result<Box<dyn EntryWriter>> {
    if !format.writable() {
        return Err(Error::unsupported(format!("writing {}", format.name())));
    }
    if format.single_file() {
        return Err(Error::unsupported(format!(
            "{} holds a single file; use create_single",
            format.name()
        )));
    }
    let file = File::create(path)?;
    match format {
        Format::Zip => Ok(Box::new(crate::zip::write::ZipWriter::new(
            BufWriter::new(file),
            level,
        ))),
        _ => {
            let out = codec::encoder(format.codec(), level, Box::new(BufWriter::new(file)))?;
            Ok(Box::new(crate::tar::write::TarWriter::new(FinishOut(out))))
        }
    }
}

/// A [`Finish`] encoder as a plain `Write` the tar writer can own, finished
/// by [`crate::tar::write::TarWriter`]'s own `finish`.
pub struct FinishOut(pub Box<dyn Finish>);

impl io::Write for FinishOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl FinishOut {
    /// Complete the stream.
    pub fn finish(self) -> io::Result<()> {
        self.0.finish()
    }
}

/// Copy exactly `size` bytes of `data` into `out`, failing when `data` holds
/// more or fewer (a file that changed size while it was being archived).
pub fn copy_exact(data: &mut dyn Read, out: &mut dyn io::Write, size: u64) -> Result<()> {
    let copied = io::copy(&mut (&mut *data).take(size), out)?;
    if copied != size {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "a file shrank while it was being archived",
        )));
    }
    let mut probe = [0u8; 1];
    if data.read(&mut probe)? != 0 {
        return Err(Error::Io(io::Error::other(
            "a file grew while it was being archived",
        )));
    }
    Ok(())
}
