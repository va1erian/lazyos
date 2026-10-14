//! One cached page: eight sectors with a valid, a dirty and a file-data
//! bit each.

use alloc::boxed::Box;

use super::SECTOR;

/// Sectors in a page.
pub(super) const PAGE_SECTORS: u64 = 8;
pub(super) const PAGE_BYTES: usize = SECTOR * PAGE_SECTORS as usize;

pub(super) struct Page {
    pub(super) data: Box<[u8; PAGE_BYTES]>,
    /// Bit `i`: sector `i` of the page holds what the device holds (or what
    /// is waiting to be written).
    pub(super) valid: u8,
    /// Bit `i`: sector `i` is waiting to be written.
    pub(super) dirty: u8,
    /// Bit `i`: sector `i` waits with a file's contents (written while the
    /// volume tagged them, see [`Cache::tag_file_data`]).
    pub(super) filedata: u8,
    pub(super) used: u64,
}

impl Page {
    pub(super) fn new() -> Page {
        Page {
            data: Box::new([0; PAGE_BYTES]),
            valid: 0,
            dirty: 0,
            filedata: 0,
            used: 0,
        }
    }
}
