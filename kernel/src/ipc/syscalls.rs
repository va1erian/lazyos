//! Native Messenger syscall surface (issue #69).
//!
//! `docs/messenger.md` section 14 sketches a family of native syscalls
//! (`msg_call`, `msg_reply`, ...) over the kernel channel fabric. This module
//! implements the first coherent slice of that surface behind a single native
//! syscall (`messenger = 5`): one gate with an op code keeps the register ABI
//! tiny and lets the surface grow without renumbering anything.
//!
//! ```text
//!   rax = 5              rdi = op
//!   rsi -> MsgArgs       rdx -> MsgResult
//! ```
//!
//! Every pointer is validated against the calling task's address space before a
//! single byte is copied. [`access_range`] walks the active page tables (via the
//! kernel's physical map, which gives a safe alias for the actual copy) and
//! returns `-EFAULT` for an unmapped, supervisor-only, or read-only-for-write
//! range; it never dereferences an untrusted pointer directly, so a malformed
//! call cannot fault the kernel path. Not-present pages inside an `Anon`/`Heap`
//! VMA are materialized first, exactly as a real fault would: `sbrk` heaps are
//! demand-zero, and a `write` into a fresh `Vec` must not be refused merely
//! because the user atomically grew its break.
//!
//! Policy: parcel-bearing operations derive `(interface_id, method)` from the
//! [`libmessenger`] header and pass through [`crate::ipc::authorize`] before the
//! channel is touched, so a denied call never enqueues anything. Denials are
//! recorded by `authorize` (audit ring, with the reason code); this module maps
//! the verdict to `-EACCES`.
//!
//! The registry ops (issue #89) are the same shape: their request parcels count
//! the registry interface's methods (`REGISTER`/`RESOLVE`/`UNREGISTER`/`LIST`)
//! as [`crate::ipc::registry::method`] and are authorized before the name table
//! is touched. The `endpoint` handle register publishes is read from the
//! request body, not from `MsgArgs`, so a privileged proxy (`messengerd`) can
//! forward a client's number: `MsgArgs::txn_id` carries the *target task slot*
//! ([`REGISTRY_TARGET_SELF`] means "the caller") and any other slot requires
//! `CAP_IPC_CONTROL`.
//!
//! The bootstrap channel lives in [`bootstrap`]: `kernel_main` creates one
//! endpoint pair at boot, keeps the service end kernel-side (the `messengerd`
//! stub), and the first userspace task claims the client end with
//! `OP_BOOTSTRAP`.

use alloc::vec::Vec;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::PhysAddr;

use libmessenger::{Header, Parcel, ParcelView, MAX_PARCEL_BYTES, VERSION};

use crate::ipc::handles::HandleKind;
use crate::ipc::{channels, credentials, handles, registry, topics};
use crate::mem;
use crate::mem::pte;
use crate::task;

mod abi;
mod aclop;
pub mod bootstrap;
mod regops;
mod usermem;

pub use abi::*;
use regops::{op_registry, registry_target};
pub(crate) use usermem::*;

/// The `messenger` syscall entry point: validate, execute, report.
///
/// Returns 0 on success or `-errno`; every output also travels in the caller's
/// `MsgResult` block, whose writability is established before the operation
/// runs so a successful op can always report its results.
pub fn dispatch(op: u64, args_ptr: u64, result_ptr: u64) -> u64 {
    if access_range(result_ptr, RESULT_SIZE, true).is_err() {
        // Nothing can be reported through the block; the return register still
        // carries the failure.
        return negative(errno::EFAULT);
    }
    let args = match copy_in_array::<ARGS_SIZE>(args_ptr)
        .and_then(|bytes| MsgArgs::from_bytes(&bytes).ok_or(errno::EINVAL))
    {
        Ok(args) => args,
        Err(code) => return report(result_ptr, code),
    };
    // Flags are reserved, except the ones `close_endpoint` and `wait` know.
    let allowed = (op == OP_CLOSE_ENDPOINT && args.flags == CLOSE_RELEASE)
        || (op == OP_WAIT && channels::wait_flags_known(args.flags));
    if args.flags != 0 && !allowed {
        return report(result_ptr, errno::EINVAL);
    }
    match handle_op(op, &args) {
        Ok(result) => {
            if copy_out(result_ptr, &result.to_bytes()).is_err() {
                return negative(errno::EFAULT);
            }
            0
        }
        Err(code) => report(result_ptr, code),
    }
}

/// Write an error status into the (already validated) result block and return
/// the same error in `rax`.
fn report(result_ptr: u64, code: i64) -> u64 {
    let failed = MsgResult {
        status: -code,
        ..MsgResult::default()
    };
    let _ = copy_out(result_ptr, &failed.to_bytes());
    negative(code)
}

/// Two's-complement `-errno` in the syscall return register.
fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Execute one op. Errors are positive errno values; the caller negates them.
fn handle_op(op: u64, args: &MsgArgs) -> Result<MsgResult, i64> {
    match op {
        OP_CALL => op_call(args),
        OP_REPLY => op_reply(args),
        OP_SEND => op_send(args),
        OP_RECV => op_recv(args),
        OP_CANCEL => op_cancel(args),
        OP_CLOSE_ENDPOINT => op_close(args),
        OP_CREATE_PAIR => op_create_pair(args),
        OP_STATS => op_stats(args),
        OP_BOOTSTRAP => op_bootstrap(args),
        OP_CALL_BEGIN => op_call_begin(args),
        OP_CALL_AWAIT => op_call_await(args),
        OP_TOTALS => op_totals(args),
        OP_REGISTER => op_registry(args, crate::ipc::registry::method::REGISTER),
        OP_RESOLVE => op_registry(args, crate::ipc::registry::method::RESOLVE),
        OP_UNREGISTER => op_registry(args, crate::ipc::registry::method::UNREGISTER),
        OP_LIST => op_registry(args, crate::ipc::registry::method::LIST),
        OP_AUTHORIZE_TOPIC => op_authorize_topic(args),
        OP_ACL_LOAD => aclop::op_acl_load(args),
        OP_WAIT => op_wait(args),
        _ => Err(errno::EINVAL),
    }
}

/// Read a request parcel, bounded by the wire limit before any copying. This
/// is the parcel's one copy into the kernel: the same buffer is validated in
/// place, queued and handed to the receiver (P6.3).
fn read_parcel(args: &MsgArgs) -> Result<Vec<u8>, i64> {
    if args.parcel_len == 0 || args.parcel_len > MAX_PARCEL_BYTES as u64 {
        return Err(errno::E2BIG);
    }
    copy_in(args.parcel_ptr, args.parcel_len as usize)
}

/// Validate a parcel at the kernel boundary, in place; malformed bytes are
/// `EINVAL`.
fn decode_parcel(bytes: &[u8]) -> Result<ParcelView<'_>, i64> {
    ParcelView::parse(bytes).map_err(|_| errno::EINVAL)
}

/// Enforce the ACL for an outbound call or one-way send: the header is the
/// only source of `(interface_id, method)`, and `authorize` audits the verdict.
fn authorize_parcel(parcel: &ParcelView<'_>) -> Result<(), i64> {
    let decision = crate::ipc::authorize(
        task::current(),
        parcel.header.interface_id,
        parcel.header.method,
        parcel.header.txn_id,
    );
    if decision.denied() {
        // `authorize` already recorded the denial and its reason code; the
        // friendly text is in the decision for `messengerctl why <txn>`.
        return Err(errno::EACCES);
    }
    Ok(())
}

/// Copy a reply into the caller's buffer, refusing one that does not fit.
/// The transaction has already completed at this point, so an over-large
/// reply is dropped; a streaming/shared-buffer path is a follow-up.
fn write_reply(args: &MsgArgs, reply: &[u8]) -> Result<MsgResult, i64> {
    if reply.len() > args.buf_cap as usize {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, reply)?;
    Ok(MsgResult {
        bytes: reply.len() as u64,
        ..MsgResult::default()
    })
}

fn op_call(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    let method = parcel.header.method;
    let reply = channels::call_owned(args.handle, method, bytes, args.deadline_ticks())
        .map_err(channel_errno)?;
    write_reply(args, &reply)
}

fn op_call_begin(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    let method = parcel.header.method;
    let txn_id = channels::begin_call_owned(args.handle, method, bytes, args.deadline_ticks())
        .map_err(channel_errno)?;
    Ok(MsgResult {
        value: txn_id,
        ..MsgResult::default()
    })
}

fn op_call_await(args: &MsgArgs) -> Result<MsgResult, i64> {
    let reply = channels::await_reply(args.txn_id).map_err(channel_errno)?;
    write_reply(args, &reply)
}

fn op_reply(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    channels::reply_owned(args.txn_id, bytes).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_send(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    channels::send_owned(args.handle, bytes).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_recv(args: &MsgArgs) -> Result<MsgResult, i64> {
    let message = channels::recv(args.handle, args.deadline_ticks()).map_err(channel_errno)?;
    if message.bytes.len() > args.buf_cap as usize {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, &message.bytes)?;
    Ok(MsgResult {
        // The kernel transaction id (0 for one-way messages), not the
        // sender's header field, is what `reply` expects.
        value: message.txn.unwrap_or(0),
        aux: message.sender as u64,
        bytes: message.bytes.len() as u64,
        // The delivered transfers, for protocols that receive a handle or a
        // buffer (issue #113's display protocol attaches both). The counts
        // distinguish "none" from a legitimately zero handle number: handle
        // tables hand out 0 as their first slot.
        reserved: [
            message.handles.first().copied().unwrap_or(0),
            message.handles.len() as u64,
            message
                .buffers
                .first()
                .map(|buffer| buffer.handle)
                .unwrap_or(0),
            message.buffers.len() as u64,
        ],
        ..MsgResult::default()
    })
}

fn op_wait(args: &MsgArgs) -> Result<MsgResult, i64> {
    let count = args.parcel_len as usize;
    if count > channels::MAX_WAIT_ENDPOINTS {
        return Err(errno::EINVAL);
    }
    let bytes = copy_in(args.parcel_ptr, count * 8)?;
    let mut handles = [0u64; channels::MAX_WAIT_ENDPOINTS];
    for (handle, word) in handles.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *handle = u64::from_le_bytes(*word);
    }
    let ready = channels::wait_any(&handles[..count], args.flags, args.deadline_ticks())
        .map_err(channel_errno)?;
    Ok(MsgResult {
        value: ready,
        ..MsgResult::default()
    })
}

fn op_cancel(args: &MsgArgs) -> Result<MsgResult, i64> {
    channels::cancel(args.txn_id).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_close(args: &MsgArgs) -> Result<MsgResult, i64> {
    if args.flags == CLOSE_RELEASE {
        channels::release_endpoint(args.handle).map_err(channel_errno)?;
    } else {
        channels::close_endpoint(args.handle).map_err(channel_errno)?;
    }
    Ok(MsgResult::default())
}

fn op_create_pair(_args: &MsgArgs) -> Result<MsgResult, i64> {
    let (first, second) = channels::create().map_err(channel_errno)?;
    Ok(MsgResult {
        value: first,
        aux: second,
        ..MsgResult::default()
    })
}

/// The stats op: version 2 (`FabricStats`) when the caller offers a big enough
/// buffer, version 1 (`MsgStats`) otherwise. `handle != 0` always selects the
/// per-channel version 1 counters, because the snapshot is global.
fn op_stats(args: &MsgArgs) -> Result<MsgResult, i64> {
    use crate::ipc::stats::FabricStats;

    if args.handle == 0 && args.buf_cap as usize >= FabricStats::SIZE {
        let encoded = crate::ipc::stats::snapshot().to_bytes();
        copy_out(args.buf_ptr, &encoded)?;
        return Ok(MsgResult {
            bytes: encoded.len() as u64,
            ..MsgResult::default()
        });
    }
    let stats = if args.handle == 0 {
        channels::stats()
    } else {
        channels::channel_stats(args.handle).map_err(channel_errno)?
    };
    write_msg_stats(args, stats)
}

/// The totals op: the aggregate [`MsgStats`] counters, always 64 bytes.
fn op_totals(args: &MsgArgs) -> Result<MsgResult, i64> {
    write_msg_stats(args, channels::stats())
}

/// Copy a version 1 [`MsgStats`] block into the caller's buffer.
fn write_msg_stats(args: &MsgArgs, stats: channels::Stats) -> Result<MsgResult, i64> {
    let encoded = MsgStats::from(stats).to_bytes();
    if (args.buf_cap as usize) < encoded.len() {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, &encoded)?;
    Ok(MsgResult {
        bytes: encoded.len() as u64,
        ..MsgResult::default()
    })
}

fn op_bootstrap(_args: &MsgArgs) -> Result<MsgResult, i64> {
    let handle = bootstrap::claim_client()?;
    Ok(MsgResult {
        value: handle,
        ..MsgResult::default()
    })
}

// ---------------------------------------------------------------------------
// Topic ACL op (issue #92)
// ---------------------------------------------------------------------------

/// `OP_AUTHORIZE_TOPIC`: evaluate the kernel policy for every segment of a
/// topic or filter on behalf of the task named by `args.txn_id` (self, or the
/// privileged `messengerd` proxy path). Policy stays entirely kernel-side;
/// the userspace broker only asks the question and maps `-EACCES` to its
/// friendly denial reply.
fn op_authorize_topic(args: &MsgArgs) -> Result<MsgResult, i64> {
    let target = registry_target(args.txn_id)?;
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    let request = topics::decode_request(parcel.body()).map_err(|_| errno::EINVAL)?;
    let segments =
        topics::authorize(target, request.mode, &request.name, request.txn).map_err(|error| {
            match error {
                topics::Error::Denied => errno::EACCES,
                topics::Error::BadName | topics::Error::BadMode => errno::EINVAL,
            }
        })?;
    Ok(MsgResult {
        value: segments as u64,
        ..MsgResult::default()
    })
}

/// Channel errors to errno values.
fn channel_errno(error: channels::Error) -> i64 {
    use channels::Error::*;
    match error {
        InvalidHandle | WrongKind | NoTransaction => errno::ENOENT,
        MissingRight => errno::EACCES,
        NoFreeHandle | RegistryFull => errno::ENOMEM,
        BadTask | BadParcel => errno::EINVAL,
        QueueFull | TooManyOutstanding | Quota | QuotaExceeded => errno::EAGAIN,
        Deadlock => errno::EDEADLK,
        NotCaller => errno::EPERM,
        TimedOut => errno::ETIMEDOUT,
        Canceled => errno::ECANCELED,
        PeerDied => errno::EPIPE,
        BadTransfer | UnsupportedTransfer => errno::EINVAL,
    }
}

/// Handle-table errors to errno values.
fn handles_errno(error: handles::Error) -> i64 {
    use handles::Error::*;
    match error {
        NoFreeHandle | Quota => errno::ENOMEM,
        InvalidHandle => errno::ENOENT,
        MissingRight => errno::EACCES,
        BadTask => errno::EINVAL,
    }
}
