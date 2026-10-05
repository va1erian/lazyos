//! The single-file formats (`.gz`, `.xz`, `.zst`): one unnamed stream shown
//! as one entry, named by the gzip header's file name when it has one, else
//! by the archive's name without its extension.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use flate2::read::MultiGzDecoder;
use flate2::{Compression, GzBuilder};

use crate::codec::{self, Codec};
use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};
use crate::format::{Format, Level};
use crate::progress::{Counted, Progress};
use crate::writer::{copy_exact, Meta};

/// The one entry of the single-file archive at `path`, sized by
/// decompressing it (progress counts compressed bytes read).
pub fn list(path: &Path, format: Format, progress: &Arc<Progress>) -> Result<Entry> {
    let file = File::open(path)?;
    let packed = file.metadata()?.len();
    progress.set_total(packed);
    let archive_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stored_name = None;
    let size = match format.codec() {
        Codec::Gzip => {
            let mut decoder = MultiGzDecoder::new(BufReader::new(Counted::new(file, progress)));
            let size = io::copy(&mut decoder, &mut io::sink())?;
            stored_name = decoder
                .header()
                .and_then(|header| header.filename())
                .map(|name| String::from_utf8_lossy(name).into_owned());
            size
        }
        codec => {
            let counted: Box<dyn Read + Send> = Box::new(Counted::new(file, progress));
            let mut decoder = codec::decoder(codec, counted)?;
            io::copy(&mut decoder, &mut io::sink())?
        }
    };
    // A stored name is only a hint: keep its last component.
    let name = stored_name
        .and_then(|name| name.rsplit(['/', '\\']).next().map(str::to_owned))
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .unwrap_or_else(|| format.strip_extension(&archive_name));
    let mut entry = Entry::new(0, &name, EntryKind::File);
    entry.size = size;
    entry.packed = Some(packed);
    entry.method = format.codec().method().to_owned();
    entry.modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    Ok(entry)
}

/// Write `data` (exactly `size` bytes, named `name`) as a single-file
/// archive of `format` at `dest`.
pub fn write(
    dest: &Path,
    format: Format,
    level: Level,
    name: &str,
    meta: Meta,
    size: u64,
    data: &mut dyn Read,
) -> Result<()> {
    let out = BufWriter::new(File::create(dest)?);
    match format.codec() {
        Codec::Gzip => {
            let mtime = meta
                .modified
                .and_then(|t| u32::try_from(t).ok())
                .unwrap_or(0);
            let mut encoder = GzBuilder::new()
                .filename(name.as_bytes().to_vec())
                .mtime(mtime)
                .write(out, Compression::new(level.deflate()));
            copy_exact(data, &mut encoder, size)?;
            encoder.finish()?.flush()?;
        }
        Codec::Zstd => {
            let mut encoder = codec::encoder(Codec::Zstd, level, Box::new(out))?;
            copy_exact(data, &mut encoder, size)?;
            encoder.finish()?;
        }
        _ => return Err(Error::unsupported(format!("writing {}", format.name()))),
    }
    Ok(())
}
