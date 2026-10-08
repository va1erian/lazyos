//! The `messenger` syscall ABI: errnos, op numbers and the argument/result blocks.

use super::*;

/// Negative errno-style values returned in `rax` (x86_64 Linux numbering, so
/// userspace error handling is the same as the Linux ABI shim).
pub mod errno {
    /// The caller may not perform this operation.
    pub const EPERM: i64 = 1;
    /// No such object (unused or invalid handle/transaction).
    pub const ENOENT: i64 = 2;
    /// The argument list is too long (parcel over the wire limit).
    pub const E2BIG: i64 = 7;
    /// Operation would block (queue full, quota exhausted).
    pub const EAGAIN: i64 = 11;
    /// The kernel could not allocate (frame, handle, registry slot).
    pub const ENOMEM: i64 = 12;
    /// Policy refused the call.
    pub const EACCES: i64 = 13;
    /// The pointer is not a writable/readable user mapping.
    pub const EFAULT: i64 = 14;
    /// The bootstrap client end has already been claimed.
    pub const EBUSY: i64 = 16;
    /// A name registry entry already exists.
    pub const EEXIST: i64 = 17;
    /// A malformed argument, parcel, or op code.
    pub const EINVAL: i64 = 22;
    /// The caller's descriptor table is full (`ENDPOINT_FD`).
    pub const EMFILE: i64 = 24;
    /// The peer endpoint is gone.
    pub const EPIPE: i64 = 32;
    /// Refused because a call would form a synchronous cycle.
    pub const EDEADLK: i64 = 35;
    /// The deadline passed before a reply arrived.
    pub const ETIMEDOUT: i64 = 110;
    /// The caller canceled the transaction.
    pub const ECANCELED: i64 = 125;
}

/// Call a method and block until the reply arrives.
pub const OP_CALL: u64 = 1;
/// Answer a pending transaction with a reply parcel.
pub const OP_REPLY: u64 = 2;
/// Send a one-way message; never blocks.
pub const OP_SEND: u64 = 3;
/// Receive the next message, blocking until one is queued.
pub const OP_RECV: u64 = 4;
/// Cancel a pending transaction.
pub const OP_CANCEL: u64 = 5;
/// Close an endpoint handle.
pub const OP_CLOSE_ENDPOINT: u64 = 6;
/// Create a fresh channel pair; both handles open in the caller.
pub const OP_CREATE_PAIR: u64 = 7;
/// Read fabric statistics. When the caller offers a
/// [`crate::ipc::stats::FabricStats::SIZE`]-byte buffer, the versioned
/// [`crate::ipc::stats::FabricStats`] snapshot is written (ABI version 2);
/// with a 64-byte buffer the legacy [`MsgStats`] shape is kept, and a
/// non-zero `handle` still means per-channel [`MsgStats`].
pub const OP_STATS: u64 = 8;
/// Claim the boot-time client endpoint (first userspace task only).
pub const OP_BOOTSTRAP: u64 = 9;
/// Register a call and return the transaction id instead of waiting (the
/// caller stays runnable): the asynchronous completion `channels` split
/// `begin_call` for. Finish it with [`OP_CALL_AWAIT`].
pub const OP_CALL_BEGIN: u64 = 10;
/// Wait for a [`OP_CALL_BEGIN`] transaction and return its reply.
pub const OP_CALL_AWAIT: u64 = 11;
/// Global message totals in the compact 64-byte [`MsgStats`] shape,
/// independent of the buffer size (the stable "totals" path next to
/// [`OP_STATS`]'s versioned snapshot).
pub const OP_TOTALS: u64 = 12;
/// Publish a service name in the kernel registry (issue #89). The request
/// parcel's body carries the name, interfaces, lease and the endpoint handle;
/// `txn_id` names the task whose table holds that handle.
pub const OP_REGISTER: u64 = 13;
/// Resolve a service name; the returned `value` is a fresh handle to the
/// registered endpoint, opened in the target task's table.
pub const OP_RESOLVE: u64 = 14;
/// Withdraw a service name (owner, or `CAP_IPC_CONTROL`).
pub const OP_UNREGISTER: u64 = 15;
/// Snapshot the name table into the caller's buffer as an encoded parcel.
pub const OP_LIST: u64 = 16;
/// Authorize a topic or subscription filter segment by segment (issue #92).
///
/// The request parcel's body is the generated `AuthorizeTopicArgs`
/// (`idl/topics.midl`): the name, the mode (see
/// [`crate::ipc::topics::MODE_PUBLISH`]) and an optional audit correlation id.
/// `MsgArgs::txn_id` names the actor task: [`REGISTRY_TARGET_SELF`]
/// (or the caller) evaluates the caller's own credentials, any other slot is
/// the `messengerd` proxy path and requires `CAP_IPC_CONTROL`. The op returns
/// the number of segments evaluated in `value`, or `-EACCES` when policy
/// refused one of them (already audited by `ipc::authorize`).
pub const OP_AUTHORIZE_TOPIC: u64 = 17;

/// Replace every rule of one label (the `acl_load` syscall, application
/// package system phase 1). The request parcel's body is the generated
/// `LoadLabelArgs` (`idl/policy.midl`): the label string and its rule list.
/// Needs `CAP_IPC_CONTROL`; an empty list revokes the label. `value` is the
/// number of rules now held by the label.
pub const OP_ACL_LOAD: u64 = 18;

/// Park until one of several endpoints is ready (docs/performance-plan.md
/// P1.3, P1.4): `parcel_ptr` points at `parcel_len` items (`u64`, at most
/// `channels::MAX_WAIT_ENDPOINTS`), each an endpoint handle or, with
/// `channels::WAIT_ITEM_CALL` set, the transaction id of a call the caller
/// began and has not awaited (ready when it ended; issue #309), `deadline`
/// as for `recv`, and
/// `flags` may hold the doorbells `channels::WAIT_RAW_INPUT` (the caller's
/// raw input ring, syscall 25) and `channels::WAIT_DISPLAY_KEYS` (a key in
/// the display owner's input queue) and `channels::WAIT_INET` (the `AF_INET`
/// pump's doorbell, the attached `netd` only) and `channels::WAIT_CHILD` (a
/// child of the caller finished, any task; P7.1); a doorbell the caller may not use is
/// `-ENOENT`. Nothing is received; `value` is the ready mask (bit `i` for
/// handle `i`, `channels::RAW_INPUT_READY` / `DISPLAY_INPUT_READY` / `INET_READY` /
/// `CHILD_READY` for the doorbells). A kernel ABI op, not a Messenger interface: no parcel crosses
/// it, so there is nothing for MIDL to describe.
pub const OP_WAIT: u64 = 19;

/// Open a private connection to a registered name (issue #483): the request
/// parcel is a `Connect` (`idl/registry.midl`), `value` is the caller's new
/// handle to its own channel, and the service receives the other end as a
/// `Connected` message on the registered endpoint. Gated like [`OP_RESOLVE`].
pub const OP_CONNECT: u64 = 20;

/// Open a Linux descriptor watching the endpoint `handle` names (issue #667,
/// `ipc::endpointfd`): `poll`/`select`/`epoll` report it readable while a
/// message is queued or the peer closed, and it hangs up once the handle is
/// gone. `value` is the descriptor; flag [`ENDPOINT_FD_CLOEXEC`] opens it
/// close-on-exec. `ENOENT` for no such handle, `EINVAL` for one that is not
/// a channel, `EACCES` without `CALL`, `EMFILE` for a full table. A kernel
/// ABI op: no parcel crosses it.
pub const OP_ENDPOINT_FD: u64 = 21;

/// Create a shared buffer of `parcel_len` bytes, mapped into the caller
/// (`docs/messenger-core-plan.md` 3.4): `value` is the buffer handle, `aux`
/// its address, `bytes` its size (rounded up to whole pages). The handle
/// travels in a parcel's `buffers` list; the peer maps it with
/// [`OP_BUFFER_MAP`]. `EINVAL` for a zero or oversized size, `EAGAIN` over
/// the buffer quota, `ENOMEM` when no frames or handle are left.
pub const OP_BUFFER_CREATE: u64 = 22;

/// Map the buffer `handle` names into the caller (idempotent per task):
/// `value` is the address, `aux` the size. `ENOENT` for no such handle,
/// `EINVAL` for one that is not a buffer.
pub const OP_BUFFER_MAP: u64 = 23;

/// Close the buffer `handle` names: unmap it and drop the caller's reference
/// (the pages live while any other handle or in-flight message holds one).
/// `ENOENT` for a handle the caller does not hold, `EBUSY` for the bound
/// compositor's own screen buffer (`unbind` releases that one).
pub const OP_BUFFER_CLOSE: u64 = 24;

/// `MsgArgs::txn_id` marker for registry ops: act on the calling task.
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// Number of bytes in [`MsgArgs`], the first range the syscall validates.
pub const ARGS_SIZE: usize = 64;
/// Number of bytes in [`MsgResult`].
pub const RESULT_SIZE: usize = 64;

/// The syscall request block. The layout is shared with `user/src/messenger/`
/// and must stay in lockstep; every field is a little-endian `u64`.
///
/// Only the fields an op documents as input are read; the rest are ignored (and
/// `flags` must be zero, so an ABI addition is rejected loudly rather than
/// silently misread).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, close, stats; the buffer
    /// handle of `buffer_map` and `buffer_close`.
    pub handle: u64,
    /// Transaction id: reply, cancel, await.
    pub txn_id: u64,
    /// Request parcel bytes (call, begin, send, reply).
    pub parcel_ptr: u64,
    /// Request parcel length in bytes; the size of `buffer_create`.
    pub parcel_len: u64,
    /// Reply or receive buffer (call, recv, await, stats).
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline in ticks (100 Hz); `0` waits forever.
    pub deadline: u64,
    /// Reserved for future flags; must be zero except where an op names one
    /// ([`CLOSE_RELEASE`] on `close_endpoint`, [`RECV_SENDER_ID`] on `recv`).
    pub flags: u64,
}

/// `close_endpoint` flag: release the handle, and close the side only if no
/// other handle names it (see `channels::release_endpoint`).
pub const CLOSE_RELEASE: u64 = 1;

/// `recv` flag: also write the sender's kernel-stamped credentials, as
/// queued ([`crate::ipc::channels::SenderId`]), to `parcel_ptr`: the whole
/// `SenderId::SIZE`-byte block (capability bits included) when `parcel_len`
/// allows it, else the `SenderId::IDENTITY_SIZE`-byte identity; less is
/// `EINVAL`. This is how a service authorizes a caller by uid, label or
/// capability without `CAP_SETUID`, which reading an arbitrary task's
/// credentials needs.
pub const RECV_SENDER_ID: u64 = 1;

/// `endpoint_fd` flag: open the descriptor close-on-exec.
pub const ENDPOINT_FD_CLOEXEC: u64 = 1;

impl MsgArgs {
    /// Decode a little-endian block of exactly [`ARGS_SIZE`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgArgs> {
        if bytes.len() != ARGS_SIZE {
            return None;
        }
        let mut words = [0u64; 8];
        for (index, word) in words.iter_mut().enumerate() {
            let at = index * 8;
            *word = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        }
        Some(MsgArgs {
            handle: words[0],
            txn_id: words[1],
            parcel_ptr: words[2],
            parcel_len: words[3],
            buf_ptr: words[4],
            buf_cap: words[5],
            deadline: words[6],
            flags: words[7],
        })
    }

    /// The absolute deadline, or `None` for "wait forever".
    pub(super) fn deadline_ticks(&self) -> Option<u64> {
        (self.deadline != 0).then_some(self.deadline)
    }

    /// Encode little-endian; the user library's mirror keeps the same order.
    pub fn to_bytes(self) -> [u8; ARGS_SIZE] {
        let words = [
            self.handle,
            self.txn_id,
            self.parcel_ptr,
            self.parcel_len,
            self.buf_ptr,
            self.buf_cap,
            self.deadline,
            self.flags,
        ];
        encode_words(&words)
    }
}

/// The syscall response block: `status`/`value`/`aux`/`bytes` plus reserved
/// space. Shared with `user/src/messenger/`, little-endian `u64` fields.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgResult {
    /// `0` on success, or a negative errno (the same value as `rax`).
    pub status: i64,
    /// Primary output: new handle (create_pair/bootstrap), transaction id
    /// (call_begin/recv), or 0.
    pub value: u64,
    /// Secondary output: second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr` (call, call_await, recv, stats).
    pub bytes: u64,
    /// `recv` reports the delivered transfers here: `[first handle, handle
    /// count, first buffer handle, buffer count]`; zero otherwise.
    pub reserved: [u64; 4],
}

impl MsgResult {
    /// Decode a little-endian block of exactly [`RESULT_SIZE`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgResult> {
        if bytes.len() != RESULT_SIZE {
            return None;
        }
        let mut words = [0u64; 8];
        for (index, word) in words.iter_mut().enumerate() {
            let at = index * 8;
            *word = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        }
        Some(MsgResult {
            status: words[0] as i64,
            value: words[1],
            aux: words[2],
            bytes: words[3],
            reserved: [words[4], words[5], words[6], words[7]],
        })
    }

    /// Encode little-endian; the user library's mirror keeps the same order.
    pub fn to_bytes(self) -> [u8; RESULT_SIZE] {
        let words = [
            self.status as u64,
            self.value,
            self.aux,
            self.bytes,
            self.reserved[0],
            self.reserved[1],
            self.reserved[2],
            self.reserved[3],
        ];
        encode_words(&words)
    }
}

/// Fixed-size little-endian encoding of a word block.
fn encode_words(words: &[u64; 8]) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    for (index, word) in words.iter().enumerate() {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Compact channel counters (stats ABI version 1), exactly the byte order
/// [`OP_STATS`] writes for a 64-byte buffer and [`OP_TOTALS`] always writes.
/// The layout is shared with `user/src/messenger/`; the richer version 2
/// block lives in [`crate::ipc::stats::FabricStats`].
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgStats {
    pub calls: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub cancels: u64,
    pub drops: u64,
    pub queued: u64,
    pub queued_bytes: u64,
    pub outstanding: u64,
}

impl MsgStats {
    /// Number of bytes [`OP_STATS`] writes.
    pub const SIZE: usize = 64;

    /// Decode the block written by [`OP_STATS`].
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgStats> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        Some(MsgStats {
            calls: word(0)?,
            replies: word(1)?,
            timeouts: word(2)?,
            cancels: word(3)?,
            drops: word(4)?,
            queued: word(5)?,
            queued_bytes: word(6)?,
            outstanding: word(7)?,
        })
    }
}

impl From<channels::Stats> for MsgStats {
    fn from(stats: channels::Stats) -> MsgStats {
        MsgStats {
            calls: stats.calls,
            replies: stats.replies,
            timeouts: stats.timeouts,
            cancels: stats.cancels,
            drops: stats.drops,
            queued: stats.queued,
            queued_bytes: stats.queued_bytes,
            outstanding: stats.outstanding,
        }
    }
}

impl MsgStats {
    /// Encode for `copy_out` into the caller's buffer.
    pub(super) fn to_bytes(self) -> [u8; Self::SIZE] {
        let words = [
            self.calls,
            self.replies,
            self.timeouts,
            self.cancels,
            self.drops,
            self.queued,
            self.queued_bytes,
            self.outstanding,
        ];
        encode_words(&words)
    }
}

const _: () = {
    // The user library mirrors these blocks; keep the sizes pinned so an
    // accidental field addition is a compile error, not an ABI mismatch.
    assert!(core::mem::size_of::<MsgArgs>() == ARGS_SIZE);
    assert!(core::mem::size_of::<MsgResult>() == RESULT_SIZE);
    assert!(core::mem::size_of::<MsgStats>() == MsgStats::SIZE);
};
