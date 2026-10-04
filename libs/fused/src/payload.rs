//! What travels in the data buffer beside a record: paths, directory
//! entries, `statfs` figures and attribute changes. Every decoder checks
//! every length and answers `None` rather than guess.

use alloc::string::String;
use alloc::vec::Vec;

use crate::wire::{MAX_NAME, MAX_PATH};

/// Whether `name` may be one directory entry: 1..=255 bytes, no `/` or NUL,
/// and not `.` or `..`.
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name != "."
        && name != ".."
        && !name.bytes().any(|b| b == b'/' || b == 0)
}

/// `bytes` as a path relative to a mount root: UTF-8, at most
/// [`MAX_PATH`] bytes, `""` (the root) or names joined by single `/`s.
pub fn parse_path(bytes: &[u8]) -> Option<&str> {
    if bytes.len() > MAX_PATH {
        return None;
    }
    let path = core::str::from_utf8(bytes).ok()?;
    if path.is_empty() || path.split('/').all(valid_name) {
        Some(path)
    } else {
        None
    }
}

/// Bytes before an entry's name: ino (8, LE), kind (1), name length (1).
pub const DIRENT_HEADER: usize = 10;

/// One directory entry, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEnt {
    pub ino: u64,
    pub dir: bool,
    pub name: String,
}

/// Append one entry to `out` at `at`; the position after it, or `None` when
/// it does not fit (or the name is not a valid entry name).
pub fn encode_dirent(out: &mut [u8], at: usize, ino: u64, dir: bool, name: &str) -> Option<usize> {
    if !valid_name(name) {
        return None;
    }
    let end = at.checked_add(DIRENT_HEADER + name.len())?;
    let slot = out.get_mut(at..end)?;
    slot[..8].copy_from_slice(&ino.to_le_bytes());
    slot[8] = u8::from(dir);
    slot[9] = name.len() as u8;
    slot[DIRENT_HEADER..].copy_from_slice(name.as_bytes());
    Some(end)
}

/// Exactly `count` entries filling all of `bytes`; `None` when the encoding
/// is broken, a name is invalid, or bytes are left over.
pub fn decode_dirents(bytes: &[u8], count: usize) -> Option<Vec<DirEnt>> {
    // Every entry takes at least DIRENT_HEADER + 1 bytes, which bounds the
    // allocation by the payload rather than by a claimed count.
    if count > bytes.len() / (DIRENT_HEADER + 1) {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    let mut at = 0;
    for _ in 0..count {
        let header = bytes.get(at..at + DIRENT_HEADER)?;
        let ino = u64::from_le_bytes(header[..8].try_into().ok()?);
        let dir = match header[8] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let len = usize::from(header[9]);
        let start = at + DIRENT_HEADER;
        let name = core::str::from_utf8(bytes.get(start..start + len)?).ok()?;
        if !valid_name(name) {
            return None;
        }
        entries.push(DirEnt {
            ino,
            dir,
            name: String::from(name),
        });
        at = start + len;
    }
    (at == bytes.len()).then_some(entries)
}

fn put_words(words: &[u64], out: &mut [u8]) -> Option<usize> {
    let len = words.len() * 8;
    let out = out.get_mut(..len)?;
    for (chunk, word) in out.chunks_exact_mut(8).zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    Some(len)
}

fn get_words<const N: usize>(bytes: &[u8]) -> Option<[u64; N]> {
    if bytes.len() != N * 8 {
        return None;
    }
    let mut words = [0u64; N];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().ok()?);
    }
    Some(words)
}

/// Capacity figures, a `STATFS` reply's payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatFsRecord {
    pub magic: u64,
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub files: u64,
    pub files_free: u64,
    pub name_max: u64,
}

/// Bytes of an encoded [`StatFsRecord`].
pub const STATFS_LEN: usize = 56;

impl StatFsRecord {
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        put_words(
            &[
                self.magic,
                self.block_size,
                self.blocks,
                self.blocks_free,
                self.files,
                self.files_free,
                self.name_max,
            ],
            out,
        )
    }

    pub fn decode(bytes: &[u8]) -> Option<StatFsRecord> {
        let [magic, block_size, blocks, blocks_free, files, files_free, name_max] =
            get_words::<7>(bytes)?;
        Some(StatFsRecord {
            magic,
            block_size,
            blocks,
            blocks_free,
            files,
            files_free,
            name_max,
        })
    }
}

/// An attribute change, a `SETATTR` request's payload after the path. A
/// field is applied only when its bit is in `mask`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetAttrRecord {
    pub mask: u64,
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// [`SetAttrRecord::mask`] bits.
pub mod set {
    pub const MODE: u64 = 1;
    pub const UID: u64 = 2;
    pub const GID: u64 = 4;
    pub const ATIME: u64 = 8;
    pub const MTIME: u64 = 16;
    pub const CTIME: u64 = 32;
    pub const ALL: u64 = 63;
}

/// Bytes of an encoded [`SetAttrRecord`].
pub const SETATTR_LEN: usize = 56;

impl SetAttrRecord {
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        put_words(
            &[
                self.mask,
                self.mode,
                self.uid,
                self.gid,
                self.atime as u64,
                self.mtime as u64,
                self.ctime as u64,
            ],
            out,
        )
    }

    /// `None` for a wrong length or unknown mask bits.
    pub fn decode(bytes: &[u8]) -> Option<SetAttrRecord> {
        let [mask, mode, uid, gid, atime, mtime, ctime] = get_words::<7>(bytes)?;
        if mask & !set::ALL != 0 {
            return None;
        }
        Some(SetAttrRecord {
            mask,
            mode,
            uid,
            gid,
            atime: atime as i64,
            mtime: mtime as i64,
            ctime: ctime as i64,
        })
    }
}
