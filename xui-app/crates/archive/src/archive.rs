//! An open archive: its detected format, its entry list, and the one way to
//! read entries' data, [`Archive::visit`].

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::codec::{self, Codec};
use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};
use crate::format::{sniff, Format, Magic};
use crate::progress::{Counted, Progress};
use crate::sevenz::SevenZArchive;
use crate::tar::{self, Flow};
use crate::zip::ZipArchive;

/// Largest zip symlink target read at open.
const MAX_LINK: u64 = 4096;

/// Per-format state behind the entry list.
enum Backend {
    Zip(ZipArchive),
    SevenZ(SevenZArchive),
    /// Tarballs and single files are streams: nothing to keep.
    Stream,
}

/// An archive on disk, listed.
pub struct Archive {
    /// Where it is.
    pub path: PathBuf,
    /// What it is.
    pub format: Format,
    /// Every member, in archive order (`entries[i].index == i`).
    pub entries: Vec<Entry>,
    /// The archive file's size.
    pub size: u64,
    backend: Backend,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("path", &self.path)
            .field("format", &self.format)
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// Called by [`Archive::visit`] for each wanted entry with a reader over its
/// data (empty for anything but a file).
pub type Visitor<'a> = dyn FnMut(&Entry, &mut dyn Read) -> Result<()> + 'a;

impl Archive {
    /// Detect `path`'s format and list it. Tarballs and single files are
    /// read to the end to list them, so the progress counts archive bytes.
    pub fn open(path: &Path, progress: &Arc<Progress>) -> Result<Archive> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut head = Vec::new();
        (&mut file).take(512).read_to_end(&mut head)?;
        let format = match sniff(&head) {
            Some(Magic::Zip) => Format::Zip,
            Some(Magic::SevenZ) => Format::SevenZ,
            Some(Magic::Tar) => Format::Tar,
            Some(Magic::Gzip) => compressed(path, Codec::Gzip, Format::TarGz, Format::Gz)?,
            Some(Magic::Xz) => compressed(path, Codec::Xz, Format::TarXz, Format::Xz)?,
            Some(Magic::Zstd) => compressed(path, Codec::Zstd, Format::TarZst, Format::Zst)?,
            // A self-extractor or a zip with a stub: its end record finds it.
            None if ZipArchive::open(&mut File::open(path)?).is_ok() => Format::Zip,
            None => return Err(Error::unsupported("not an archive this app can read")),
        };
        let (entries, backend) = match format {
            Format::Zip => {
                let zip = ZipArchive::open(&mut File::open(path)?)?;
                let mut entries = zip.entries.clone();
                fill_zip_links(path, &zip, &mut entries);
                (entries, Backend::Zip(zip))
            }
            Format::SevenZ => {
                let sevenz = SevenZArchive::open(&mut File::open(path)?)?;
                (sevenz.entries.clone(), Backend::SevenZ(sevenz))
            }
            format if format.single_file() => (
                vec![crate::single::list(path, format, progress)?],
                Backend::Stream,
            ),
            format => (list_tar(path, format, progress)?, Backend::Stream),
        };
        Ok(Archive {
            path: path.to_path_buf(),
            format,
            entries,
            size,
            backend,
        })
    }

    /// The bytes the files among `entries` hold.
    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(|entry| entry.size).sum()
    }

    /// Stream the entries `wanted` selects, in archive order, to `visit`.
    /// The progress total is set to the bytes the visit will read.
    pub fn visit(
        &self,
        wanted: &dyn Fn(&Entry) -> bool,
        progress: &Arc<Progress>,
        visit: &mut Visitor<'_>,
    ) -> Result<()> {
        let mut remaining = self.entries.iter().filter(|e| wanted(e)).count();
        if remaining == 0 {
            return Ok(());
        }
        match &self.backend {
            Backend::Zip(zip) => {
                progress.set_total(self.wanted_size(wanted));
                for entry in self.entries.iter().filter(|e| wanted(e)) {
                    progress.check()?;
                    progress.begin(&entry.path);
                    if matches!(entry.kind, EntryKind::File) {
                        // A member this library cannot read (an unknown
                        // method, a damaged header) fails only its own read.
                        match zip.reader(File::open(&self.path)?, entry.index) {
                            Ok(data) => visit(entry, &mut Counted::new(data, progress))?,
                            Err(error @ (Error::Unsupported(_) | Error::Corrupt(_))) => {
                                visit(entry, &mut Failing(Some(error.into())))?
                            }
                            Err(error) => return Err(error),
                        }
                    } else {
                        visit(entry, &mut io::empty())?;
                    }
                }
                Ok(())
            }
            Backend::SevenZ(sevenz) => {
                progress.set_total(self.wanted_size(wanted));
                sevenz.visit(&self.path, &self.entries, wanted, progress, visit)
            }
            Backend::Stream => {
                progress.set_total(self.size);
                let file: Box<dyn Read + Send> =
                    Box::new(Counted::new(File::open(&self.path)?, progress));
                let mut stream = codec::decoder(self.format.codec(), file)?;
                if self.format.single_file() {
                    progress.begin(&self.entries[0].path);
                    return visit(&self.entries[0], &mut stream);
                }
                tar::walk(&mut stream, &mut |walked, data| {
                    let Some(entry) = self.entries.get(walked.index) else {
                        return Err(Error::corrupt("tar: the archive changed while it was open"));
                    };
                    if !wanted(entry) {
                        return Ok(Flow::Continue);
                    }
                    progress.begin(&entry.path);
                    visit(entry, data)?;
                    remaining -= 1;
                    Ok(if remaining == 0 {
                        Flow::Stop
                    } else {
                        Flow::Continue
                    })
                })
            }
        }
    }

    fn wanted_size(&self, wanted: &dyn Fn(&Entry) -> bool) -> u64 {
        self.entries
            .iter()
            .filter(|e| wanted(e))
            .map(|e| e.size)
            .sum()
    }

    /// The zip state, for a raw-copy rewrite.
    pub(crate) fn zip(&self) -> Option<&ZipArchive> {
        match &self.backend {
            Backend::Zip(zip) => Some(zip),
            _ => None,
        }
    }
}

/// A reader that fails with one error.
pub(crate) struct Failing(pub(crate) Option<io::Error>);

impl Read for Failing {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(self
            .0
            .take()
            .unwrap_or_else(|| io::Error::other("unreadable member")))
    }
}

/// A compressed stream is a tarball when its first decompressed block is a
/// tar header, else a single compressed file.
fn compressed(path: &Path, codec: Codec, tarball: Format, single: Format) -> Result<Format> {
    let file: Box<dyn Read + Send> = Box::new(File::open(path)?);
    let mut stream = codec::decoder(codec, file)?;
    let mut block = Vec::with_capacity(tar::BLOCK);
    // A stream too damaged to yield a block is still shown, as a single file
    // whose test fails, rather than refused at open.
    let _ = (&mut stream)
        .take(tar::BLOCK as u64)
        .read_to_end(&mut block);
    // An empty tarball is all zeros: still a tarball, not a file of zeros.
    let empty_tar = block.len() == tar::BLOCK && block.iter().all(|&b| b == 0);
    Ok(
        if block.len() == tar::BLOCK && (tar::is_header(&block) || empty_tar) {
            tarball
        } else {
            single
        },
    )
}

/// List a tarball by walking it.
fn list_tar(path: &Path, format: Format, progress: &Arc<Progress>) -> Result<Vec<Entry>> {
    let file = File::open(path)?;
    progress.set_total(file.metadata()?.len());
    let input: Box<dyn Read + Send> = Box::new(Counted::new(BufReader::new(file), progress));
    let mut stream = codec::decoder(format.codec(), input)?;
    let method = format.codec().method();
    let mut entries = Vec::new();
    tar::walk(&mut stream, &mut |mut entry, _data| {
        entry.method = method.to_owned();
        entries.push(entry);
        Ok(Flow::Continue)
    })?;
    Ok(entries)
}

/// Read each zip symlink's target (its data) into its entry.
fn fill_zip_links(path: &Path, zip: &ZipArchive, entries: &mut [Entry]) {
    for entry in entries.iter_mut() {
        if !matches!(entry.kind, EntryKind::Symlink { .. }) {
            continue;
        }
        let target = File::open(path)
            .map_err(Error::from)
            .and_then(|file| zip.reader(file, entry.index))
            .and_then(|reader| {
                let mut target = Vec::new();
                reader.take(MAX_LINK).read_to_end(&mut target)?;
                Ok(target)
            });
        entry.kind = match target {
            Ok(target) => EntryKind::Symlink {
                target: String::from_utf8_lossy(&target).into_owned(),
            },
            // An unreadable target is listed as a plain file instead.
            Err(_) => EntryKind::File,
        };
    }
}
