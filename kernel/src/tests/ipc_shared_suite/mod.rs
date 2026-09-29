//! Messenger shared buffers, transfers and fences (issue #67).

use super::*;
use crate::ipc::channels::{self, Error as ChannelError};
use crate::ipc::handles::{self, rights, Error as HandleError, HandleKind};
use crate::ipc::shared::{self, Error as BufferError};
use crate::task::{TaskState, WakeReason};
use alloc::vec;
use libmessenger::{flags, BufferDesc, Encoder, Header, Parcel, VERSION};

pub(crate) fn buffer_reason(error: BufferError) -> String {
    error.message().into()
}

pub(crate) fn channel_reason(error: ChannelError) -> String {
    error.message().into()
}

fn handle_reason(error: HandleError) -> String {
    error.message().into()
}

/// Every shared-buffer test starts from empty registries and a clean kernel
/// task. `channels::reset` runs first so it can release the buffer
/// references held by queued messages before the buffers go away.
pub(crate) fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    channels::reset();
    shared::reset();
    handles::reset_for_task(task::current());
    let me = task::current();
    let _ = task::harness::take_wake_reason(me);
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "kernel task is not runnable after reset: {:?}",
        task::harness::state(me)
    );
    Ok(())
}

/// Open a channel in the calling task, then mirror the receiving endpoint
/// handle into `slot`'s table. Handles are per task and there is no
/// cross-task open call yet, so the harness builds the receiver's half
/// directly; the transfer under test is the buffer handle, not the
/// endpoint.
pub(crate) fn channel_to(slot: usize) -> Result<(u64, u64), String> {
    let (client, server) = channels::create().map_err(channel_reason)?;
    let entry = handles::get(server).map_err(handle_reason)?;
    let caller = task::current();
    task::harness::switch_current(slot);
    let mirror =
        handles::open(HandleKind::Channel, entry.rights, entry.object_id).map_err(handle_reason)?;
    task::harness::switch_current(caller);
    Ok((client, mirror))
}

/// Build a one-way parcel carrying `handles` and `buffers`.
pub(crate) fn parcel_with_transfers(
    method: u32,
    text: &str,
    handles: Vec<u64>,
    buffers: Vec<BufferDesc>,
) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: 0x0bad_cafe,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles,
        buffers,
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// Spawn a fork child with an empty handle table; the caller reaps it.
pub(crate) fn spawn_receiver() -> Result<usize, String> {
    let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    handles::reset_for_task(child);
    Ok(child)
}

/// Finish and reap `child`, returning to the kernel task and resetting the
/// task table.
pub(crate) fn reap(child: usize) -> Result<(), String> {
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(child, 0);
    check!(
        task::reap_child().is_some(),
        "child {child} was not reapable"
    );
    task::harness::reset();
    Ok(())
}

mod basic;
mod transfer;
mod va_reuse;

pub(super) use basic::*;
pub(super) use transfer::*;
pub(super) use va_reuse::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_buffer_create_write_read", buffer_create_write_read),
    ("ipc_buffer_quota", buffer_quota),
    (
        "ipc_buffer_share_only_not_mappable",
        buffer_share_only_not_mappable,
    ),
    (
        "ipc_buffer_handle_transfer_rights",
        buffer_handle_transfer_rights,
    ),
    ("ipc_buffer_fence_submit_wait", buffer_fence_submit_wait),
    ("ipc_buffer_zero_copy_handoff", buffer_zero_copy_handoff),
    (
        "ipc_buffer_va_reused_after_close",
        buffer_va_reused_after_close,
    ),
    (
        "ipc_buffer_va_no_overlap_and_coalesce",
        buffer_va_no_overlap_and_coalesce,
    ),
    ("ipc_buffer_va_soak_bounded", buffer_va_soak_bounded),
];
