//! The `ENDPOINT_FD` op through the native gate, and its refusals.

use super::*;
use crate::ipc::endpointfd::OpenError;
use crate::ipc::handles::{rights, HandleKind};
use crate::ipc::syscalls::{
    errno, MsgArgs, MsgResult, ENDPOINT_FD_CLOEXEC, OP_ENDPOINT_FD, RESULT_SIZE,
};

/// A user page for the op's two blocks.
const SPACE: u64 = 0x0040_0000;
const ARGS: u64 = SPACE;
const RESULT: u64 = SPACE + 0x100;

/// Issue `ENDPOINT_FD` for `handle` with `flags`, as a user task would:
/// the blocks in a scratch address space installed as CR3.
fn op(handle: u64, flags: u64) -> Result<(u64, MsgResult), String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + 4096).map_err(to_string)?;
    mem::switch_to(table);
    let args = MsgArgs {
        handle,
        flags,
        ..MsgArgs::default()
    }
    .to_bytes();
    // SAFETY: the scratch page is mapped writable while installed.
    unsafe { core::ptr::copy_nonoverlapping(args.as_ptr(), ARGS as *mut u8, args.len()) };
    let code = process::dispatch_for_test(5, OP_ENDPOINT_FD, ARGS, RESULT);
    let mut block = [0u8; RESULT_SIZE];
    // SAFETY: as above; the kernel wrote the result block there.
    unsafe { core::ptr::copy_nonoverlapping(RESULT as *const u8, block.as_mut_ptr(), RESULT_SIZE) };
    mem::switch_to(kernel);
    mem::free_user_table(table);
    let result = MsgResult::from_bytes(&block).ok_or("malformed result block")?;
    Ok((code, result))
}

/// Two's-complement `-errno` as the gate returns it.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// The op opens a descriptor in the caller's table (close-on-exec on
/// request) that watches the handle; unknown flags are refused.
pub fn op_abi() -> Result<(), String> {
    fresh()?;
    let (a, b) = pair()?;
    let (code, result) = op(b, 0)?;
    check!(code == 0, "ENDPOINT_FD returned {code:#x}");
    let fd = result.value;
    check!(
        !task::fd_cloexec(fd as usize),
        "opened close-on-exec unasked"
    );
    check!(revents(fd, POLLIN) == 0, "an empty endpoint reads ready");
    send(a, &message()?)?;
    check!(
        revents(fd, POLLIN) == POLLIN,
        "a queued message is not POLLIN"
    );
    let (code, result) = op(b, ENDPOINT_FD_CLOEXEC)?;
    check!(code == 0, "ENDPOINT_FD (cloexec) returned {code:#x}");
    check!(
        task::fd_cloexec(result.value as usize),
        "ENDPOINT_FD_CLOEXEC did not set FD_CLOEXEC"
    );
    let (code, _) = op(b, 2)?;
    check!(
        code == failed(errno::EINVAL),
        "an unknown flag gave {code:#x}"
    );
    close(fd)?;
    close(result.value)?;
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    channels::close_endpoint(b).map_err(|e| format!("{e:?}"))?;
    nothing_left("op_abi")
}

/// Only a channel handle with `CALL` the caller holds can be watched.
pub fn refuses_bad_handles() -> Result<(), String> {
    fresh()?;
    let (code, _) = op(4242, 0)?;
    check!(
        code == failed(errno::ENOENT),
        "an unknown handle gave {code:#x}"
    );
    check!(
        endpointfd::open_fd(4242, false) == Err(OpenError::NoHandle),
        "an unknown handle was watched"
    );
    let (a, _b) = pair()?;
    let object = handles::get(a).map_err(|e| format!("{e:?}"))?.object_id;
    let object_handle =
        handles::open(HandleKind::Object, rights::ALL, 7).map_err(|e| format!("{e:?}"))?;
    check!(
        endpointfd::open_fd(object_handle, false) == Err(OpenError::WrongKind),
        "a non-channel handle was watched"
    );
    let (code, _) = op(object_handle, 0)?;
    check!(
        code == failed(errno::EINVAL),
        "a non-channel handle gave {code:#x}"
    );
    let blind = handles::open(HandleKind::Channel, rights::MONITOR, object)
        .map_err(|e| format!("{e:?}"))?;
    check!(
        endpointfd::open_fd(blind, false) == Err(OpenError::MissingRight),
        "a handle without CALL was watched"
    );
    let (code, _) = op(blind, 0)?;
    check!(
        code == failed(errno::EACCES),
        "a handle without CALL gave {code:#x}"
    );
    channels::reset();
    handles::reset_for_task(task::KERNEL_TASK);
    nothing_left("refuses_bad_handles")
}
