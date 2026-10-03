//! Writeback: every dirty block to the disk, phase by phase.
//!
//! The dirty blocks are sorted by ([`Phase`], block number); each run of
//! consecutive block numbers within a phase becomes one vectored request of
//! at most `max_run` blocks. A request that fails stops the writeback on the
//! spot: the blocks it carried and everything after stay dirty, so a later
//! phase never lands ahead of an earlier one, and the next writeback retries.

use alloc::vec::Vec;

use super::roles::Phase;
use super::{page, BlockCache};
use crate::{BlockIo, IoError};

impl BlockCache {
    /// Write every dirty block back in phase order. On error the cache keeps
    /// what did not land dirty and reports the device's error.
    pub fn flush(&mut self, io: &dyn BlockIo) -> Result<(), IoError> {
        if self.dirty == 0 {
            return Ok(());
        }
        let mut order: Vec<(Phase, u64, usize)> = self
            .map
            .iter()
            .filter(|&(_, &index)| self.slots[index].dirty)
            .map(|(&block, &index)| {
                (
                    self.roles.phase(block, self.slots[index].fresh),
                    block,
                    index,
                )
            })
            .collect();
        order.sort_unstable();
        self.stats.writebacks += 1;
        let mut start = 0;
        while start < order.len() {
            let end = self.run_end(&order, start);
            self.write_run(io, &order[start..end])?;
            start = end;
        }
        Ok(())
    }

    /// One past the last entry of the run starting at `start`: same phase,
    /// consecutive blocks, at most `max_run` long.
    fn run_end(&self, order: &[(Phase, u64, usize)], start: usize) -> usize {
        let (phase, first, _) = order[start];
        let mut end = start + 1;
        while end < order.len()
            && end - start < self.max_run
            && order[end].0 == phase
            && order[end].1 == first + (end - start) as u64
        {
            end += 1;
        }
        end
    }

    /// Write one run as a single request and mark it clean.
    fn write_run(&mut self, io: &dyn BlockIo, run: &[(Phase, u64, usize)]) -> Result<(), IoError> {
        let first = run[0].1;
        let result = {
            let bufs: Vec<&[u8]> = run
                .iter()
                .map(|&(_, _, index)| &page(&self.slots[index])[..self.block_size])
                .collect();
            io.write_sectors_vectored(first * self.sectors_per_block, &bufs)
        };
        if result.is_err() {
            self.failed = true;
        }
        result?;
        self.stats.write_requests += 1;
        self.stats.written += run.len() as u64;
        for &(_, _, index) in run {
            let slot = &mut self.slots[index];
            slot.dirty = false;
            slot.fresh = false;
            self.dirty -= 1;
        }
        Ok(())
    }
}
