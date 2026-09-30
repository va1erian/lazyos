//! The Messenger syscall client: op codes, the request/response blocks and the
//! libmessenger parcel helpers (`resolve`, `call`, `recv`, `create_pair`) that
//! the display protocol and the fabric panels share.

use libmessenger::{Encoder, Header, Parcel, VERSION};

use super::{errno, native, SYS_MESSENGER};

/// Messenger op codes, mirroring `user/src/messenger/::op`.
pub mod msg_op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Receive one queued message.
    pub const RECV: u64 = 4;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
    /// Read the versioned fabric snapshot (`FabricStats`).
    pub const STATS: u64 = 8;
    /// Resolve a service name to a new handle.
    pub const RESOLVE: u64 = 14;
    /// Snapshot the name table into the caller's buffer.
    pub const LIST: u64 = 16;
}

/// `MsgArgs::txn_id` marker for registry ops: act on the calling task.
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// The Messenger syscall request block; mirrors the kernel's `MsgArgs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, cancel, close, stats.
    pub handle: u64,
    /// Transaction id, or the registry target task.
    pub txn_id: u64,
    /// Request parcel bytes.
    pub parcel_ptr: u64,
    /// Request parcel length in bytes.
    pub parcel_len: u64,
    /// Reply or receive buffer.
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline; 0 waits forever.
    pub deadline: u64,
    /// Reserved; must be zero.
    pub flags: u64,
}

/// The Messenger syscall response block; mirrors the kernel's `MsgResult`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgResult {
    /// 0 on success, or a negative errno.
    pub status: i64,
    /// New handle (resolve), transaction id (begin/recv).
    pub value: u64,
    /// Second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr`.
    pub bytes: u64,
    /// Delivered transfers for `recv`; zero otherwise.
    pub reserved: [u64; 4],
}
/// One `messenger` syscall; 0 or a negative errno.
pub fn messenger(op: u64, args: u64, result: u64) -> i64 {
    native(SYS_MESSENGER, op, args, result)
}

/// Run one Messenger syscall carrying `MsgArgs`/`MsgResult` blocks; `Ok` when
/// the syscall returned 0, `Err(negative errno)` otherwise.
fn messenger_syscall(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<(), i64> {
    let code = messenger(
        op,
        args as *const MsgArgs as u64,
        result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

/// Create a fresh Messenger channel pair; both handles open in this task.
///
/// The compositor protocol moves one end to `xuid` inside `CreateSurface` and
/// receives input events on the other.
pub fn msg_create_pair() -> Result<(u64, u64), i64> {
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::CREATE_PAIR, &MsgArgs::default(), &mut result)?;
    Ok((result.value, result.aux))
}

/// Resolve `name` into this task's handle table through the kernel registry.
///
/// The request is a `libmessenger` parcel with a single `NAME` string field;
/// the kernel opens the service's published endpoint straight into the
/// caller's table and returns its handle in `MsgResult::value`.
pub fn msg_resolve(name: &str) -> Result<u64, i64> {
    /// Registry interface id: the first eight bytes of `os.lazy.…`.
    const REGISTRY_INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");
    /// Registry method `resolve`.
    const REGISTRY_RESOLVE: u32 = 2;
    /// Registry TLV field id for a name.
    const REGISTRY_FIELD_NAME: u16 = 1;

    let mut body = Encoder::new();
    body.string(REGISTRY_FIELD_NAME, name)
        .map_err(|_| -errno::EINVAL)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: REGISTRY_INTERFACE,
            method: REGISTRY_RESOLVE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -errno::EINVAL)?;
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::RESOLVE, &args, &mut result)?;
    Ok(result.value)
}

/// One synchronous `call`: send `parcel` on `handle`, wait for the reply into
/// `buf` (bounded by `deadline`, an absolute PIT tick; `0` waits forever), and
/// decode it.
pub fn msg_call(
    handle: u64,
    parcel: &Parcel,
    buf: &mut [u8],
    deadline: u64,
) -> Result<Parcel, i64> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -errno::EINVAL)?;
    let args = MsgArgs {
        handle,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::CALL, &args, &mut result)?;
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(-errno::E2BIG);
    }
    Parcel::decode(&buf[..len]).map_err(|_| -errno::EINVAL)
}

/// How many messages are queued on `handle`'s channel, without parking.
///
/// An expired-deadline `recv` on an empty queue parks until the next timer
/// gate (up to one tick), which would make polling a second endpoint every
/// loop pass cost a tick; the channel counters answer instantly instead.
/// Only meaningful for a channel that carries traffic in one direction, like
/// an input session's event channel.
pub fn msg_queued(handle: u64) -> Result<u64, i64> {
    /// `Stats::queued` in the compact 64-byte shape (calls, replies, timeouts,
    /// cancels, drops, queued, queued_bytes, outstanding).
    const QUEUED_WORD: usize = 5;
    let mut words = [0u64; 8];
    let args = MsgArgs {
        handle,
        buf_ptr: words.as_mut_ptr() as u64,
        buf_cap: core::mem::size_of_val(&words) as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::STATS, &args, &mut result)?;
    if result.bytes as usize != core::mem::size_of_val(&words) {
        return Err(-errno::E2BIG);
    }
    Ok(words[QUEUED_WORD])
}

/// Receive one queued message into `buf`; the full [`MsgResult`] carries the
/// reply length (`bytes`) and transaction id (`value`). `deadline` is an
/// absolute PIT tick, or [`EXPIRED_DEADLINE`] for a non-blocking poll.
pub fn msg_recv(handle: u64, buf: &mut [u8], deadline: u64) -> Result<MsgResult, i64> {
    let args = MsgArgs {
        handle,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::RECV, &args, &mut result)?;
    if result.bytes as usize > buf.len() {
        return Err(-errno::E2BIG);
    }
    Ok(result)
}
