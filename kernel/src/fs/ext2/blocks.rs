//! Block and inode allocation and inode I/O.

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

    /// Whether bit `index` of a bitmap is set.
    pub(super) fn bitmap_test(buf: &[u8], index: u32) -> bool {
        buf[(index / 8) as usize] & (1 << (index % 8)) != 0
    }

    fn bitmap_set(buf: &mut [u8], index: u32) {
        buf[(index / 8) as usize] |= 1 << (index % 8);
    }

    fn bitmap_clear(buf: &mut [u8], index: u32) {
        buf[(index / 8) as usize] &= !(1 << (index % 8));
    }

    /// The first clear bit in `start..bits`, scanning in allocation order.
    fn bitmap_find_zero(buf: &[u8], start: u32, bits: u32) -> Option<u32> {
        (start..bits).find(|&index| !Self::bitmap_test(buf, index))
    }
}
