//! Writeback on a journaled volume: file data goes straight home, the rest
//! of the dirty blocks go through the journal as one transaction.
//!
//! The order is the whole argument (`docs/architecture/journal.md`):
//!
//! 1. every dirty *data* block is written home, so nothing a transaction
//!    links can show a block's previous bytes;
//! 2. the dirty metadata is logged (descriptor blocks, the blocks, a commit
//!    block, a device flush between the last two), which makes it durable
//!    all at once or not at all;
//! 3. only then are the metadata blocks written home (the checkpoint) and the
//!    log marked empty.
//!
//! Inside a transaction the phases of `roles.rs` do not matter, because a
//! crash replays all of it or none of it.

use alloc::vec::Vec;

use super::roles::Phase;
use super::{page, BlockCache};
use crate::journal::commit::Geometry;
use crate::journal::Journal;
use crate::{BlockIo, IoError};

type Entry = (Phase, u64, usize);

impl BlockCache {
    /// Route this cache's commits through `journal`.
    pub fn set_journal(&mut self, journal: Journal) {
        self.journal = Some(journal);
    }

    pub fn has_journal(&self) -> bool {
        self.journal.is_some()
    }

    /// Make room by writing back what can go without a commit: everything on
    /// a plain volume, only file data on a journaled one (metadata waits for
    /// the volume's next commit, so an operation's changes land together).
    pub(super) fn relieve(&mut self, io: &dyn BlockIo) -> Result<(), IoError> {
        if self.journal.is_some() {
            self.flush_data(io)
        } else {
            self.flush(io)
        }
    }

    /// The dirty blocks that are (`data`) or are not file data, by block.
    fn dirty_where(&self, data: bool) -> Vec<Entry> {
        let mut order: Vec<Entry> = self
            .map
            .iter()
            .filter(|&(_, &index)| self.slots[index].dirty && self.slots[index].data == data)
            .map(|(&block, &index)| {
                (
                    self.roles.phase(block, self.slots[index].fresh),
                    block,
                    index,
                )
            })
            .collect();
        order.sort_unstable_by_key(|&(_, block, _)| block);
        order
    }

    /// Write every dirty file-data block home.
    pub(super) fn flush_data(&mut self, io: &dyn BlockIo) -> Result<(), IoError> {
        let order = self.dirty_where(true);
        self.write_all(io, &order)
    }

    /// Write `order` home, one request per run of consecutive blocks.
    fn write_all(&mut self, io: &dyn BlockIo, order: &[Entry]) -> Result<(), IoError> {
        let mut start = 0;
        while start < order.len() {
            let end = self.run_end_by_block(order, start);
            self.write_run(io, &order[start..end])?;
            start = end;
        }
        Ok(())
    }

    /// [`BlockCache::run_end`] without the phase condition: the lists here
    /// mix phases whose order no longer matters.
    fn run_end_by_block(&self, order: &[Entry], start: usize) -> usize {
        let first = order[start].1;
        let mut end = start + 1;
        while end < order.len()
            && end - start < self.max_run
            && order[end].1 == first + (end - start) as u64
        {
            end += 1;
        }
        end
    }

    /// A full writeback of a journaled volume: data, then one transaction
    /// (several only when the dirty metadata outgrows the log).
    pub(super) fn flush_journaled(&mut self, io: &dyn BlockIo) -> Result<(), IoError> {
        self.stats.writebacks += 1;
        self.flush_data(io)?;
        let order = self.dirty_where(false);
        let geo = Geometry {
            block_size: self.block_size,
            sectors_per_block: self.sectors_per_block,
            max_run: self.max_run,
        };
        let per = match &self.journal {
            Some(journal) => journal.max_data_blocks(self.block_size).max(1),
            None => return Ok(()),
        };
        for chunk in order.chunks(per) {
            self.commit_chunk(io, &geo, chunk)?;
        }
        Ok(())
    }

    /// Log `chunk` as one transaction, write it home, empty the log. A
    /// failure after the commit leaves the journal aborted: the transaction
    /// is in the log and the next mount replays it.
    fn commit_chunk(
        &mut self,
        io: &dyn BlockIo,
        geo: &Geometry,
        chunk: &[Entry],
    ) -> Result<(), IoError> {
        let committed = {
            let size = self.block_size;
            let entries: Vec<(u64, &[u8])> = chunk
                .iter()
                .map(|&(_, block, index)| (block, &page(&self.slots[index])[..size]))
                .collect();
            match self.journal.as_mut() {
                Some(journal) => journal.commit(io, geo, 0, &entries),
                None => Err(IoError::Failed),
            }
        };
        if let Err(error) = committed {
            self.failed = true;
            return Err(error);
        }
        let checkpointed = self
            .write_all(io, chunk)
            .and_then(|()| io.flush())
            .and_then(|()| match self.journal.as_mut() {
                Some(journal) => journal.finish(io, geo),
                None => Ok(()),
            });
        if let Err(error) = checkpointed {
            self.failed = true;
            if let Some(journal) = self.journal.as_mut() {
                journal.aborted = true;
            }
            return Err(error);
        }
        Ok(())
    }
}
