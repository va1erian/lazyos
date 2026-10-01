//! Mounting: validate the superblock and the group descriptor table.

use super::*;

impl Ext2 {
    /// Probe `io` for an ext2 superblock and mount it. Any malformed or
    /// unsupported image is refused with a friendly [`Ext2Error`]; nothing here
    /// trusts the disk. `clock` supplies the UTC seconds stamped into inodes.
    pub fn open(io: Box<dyn BlockIo>, clock: Clock) -> Result<Ext2, Ext2Error> {
        // Every device speaks 512-byte sectors ([`SECTOR_SIZE`]). The
        // superblock sits at byte 1024, so reading sectors 2..4 works for any
        // block size.
        let device_bytes = io.sector_count().saturating_mul(SECTOR_SIZE as u64);
        if device_bytes < SUPER_OFFSET + 1024 {
            return Err(Ext2Error::Invalid);
        }
        let mut superblock = [0u8; 1024];
        io.read_sectors(SUPER_OFFSET / SECTOR_SIZE as u64, &mut superblock)
            .map_err(io_error)?;
        if le16(&superblock, SB_MAGIC) != EXT2_MAGIC {
            return Err(Ext2Error::Invalid);
        }

        let log_block_size = le32(&superblock, SB_LOG_BLOCK_SIZE);
        if log_block_size > 2 {
            return Err(Ext2Error::Invalid); // 1024 << n, at most 4096
        }
        let block_size = 1024u32 << log_block_size;
        let inodes_count = le32(&superblock, SB_INODES_COUNT);
        let blocks_count = le32(&superblock, SB_BLOCKS_COUNT);
        let free_blocks = le32(&superblock, SB_FREE_BLOCKS);
        let free_inodes = le32(&superblock, SB_FREE_INODES);
        let first_data_block = le32(&superblock, SB_FIRST_DATA_BLOCK);
        let blocks_per_group = le32(&superblock, SB_BLOCKS_PER_GROUP);
        let inodes_per_group = le32(&superblock, SB_INODES_PER_GROUP);
        let rev_level = le32(&superblock, SB_REV_LEVEL);
        let inode_size = if rev_level >= 1 {
            le16(&superblock, SB_INODE_SIZE)
        } else {
            INODE_CORE_SIZE as u16
        };
        let first_ino = if rev_level >= 1 {
            le32(&superblock, SB_FIRST_INO)
        } else {
            DEFAULT_FIRST_INO
        };
        let feature_incompat = le32(&superblock, SB_FEATURE_INCOMPAT);
        let feature_ro = le32(&superblock, SB_FEATURE_RO_COMPAT);

        if inodes_count < ROOT_INO || blocks_count <= first_data_block {
            return Err(Ext2Error::Invalid);
        }
        if free_blocks > blocks_count || free_inodes > inodes_count {
            return Err(Ext2Error::Invalid);
        }
        if first_data_block > 1 {
            return Err(Ext2Error::Invalid); // 1 for 1K blocks, 0 otherwise
        }
        if blocks_per_group == 0 || inodes_per_group == 0 {
            return Err(Ext2Error::Invalid);
        }
        if inode_size < INODE_CORE_SIZE as u16
            || u32::from(inode_size) > block_size
            || !block_size.is_multiple_of(u32::from(inode_size))
        {
            return Err(Ext2Error::Invalid);
        }
        if first_ino == 0 || first_ino > inodes_count {
            return Err(Ext2Error::Invalid);
        }
        // The descriptor table has one 32-byte entry per group; the group count
        // comes from the block count, and the inodes may not span more groups.
        let block_span = blocks_count - first_data_block;
        let groups = block_span.div_ceil(blocks_per_group);
        let inode_groups = inodes_count.div_ceil(inodes_per_group);
        if groups == 0 || groups > MAX_GROUPS || inode_groups > groups {
            return Err(Ext2Error::Invalid);
        }
        let gdt_block = u64::from(first_data_block) + 1;
        let gdt_blocks = (u64::from(groups) * GD_SIZE as u64).div_ceil(u64::from(block_size));
        if gdt_block + gdt_blocks > u64::from(blocks_count) {
            return Err(Ext2Error::Invalid);
        }
        if u64::from(blocks_count)
            .checked_mul(u64::from(block_size))
            .is_none_or(|bytes| bytes > device_bytes)
        {
            return Err(Ext2Error::Invalid);
        }
        if feature_incompat & !FEATURE_INCOMPAT_FILETYPE != 0 {
            return Err(Ext2Error::NotSupported);
        }
        if feature_ro & !(FEATURE_RO_SPARSE_SUPER | FEATURE_RO_LARGE_FILE) != 0 {
            return Err(Ext2Error::NotSupported);
        }
        let inodes_per_block = block_size / u32::from(inode_size);
        if !inodes_per_group.is_multiple_of(inodes_per_block) {
            return Err(Ext2Error::Invalid);
        }
        let mount_state = le16(&superblock, SB_STATE);
        let read_only = !io.is_writable();

        Ok(Ext2 {
            io,
            clock,
            block_size,
            sectors_per_block: block_size / SECTOR_SIZE as u32,
            inodes_count,
            blocks_count,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            inodes_per_block,
            ptrs_per_block: block_size / 4,
            first_ino,
            groups,
            gdt_block,
            has_file_type: feature_incompat & FEATURE_INCOMPAT_FILETYPE != 0,
            has_large_file: feature_ro & FEATURE_RO_LARGE_FILE != 0,
            read_only,
            mount_state,
            uuid: array16(&superblock, SB_UUID),
            label: array16(&superblock, SB_VOLUME_NAME),
            clean: AtomicBool::new(!read_only && mount_state & STATE_VALID != 0),
            lock: Mutex::new(()),
        })
    }
}
