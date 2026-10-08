//! An endpoint handle that is released when dropped.

/// Access to the raw handle number of an endpoint wrapper, for the byte-level
/// calls in [`super`] (the `std::os::fd::AsRawFd` of the fabric).
pub trait AsRawHandle {
    /// The handle in this task's table.
    fn as_raw_handle(&self) -> u64;
}

impl AsRawHandle for u64 {
    fn as_raw_handle(&self) -> u64 {
        *self
    }
}

/// An endpoint handle this task owns: released ([`super::release`]) when
/// dropped, so this task's reference goes away while any other holder of the
/// same side keeps it open. Use [`OwnedHandle::close`] to close the side for
/// everyone (the peer then sees `EPIPE`).
#[derive(Debug, PartialEq, Eq)]
pub struct OwnedHandle(u64);

impl OwnedHandle {
    /// Take ownership of `handle`.
    pub const fn from_raw(handle: u64) -> OwnedHandle {
        OwnedHandle(handle)
    }

    /// Give the handle up without releasing it.
    pub fn into_raw(self) -> u64 {
        let handle = self.0;
        core::mem::forget(self);
        handle
    }

    /// Close the side for every holder.
    pub fn close(self) -> Result<(), i64> {
        super::close(self.into_raw())
    }
}

impl AsRawHandle for OwnedHandle {
    fn as_raw_handle(&self) -> u64 {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // Nothing to report from a destructor: a handle the kernel already
        // dropped (its task's table was torn down) is simply gone.
        let _ = super::release(self.0);
    }
}
