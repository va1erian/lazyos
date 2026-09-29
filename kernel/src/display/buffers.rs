//! Shared-buffer ops of the display syscall (ops 4-6): the surface backing
//! stores every compositor client creates, maps and releases.

use super::{errno, negative, shared_errno};
use crate::ipc::shared;
use crate::{task, user_ptr};

/// syscall 12 op 4: create a shared buffer, map it, and report its handle and
/// address. Used by every app that wants a surface backing store.
pub(super) fn create_buffer(size: u64, out_ptr: u64) -> u64 {
    if out_ptr == 0 {
        return negative(errno::EFAULT);
    }
    if size == 0 {
        return negative(errno::EINVAL);
    }
    let handle = match shared::create(size, shared::flags::READ | shared::flags::WRITE) {
        Ok(handle) => handle,
        Err(error) => return shared_errno(error),
    };
    let va = match shared::map(handle) {
        Ok(va) => va,
        Err(error) => {
            shared::close(handle).ok();
            return shared_errno(error);
        }
    };
    let words = [handle, va, size];
    if user_ptr::try_copy_words(out_ptr, &words).is_err() {
        // The caller never learns the handle, so give the buffer back rather
        // than leaking a mapping it cannot name.
        shared::close(handle).ok();
        return negative(errno::EFAULT);
    }
    0
}

/// syscall 12 op 5: map a received shared-buffer handle and return its address.
pub(super) fn map_buffer(handle: u64, out_ptr: u64) -> u64 {
    if out_ptr == 0 {
        return negative(errno::EFAULT);
    }
    match shared::map(handle) {
        Ok(va) => {
            if user_ptr::try_write::<u64>(out_ptr, va).is_err() {
                return negative(errno::EFAULT);
            }
            0
        }
        Err(error) => shared_errno(error),
    }
}

/// syscall 12 op 6: close a shared buffer the caller holds.
///
/// Unmaps the caller's mapping and drops its reference (and its quota
/// charge); the compositor's own reference, taken when the buffer was
/// attached, keeps the frames alive until it detaches. The compositor's own
/// screen buffer is refused (`-EBUSY`): `unbind` releases that one.
pub(super) fn close_buffer(handle: u64) -> u64 {
    if super::grant_handle_of(task::current()) == Some(handle) {
        return negative(errno::EBUSY);
    }
    match shared::close(handle) {
        Ok(()) => 0,
        // A closed or never-issued handle is a bad descriptor, not a missing
        // named object.
        Err(shared::Error::InvalidHandle | shared::Error::NotFound) => negative(errno::EBADF),
        Err(error) => shared_errno(error),
    }
}
