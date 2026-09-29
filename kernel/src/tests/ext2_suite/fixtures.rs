//! Shared checks for the ext2 truncate/large-file/state suites: reading raw
//! superblock state and verifying that a volume's bitmaps and counters agree.

use super::*;

/// `s_state` as stored on the disk right now (bit 0 set means clean).
pub(super) fn raw_state(disk: &FakeDisk) -> u16 {
    let data = disk.data.lock();
    u16::from_le_bytes([data[SUPER + 0x3A], data[SUPER + 0x3B]])
}

fn raw32(disk: &FakeDisk, offset: usize) -> u32 {
    let data = disk.data.lock();
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// How many of the first `bits` bits of the bitmap block are clear.
fn clear_bits(disk: &FakeDisk, block: usize, block_size: usize, bits: usize) -> u32 {
    let data = disk.data.lock();
    let bitmap = &data[block * block_size..(block + 1) * block_size];
    (0..bits)
        .filter(|&bit| bitmap[bit / 8] & (1 << (bit % 8)) == 0)
        .count() as u32
}

/// Fsck in miniature for the one-group fixture: the superblock counters, the
/// group descriptor counters and the bitmaps must all say the same thing.
/// Every clean operation (including a completed truncate or unlink) has to
/// leave this true; a leaked block or inode shows up as a mismatch.
pub(super) fn check_volume(disk: &FakeDisk, total_blocks: u32) -> Result<(), String> {
    let block_size = 1024usize << raw32(disk, SUPER + 0x18);
    let first_data = raw32(disk, SUPER + 0x14) as usize;
    let inodes = raw32(disk, SUPER + 0x00) as usize;
    let gd = (first_data + 1) * block_size;
    let block_bitmap = raw32(disk, gd) as usize;
    let inode_bitmap = raw32(disk, gd + 4) as usize;
    let gd_free_blocks = {
        let data = disk.data.lock();
        u32::from(u16::from_le_bytes([data[gd + 0x0C], data[gd + 0x0D]]))
    };
    let free_by_bitmap = clear_bits(
        disk,
        block_bitmap,
        block_size,
        total_blocks as usize - first_data,
    );
    let free_inodes_by_bitmap = clear_bits(disk, inode_bitmap, block_size, inodes);
    let sb_free_blocks = raw32(disk, SUPER + 0x0C);
    let sb_free_inodes = raw32(disk, SUPER + 0x10);
    check!(
        free_by_bitmap == sb_free_blocks && free_by_bitmap == gd_free_blocks,
        "block counts disagree: bitmap {free_by_bitmap}, superblock {sb_free_blocks}, \
         group {gd_free_blocks}"
    );
    check!(
        free_inodes_by_bitmap == sb_free_inodes,
        "inode counts disagree: bitmap {free_inodes_by_bitmap}, superblock {sb_free_inodes}"
    );
    Ok(())
}

/// `(free blocks, free inodes)` counted from the bitmaps alone, straight from
/// the raw image. After a power cut the superblock and group counters can lag
/// the bitmaps (nothing recomputes them at mount), so a leak test must judge by
/// what the bitmaps say is allocated, not by the counters.
pub(super) fn bitmap_free(disk: &FakeDisk, total_blocks: u32) -> (u32, u32) {
    let block_size = 1024usize << raw32(disk, SUPER + 0x18);
    let first_data = raw32(disk, SUPER + 0x14) as usize;
    let inodes = raw32(disk, SUPER + 0x00) as usize;
    let gd = (first_data + 1) * block_size;
    let blocks = clear_bits(
        disk,
        raw32(disk, gd) as usize,
        block_size,
        total_blocks as usize - first_data,
    );
    let inodes = clear_bits(disk, raw32(disk, gd + 4) as usize, block_size, inodes);
    (blocks, inodes)
}

/// Whether `block` is marked used in the (single) group's block bitmap, read
/// straight from the raw image: the crash tests use it to prove that nothing
/// an inode still points at has been handed back to the allocator.
pub(super) fn block_allocated(disk: &FakeDisk, block: u32) -> bool {
    let block_size = 1024usize << raw32(disk, SUPER + 0x18);
    let first_data = raw32(disk, SUPER + 0x14) as usize;
    let bitmap = raw32(disk, (first_data + 1) * block_size) as usize;
    let bit = block as usize - first_data;
    let data = disk.data.lock();
    data[bitmap * block_size + bit / 8] & (1 << (bit % 8)) != 0
}

/// Mount a fresh view of `disk` in its own VFS, as a reboot would.
pub(super) fn remount_disk(disk: &'static FakeDisk) -> Result<(Arc<Ext2>, Vfs), String> {
    let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone()).map_err(fs_error)?;
    Ok((fs, vfs))
}

/// A deterministic pseudo-random stream (xorshift) so soak runs are
/// reproducible without a random source in the kernel.
pub(super) struct Rng(pub(super) u32);

impl Rng {
    pub(super) fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// A value in `0..bound`.
    pub(super) fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }
}

/// A recognisable byte pattern for offset `at` of a file seeded with `seed`.
pub(super) fn pattern(seed: u32, at: usize) -> u8 {
    (at as u32).wrapping_mul(2654435761).wrapping_add(seed) as u8 | 1
}

/// `len` bytes of [`pattern`]; never zero, so a stray hole cannot pass for it.
pub(super) fn pattern_bytes(seed: u32, len: usize) -> Vec<u8> {
    (0..len).map(|at| pattern(seed, at)).collect()
}
