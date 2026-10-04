//! Block, superblock and group-descriptor I/O.

use super::*;

impl Ext2 {
    /// Read one filesystem block into `buf` (at least `block_size` bytes).
    pub(super) fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        if block >= u64::from(self.blocks_count) || buf.len() < size {
            return Err(Ext2Error::Invalid);
        }
        if let Some(cache) = &self.cache {
            return self.with_cache(cache, |cache, io| cache.read(io, block, &mut buf[..size]));
        }
        self.io
            .read_sectors(block * u64::from(self.sectors_per_block), &mut buf[..size])
            .map_err(io_error)
    }

    /// Zero one filesystem block on disk.
    ///
    /// Every freshly allocated block is zeroed here before its pointer is
    /// linked anywhere (the inode, or an indirect block already committed to
    /// disk). Linking a pointer to an allocated-but-unwritten block would let
    /// the block's previous owner's bytes (or, on a device that reads back
    /// garbage, uninitialized bytes) surface where a hole should read as
    /// zero -- a cross-file, potentially cross-user information leak
    /// (CWE-200) if the write of the caller's actual data then fails.
    ///
    /// Cached, the zeroed block is also marked *fresh*: the writeback puts it
    /// on the disk before anything that points at it (`cache/roles.rs`).
    pub(super) fn zero_block(&self, block: u64) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        let zeroed = [0u8; MAX_BLOCK_SIZE];
        self.store_block(block, &zeroed[..size], true, false)
    }

    /// Write one filesystem block from `buf`.
    pub(super) fn write_block(&self, block: u64, buf: &[u8]) -> Result<(), Ext2Error> {
        self.store_block(block, buf, false, false)
    }

    /// [`Ext2::write_block`] for a block of file contents. A journaled volume
    /// writes these home before the transaction that links them, instead of
    /// logging them (`journal/`).
    pub(super) fn write_data_block(&self, block: u64, buf: &[u8]) -> Result<(), Ext2Error> {
        self.store_block(block, buf, false, true)
    }

    /// [`Ext2::write_block`], saying whether the block was just allocated
    /// and whether it holds file data.
    fn store_block(
        &self,
        block: u64,
        buf: &[u8],
        fresh: bool,
        data: bool,
    ) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        if block >= u64::from(self.blocks_count) || buf.len() < size {
            return Err(Ext2Error::Invalid);
        }
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        // Before the first change lands, the volume must already say "dirty".
        self.mark_dirty()?;
        if let Some(cache) = &self.cache {
            return self.with_cache(cache, |cache, io| {
                cache.write(io, block, &buf[..size], fresh, data)
            });
        }
        self.io
            .write_sectors(block * u64::from(self.sectors_per_block), &buf[..size])
            .map_err(io_error)
    }

    /// Run `call` on the cache, recording a write-back that failed inside it
    /// (a miss or a full dirty set writes back) as a volume error.
    pub(super) fn with_cache<T>(
        &self,
        cache: &Mutex<cache::BlockCache>,
        call: impl FnOnce(&mut cache::BlockCache, &dyn BlockIo) -> Result<T, IoError>,
    ) -> Result<T, Ext2Error> {
        let mut cache = cache.lock();
        let result = call(&mut cache, &*self.io);
        if cache.take_failure() {
            self.note_write_back_failure();
        }
        result.map_err(io_error)
    }

    /// Read the 1024-byte superblock, wherever its block starts.
    pub(super) fn read_super_raw(&self, raw: &mut [u8; 1024]) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        let mut block = [0u8; MAX_BLOCK_SIZE];
        let sb_block = SUPER_OFFSET / u64::from(self.block_size);
        self.read_block(sb_block, &mut block[..size])?;
        let start = (SUPER_OFFSET % u64::from(self.block_size)) as usize;
        raw.copy_from_slice(&block[start..start + 1024]);
        Ok(())
    }

    /// Patch the superblock in place (read-modify-write keeps every other byte).
    pub(super) fn write_super_raw(&self, raw: &[u8; 1024]) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        let mut block = [0u8; MAX_BLOCK_SIZE];
        let sb_block = SUPER_OFFSET / u64::from(self.block_size);
        self.read_block(sb_block, &mut block[..size])?;
        let start = (SUPER_OFFSET % u64::from(self.block_size)) as usize;
        block[start..start + 1024].copy_from_slice(raw);
        self.write_block(sb_block, &block[..size])
    }

    /// Add a delta to the superblock's free counters, refusing to cross zero
    /// or exceed the totals (which would mean the image is inconsistent).
    pub(super) fn update_counts(
        &self,
        blocks_delta: i32,
        inodes_delta: i32,
    ) -> Result<(), Ext2Error> {
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        let blocks = i64::from(le32(&raw, SB_BLOCKS_COUNT));
        let inodes = i64::from(le32(&raw, SB_INODES_COUNT));
        let free_blocks = i64::from(le32(&raw, SB_FREE_BLOCKS)) + i64::from(blocks_delta);
        let free_inodes = i64::from(le32(&raw, SB_FREE_INODES)) + i64::from(inodes_delta);
        if free_blocks < 0 || free_blocks > blocks || free_inodes < 0 || free_inodes > inodes {
            return Err(Ext2Error::Invalid);
        }
        put32(&mut raw, SB_FREE_BLOCKS, free_blocks as u32);
        put32(&mut raw, SB_FREE_INODES, free_inodes as u32);
        put32(&mut raw, SB_WTIME, self.now());
        self.write_super_raw(&raw)
    }

    /// Read one group descriptor, validating its block pointers.
    pub(super) fn read_group(&self, group: u32) -> Result<GroupDesc, Ext2Error> {
        if group >= self.groups {
            return Err(Ext2Error::Invalid);
        }
        let byte = self.gdt_block * u64::from(self.block_size) + u64::from(group) * GD_SIZE as u64;
        let block = byte / u64::from(self.block_size);
        let offset = (byte % u64::from(self.block_size)) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let desc = GroupDesc {
            block_bitmap: le32(&buf, offset + GD_BLOCK_BITMAP),
            inode_bitmap: le32(&buf, offset + GD_INODE_BITMAP),
            inode_table: le32(&buf, offset + GD_INODE_TABLE),
            free_blocks: le16(&buf, offset + GD_FREE_BLOCKS),
            free_inodes: le16(&buf, offset + GD_FREE_INODES),
            used_dirs: le16(&buf, offset + GD_USED_DIRS),
        };
        if desc.block_bitmap >= self.blocks_count
            || desc.inode_bitmap >= self.blocks_count
            || desc.inode_table >= self.blocks_count
        {
            return Err(Ext2Error::Invalid);
        }
        Ok(desc)
    }

    /// Patch one group descriptor in place.
    pub(super) fn write_group(&self, group: u32, desc: &GroupDesc) -> Result<(), Ext2Error> {
        let byte = self.gdt_block * u64::from(self.block_size) + u64::from(group) * GD_SIZE as u64;
        let block = byte / u64::from(self.block_size);
        let offset = (byte % u64::from(self.block_size)) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        put32(&mut buf, offset + GD_BLOCK_BITMAP, desc.block_bitmap);
        put32(&mut buf, offset + GD_INODE_BITMAP, desc.inode_bitmap);
        put32(&mut buf, offset + GD_INODE_TABLE, desc.inode_table);
        put16(&mut buf, offset + GD_FREE_BLOCKS, desc.free_blocks);
        put16(&mut buf, offset + GD_FREE_INODES, desc.free_inodes);
        put16(&mut buf, offset + GD_USED_DIRS, desc.used_dirs);
        self.write_block(block, &buf[..size])
    }
}
