//! An internal journal in the JBD2 on-disk format (inode 8), the format
//! ext3/ext4 and `e2fsck` use. `docs/architecture/journal.md` has the design.
//!
//! The journal protects metadata: a transaction is every dirty block that is
//! not file data (inodes, bitmaps, descriptors, the superblock, directory and
//! indirect blocks), logged whole, so replaying it after a crash restores a
//! state the driver had committed. File data is written to its home blocks
//! *before* the transaction that links it (ordered mode), so a crash never
//! shows a file another file's bytes.
//!
//! Every commit is one transaction that is checkpointed (written to its home
//! blocks) before the commit returns, then the journal is marked empty. The
//! journal therefore never holds more than the transaction in flight, which
//! is why no revoke records are written (replay still honours them in
//! journals other drivers wrote) and why a log wrap never happens here.
//!
//! Supported: version-1 and version-2 superblocks, descriptor, commit and
//! revoke blocks, 32-bit block numbers, no checksums. A journal with any
//! other incompatible feature (64-bit, checksums, fast commit) is refused.

use alloc::vec::Vec;

use crate::layout::*;
use crate::{BlockIo, Ext2Error, IoError};

pub(crate) mod commit;
mod create;
mod overlay;
mod replay;

/// `JBD2_MAGIC_NUMBER`; every journal metadata block starts with it.
const MAGIC: u32 = 0xC03B_3998;
const BLOCK_DESCRIPTOR: u32 = 1;
const BLOCK_COMMIT: u32 = 2;
const BLOCK_SUPER_V1: u32 = 3;
const BLOCK_SUPER_V2: u32 = 4;
const BLOCK_REVOKE: u32 = 5;
/// Bytes of the common header: magic, block type, transaction id.
const HEADER: usize = 12;

// Journal superblock fields (big-endian).
const JS_BLOCKSIZE: usize = 0x0C;
const JS_MAXLEN: usize = 0x10;
const JS_FIRST: usize = 0x14;
const JS_SEQUENCE: usize = 0x18;
const JS_START: usize = 0x1C;
const JS_INCOMPAT: usize = 0x28;
const JS_UUID: usize = 0x30;
const JS_NR_USERS: usize = 0x40;
const JS_USERS: usize = 0x100;
/// The one incompatible feature a journal may carry: revoke records.
const INCOMPAT_REVOKE: u32 = 0x1;

// Descriptor tags.
const TAG_BYTES: usize = 8;
const TAG_ESCAPE: u16 = 1;
const TAG_SAME_UUID: u16 = 2;
const TAG_LAST: u16 = 8;
const UUID_BYTES: usize = 16;

/// Smallest journal this driver creates (blocks).
pub const MIN_JOURNAL_BLOCKS: u32 = 64;

fn be16(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

fn be32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn put_be16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

fn put_be32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// Write the common header of a journal metadata block.
fn put_header(buf: &mut [u8], block_type: u32, sequence: u32) {
    put_be32(buf, 0, MAGIC);
    put_be32(buf, 4, block_type);
    put_be32(buf, 8, sequence);
}

/// The block type and transaction id of `buf`, if it is journal metadata.
fn header(buf: &[u8]) -> Option<(u32, u32)> {
    (be32(buf, 0) == MAGIC).then(|| (be32(buf, 4), be32(buf, 8)))
}

/// The decoded journal superblock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct JournalSb {
    pub maxlen: u32,
    pub first: u32,
    pub sequence: u32,
    /// Log block of the oldest transaction; 0 when the journal is empty.
    pub start: u32,
}

impl JournalSb {
    /// Validate the journal's block 0 against the filesystem's block size.
    pub fn parse(buf: &[u8], block_size: u32, journal_blocks: u32) -> Result<Self, Ext2Error> {
        match header(buf) {
            Some((BLOCK_SUPER_V1 | BLOCK_SUPER_V2, _)) => {}
            _ => return Err(Ext2Error::Invalid),
        }
        let maxlen = be32(buf, JS_MAXLEN);
        let first = be32(buf, JS_FIRST);
        let start = be32(buf, JS_START);
        let sane = be32(buf, JS_BLOCKSIZE) == block_size
            && maxlen <= journal_blocks
            && maxlen >= 16
            && first >= 1
            && first < maxlen
            && (start == 0 || (start >= first && start < maxlen));
        if !sane {
            return Err(Ext2Error::Invalid);
        }
        let v2 = be32(buf, 4) == BLOCK_SUPER_V2;
        if v2 && be32(buf, JS_INCOMPAT) & !INCOMPAT_REVOKE != 0 {
            return Err(Ext2Error::NotSupported);
        }
        Ok(JournalSb {
            maxlen,
            first,
            sequence: be32(buf, JS_SEQUENCE),
            start,
        })
    }
}

/// The journal of one mounted volume, as the cache's commit path uses it.
pub(crate) struct Journal {
    /// Physical filesystem block of journal block `i`.
    pub(crate) blocks: Vec<u64>,
    pub(crate) first: u32,
    /// The id the next transaction takes.
    pub(crate) sequence: u32,
    /// Filesystem UUID, named in the first descriptor of every transaction.
    pub(crate) uuid: [u8; 16],
    /// The journal superblock as found (block 0 of the log), the template
    /// every update of `s_start` and `s_sequence` patches.
    pub(crate) sb: Vec<u8>,
    /// A journal write failed: no further transaction is attempted this
    /// mount, because the log's state is no longer known (jbd2's "abort").
    pub(crate) aborted: bool,
}

impl Journal {
    /// Most data blocks one transaction of `bs`-byte blocks can log in a
    /// log of `capacity` blocks: each descriptor carries a limited tag run,
    /// and the transaction needs its commit block.
    pub fn max_data_blocks(&self, bs: usize) -> usize {
        let capacity = self.blocks.len() - self.first as usize;
        let per_descriptor = tags_per_descriptor(bs);
        // d descriptors carry d * per_descriptor tags; a transaction of n
        // data blocks needs n + ceil(n / per_descriptor) + 1 log blocks.
        let mut n = capacity.saturating_sub(2) * per_descriptor / (per_descriptor + 1);
        while n > 0 && n + n.div_ceil(per_descriptor) + 1 > capacity {
            n -= 1;
        }
        n
    }
}

/// Tags one descriptor block holds: the first also carries the UUID.
pub(crate) fn tags_per_descriptor(bs: usize) -> usize {
    1 + (bs - HEADER - TAG_BYTES - UUID_BYTES) / TAG_BYTES
}

/// Write `bufs` (consecutive journal blocks from log index `start`) to the
/// device, one request per run of consecutive physical blocks.
pub(crate) fn write_log(
    io: &dyn BlockIo,
    blocks: &[u64],
    sectors_per_block: u64,
    max_run: usize,
    start: usize,
    bufs: &[&[u8]],
) -> Result<(), IoError> {
    let mut at = 0;
    while at < bufs.len() {
        io.pace();
        let mut end = at + 1;
        while end < bufs.len()
            && end - at < max_run
            && blocks[start + end] == blocks[start + end - 1] + 1
        {
            end += 1;
        }
        io.write_sectors_vectored(blocks[start + at] * sectors_per_block, &bufs[at..end])?;
        at = end;
    }
    Ok(())
}
