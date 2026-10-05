//! Adding a journal to a formatted volume (what `mke2fs -j` / `tune2fs -j`
//! do): inode 8 gets the log's blocks, block 0 of the log the journal
//! superblock, and the volume superblock the feature bit and a backup of the
//! inode's block map.

use alloc::vec;

use crate::*;

use super::*;

impl Ext2 {
    /// Give the volume an internal journal of `blocks` blocks. The journal
    /// is used from the next [`Ext2::open_cached`]; the volume is flushed
    /// (and marked clean) before this returns.
    ///
    /// `blocks` is at least [`MIN_JOURNAL_BLOCKS`] and at most a quarter of
    /// the volume. Fails with [`Ext2Error::Exists`] when the volume already
    /// has one.
    pub fn add_journal(&self, blocks: u32) -> Result<(), Ext2Error> {
        {
            let _guard = self.lock.lock();
            self.create_journal(blocks)?;
        }
        self.sync_volume()
    }

    /// Whether the volume has a journal (a mount through the cache uses it).
    pub fn has_journal(&self) -> bool {
        self.journaled
    }

    fn create_journal(&self, blocks: u32) -> Result<(), Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if self.journaled {
            return Err(Ext2Error::Exists);
        }
        if !(MIN_JOURNAL_BLOCKS..=self.blocks_count / 4).contains(&blocks) {
            return Err(Ext2Error::Invalid);
        }
        let mut inode = self.read_inode(JOURNAL_INO)?;
        if le32(&inode, INO_SIZE) != 0 || (0..BLOCK_SLOTS).any(|s| Self::direct_ptr(&inode, s) != 0)
        {
            return Err(Ext2Error::Invalid); // inode 8 is in use for something else
        }
        let tables = blocks.div_ceil(self.ptrs_per_block) + 4;
        if u64::from(self.free_blocks_locked()?) < u64::from(blocks) + u64::from(tables) {
            return Err(Ext2Error::NoSpace);
        }
        let mut first = 0;
        for index in 0..blocks {
            let (block, _) = self.ensure_block(&mut inode, index)?;
            if index == 0 {
                first = block;
            }
        }
        let size = self.block_size as usize;
        let mut sb = vec![0u8; size];
        put_header(&mut sb, BLOCK_SUPER_V2, 0);
        put_be32(&mut sb, JS_BLOCKSIZE, self.block_size);
        put_be32(&mut sb, JS_MAXLEN, blocks);
        put_be32(&mut sb, JS_FIRST, 1);
        put_be32(&mut sb, JS_SEQUENCE, 1);
        put_be32(&mut sb, JS_NR_USERS, 1);
        sb[JS_UUID..JS_UUID + 16].copy_from_slice(&self.uuid);
        sb[JS_USERS..JS_USERS + 16].copy_from_slice(&self.uuid);
        self.write_block(u64::from(first), &sb)?;

        let now = self.now();
        put16(&mut inode, INO_MODE, S_IFREG | 0o600);
        put32(&mut inode, INO_SIZE, blocks * self.block_size);
        for field in [INO_ATIME, INO_CTIME, INO_MTIME] {
            put32(&mut inode, field, now);
        }
        put16(&mut inode, INO_LINKS, 1);
        self.write_inode(JOURNAL_INO, &inode)?;

        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        let compat = le32(&raw, SB_FEATURE_COMPAT) | FEATURE_COMPAT_HAS_JOURNAL;
        put32(&mut raw, SB_FEATURE_COMPAT, compat);
        put32(&mut raw, SB_JOURNAL_INUM, JOURNAL_INO);
        // e2fsprogs keeps the inode's block map here to find a lost journal.
        raw[SB_JNL_BACKUP_TYPE] = 1;
        for slot in 0..BLOCK_SLOTS as usize {
            let pointer = le32(&inode, INO_BLOCK + slot * 4);
            put32(&mut raw, SB_JNL_BLOCKS + slot * 4, pointer);
        }
        put32(&mut raw, SB_JNL_BLOCKS + 15 * 4, 0);
        put32(&mut raw, SB_JNL_BLOCKS + 16 * 4, blocks * self.block_size);
        self.write_super_raw(&raw)
    }

    fn free_blocks_locked(&self) -> Result<u32, Ext2Error> {
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_BLOCKS).saturating_add(self.pending_frees().0))
    }
}
