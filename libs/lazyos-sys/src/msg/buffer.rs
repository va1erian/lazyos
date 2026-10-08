//! Shared buffers (`docs/messenger-core-plan.md` 2.2): pages the kernel
//! knows nothing more about. `buffer_create` gives the creator a handle and a
//! mapping, the handle travels in a parcel's `buffers` list (never in
//! `handles`), the receiver maps it with `buffer_map` and reads the size the
//! kernel reports, and `buffer_close` unmaps and drops a reference. The
//! pages live while any handle or in-flight message references them.

use super::{op, plain, MsgArgs};

/// Create a shared buffer of `size` bytes (rounded up to whole pages),
/// mapped into this task; returns `(handle, address, size)`.
pub fn buffer_create(size: u64) -> Result<(u64, u64, u64), i64> {
    let args = MsgArgs {
        parcel_len: size,
        ..MsgArgs::default()
    };
    let result = plain(op::BUFFER_CREATE, args)?;
    Ok((result.value, result.aux, result.bytes))
}

/// Map a buffer handle (one received in a message, or one's own) into this
/// task; returns `(address, size)`. Mapping twice returns the same address.
pub fn buffer_map(handle: u64) -> Result<(u64, u64), i64> {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    let result = plain(op::BUFFER_MAP, args)?;
    Ok((result.value, result.aux))
}

/// Close a buffer handle: unmap it here and drop this task's reference (and
/// its quota charge). Another holder's reference keeps the pages alive.
pub fn buffer_close(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    plain(op::BUFFER_CLOSE, args).map(drop)
}
