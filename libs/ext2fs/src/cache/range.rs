//! Reading a run of blocks into a caller's buffer (`Ext2::read_run`).
//!
//! Cached blocks, dirty ones included, are copied from their pages. A stretch
//! of uncached blocks is read in one request: through cache pages (with
//! read-ahead) when it is short, so small files and metadata stay cached, or,
//! when it is at least [`BYPASS_BLOCKS`] long and wanted whole, straight into
//! the caller's buffer. A block that is not cached is current on the disk
//! (dirty blocks are never dropped, and nothing writes the disk but a
//! writeback under the same lock), so reading past the cache is exact.

use super::{page, BlockCache};
use crate::{BlockIo, IoError};

/// Uncached blocks in a row from which a read skips the cache: 256 KiB of
/// 4 KiB blocks. Streaming a large file then neither evicts the working set
/// nor pays a copy through cache pages.
pub const BYPASS_BLOCKS: u64 = 64;

impl BlockCache {
    /// Copy bytes `skip..skip + out.len()` of blocks `first..first + count`
    /// into `out`. The caller checked the range against the volume.
    pub fn read_range(
        &mut self,
        io: &dyn BlockIo,
        first: u64,
        count: u64,
        skip: usize,
        out: &mut [u8],
    ) -> Result<(), IoError> {
        let size = self.block_size;
        let end = first + count;
        let (mut block, mut pos, mut inner) = (first, 0usize, skip);
        while pos < out.len() && block < end {
            let take = (size - inner).min(out.len() - pos);
            let index = match self.map.get(&block) {
                Some(&index) => {
                    self.stats.hits += 1;
                    index
                }
                None => {
                    let mut stop = block + 1;
                    while stop < end && !self.map.contains_key(&stop) {
                        stop += 1;
                    }
                    let whole = ((out.len() - pos) / size) as u64;
                    let run = (stop - block).min(whole);
                    if inner == 0 && run >= BYPASS_BLOCKS {
                        let bytes = run as usize * size;
                        io.read_sectors(
                            block * self.sectors_per_block,
                            &mut out[pos..pos + bytes],
                        )?;
                        self.stats.bypassed += run;
                        block += run;
                        pos += bytes;
                        continue;
                    }
                    self.stats.misses += 1;
                    let wanted = (stop - block).min(self.max_run as u64) as usize;
                    self.fill(io, block, wanted)?
                }
            };
            let slot = &mut self.slots[index];
            slot.referenced = true;
            out[pos..pos + take].copy_from_slice(&page(slot)[inner..inner + take]);
            block += 1;
            pos += take;
            inner = 0;
        }
        Ok(())
    }
}
