//! The block map: logical file block -> physical block through the inode's
//! direct slots and its single/double/triple indirect trees.
//!
//! An indirect slot roots a tree of pointer tables `depth` levels deep, and a
//! table holds `ptrs_per_block` pointers. A logical block is therefore
//! addressed by a slot plus one table offset per level (a "path"); reading,
//! allocating and truncating all walk the same path shape.

use super::*;

/// Where one logical block hangs off the inode.
struct BlockPath {
    /// Index into the inode's fifteen block slots.
    slot: u32,
    /// Tables between the slot and the data block (`0` for a direct slot).
    depth: usize,
    /// Pointer offset inside the table at each level, outermost first.
    offsets: [u32; MAX_DEPTH],
}

impl Ext2 {
    /// The block pointer in direct slot `index`.
    pub(super) fn direct_ptr(inode: &[u8; INODE_CORE_SIZE], index: u32) -> u32 {
        le32(inode, INO_BLOCK + index as usize * 4)
    }

    /// Locate logical block `index`: peel off the direct slots, then walk the
    /// indirect depths, each of which covers `ptrs_per_block` times the last.
    fn locate(&self, index: u32) -> Result<BlockPath, FsError> {
        if index < DIRECT_BLOCKS {
            return Ok(BlockPath {
                slot: index,
                depth: 0,
                offsets: [0; MAX_DEPTH],
            });
        }
        let ptrs = u64::from(self.ptrs_per_block);
        let mut rest = u64::from(index - DIRECT_BLOCKS);
        let mut span = ptrs; // blocks reachable through one slot at this depth
        for depth in 1..=MAX_DEPTH {
            if rest < span {
                // `rest` written in base `ptrs` is the offset at each level.
                let mut offsets = [0u32; MAX_DEPTH];
                for offset in offsets[..depth].iter_mut().rev() {
                    *offset = (rest % ptrs) as u32;
                    rest /= ptrs;
                }
                return Ok(BlockPath {
                    slot: SINGLE_INDIRECT_SLOT + depth as u32 - 1,
                    depth,
                    offsets,
                });
            }
            rest -= span;
            span *= ptrs;
        }
        Err(FsError::NoSpace)
    }

    /// The logical block holding byte `position`, or [`FsError::NotSupported`]
    /// when the position lies beyond what the block map can address. A plain
    /// `as u32` would silently alias a huge offset onto a low block.
    pub(super) fn block_index(&self, position: u64) -> Result<u32, FsError> {
        u32::try_from(position / u64::from(self.block_size)).map_err(|_| FsError::NotSupported)
    }

    /// Resolve logical block `index`; `0` means a hole. Indices beyond the
    /// triple-indirect range are refused rather than misread.
    pub(super) fn block_map(
        &self,
        inode: &[u8; INODE_CORE_SIZE],
        index: u32,
    ) -> Result<u32, FsError> {
        let path = self.locate(index).map_err(|_| FsError::NotSupported)?;
        let size = self.block_size as usize;
        let mut table = [0u8; MAX_BLOCK_SIZE];
        let mut block = Self::direct_ptr(inode, path.slot);
        for &offset in &path.offsets[..path.depth] {
            if block == 0 {
                return Ok(0); // a hole anywhere on the path is a hole
            }
            self.read_block(u64::from(block), &mut table[..size])?;
            block = le32(&table, offset as usize * 4);
        }
        Ok(block)
    }

    /// Account one newly allocated block in `i_blocks` (512-byte units).
    pub(super) fn add_inode_sectors(
        &self,
        inode: &mut [u8; INODE_CORE_SIZE],
    ) -> Result<(), FsError> {
        let sectors = self.block_size / SECTOR_SIZE as u32;
        let value = le32(inode, INO_BLOCKS)
            .checked_add(sectors)
            .ok_or(FsError::Invalid)?;
        put32(inode, INO_BLOCKS, value);
        Ok(())
    }

    /// Allocate a block and zero it on disk. Zeroing happens before the block
    /// is linked anywhere (see [`Ext2::zero_block`]); a failure returns the
    /// block so it is not leaked.
    fn alloc_zeroed(&self) -> Result<u32, FsError> {
        let block = self.alloc_block()?;
        if let Err(error) = self.zero_block(u64::from(block)) {
            let _ = self.free_block(block);
            return Err(error);
        }
        Ok(block)
    }

    /// Resolve logical block `index` for writing, allocating the data block and
    /// any missing tables on its path. Returns `(block, fresh)` so the caller
    /// can skip reading a brand-new (zeroed) block before a short write.
    ///
    /// Every new block is zeroed before the pointer to it is written, and a
    /// new table's parent pointer is written last, so a failure part-way
    /// leaves only whole, valid links behind. Blocks linked into the in-memory
    /// `inode` are the caller's to persist (the write path always does).
    pub(super) fn ensure_block(
        &self,
        inode: &mut [u8; INODE_CORE_SIZE],
        index: u32,
    ) -> Result<(u32, bool), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let path = self.locate(index)?;
        let mut block = Self::direct_ptr(inode, path.slot);
        let mut fresh = block == 0;
        if fresh {
            block = self.alloc_zeroed()?;
            put32(inode, INO_BLOCK + path.slot as usize * 4, block);
            self.add_inode_sectors(inode)?;
        }
        for &offset in &path.offsets[..path.depth] {
            (block, fresh) = self.ensure_child(inode, block, fresh, offset)?;
        }
        Ok((block, fresh))
    }

    /// Resolve pointer `offset` of `table`, allocating and linking a zeroed
    /// child when it is empty. `table_is_new` skips reading a table that was
    /// just zeroed. Returns the child and whether it was just allocated.
    fn ensure_child(
        &self,
        inode: &mut [u8; INODE_CORE_SIZE],
        table_block: u32,
        table_is_new: bool,
        offset: u32,
    ) -> Result<(u32, bool), FsError> {
        let size = self.block_size as usize;
        let mut table = [0u8; MAX_BLOCK_SIZE];
        if !table_is_new {
            self.read_block(u64::from(table_block), &mut table[..size])?;
        }
        let slot = offset as usize * 4;
        let existing = le32(&table, slot);
        if existing != 0 {
            return Ok((existing, false));
        }
        let child = self.alloc_zeroed()?;
        put32(&mut table, slot, child);
        if let Err(error) = self.write_block(u64::from(table_block), &table[..size]) {
            let _ = self.free_block(child);
            return Err(error);
        }
        self.add_inode_sectors(inode)?;
        Ok((child, true))
    }
}

/// How many of `len` bytes written at `offset` fit under [`MAX_FILE_SIZE`]: a
/// write straddling the cap is short, one wholly past it is [`FsError::NoSpace`].
pub(super) fn writable_len(offset: u64, len: usize) -> Result<usize, FsError> {
    if offset >= MAX_FILE_SIZE {
        return Err(FsError::NoSpace);
    }
    Ok(len.min((MAX_FILE_SIZE - offset) as usize))
}
