//! Fallible buffer allocation for filesystem code: file sizes are
//! caller-influenced and the kernel heap is small, so an oversized request is
//! `NoSpace` for that caller rather than an allocation-failure abort.

use alloc::vec::Vec;

use super::vfs::FsError;

/// A zero-filled `Vec` of exactly `len` bytes, or [`FsError::NoSpace`].
pub fn zeroed(len: u64) -> Result<Vec<u8>, FsError> {
    let len = usize::try_from(len).map_err(|_| FsError::NoSpace)?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| FsError::NoSpace)?;
    data.resize(len, 0);
    Ok(data)
}
