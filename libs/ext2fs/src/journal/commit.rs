//! Writing one transaction: descriptor blocks, the logged blocks, a commit
//! block, with a device flush between the data and the commit record.
//!
//! The journal is empty (`s_start == 0`) whenever a transaction begins, so
//! the superblock that arms it (`s_start` = the first log block, `s_sequence`
//! = this transaction) may be written together with the log: until the commit
//! block lands, replay finds either old blocks with another id or a
//! descriptor run with no commit, and applies nothing.

use alloc::borrow::Cow;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// `h_commit_sec` of a version-2 commit block.
const COMMIT_SEC: usize = 48;

/// What a commit needs to know about the device and the cache's limits.
pub(crate) struct Geometry {
    pub block_size: usize,
    pub sectors_per_block: u64,
    /// Largest vectored request, in blocks.
    pub max_run: usize,
}

impl Journal {
    /// Log `entries` (home block number and contents) as one transaction and
    /// make it durable. The caller then writes the blocks home and calls
    /// [`Journal::finish`]. Any failure aborts the journal.
    pub fn commit(
        &mut self,
        io: &dyn BlockIo,
        geo: &Geometry,
        now: u32,
        entries: &[(u64, &[u8])],
    ) -> Result<(), IoError> {
        if self.aborted {
            return Err(IoError::Failed);
        }
        let result = self.write_transaction(io, geo, now, entries);
        if result.is_err() {
            self.aborted = true;
        }
        result
    }

    fn write_transaction(
        &mut self,
        io: &dyn BlockIo,
        geo: &Geometry,
        now: u32,
        entries: &[(u64, &[u8])],
    ) -> Result<(), IoError> {
        let bs = geo.block_size;
        let sequence = self.sequence;
        let log = self.log_blocks(bs, sequence, entries);
        let first = self.first as usize;
        if first + log.len() + 1 > self.blocks.len() {
            return Err(IoError::Failed); // the caller splits to fit; this is a bug guard
        }
        self.sequence = sequence.wrapping_add(1);

        let armed = self.sb_image(sequence, self.first);
        let mut bufs: Vec<&[u8]> = vec![&armed];
        write_log(
            io,
            &self.blocks,
            geo.sectors_per_block,
            geo.max_run,
            0,
            &bufs,
        )?;
        bufs = log.iter().map(|block| &**block).collect();
        write_log(
            io,
            &self.blocks,
            geo.sectors_per_block,
            geo.max_run,
            first,
            &bufs,
        )?;
        io.flush()?; // the logged blocks (and the data written before them)

        let mut commit = vec![0u8; bs];
        put_header(&mut commit, BLOCK_COMMIT, sequence);
        commit[COMMIT_SEC..COMMIT_SEC + 8].copy_from_slice(&u64::from(now).to_be_bytes());
        let at = first + log.len();
        write_log(
            io,
            &self.blocks,
            geo.sectors_per_block,
            geo.max_run,
            at,
            &[&commit],
        )?;
        io.flush() // the transaction is committed
    }

    /// Mark the journal empty once the transaction is checkpointed.
    pub fn finish(&mut self, io: &dyn BlockIo, geo: &Geometry) -> Result<(), IoError> {
        let empty = self.sb_image(self.sequence, 0);
        let result = write_log(
            io,
            &self.blocks,
            geo.sectors_per_block,
            geo.max_run,
            0,
            &[&empty],
        )
        .and_then(|()| io.flush());
        if result.is_err() {
            self.aborted = true;
        }
        result
    }

    /// The journal superblock with `s_sequence` and `s_start` set.
    fn sb_image(&self, sequence: u32, start: u32) -> Vec<u8> {
        let mut image = self.sb.clone();
        put_be32(&mut image, JS_SEQUENCE, sequence);
        put_be32(&mut image, JS_START, start);
        image
    }

    /// The log blocks of a transaction: each descriptor followed by the
    /// blocks its tags name. A block that begins with the journal magic is
    /// logged with its first word zeroed and the escape flag set.
    fn log_blocks<'a>(
        &self,
        bs: usize,
        sequence: u32,
        entries: &[(u64, &'a [u8])],
    ) -> Vec<Cow<'a, [u8]>> {
        let mut log = Vec::new();
        for run in entries.chunks(tags_per_descriptor(bs)) {
            let mut descriptor = vec![0u8; bs];
            put_header(&mut descriptor, BLOCK_DESCRIPTOR, sequence);
            let mut at = HEADER;
            for (index, &(home, data)) in run.iter().enumerate() {
                let mut flags = 0;
                if be32(data, 0) == MAGIC {
                    flags |= TAG_ESCAPE;
                }
                if index > 0 {
                    flags |= TAG_SAME_UUID;
                }
                if index + 1 == run.len() {
                    flags |= TAG_LAST;
                }
                put_be32(&mut descriptor, at, home as u32);
                put_be16(&mut descriptor, at + 6, flags);
                at += TAG_BYTES;
                if index == 0 {
                    descriptor[at..at + UUID_BYTES].copy_from_slice(&self.uuid);
                    at += UUID_BYTES;
                }
            }
            log.push(Cow::Owned(descriptor));
            for &(_, data) in run {
                log.push(if be32(data, 0) == MAGIC {
                    let mut copy = data[..bs].to_vec();
                    copy[..4].fill(0);
                    Cow::Owned(copy)
                } else {
                    Cow::Borrowed(&data[..bs])
                });
            }
        }
        log
    }
}
