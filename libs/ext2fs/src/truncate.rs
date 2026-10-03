//! Truncation and block release.
//!
//! # Write ordering
//!
//! Releasing a block is the dangerous direction: if the bitmap says "free"
//! while some inode still points at the block, the next allocation hands it to
//! a second owner and two files silently share bytes. Every release here
//! therefore follows **detach, then free**:
//!
//! 1. the pointer (in the inode, or in the parent table) is zeroed and written;
//! 2. only then are the blocks it led to returned to the bitmaps.
//!
//! A stop between the steps strands blocks that are marked used but owned by
//! nobody -- a leak the dirty flag lets a later fsck reclaim -- and never a
//! block that is both free and reachable. Bytes about to be cut off inside a
//! kept block are zeroed *before* the new size is committed, so a partial
//! truncate can only ever destroy data the caller asked to delete, and a later
//! grow can never resurrect it.

use super::*;

/// The run of logical blocks one inode slot covers.
struct SlotSpan {
    /// Tables between the slot and the data blocks (`0` for a direct slot).
    depth: usize,
    /// Logical index of the first block the slot maps.
    first: u64,
    /// How many logical blocks the slot maps.
    capacity: u64,
}

/// What a shrink releases, decided before anything is written.
struct ShrinkPlan {
    /// Whole subtrees `(root, depth)` the inode no longer points at.
    detached: Vec<(u32, usize)>,
    /// The one slot cut part-way: `(root, depth, blocks kept beneath it)`.
    partial: Option<(u32, usize, u64)>,
}

impl Ext2 {
    /// Set a regular file's size, freeing blocks on a shrink and leaving a
    /// sparse hole on a grow.
    pub fn truncate(&self, path: &str, size: u64) -> Result<(), Ext2Error> {
        let _guard = self.lock.lock();
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        let ino = self.resolve(path)?;
        let mut inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        if size > MAX_FILE_SIZE {
            return Err(Ext2Error::NoSpace);
        }
        let old_size = self.file_size(&inode);
        if size > old_size {
            // Growing allocates nothing: the new range is a hole that reads as
            // zeros. Shrinks zero the tail of the last block, so no stale bytes
            // lurk past the old end of file.
            self.set_file_size(&mut inode, size);
            touch(&mut inode, self.now());
            return self.write_inode(ino, &inode);
        }
        if size == old_size {
            return Ok(());
        }
        self.zero_tail(&inode, size)?;
        if !size.is_multiple_of(u64::from(self.block_size)) {
            // Cached, the zeroed tail must still land before the new size
            // (the phases would put the inode first): see [`Ext2::barrier`].
            self.barrier()?;
        }
        self.set_file_size(&mut inode, size);
        touch(&mut inode, self.now());
        self.shrink_blocks(ino, &mut inode, size.div_ceil(u64::from(self.block_size)))
    }

    /// Store a regular file's size in `i_size` (plus the high word when the
    /// volume has the large-file feature).
    fn set_file_size(&self, inode: &mut [u8; INODE_CORE_SIZE], size: u64) {
        put32(inode, INO_SIZE, size as u32);
        if self.has_large_file {
            put32(inode, INO_DIR_ACL, (size >> 32) as u32);
        }
    }

    /// Free every block an inode owns, for an inode being deleted. The inode is
    /// persisted with no block pointers before any block is freed.
    pub(super) fn free_inode_blocks(
        &self,
        ino: u32,
        inode: &mut [u8; INODE_CORE_SIZE],
    ) -> Result<(), Ext2Error> {
        self.shrink_blocks(ino, inode, 0)
    }

    /// Release every block beyond the first `keep` logical blocks: commit the
    /// inode without them, then free them (see the module docs).
    fn shrink_blocks(
        &self,
        ino: u32,
        inode: &mut [u8; INODE_CORE_SIZE],
        keep: u64,
    ) -> Result<(), Ext2Error> {
        let plan = self.plan_shrink(inode, keep);
        self.write_inode(ino, inode)?; // the commit point: detached, not yet freed
        let mut freed = 0u32;
        if let Some((root, depth, kept)) = plan.partial {
            freed += self.trim_tree(root, depth, kept)?;
        }
        for (root, depth) in plan.detached {
            freed += self.free_tree(root, depth)?;
        }
        // Saturate: a foreign image with a wrong `i_blocks` must still shrink.
        let sectors = freed.saturating_mul(self.block_size / SECTOR_SIZE as u32);
        let remaining = le32(inode, INO_BLOCKS).saturating_sub(sectors);
        put32(inode, INO_BLOCKS, remaining);
        self.write_inode(ino, inode)
    }

    /// Zero the inode slots that lie wholly beyond `keep` blocks (in memory
    /// only) and note the one slot, if any, that straddles the cut.
    fn plan_shrink(&self, inode: &mut [u8; INODE_CORE_SIZE], keep: u64) -> ShrinkPlan {
        let mut plan = ShrinkPlan {
            detached: Vec::new(),
            partial: None,
        };
        for slot in 0..BLOCK_SLOTS {
            let root = Self::direct_ptr(inode, slot);
            let span = self.slot_span(slot);
            if root == 0 || keep >= span.first + span.capacity {
                continue; // empty, or wholly kept
            }
            if keep <= span.first {
                put32(inode, INO_BLOCK + slot as usize * 4, 0);
                plan.detached.push((root, span.depth));
            } else {
                plan.partial = Some((root, span.depth, keep - span.first));
            }
        }
        plan
    }

    /// The logical blocks inode slot `slot` maps.
    fn slot_span(&self, slot: u32) -> SlotSpan {
        if slot < DIRECT_BLOCKS {
            return SlotSpan {
                depth: 0,
                first: u64::from(slot),
                capacity: 1,
            };
        }
        let ptrs = u64::from(self.ptrs_per_block);
        let mut first = u64::from(DIRECT_BLOCKS);
        let mut capacity = ptrs;
        for _ in SINGLE_INDIRECT_SLOT..slot {
            first += capacity;
            capacity *= ptrs;
        }
        SlotSpan {
            depth: (slot - SINGLE_INDIRECT_SLOT) as usize + 1,
            first,
            capacity,
        }
    }

    /// Cut the table tree rooted at `block` (`depth` levels of tables) down to
    /// its first `keep` data blocks, `0 < keep < capacity`. The table's own
    /// dropped pointers are zeroed on disk before the blocks they named are
    /// freed. Returns how many blocks were freed.
    fn trim_tree(&self, block: u32, depth: usize, keep: u64) -> Result<u32, Ext2Error> {
        let mut table = self.read_table(block)?;
        let child_capacity = u64::from(self.ptrs_per_block).pow(depth as u32 - 1);
        let boundary = (keep / child_capacity) as usize; // first not wholly kept
        let remainder = keep % child_capacity; // kept inside that child
        let first_dropped = boundary + usize::from(remainder != 0);

        let dropped: Vec<u32> = table[first_dropped..]
            .iter()
            .copied()
            .filter(|&child| child != 0)
            .collect();
        if !dropped.is_empty() {
            table[first_dropped..].fill(0);
            self.write_table(block, &table)?; // detach before freeing
        }
        let mut freed = 0;
        if remainder != 0 && table[boundary] != 0 {
            freed += self.trim_tree(table[boundary], depth - 1, remainder)?;
        }
        for child in dropped {
            freed += self.free_tree(child, depth - 1)?;
        }
        Ok(freed)
    }

    /// Free a whole detached tree: its data blocks, then its tables. The
    /// recursion is at most [`MAX_DEPTH`] deep and each level scans one table,
    /// and a corrupt pointer ends in [`Ext2::free_block`]'s bounds and
    /// double-free checks. Returns how many blocks were freed.
    fn free_tree(&self, block: u32, depth: usize) -> Result<u32, Ext2Error> {
        let mut freed = 0;
        if depth > 0 {
            for child in self.read_table(block)? {
                if child != 0 {
                    freed += self.free_tree(child, depth - 1)?;
                }
            }
        }
        self.free_block(block)?;
        Ok(freed + 1)
    }

    /// Zero the bytes of the block holding offset `size` from `size` to the end
    /// of that block, so a later grow reads zeros there.
    fn zero_tail(&self, inode: &[u8; INODE_CORE_SIZE], size: u64) -> Result<(), Ext2Error> {
        let inner = (size % u64::from(self.block_size)) as usize;
        if inner == 0 {
            return Ok(()); // the cut is on a block boundary
        }
        let block = self.block_map(inode, self.block_index(size)?)?;
        if block == 0 {
            return Ok(()); // a hole already reads as zeros
        }
        let mut data = [0u8; MAX_BLOCK_SIZE];
        let bytes = self.block_size as usize;
        self.read_block(u64::from(block), &mut data[..bytes])?;
        data[inner..bytes].fill(0);
        self.write_block(u64::from(block), &data[..bytes])
    }

    /// Read a pointer table into host order. Heap-backed: the recursion above
    /// would otherwise stack up a 4 KiB buffer per level on a 32 KiB stack.
    pub(super) fn read_table(&self, block: u32) -> Result<Vec<u32>, Ext2Error> {
        let mut raw = zeroed(u64::from(self.block_size))?;
        self.read_block(u64::from(block), &mut raw)?;
        Ok(raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect())
    }

    /// Write a pointer table back.
    fn write_table(&self, block: u32, table: &[u32]) -> Result<(), Ext2Error> {
        let mut raw = zeroed(u64::from(self.block_size))?;
        for (word, pointer) in raw.as_chunks_mut::<4>().0.iter_mut().zip(table) {
            *word = pointer.to_le_bytes();
        }
        self.write_block(u64::from(block), &raw)
    }
}
