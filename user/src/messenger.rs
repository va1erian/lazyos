//! Blocking userspace Messenger API over the native `messenger` syscall
//! (issue #69).
//!
//! This is the synchronous `Connection::call` / `Server::serve` shape from
//! `docs/messenger.md` section 15, layered straight on [`crate::sys`]: each
//! operation is one `int 0x80` with a small request/response block, and parcels
//! are encoded with [`libmessenger`]. The split `begin_call` + `await_reply`
//! ops are exposed too, because the kernel transaction already supports them
//! (`channels::begin_call`); the async API builds on the same pair later.
//!
//! The ABI blocks below mirror `kernel/src/ipc/syscalls.rs` field for field;
//! keep them in lockstep (the kernel pins the sizes at compile time).
//!
//! Deadlines are absolute PIT ticks (100 Hz), kernel-relative: `None` (encoded
//! as 0) waits forever, and userspace has no clock syscall yet, so callers that
//! want a timeout will need one before it is useful.

use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{Error as ParcelError, Parcel};

use crate::sys;

/// Native syscall ops, matching the kernel's `ipc::syscalls::OP_*`.
pub mod op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Answer a pending transaction with a reply parcel.
    pub const REPLY: u64 = 2;
    /// Send a one-way message; never blocks.
    pub const SEND: u64 = 3;
    /// Receive the next message, blocking until one is queued.
    pub const RECV: u64 = 4;
    /// Cancel a pending transaction.
    pub const CANCEL: u64 = 5;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
    /// Read channel counters (`handle = 0` means every live channel).
    pub const STATS: u64 = 8;
    /// Claim the boot-time client endpoint (first userspace task only).
    pub const BOOTSTRAP: u64 = 9;
    /// Register a call and park, returning the transaction id.
    pub const CALL_BEGIN: u64 = 10;
    /// Wait for a `CALL_BEGIN` transaction and return its reply.
    pub const CALL_AWAIT: u64 = 11;
}

/// Negative errno values the kernel returns; see the kernel's
/// `ipc::syscalls::errno`.
pub mod errno {
    pub const E2BIG: i64 = 7;
    pub const EAGAIN: i64 = 11;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const EINVAL: i64 = 22;
    pub const EPIPE: i64 = 32;
    pub const EDEADLK: i64 = 35;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECANCELED: i64 = 125;
}

/// The syscall request block; mirrors the kernel's `MsgArgs`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, cancel, close, stats.
    pub handle: u64,
    /// Transaction id: reply, cancel, await.
    pub txn_id: u64,
    /// Request parcel bytes (call, begin, send, reply).
    pub parcel_ptr: u64,
    /// Request parcel length in bytes.
    pub parcel_len: u64,
    /// Reply or receive buffer (call, recv, await, stats).
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline; 0 waits forever.
    pub deadline: u64,
    /// Reserved; must be zero.
    pub flags: u64,
}

/// The syscall response block; mirrors the kernel's `MsgResult`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgResult {
    /// 0 on success, or a negative errno.
    pub status: i64,
    /// New handle (create_pair/bootstrap), transaction id (begin/recv).
    pub value: u64,
    /// Second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr` (call, await, recv, stats).
    pub bytes: u64,
    /// Reserved; always zero today.
    pub reserved: [u64; 4],
}

/// Channel counters; mirrors the kernel's `MsgStats` byte for byte.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    pub calls: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub cancels: u64,
    pub drops: u64,
    pub queued: u64,
    pub queued_bytes: u64,
    pub outstanding: u64,
}

impl Stats {
    /// Number of bytes the `stats` op writes.
    pub const SIZE: usize = 64;
}

/// A Messenger or kernel error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The kernel refused the operation with a negative errno.
    Errno(i64),
    /// A parcel was malformed on encode or decode.
    Parcel(ParcelError),
}

impl Error {
    /// The negative errno the kernel returned, if this is a kernel error.
    pub fn errno(self) -> Option<i64> {
        match self {
            Error::Errno(code) => Some(code),
            Error::Parcel(_) => None,
        }
    }

    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::Parcel(error) => error.message(),
            // A match guard keeps the named constants readable; a bare
            // `-CONST` is not a valid pattern.
            Error::Errno(code) => match code {
                code if code == -errno::E2BIG => {
                    "the parcel or reply exceeds the Messenger buffer limit"
                }
                code if code == -errno::EAGAIN => "the peer's queue is full; try again shortly",
                code if code == -errno::ENOMEM => "the kernel is out of Messenger resources",
                code if code == -errno::EACCES => {
                    "this app is not allowed to make that Messenger call"
                }
                code if code == -errno::EFAULT => "a Messenger buffer pointer is invalid",
                code if code == -errno::EBUSY => "the bootstrap endpoint has already been claimed",
                code if code == -errno::EINVAL => "the Messenger request is malformed",
                code if code == -errno::EPIPE => "the other end of the channel closed",
                code if code == -errno::EDEADLK => {
                    "the call would deadlock with an open transaction"
                }
                code if code == -errno::ETIMEDOUT => "the call timed out before a reply arrived",
                code if code == -errno::ECANCELED => "the call was canceled",
                _ => "the Messenger call failed",
            },
        }
    }
}

/// Result alias for the userspace API.
pub type Result<T> = core::result::Result<T, Error>;

/// Default reply/receive buffer for the convenience methods. A reply that does
/// not fit is refused with `-E2BIG` *after* the transaction completes, so the
/// bytes are lost; a streaming/shared-buffer path is the follow-up for large
/// payloads (`docs/messenger.md` section 10).
pub const DEFAULT_BUFFER: usize = 16 * 1024;

/// One end of a Messenger channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    handle: u64,
}

impl Endpoint {
    /// Wrap a raw handle number (e.g. one carried inside a parcel).
    pub const fn from_raw(handle: u64) -> Endpoint {
        Endpoint { handle }
    }

    /// The underlying handle number.
    pub const fn handle(self) -> u64 {
        self.handle
    }

    /// Blocking call: encode `request`, wait for the reply, decode it.
    pub fn call(&self, request: &Parcel, deadline: Option<u64>) -> Result<Parcel> {
        let bytes = encode(request)?;
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        Parcel::decode(&buf[..len]).map_err(Error::Parcel)
    }

    /// Register a call and park the task; returns the transaction id. Complete
    /// it with [`Endpoint::await_reply`] — the same split the kernel uses for
    /// asynchronous completion.
    pub fn begin_call(&self, request: &Parcel, deadline: Option<u64>) -> Result<u64> {
        let bytes = encode(request)?;
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL_BEGIN, &args, &mut result)?;
        Ok(result.value)
    }

    /// Wait for a [`Endpoint::begin_call`] transaction and decode its reply.
    pub fn await_reply(&self, txn_id: u64) -> Result<Parcel> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        let args = MsgArgs {
            txn_id,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL_AWAIT, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        Parcel::decode(&buf[..len]).map_err(Error::Parcel)
    }

    /// Answer a pending transaction with `reply` (the server side of a call).
    pub fn reply(&self, txn_id: u64, reply: &Parcel) -> Result<()> {
        let bytes = encode(reply)?;
        let args = MsgArgs {
            txn_id,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        syscall(op::REPLY, &args, &mut MsgResult::default())
    }

    /// Fire-and-forget send; returns once the message is queued.
    pub fn send(&self, parcel: &Parcel) -> Result<()> {
        let bytes = encode(parcel)?;
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        syscall(op::SEND, &args, &mut MsgResult::default())
    }

    /// Block until a message arrives (or the deadline passes).
    pub fn recv(&self, deadline: Option<u64>) -> Result<Message> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        let args = MsgArgs {
            handle: self.handle,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::RECV, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let parcel = Parcel::decode(&buf[..len]).map_err(Error::Parcel)?;
        Ok(Message {
            sender: result.aux,
            txn: (result.value != 0).then_some(result.value),
            parcel,
        })
    }

    /// Cancel a pending transaction started by this task.
    pub fn cancel(&self, txn_id: u64) -> Result<()> {
        let args = MsgArgs {
            txn_id,
            ..MsgArgs::default()
        };
        syscall(op::CANCEL, &args, &mut MsgResult::default())
    }

    /// Close this endpoint; the channel dies when both ends close.
    pub fn close(self) -> Result<()> {
        let args = MsgArgs {
            handle: self.handle,
            ..MsgArgs::default()
        };
        syscall(op::CLOSE_ENDPOINT, &args, &mut MsgResult::default())
    }

    /// Counters for this endpoint's channel.
    pub fn stats(&self) -> Result<Stats> {
        stats_call(self.handle)
    }
}

/// A received message: the decoded parcel plus the kernel-stamped metadata
/// userspace cannot otherwise see.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// Task slot that sent the message.
    pub sender: u64,
    /// Kernel transaction id for a call; `None` for one-way messages.
    pub txn: Option<u64>,
    /// The decoded parcel.
    pub parcel: Parcel,
}

impl Message {
    /// Method id from the parcel header.
    pub fn method(&self) -> u32 {
        self.parcel.header.method
    }

    /// Interface id from the parcel header.
    pub fn interface_id(&self) -> u64 {
        self.parcel.header.interface_id
    }
}

/// Claim the boot-time client endpoint. Only the first userspace task can; a
/// later caller gets `-EBUSY`.
pub fn bootstrap() -> Result<Endpoint> {
    let mut result = MsgResult::default();
    syscall(op::BOOTSTRAP, &MsgArgs::default(), &mut result)?;
    Ok(Endpoint::from_raw(result.value))
}

/// Create a fresh channel pair; both endpoints open in this task.
pub fn create_pair() -> Result<(Endpoint, Endpoint)> {
    let mut result = MsgResult::default();
    syscall(op::CREATE_PAIR, &MsgArgs::default(), &mut result)?;
    Ok((
        Endpoint::from_raw(result.value),
        Endpoint::from_raw(result.aux),
    ))
}

/// Aggregated counters across every live channel.
pub fn global_stats() -> Result<Stats> {
    stats_call(0)
}

/// A blocking request loop in the `Server::serve` shape.
pub struct Server {
    endpoint: Endpoint,
}

impl Server {
    /// Wrap an endpoint that is being served.
    pub const fn new(endpoint: Endpoint) -> Server {
        Server { endpoint }
    }

    /// Receive one request, dispatch it to `handler`, and reply when the
    /// message was a call. One-way messages are passed to the handler too, but
    /// their (ignored) reply parcel is not sent.
    pub fn serve_once<F>(&self, handler: &mut F) -> Result<()>
    where
        F: FnMut(&Message) -> Result<Parcel>,
    {
        let message = self.endpoint.recv(None)?;
        let reply = handler(&message)?;
        if let Some(txn) = message.txn {
            self.endpoint.reply(txn, &reply)?;
        }
        Ok(())
    }

    /// [`Server::serve_once`] forever.
    pub fn serve<F>(&self, mut handler: F) -> Result<()>
    where
        F: FnMut(&Message) -> Result<Parcel>,
    {
        loop {
            self.serve_once(&mut handler)?;
        }
    }
}

/// One `messenger` syscall; a negative return is an errno.
fn syscall(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<()> {
    let code = sys::messenger(
        op,
        args as *const MsgArgs as u64,
        result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(Error::Errno(code))
    } else {
        Ok(())
    }
}

/// Encode a parcel for the wire.
fn encode(parcel: &Parcel) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}

/// Read `Stats` into an aligned local and hand the kernel its address. The
/// kernel writes little-endian `u64`s in the same field order, so on x86_64 the
/// struct is already the wire layout.
fn stats_call(handle: u64) -> Result<Stats> {
    let mut stats = Stats::default();
    let args = MsgArgs {
        handle,
        buf_ptr: &mut stats as *mut Stats as u64,
        buf_cap: Stats::SIZE as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::STATS, &args, &mut result)?;
    if result.bytes as usize != Stats::SIZE {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(stats)
}
