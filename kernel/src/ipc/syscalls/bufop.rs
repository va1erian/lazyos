//! The shared-buffer ops of the `messenger` syscall (`OP_BUFFER_CREATE`,
//! `OP_BUFFER_MAP`, `OP_BUFFER_CLOSE`; `docs/messenger-core-plan.md` 3.4):
//! the backing store every compositor client draws into, the rings audio and
//! network clients fill, the key-state page `inputd` publishes. A buffer is
//! pages the kernel knows nothing more about; it travels in a parcel's
//! `buffers` list and nowhere else.

use super::{errno, MsgArgs, MsgResult};
use crate::ipc::shared;
use crate::task;

/// `OP_BUFFER_CREATE`: `parcel_len` bytes of zeroed pages, mapped into the
/// caller. `value` is the handle, `aux` the address, `bytes` the size.
pub(super) fn op_buffer_create(args: &MsgArgs) -> Result<MsgResult, i64> {
    if args.parcel_len == 0 {
        return Err(errno::EINVAL);
    }
    let handle = shared::create(args.parcel_len).map_err(buffer_errno)?;
    match mapping_of(handle) {
        Ok((va, size)) => Ok(MsgResult {
            value: handle,
            aux: va,
            bytes: size,
            ..MsgResult::default()
        }),
        Err(code) => {
            // The caller never learns the handle, so give the buffer back
            // rather than leaking a mapping it cannot name.
            shared::close(handle).ok();
            Err(code)
        }
    }
}

/// `OP_BUFFER_MAP`: map the buffer `handle` names (a second call returns the
/// recorded mapping). `value` is the address, `aux` the size.
pub(super) fn op_buffer_map(args: &MsgArgs) -> Result<MsgResult, i64> {
    let (va, size) = mapping_of(args.handle)?;
    Ok(MsgResult {
        value: va,
        aux: size,
        ..MsgResult::default()
    })
}

/// `OP_BUFFER_CLOSE`: unmap the caller's mapping and drop its reference.
/// The bound compositor's own screen buffer is refused (`EBUSY`): closing it
/// would leave the display grant pointing at a freed mapping that `present`
/// still blits from, and `unbind` releases that one.
pub(super) fn op_buffer_close(args: &MsgArgs) -> Result<MsgResult, i64> {
    if crate::display::grant_handle_of(task::current()) == Some(args.handle) {
        return Err(errno::EBUSY);
    }
    shared::close(args.handle).map_err(buffer_errno)?;
    Ok(MsgResult::default())
}

/// The caller's mapping of the buffer `handle` names, and the buffer's size.
fn mapping_of(handle: u64) -> Result<(u64, u64), i64> {
    let va = shared::map(handle).map_err(buffer_errno)?;
    let size = shared::info(handle).map_err(buffer_errno)?.size;
    Ok((va, size))
}

/// Shared-buffer errors to errno values, in the vocabulary of the rest of
/// the `messenger` family (`channel_errno`): a missing handle is `ENOENT`, a
/// quota `EAGAIN`, exhausted kernel memory `ENOMEM`, anything malformed
/// `EINVAL`.
fn buffer_errno(error: shared::Error) -> i64 {
    use shared::Error::*;
    match error {
        InvalidHandle | NotFound => errno::ENOENT,
        MissingRight => errno::EACCES,
        Quota | UserQuota => errno::EAGAIN,
        NoFreeHandle | RegistryFull | OutOfMemory => errno::ENOMEM,
        _ => errno::EINVAL,
    }
}
