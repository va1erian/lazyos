//! Fallible buffer allocation for filesystem code: file sizes are
//! caller-influenced and the kernel heap is small, so an oversized request is
//! `NoSpace` for that caller rather than an allocation-failure abort.

use alloc::vec::Vec;

use super::vfs::FsError;

/// Bytes zeroed between two interrupt-window poll points (`arch::irq_window`):
/// a megabyte memset, plus a hypervisor fault per fresh heap page, is too long
/// to run with interrupts off in one go.
const ZERO_PIECE: usize = 64 * 1024;

/// A zero-filled `Vec` of exactly `len` bytes, or [`FsError::NoSpace`].
pub fn zeroed(len: u64) -> Result<Vec<u8>, FsError> {
    let len = usize::try_from(len).map_err(|_| FsError::NoSpace)?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| FsError::NoSpace)?;
    while data.len() < len {
        crate::arch::irq_window::poll_point();
        data.resize((data.len() + ZERO_PIECE).min(len), 0);
    }
    Ok(data)
}
