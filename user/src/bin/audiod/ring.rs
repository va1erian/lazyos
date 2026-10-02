//! A client's ring as `audiod` sees it: the shared buffer the client handed
//! over with `AttachRing`, mapped into this task.
//!
//! The client keeps writing this memory while the mixer reads it, so it is
//! never referenced: every read is a raw copy of a range checked against the
//! extent the kernel validated. A client scribbling over its ring therefore
//! produces noise in its own stream, never an out-of-bounds access. Dropping
//! the ring closes the buffer, which unmaps it.

use core::ptr;

use libmessenger::BufferDesc;
use user::sys;

pub(super) struct MappedRing {
    handle: u64,
    base: *const u8,
    len: usize,
}

impl MappedRing {
    /// Map the request's buffer `handle`; `desc` is the range the request
    /// declared, which the kernel already checked lies inside the object. On
    /// failure the handle is closed, so nothing leaks either way.
    pub(super) fn map(handle: u64, desc: &BufferDesc) -> Option<MappedRing> {
        let mapped = (|| {
            let offset = usize::try_from(desc.offset).ok()?;
            let len = usize::try_from(desc.len).ok()?;
            let va = sys::display_map_buffer(handle).ok()?;
            let base = (va as usize).checked_add(offset)? as *const u8;
            Some((base, len))
        })();
        match mapped {
            Some((base, len)) => Some(MappedRing { handle, base, len }),
            None => {
                let _ = sys::display_close_buffer(handle);
                None
            }
        }
    }
}

impl audiomix::Ring for MappedRing {
    fn len(&self) -> usize {
        self.len
    }

    fn read(&self, offset: usize, dst: &mut [u8]) {
        let fits = offset
            .checked_add(dst.len())
            .is_some_and(|end| end <= self.len);
        if !fits {
            // The engine never asks for this; refuse it as silence anyway.
            dst.fill(0);
            return;
        }
        // SAFETY: `offset + dst.len() <= len`, the extent of the mapping the
        // kernel validated for this buffer and kept alive until `drop` closes
        // it. Source and destination are different mappings, so they cannot
        // overlap. The client may write the source concurrently; a raw byte
        // copy (no reference into client memory) turns that into noise.
        unsafe { ptr::copy_nonoverlapping(self.base.add(offset), dst.as_mut_ptr(), dst.len()) };
    }
}

impl Drop for MappedRing {
    fn drop(&mut self) {
        let _ = sys::display_close_buffer(self.handle);
    }
}
