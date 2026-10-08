//! The `messenger` buffer ops (`OP_BUFFER_CREATE`/`MAP`/`CLOSE`) through the
//! native gate, for every suite that wants a buffer the way a user program
//! makes one. The gate validates its two blocks against the active page
//! tables, so the ops run inside [`in_space`]: a scratch user address space
//! that lives for one closure. Everything a closure creates it must also
//! close, because a buffer's mapping is recorded against that space.

use super::*;
use crate::ipc::syscalls::{MsgArgs, MsgResult, OP_BUFFER_CLOSE, OP_BUFFER_CREATE, OP_BUFFER_MAP};

/// A user page for the two blocks.
const SPACE: u64 = 0x0040_0000;
const ARGS: u64 = SPACE;
const RESULT: u64 = SPACE + 0x100;

/// Run `f` with a fresh user address space installed as CR3 and the block
/// page mapped, then tear the space down.
pub(crate) fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + 4096).map_err(to_string)?;
    mem::switch_to(table);
    let outcome = f();
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// One buffer op through the gate: the return code and the result block.
pub(crate) fn op(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
    let bytes = args.to_bytes();
    // SAFETY: the block page is mapped writable while the space is installed.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ARGS as *mut u8, bytes.len()) };
    let code = process::dispatch_for_test(5, op, ARGS, RESULT);
    let mut block = [0u8; 64];
    // SAFETY: as above; the kernel wrote the result block there.
    unsafe { core::ptr::copy_nonoverlapping(RESULT as *const u8, block.as_mut_ptr(), 64) };
    let result = MsgResult::from_bytes(&block).expect("the kernel wrote a malformed result block");
    (code, result)
}

/// `OP_BUFFER_CREATE` with the result block at a kernel address: the gate
/// must refuse it before anything is created.
pub(crate) fn create_into_kernel(size: u64, result_ptr: u64) -> u64 {
    let bytes = MsgArgs {
        parcel_len: size,
        ..MsgArgs::default()
    }
    .to_bytes();
    // SAFETY: the block page is mapped writable while the space is installed.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ARGS as *mut u8, bytes.len()) };
    process::dispatch_for_test(5, OP_BUFFER_CREATE, ARGS, result_ptr)
}

/// `OP_BUFFER_CREATE`: `(code, handle, address, size)`.
pub(crate) fn create_raw(size: u64) -> (u64, u64, u64, u64) {
    let args = MsgArgs {
        parcel_len: size,
        ..MsgArgs::default()
    };
    let (code, result) = op(OP_BUFFER_CREATE, &args);
    (code, result.value, result.aux, result.bytes)
}

/// A buffer of `size` bytes: `(handle, address)`, or the failing code.
pub(crate) fn create(size: u64) -> Result<(u64, u64), String> {
    let (code, handle, va, _) = create_raw(size);
    check!(code == 0, "buffer_create({size}) -> {code:#x}");
    Ok((handle, va))
}

/// `OP_BUFFER_MAP`: `(code, address, size)`.
pub(crate) fn map(handle: u64) -> (u64, u64, u64) {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    let (code, result) = op(OP_BUFFER_MAP, &args);
    (code, result.value, result.aux)
}

/// `OP_BUFFER_CLOSE`: the return code.
pub(crate) fn close(handle: u64) -> u64 {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    op(OP_BUFFER_CLOSE, &args).0
}

/// Two's-complement `-errno` as the gate returns it.
pub(crate) fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}
