//! Block and inode allocation, inode I/O and the block map.

use super::*;

impl Ext2 {
    /// Allocate a data block from the first group with a free bit, updating
    /// the group descriptor and the superblock counters together.
    pub(super) fn alloc_block(&self) -> Result<u32, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            if desc.free_blocks == 0 {
                continue;
            }
            let base = self.first_data_block + group * self.blocks_per_group;
            if base >= self.blocks_count {
                continue;
            }
            let bits = min(self.blocks_per_group, self.blocks_count - base);
            let mut bitmap = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], 0, bits) else {
                continue; // counts and bitmap disagree; try the next group
            };
            Self::bitmap_set(&mut bitmap[..size], index);
            self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_blocks = updated.free_blocks.checked_sub(1).ok_or(FsError::Invalid)?;
            self.write_group(group, &updated)?;
            self.update_counts(-1, 0)?;
            return Ok(base + index);
        }
        Err(FsError::NoSpace)
    }

    /// Return a data block to its group bitmap and counters.
    pub(super) fn free_block(&self, block: u32) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(FsError::Invalid);
        }
        let group = (block - self.first_data_block) / self.blocks_per_group;
        let index = (block - self.first_data_block) % self.blocks_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], index) {
            return Err(FsError::Invalid); // double free: the image is inconsistent
        }
        Self::bitmap_clear(&mut bitmap[..size], index);
        self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_blocks = updated.free_blocks.checked_add(1).ok_or(FsError::Invalid)?;
        self.write_group(group, &updated)?;
        self.update_counts(1, 0)
    }

    /// Allocate an inode, updating the group's free count and directory count.
    pub(super) fn alloc_inode(&self, is_dir: bool) -> Result<u32, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            if desc.free_inodes == 0 {
                continue;
            }
            let base = group * self.inodes_per_group;
            if base >= self.inodes_count {
                continue;
            }
            let bits = min(self.inodes_per_group, self.inodes_count - base);
            // Group 0 keeps the reserved inodes below `first_ino`.
            let start = if group == 0 {
                self.first_ino.saturating_sub(1)
            } else {
                0
            };
            if start >= bits {
                continue;
            }
            let mut bitmap = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], start, bits) else {
                continue;
            };
            Self::bitmap_set(&mut bitmap[..size], index);
            self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_inodes = updated.free_inodes.checked_sub(1).ok_or(FsError::Invalid)?;
            if is_dir {
                updated.used_dirs = updated.used_dirs.checked_add(1).ok_or(FsError::Invalid)?;
            }
            self.write_group(group, &updated)?;
            self.update_counts(0, -1)?;
            return Ok(base + index + 1);
        }
        Err(FsError::NoSpace)
    }

    /// Return an inode to its group bitmap and counters.
    pub(super) fn free_inode(&self, ino: u32, is_dir: bool) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if ino < ROOT_INO || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], local) {
            return Err(FsError::Invalid); // double free
        }
        Self::bitmap_clear(&mut bitmap[..size], local);
        self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_inodes = updated.free_inodes.checked_add(1).ok_or(FsError::Invalid)?;
        if is_dir {
            updated.used_dirs = updated.used_dirs.checked_sub(1).ok_or(FsError::Invalid)?;
        }
        self.write_group(group, &updated)?;
        self.update_counts(0, 1)
    }

    /// Read the 128-byte core of an inode; larger inode tails are left alone.
    pub(super) fn read_inode(&self, ino: u32) -> Result<[u8; INODE_CORE_SIZE], FsError> {
        if ino == 0 || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let block = desc.inode_table as u64 + u64::from(local / self.inodes_per_block);
        let slot = (local % self.inodes_per_block) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let offset = slot * usize::from(self.inode_size);
        let end = offset + INODE_CORE_SIZE;
        if end > size {
            return Err(FsError::Invalid);
        }
        let mut core = [0u8; INODE_CORE_SIZE];
        core.copy_from_slice(&buf[offset..end]);
        Ok(core)
    }

    /// Write the 128-byte core of an inode (read-modify-write).
    pub(super) fn write_inode(
        &self,
        ino: u32,
        inode: &[u8; INODE_CORE_SIZE],
    ) -> Result<(), FsError> {
        if ino == 0 || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let block = desc.inode_table as u64 + u64::from(local / self.inodes_per_block);
        let slot = (local % self.inodes_per_block) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let offset = slot * usize::from(self.inode_size);
        let end = offset + INODE_CORE_SIZE;
        if end > size {
            return Err(FsError::Invalid);
        }
        buf[offset..end].copy_from_slice(inode);
        self.write_block(block, &buf[..size])
    }

    /// The block pointer in direct slot `index`.
    pub(super) fn direct_ptr(inode: &[u8; INODE_CORE_SIZE], index: u32) -> u32 {
        le32(inode, INO_BLOCK + index as usize * 4)
    }

    /// Resolve logical block `index` through the direct or single-indirect
    /// map; `0` means a hole. Double/triple indirect files are refused rather
    /// than misread.
    pub(super) fn block_map(
        &self,
        inode: &[u8; INODE_CORE_SIZE],
        index: u32,
    ) -> Result<u32, FsError> {
        if index < DIRECT_BLOCKS {
            return Ok(Self::direct_ptr(inode, index));
        }
        if index < DIRECT_BLOCKS + self.ptrs_per_block {
            let indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
            if indirect == 0 {
                return Ok(0);
            }
            let size = self.block_size as usize;
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(indirect), &mut buf[..size])?;
            return Ok(le32(&buf, ((index - DIRECT_BLOCKS) * 4) as usize));
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Ok(0)
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

    /// Resolve logical block `index` for writing, allocating the data block
    /// (and the indirect block) when missing. Returns `(block, fresh)` so the
    /// caller can zero-fill a brand-new block before the short write.
    pub(super) fn ensure_block(
        &self,
        inode: &mut [u8; INODE_CORE_SIZE],
        index: u32,
    ) -> Result<(u32, bool), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if index < DIRECT_BLOCKS {
            let existing = Self::direct_ptr(inode, index);
            if existing != 0 {
                return Ok((existing, false));
            }
            let fresh = self.alloc_block()?;
            if let Err(error) = self.zero_block(u64::from(fresh)) {
                let _ = self.free_block(fresh);
                return Err(error);
            }
            put32(inode, INO_BLOCK + index as usize * 4, fresh);
            self.add_inode_sectors(inode)?;
            return Ok((fresh, true));
        }
        if index < DIRECT_BLOCKS + self.ptrs_per_block {
            let size = self.block_size as usize;
            let mut table = [0u8; MAX_BLOCK_SIZE];
            let mut indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
            if indirect == 0 {
                indirect = self.alloc_block()?;
                self.write_block(u64::from(indirect), &table[..size])?; // zeroed
                put32(
                    inode,
                    INO_BLOCK + SINGLE_INDIRECT_SLOT as usize * 4,
                    indirect,
                );
                self.add_inode_sectors(inode)?;
            } else {
                self.read_block(u64::from(indirect), &mut table[..size])?;
            }
            let slot = ((index - DIRECT_BLOCKS) * 4) as usize;
            let existing = le32(&table, slot);
            if existing != 0 {
                return Ok((existing, false));
            }
            let fresh = self.alloc_block()?;
            if let Err(error) = self.zero_block(u64::from(fresh)) {
                let _ = self.free_block(fresh);
                return Err(error);
            }
            put32(&mut table, slot, fresh);
            self.write_block(u64::from(indirect), &table[..size])?;
            self.add_inode_sectors(inode)?;
            return Ok((fresh, true));
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Err(FsError::NoSpace)
    }

    /// Free every block a file inode owns (direct, then single indirect).
    pub(super) fn free_inode_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        for slot in 0..DIRECT_BLOCKS {
            let block = Self::direct_ptr(inode, slot);
            if block != 0 {
                self.free_block(block)?;
            }
        }
        let indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
        if indirect != 0 {
            let size = self.block_size as usize;
            let mut table = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(indirect), &mut table[..size])?;
            for index in 0..self.ptrs_per_block {
                let block = le32(&table, index as usize * 4);
                if block != 0 {
                    self.free_block(block)?;
                }
            }
            self.free_block(indirect)?;
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Ok(())
    }
}
