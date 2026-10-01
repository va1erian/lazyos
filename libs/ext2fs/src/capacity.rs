//! `statfs`: the volume's size and free space, read from the superblock.

use super::*;

/// The Linux `f_type` for ext2/3/4 (`EXT2_SUPER_MAGIC`).
const EXT2_SUPER_MAGIC: u32 = 0xEF53;

impl Ext2 {
    /// Capacity figures for `statfs(2)`. The free counts come from the
    /// superblock, which every allocation keeps in step with the group
    /// descriptors (`update_counts`), so no bitmap scan is needed.
    pub fn statfs(&self) -> Result<FsStats, Ext2Error> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(FsStats {
            magic: EXT2_SUPER_MAGIC,
            block_size: self.block_size,
            blocks: u64::from(self.blocks_count),
            // Clamp in case a damaged image claims more free than exist.
            blocks_free: u64::from(le32(&raw, SB_FREE_BLOCKS).min(self.blocks_count)),
            files: u64::from(self.inodes_count),
            files_free: u64::from(le32(&raw, SB_FREE_INODES).min(self.inodes_count)),
            name_max: MAX_NAME as u32,
        })
    }
}
