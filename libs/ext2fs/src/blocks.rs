//! Block and inode allocation and inode I/O.

use super::*;

impl Ext2 {
    /// Give the host its pause between two units of work ([`BlockIo::pace`]),
    /// and let the caller's pause hook run: the kernel opens an interrupt
    /// window here (issue #567) and, when the gate holder may sleep, breathes
    /// (docs/performance-plan.md P5).
    pub(super) fn pace(&self) {
        self.io.pace();
        self.pause_point();
    }

    /// Allocate a data block. A full volume with frees waiting for a commit
    /// commits and tries again. Cached, the new block starts as a zeroed
    /// *fresh* block, so whatever is written into it reaches the disk before
    /// any pointer to it (`cache/roles.rs`).
    pub(super) fn alloc_block(&self) -> Result<u32, Ext2Error> {
        let block = match self.alloc_block_now() {
            Err(Ext2Error::NoSpace) if self.has_pending() => {
                self.commit_locked()?;
                self.alloc_block_now()?
            }
            other => other?,
        };
        if self.cache.is_some() {
            if let Err(error) = self.zero_block(u64::from(block)) {
                let _ = self.free_block(block);
                return Err(error);
            }
        }
        Ok(block)
    }

    /// Allocate an inode, committing first when only pending frees would
    /// make room (see [`Ext2::alloc_block`]).
    pub(super) fn alloc_inode(&self, is_dir: bool) -> Result<u32, Ext2Error> {
        match self.alloc_inode_now(is_dir) {
            Err(Ext2Error::NoSpace) if self.has_pending() => {
                self.commit_locked()?;
                self.alloc_inode_now(is_dir)
            }
            other => other,
        }
    }

    /// Allocate a data block from the first group with a free bit, updating
    /// the group descriptor and the superblock counters together.
    fn alloc_block_now(&self) -> Result<u32, Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            self.pace();
            let desc = self.read_group(group)?;
            if desc.free_blocks == 0 {
                continue;
            }
            let base = group
                .checked_mul(self.blocks_per_group)
                .and_then(|offset| offset.checked_add(self.first_data_block))
                .ok_or(Ext2Error::Invalid)?;
            if base >= self.blocks_count {
                continue;
            }
            let bits = min(self.blocks_per_group, self.blocks_count - base);
            let mut bitmap = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], 0, bits)? else {
                continue; // counts and bitmap disagree; try the next group
            };
            Self::bitmap_set(&mut bitmap[..size], index)?;
            self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_blocks = updated
                .free_blocks
                .checked_sub(1)
                .ok_or(Ext2Error::Invalid)?;
            self.write_group(group, &updated)?;
            self.update_counts(-1, 0)?;
            return Ok(base + index);
        }
        Err(Ext2Error::NoSpace)
    }

    /// Return a data block to its group bitmap and counters.
    pub(super) fn release_block(&self, block: u32) -> Result<(), Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(Ext2Error::Invalid);
        }
        let group = (block - self.first_data_block) / self.blocks_per_group;
        let index = (block - self.first_data_block) % self.blocks_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], index)? {
            return Err(Ext2Error::Invalid); // double free: the image is inconsistent
        }
        Self::bitmap_clear(&mut bitmap[..size], index)?;
        self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_blocks = updated
            .free_blocks
            .checked_add(1)
            .ok_or(Ext2Error::Invalid)?;
        self.write_group(group, &updated)?;
        self.update_counts(1, 0)
    }

    /// Allocate an inode, updating the group's free count and directory count.
    fn alloc_inode_now(&self, is_dir: bool) -> Result<u32, Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            if desc.free_inodes == 0 {
                continue;
            }
            let base = group
                .checked_mul(self.inodes_per_group)
                .ok_or(Ext2Error::Invalid)?;
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
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], start, bits)? else {
                continue;
            };
            Self::bitmap_set(&mut bitmap[..size], index)?;
            self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_inodes = updated
                .free_inodes
                .checked_sub(1)
                .ok_or(Ext2Error::Invalid)?;
            if is_dir {
                updated.used_dirs = updated.used_dirs.checked_add(1).ok_or(Ext2Error::Invalid)?;
            }
            self.write_group(group, &updated)?;
            self.update_counts(0, -1)?;
            return Ok(base + index + 1);
        }
        Err(Ext2Error::NoSpace)
    }

    /// Return an inode to its group bitmap and counters.
    pub(super) fn release_inode(&self, ino: u32, is_dir: bool) -> Result<(), Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if ino < ROOT_INO || ino > self.inodes_count {
            return Err(Ext2Error::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], local)? {
            return Err(Ext2Error::Invalid); // double free
        }
        Self::bitmap_clear(&mut bitmap[..size], local)?;
        self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_inodes = updated
            .free_inodes
            .checked_add(1)
            .ok_or(Ext2Error::Invalid)?;
        if is_dir {
            updated.used_dirs = updated.used_dirs.checked_sub(1).ok_or(Ext2Error::Invalid)?;
        }
        self.write_group(group, &updated)?;
        self.update_counts(0, 1)
    }

    /// Read the 128-byte core of an inode; larger inode tails are left alone.
    pub(super) fn read_inode(&self, ino: u32) -> Result<[u8; INODE_CORE_SIZE], Ext2Error> {
        if ino == 0 || ino > self.inodes_count {
            return Err(Ext2Error::Invalid);
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
            return Err(Ext2Error::Invalid);
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
    ) -> Result<(), Ext2Error> {
        if ino == 0 || ino > self.inodes_count {
            return Err(Ext2Error::Invalid);
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
            return Err(Ext2Error::Invalid);
        }
        buf[offset..end].copy_from_slice(inode);
        self.write_block(block, &buf[..size])
    }

    /// Whether bit `index` of a bitmap is set.
    /// A bit past the end of the buffer is corruption, never a panic.
    pub(super) fn bitmap_test(buf: &[u8], index: u32) -> Result<bool, Ext2Error> {
        let byte = buf.get((index / 8) as usize).ok_or(Ext2Error::Invalid)?;
        Ok(byte & (1 << (index % 8)) != 0)
    }

    pub(super) fn bitmap_set(buf: &mut [u8], index: u32) -> Result<(), Ext2Error> {
        let byte = buf
            .get_mut((index / 8) as usize)
            .ok_or(Ext2Error::Invalid)?;
        *byte |= 1 << (index % 8);
        Ok(())
    }

    pub(super) fn bitmap_clear(buf: &mut [u8], index: u32) -> Result<(), Ext2Error> {
        let byte = buf
            .get_mut((index / 8) as usize)
            .ok_or(Ext2Error::Invalid)?;
        *byte &= !(1 << (index % 8));
        Ok(())
    }

    /// The first clear bit in `start..bits`, scanning in allocation order.
    pub(super) fn bitmap_find_zero(
        buf: &[u8],
        start: u32,
        bits: u32,
    ) -> Result<Option<u32>, Ext2Error> {
        let mut index = start;
        while index < bits {
            // A full byte holds no clear bit: skip it whole. This is the
            // common case on a filling volume, where every allocation scans
            // the group's bitmap from the start.
            if index.is_multiple_of(8) && bits - index >= 8 {
                let byte = *buf.get((index / 8) as usize).ok_or(Ext2Error::Invalid)?;
                if byte == 0xFF {
                    index += 8;
                    continue;
                }
            }
            if !Self::bitmap_test(buf, index)? {
                return Ok(Some(index));
            }
            index += 1;
        }
        Ok(None)
    }
}
