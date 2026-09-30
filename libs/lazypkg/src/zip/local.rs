//! Local file headers.
//!
//! Each central directory entry points at a local header that repeats the name,
//! method, sizes and CRC. All of them must agree with the central directory:
//! a mismatch means the archive is inconsistent or crafted, and the data would
//! be parsed differently than validated. The resolved data range must also stay
//! inside the archive and before the central directory, so an entry can never
//! overlap the directory that describes it.

use crate::error::OpenError;
use crate::zip::{extra_has_zip64, ZipEntry, LOCAL_SIG};

/// Bytes of a local file header before its variable-length fields.
const LOCAL_HEADER: usize = 30;

/// Resolve the offset of the first compressed byte for `entry`, checking its
/// local header against the central directory and `directory_offset`.
pub(crate) fn data_start(
    bytes: &[u8],
    entry: &ZipEntry<'_>,
    directory_offset: usize,
) -> Result<usize, OpenError> {
    let name = entry.info.name;
    let at = entry.local_offset;
    if at >= directory_offset {
        return Err(bad_local(name));
    }
    let header_end = at
        .checked_add(LOCAL_HEADER)
        .ok_or_else(|| bad_local(name))?;
    let header = bytes.get(at..header_end).ok_or_else(|| bad_local(name))?;
    if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != LOCAL_SIG {
        return Err(bad_local(name));
    }
    let flags = u16::from_le_bytes([header[6], header[7]]);
    let method = u16::from_le_bytes([header[8], header[9]]);
    let crc32 = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
    let compressed = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
    let size = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as usize;

    if flags & 0x0001 != 0 || flags & 0x0008 != 0 {
        return Err(OpenError::UnsupportedFlags { name: name.into() });
    }
    if method != entry.method
        || crc32 != entry.info.crc32
        || compressed != entry.info.compressed_size
        || size != entry.info.size
    {
        return Err(bad_local(name));
    }
    if entry.method == 0 && compressed != size {
        return Err(bad_local(name));
    }

    let name_at = header_end;
    let name_end = name_at
        .checked_add(name_len)
        .ok_or_else(|| bad_local(name))?;
    let extra_end = name_end
        .checked_add(extra_len)
        .ok_or_else(|| bad_local(name))?;
    let data_start = extra_end;
    let data_end = data_start
        .checked_add(entry.info.compressed_size as usize)
        .ok_or_else(|| bad_local(name))?;

    if bytes.get(name_at..name_end) != Some(name.as_bytes()) {
        return Err(bad_local(name));
    }
    if extra_end > bytes.len() || data_end > bytes.len() {
        return Err(bad_local(name));
    }
    if extra_has_zip64(&bytes[name_end..extra_end]) {
        return Err(OpenError::Zip64Unsupported);
    }
    // The data must sit before the central directory, never inside it.
    if data_end > directory_offset {
        return Err(bad_local(name));
    }
    Ok(data_start)
}

fn bad_local(name: &str) -> OpenError {
    OpenError::BadLocalHeader { name: name.into() }
}
