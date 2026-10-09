//! The Messenger fabric (syscall 5): the ABI blocks ([`MsgArgs`],
//! [`MsgResult`]), the op table, and safe byte-level wrappers over each op.
//!
//! Everything here moves bytes; parcels are the caller's business. The
//! native runtime builds its `Endpoint`/`Server` API on [`messenger`];
//! [`parcel`] (feature `parcel`) adds the `libmessenger` call and registry
//! helpers the static-musl programs share. The shared buffers messages
//! carry are [`buffer_create`], [`buffer_map`] and [`buffer_close`].

mod abi;
mod buffer;
mod fabric;
mod handle;
#[cfg(feature = "parcel")]
pub mod parcel;
mod pollfd;

pub use abi::*;
pub use buffer::{buffer_close, buffer_create, buffer_map};
#[cfg(feature = "alloc")]
pub use fabric::fabric_stats;
pub use fabric::{fabric_stats_into, FabricStats, TaskUsage, FABRIC_TASKS};
pub use handle::{AsRawHandle, OwnedHandle};
pub use pollfd::endpoint_fd;
#[cfg(all(feature = "std", unix))]
pub use pollfd::Pollable;

use crate::errno::E2BIG;
use crate::nr;

/// One `messenger` syscall: `Ok(())` when it returned 0, the negative errno
/// otherwise. The kernel fills `result`.
///
/// # Safety
///
/// Every pointer `args` carries (`parcel_ptr`/`parcel_len`,
/// `buf_ptr`/`buf_cap`) must be valid for the kernel's read (the parcel or
/// wait set) or write (the buffer, a sender-id block) for the duration of
/// the call, and nothing may hold a live reference into a buffer the kernel
/// writes.
pub unsafe fn messenger(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<(), i64> {
    // SAFETY: both blocks live for the call; the caller vouches for the
    // pointers they carry.
    let code = unsafe {
        crate::raw::syscall3(
            nr::MESSENGER,
            op,
            args as *const MsgArgs as u64,
            result as *mut MsgResult as u64,
        )
    };
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

/// [`messenger`] for an op whose `args` carry no pointer.
fn plain(op: u64, args: MsgArgs) -> Result<MsgResult, i64> {
    let mut result = MsgResult::default();
    // SAFETY: the callers pass only handles, ids and flags; no pointer.
    unsafe { messenger(op, &args, &mut result) }?;
    Ok(result)
}

/// `args` pointing at `request` and `buf`; an empty one is a null pointer,
/// as the kernel expects for an absent parcel or buffer.
fn with_buffers(request: &[u8], buf: &mut [u8]) -> MsgArgs {
    let address = |ptr: u64, len: usize| if len == 0 { 0 } else { ptr };
    MsgArgs {
        parcel_ptr: address(request.as_ptr() as u64, request.len()),
        parcel_len: request.len() as u64,
        buf_ptr: address(buf.as_mut_ptr() as u64, buf.len()),
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    }
}

/// [`messenger`] with `args` built by [`with_buffers`] (plus scalar fields):
/// the kernel reads `request` and writes at most `buf.len()` bytes of `buf`.
/// A reply longer than `buf` is `-E2BIG`.
fn transfer(op: u64, args: MsgArgs, buf_len: usize) -> Result<MsgResult, i64> {
    let mut result = MsgResult::default();
    // SAFETY: `args` points only at slices borrowed (shared for the request,
    // exclusive for the buffer) by the caller for this call.
    unsafe { messenger(op, &args, &mut result) }?;
    if result.bytes > buf_len as u64 {
        return Err(-E2BIG);
    }
    Ok(result)
}

/// Create a fresh channel pair; both handles open in this task.
pub fn create_pair() -> Result<(u64, u64), i64> {
    let result = plain(op::CREATE_PAIR, MsgArgs::default())?;
    Ok((result.value, result.aux))
}

/// Close `handle`, a channel end this task holds (the peer then sees
/// `EPIPE`).
pub fn close(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    plain(op::CLOSE_ENDPOINT, args).map(drop)
}

/// Release this task's handle, closing its side only when no other handle
/// names it. A handle from `resolve` must be released, not closed: every
/// client of a service shares that side.
pub fn release(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        flags: op::CLOSE_RELEASE,
        ..MsgArgs::default()
    };
    plain(op::CLOSE_ENDPOINT, args).map(drop)
}

/// Send the encoded parcel `request` one way on `handle`.
pub fn send(handle: u64, request: &[u8]) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        ..with_buffers(request, &mut [])
    };
    transfer(op::SEND, args, 0).map(drop)
}

/// Call on `handle` with the encoded `request` and wait (until `deadline`,
/// an absolute PIT tick; `0` waits forever) for the reply in `buf`. Returns
/// the reply's length.
pub fn call(handle: u64, request: &[u8], buf: &mut [u8], deadline: u64) -> Result<usize, i64> {
    let len = buf.len();
    let args = MsgArgs {
        handle,
        deadline,
        ..with_buffers(request, buf)
    };
    Ok(transfer(op::CALL, args, len)?.bytes as usize)
}

/// Answer the call `txn` with the encoded `reply`.
pub fn reply(txn: u64, reply: &[u8]) -> Result<(), i64> {
    let args = MsgArgs {
        txn_id: txn,
        ..with_buffers(reply, &mut [])
    };
    transfer(op::REPLY, args, 0).map(drop)
}

/// Receive one queued message on `handle` into `buf`; the result carries its
/// length (`bytes`) and transaction id (`value`, 0 for one-way). `deadline`
/// is an absolute PIT tick, [`EXPIRED_DEADLINE`] to poll, `0` forever.
pub fn recv(handle: u64, buf: &mut [u8], deadline: u64) -> Result<MsgResult, i64> {
    let len = buf.len();
    let args = MsgArgs {
        handle,
        deadline,
        ..with_buffers(&[], buf)
    };
    transfer(op::RECV, args, len)
}

/// [`recv`] that also returns who sent the message, as the kernel stamped it
/// at queue time ([`op::RECV_SENDER_ID`]; needs no capability).
pub fn recv_from(handle: u64, buf: &mut [u8], deadline: u64) -> Result<(MsgResult, SenderId), i64> {
    let mut id = [0u8; SenderId::SIZE];
    let len = buf.len();
    let args = MsgArgs {
        handle,
        parcel_ptr: id.as_mut_ptr() as u64,
        parcel_len: id.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: len as u64,
        deadline,
        flags: op::RECV_SENDER_ID,
        ..MsgArgs::default()
    };
    // `transfer`'s contract holds for the sender block too: the kernel
    // writes exactly `SenderId::SIZE` bytes into `id`, which nothing borrows.
    let result = transfer(op::RECV, args, len)?;
    let sender = SenderId::from_bytes(&id).ok_or(-crate::errno::EINVAL)?;
    Ok((result, sender))
}

/// A registry op (`REGISTER`, `RESOLVE`, `UNREGISTER`, `CONNECT`) with the
/// encoded registry parcel `request`, on behalf of `target`
/// ([`REGISTRY_TARGET_SELF`] for this task).
pub fn registry(op: u64, target: u64, request: &[u8]) -> Result<MsgResult, i64> {
    let args = MsgArgs {
        txn_id: target,
        ..with_buffers(request, &mut [])
    };
    transfer(op, args, 0)
}

/// Snapshot the kernel name table (an encoded registry `List` reply) into
/// `buf`; returns its length.
pub fn list(buf: &mut [u8]) -> Result<usize, i64> {
    let len = buf.len();
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        ..with_buffers(&[], buf)
    };
    Ok(transfer(op::LIST, args, len)?.bytes as usize)
}

/// `handle`'s channel counters (`0`: every live channel).
pub fn stats(handle: u64) -> Result<Stats, i64> {
    counters(op::STATS, handle)
}

/// The global message totals.
pub fn totals() -> Result<Stats, i64> {
    counters(op::TOTALS, 0)
}

fn counters(op: u64, handle: u64) -> Result<Stats, i64> {
    let mut stats = Stats::default();
    let args = MsgArgs {
        handle,
        buf_ptr: &mut stats as *mut Stats as u64,
        buf_cap: Stats::SIZE as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    // SAFETY: the buffer is `stats`, a `repr(C)` block of `Stats::SIZE`
    // bytes of plain words, exclusively borrowed for the call.
    unsafe { messenger(op, &args, &mut result) }?;
    if result.bytes != Stats::SIZE as u64 {
        return Err(-E2BIG);
    }
    Ok(stats)
}

/// The `stats` op into a buffer of any size (the versioned fabric snapshot
/// for handle 0 and a [`FabricStats::SIZE`] buffer); the bytes written.
pub fn stats_bytes(handle: u64, buf: &mut [u8]) -> Result<usize, i64> {
    let len = buf.len();
    let args = MsgArgs {
        handle,
        ..with_buffers(&[], buf)
    };
    Ok(transfer(op::STATS, args, len)?.bytes as usize)
}

/// How many messages are queued on `handle`'s channel, without parking: an
/// expired-deadline `recv` on an empty queue parks until the next tick, the
/// counters answer at once. Meaningful for a one-direction channel.
pub fn queued(handle: u64) -> Result<u64, i64> {
    Ok(stats(handle)?.queued)
}

/// Park until one of `words` (endpoint handles, or call transaction ids
/// marked with [`WAIT_ITEM_CALL`]) is ready, a doorbell in `flags` rings, or
/// the absolute PIT `deadline` passes (`0`: forever). Returns the ready mask
/// (bit `i` for `words[i]`, the `*_READY` bits for doorbells), or
/// `-ETIMEDOUT`. Nothing is received. More than [`WAIT_MAX_ENDPOINTS`]
/// words is refused by the kernel (`-EINVAL`).
pub fn wait_any(words: &[u64], flags: u64, deadline: u64) -> Result<u64, i64> {
    let args = MsgArgs {
        parcel_ptr: words.as_ptr() as u64,
        // An oversized set reaches the kernel as its real length, which it
        // refuses before reading, rather than being silently cut short.
        parcel_len: words.len() as u64,
        deadline,
        flags,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    // SAFETY: the kernel reads `words.len()` words from `words`, borrowed
    // for the call; nothing is written but `result`.
    unsafe { messenger(op::WAIT, &args, &mut result) }?;
    Ok(result.value)
}

/// [`wait_any`] with an absolute [`crate::time::monotonic_ns`] deadline, so
/// a timer is not rounded to the 10 ms tick. `words` may be empty when a
/// descriptor ([`WAIT_FD`]) is watched.
pub fn wait_any_ns(words: &[u64], flags: u64, deadline_ns: u64) -> Result<u64, i64> {
    wait_any(words, flags | WAIT_DEADLINE_NS, deadline_ns.max(1))
}

/// [`WAIT_FD`] for descriptor `fd`, to OR into a wait's flags.
pub const fn wait_fd(fd: u32) -> u64 {
    WAIT_FD | (fd as u64) << WAIT_FD_SHIFT
}
