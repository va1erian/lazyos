//! Reading a file's bytes in runs of physically contiguous blocks.
//!
//! A file written front to back on a fresh volume is mostly one long run of
//! blocks. Reading it a block at a time cost one cache lookup, one copy into a
//! scratch block and one copy out per block, and an indirect-table re-read per
//! block for the block map. [`Ext2::read_data`] maps the blocks with a memo of
//! the last pointer table ([`Ext2::block_map_memo`]), groups consecutive
//! physical blocks into runs of up to [`MAX_RUN_BLOCKS`], and hands each run to
//! [`Ext2::read_run`]: one device request for the uncached part, copied
//! straight into the caller's buffer.

use super::indirect::MapMemo;
use super::*;

/// Most blocks one run spans: 1 MiB of 4 KiB blocks. Bounds a single device
/// request and the time one cache call holds the volume.
const MAX_RUN_BLOCKS: u64 = 256;

impl Ext2 {
    /// Read up to `buf.len()` bytes of the regular file `inode` at `offset`;
    /// holes read as zeros and a read past the end returns 0.
    pub(super) fn read_data(
        &self,
        inode: &[u8; INODE_CORE_SIZE],
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Ext2Error> {
        let size = self.file_size(inode);
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let count = min(size - offset, buf.len() as u64) as usize;
        let block_size = u64::from(self.block_size);
        let mut memo = MapMemo::new();
        let mut done = 0usize;
        while done < count {
            if done > 0 {
                self.pause_point(); // between two runs
            }
            let position = offset + done as u64;
            let index = self.block_index(position)?;
            let inner = position % block_size;
            // Blocks from `index` on that hold wanted bytes.
            let wanted = (inner + (count - done) as u64).div_ceil(block_size);
            let first = self.block_map_memo(inode, index, &mut memo)?;
            let mut run = 1u64;
            if first != 0 {
                while run < wanted.min(MAX_RUN_BLOCKS) {
                    // `index + run` is inside the file, so it is addressable.
                    let next = self.block_map_memo(inode, index + run as u32, &mut memo)?;
                    if next == 0 || u64::from(next) != u64::from(first) + run {
                        break;
                    }
                    run += 1;
                }
            }
            let len = min((run * block_size - inner) as usize, count - done);
            let out = &mut buf[done..done + len];
            if first == 0 {
                out.fill(0); // a sparse hole reads as zero
            } else {
                self.read_run(u64::from(first), run, inner as usize, out)?;
            }
            done += len;
        }
        Ok(done)
    }

    /// Copy bytes `skip..skip + out.len()` of blocks `first..first + count`
    /// into `out` (`skip` is under one block, and the bytes lie inside the run).
    pub(super) fn read_run(
        &self,
        first: u64,
        count: u64,
        skip: usize,
        out: &mut [u8],
    ) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        let end = first.checked_add(count).ok_or(Ext2Error::Invalid)?;
        if end > u64::from(self.blocks_count)
            || skip >= size
            || skip + out.len() > count as usize * size
        {
            return Err(Ext2Error::Invalid);
        }
        if let Some(cache) = &self.cache {
            return self.with_cache(cache, |cache, io| {
                cache.read_range(io, first, count, skip, out)
            });
        }
        // Uncached: whole blocks straight into `out`, partial edges through a
        // block buffer.
        let sectors = u64::from(self.sectors_per_block);
        let (mut block, mut pos, mut inner) = (first, 0usize, skip);
        while pos < out.len() {
            let left = out.len() - pos;
            if inner == 0 && left >= size {
                let whole = left / size;
                self.io
                    .read_sectors(block * sectors, &mut out[pos..pos + whole * size])
                    .map_err(io_error)?;
                block += whole as u64;
                pos += whole * size;
            } else {
                let mut tmp = [0u8; MAX_BLOCK_SIZE];
                self.io
                    .read_sectors(block * sectors, &mut tmp[..size])
                    .map_err(io_error)?;
                let take = min(size - inner, left);
                out[pos..pos + take].copy_from_slice(&tmp[inner..inner + take]);
                block += 1;
                pos += take;
                inner = 0;
            }
        }
        Ok(())
    }
}
