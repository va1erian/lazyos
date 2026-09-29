//! `statfs` for the ramfs: its byte and node caps, reported as blocks and
//! inodes, so `df` on `/tmp` shows how close the scratch area is to `ENOSPC`.

use super::RamFs;
use crate::fs::vfs::StatFs;

/// The Linux `f_type` for ramfs (`RAMFS_MAGIC`).
pub(super) const RAMFS_MAGIC: u32 = 0x8584_58f6;
/// Reported block size: the page size a ramfs file is accounted in.
const BLOCK_SIZE: usize = 4096;

impl RamFs {
    /// Capacity as `statfs` reports it: the caps are the size, the current
    /// usage is what is not free.
    pub(super) fn capacity(&self) -> StatFs {
        let inner = self.inner.lock();
        StatFs {
            magic: RAMFS_MAGIC,
            block_size: BLOCK_SIZE as u32,
            blocks: (inner.max_bytes / BLOCK_SIZE) as u64,
            // Partly used blocks count as used, as `df` expects.
            blocks_free: (inner.max_bytes.saturating_sub(inner.bytes) / BLOCK_SIZE) as u64,
            files: inner.max_nodes as u64,
            files_free: inner.max_nodes.saturating_sub(inner.live) as u64,
            name_max: 255,
        }
    }
}
