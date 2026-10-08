//! The core transport: [`Endpoint`], [`Message`], [`Server`], and the free
//! functions that wrap the bootstrap/create_pair/stats native ops.
//!
//! This is the synchronous `Connection::call` / `Server::serve` shape from
//! `docs/messenger.md` section 15, layered straight on [`crate::sys`]: each
//! operation is one `int 0x80` with a small request/response block, and parcels
//! are encoded with [`libmessenger`]. The split `begin_call` + `await_reply`
//! ops are exposed too, because the kernel transaction already supports them
//! (`channels::begin_call`); the async API builds on the same pair later.

use alloc::vec;
use alloc::vec::Vec;

use libmessenger::Parcel;

use super::message::{decode_caller, Message, CALLER_BLOCK, RECV_SENDER_ID};
use super::types::{
    Error, FabricStats, MsgArgs, MsgResult, Result, Stats, DEFAULT_BUFFER, EXPIRED_DEADLINE,
};
use super::{errno, op};

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
    ///
    /// The reply buffer is allocated per call (not zeroed: [`ReplyBuffer`]);
    /// a loop that calls often should still pass one buffer to
    /// [`Endpoint::call_with`] and skip the allocation.
    pub fn call(&self, request: &Parcel, deadline: Option<u64>) -> Result<Parcel> {
        let bytes = encode(request)?;
        let mut buf = ReplyBuffer::new();
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            buf_ptr: buf.ptr(),
            buf_cap: buf.capacity(),
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL, &args, &mut result)?;
        Parcel::decode(buf.filled(result.bytes)?).map_err(Error::Parcel)
    }

    /// [`Endpoint::call`] with a caller-owned reply buffer, for loops that
    /// cannot afford a fresh allocation per message.
    pub fn call_with(
        &self,
        request: &Parcel,
        buf: &mut [u8],
        deadline: Option<u64>,
    ) -> Result<Parcel> {
        let bytes = encode(request)?;
        self.call_bytes_with(&bytes, buf, deadline)
    }

    /// [`Endpoint::call_with`] for a caller that already holds the encoded
    /// request bytes, e.g. a poll loop that re-sends the same fixed request
    /// every call: `call_with` would otherwise re-encode (and reallocate) it
    /// every time, and the user runtime's bump allocator never reclaims that.
    pub fn call_bytes_with(
        &self,
        request: &[u8],
        buf: &mut [u8],
        deadline: Option<u64>,
    ) -> Result<Parcel> {
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: request.as_ptr() as u64,
            parcel_len: request.len() as u64,
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
    /// it with [`Endpoint::await_reply`] Ã¢â‚¬â€ the same split the kernel uses for
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
        let mut buf = ReplyBuffer::new();
        let args = MsgArgs {
            txn_id,
            buf_ptr: buf.ptr(),
            buf_cap: buf.capacity(),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL_AWAIT, &args, &mut result)?;
        Parcel::decode(buf.filled(result.bytes)?).map_err(Error::Parcel)
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

    /// [`reply`](Endpoint::reply) for a service loop.
    ///
    /// A caller whose deadline passed, who canceled, or who exited has no
    /// transaction left, and the kernel answers the late reply with `-ENOENT`.
    /// That is an ordinary race that any client can trigger at will, not a fault
    /// of the service: swallow it so one impatient (or hostile) client cannot
    /// take the whole service down by hanging up before its answer. Every other
    /// error still propagates.
    pub fn reply_or_drop(&self, txn_id: u64, reply: &Parcel) -> Result<()> {
        match self.reply(txn_id, reply) {
            Err(error) if error.errno() == Some(-errno::ENOENT) => Ok(()),
            other => other,
        }
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
    ///
    /// Allocates the receive buffer per call; a service loop should use
    /// [`Endpoint::recv_with`] and reuse one buffer.
    pub fn recv(&self, deadline: Option<u64>) -> Result<Message> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        self.recv_into(&mut buf, deadline)
    }

    /// [`Endpoint::recv`] into a caller-provided buffer.
    ///
    /// A polling loop can avoid a fresh [`DEFAULT_BUFFER`] allocation per
    /// iteration by reusing one scratch buffer here. A message larger than `buf` is refused with
    /// `-E2BIG` after delivery, exactly like [`Endpoint::recv`].
    pub fn recv_into(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Message> {
        let mut block = [0u8; CALLER_BLOCK];
        let args = MsgArgs {
            handle: self.handle,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: deadline.unwrap_or(0),
            // The sender's stamped credentials land here (issue #446).
            parcel_ptr: block.as_mut_ptr() as u64,
            parcel_len: CALLER_BLOCK as u64,
            flags: RECV_SENDER_ID,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::RECV, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let parcel = Parcel::decode(&buf[..len]).map_err(Error::Parcel)?;
        let caller = decode_caller(&block).ok_or(Error::Errno(-errno::EINVAL))?;
        Message::new(
            result.aux,
            caller,
            (result.value != 0).then_some(result.value),
            parcel,
            result.delivered(),
        )
    }

    /// Alias for [`Endpoint::recv_into`], kept for the supervisor services.
    pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Message> {
        self.recv_into(buf, deadline)
    }

    /// Poll for a queued message without blocking (issue #91).
    ///
    /// `Ok(None)` means "nothing queued yet"; a dead peer still reports
    /// `-EPIPE`. The kernel checks the inbox first, so a queued message is
    /// returned immediately; otherwise the call parks with an already-expired
    /// deadline ([`EXPIRED_DEADLINE`]) and wakes with `-ETIMEDOUT` on the first
    /// timer gate. Before the PIT's first tick that wake can take up to 10 ms,
    /// and a busy runnable peer may be scheduled before this task resumes.
    pub fn poll_recv(&self) -> Result<Option<Message>> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        self.poll_recv_with(&mut buf)
    }

    /// [`Endpoint::poll_recv`] with a caller-owned buffer.
    pub fn poll_recv_with(&self, buf: &mut [u8]) -> Result<Option<Message>> {
        match self.recv_with(buf, Some(EXPIRED_DEADLINE)) {
            Ok(message) => Ok(Some(message)),
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
            Err(error) => Err(error),
        }
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

    /// Release this task's handle to the endpoint. Unlike [`Endpoint::close`],
    /// which ends the channel side for everyone, this closes the side only when
    /// no other handle names it: the right call for an endpoint received from
    /// another task or obtained by name, which other holders still depend on.
    pub fn release(self) -> Result<()> {
        let args = MsgArgs {
            handle: self.handle,
            flags: op::CLOSE_RELEASE,
            ..MsgArgs::default()
        };
        syscall(op::CLOSE_ENDPOINT, &args, &mut MsgResult::default())
    }

    /// Counters for this endpoint's channel.
    pub fn stats(&self) -> Result<Stats> {
        stats_call(self.handle)
    }
}

/// A [`DEFAULT_BUFFER`]-byte reply buffer the kernel fills, allocated but not
/// zeroed (P7.4): zero-filling 16 KiB on every [`Endpoint::call`] was most of
/// a small call's userspace cost, and only the bytes the kernel reports
/// written are ever read.
struct ReplyBuffer(Vec<u8>);

impl ReplyBuffer {
    fn new() -> ReplyBuffer {
        ReplyBuffer(Vec::with_capacity(DEFAULT_BUFFER))
    }

    fn ptr(&mut self) -> u64 {
        self.0.as_mut_ptr() as u64
    }

    fn capacity(&self) -> u64 {
        self.0.capacity() as u64
    }

    /// The `len` bytes a successful call or await wrote.
    fn filled(&mut self, len: u64) -> Result<&[u8]> {
        let len = usize::try_from(len).map_err(|_| Error::Errno(-errno::E2BIG))?;
        if len > self.0.capacity() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        // SAFETY: `len` is the byte count of the syscall that just returned
        // successfully on this buffer; the kernel copies exactly that many
        // bytes to its start (`write_reply`, which refuses a reply larger
        // than the capacity instead), and `len` fits the allocation (checked
        // above), so `..len` is initialized `u8` data.
        unsafe { self.0.set_len(len) };
        Ok(&self.0)
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

/// Aggregated counters across every live channel (compact shape).
pub fn global_stats() -> Result<Stats> {
    stats_call(0)
}

/// Global message totals through the dedicated `TOTALS` op.
pub fn global_totals() -> Result<Stats> {
    lazyos_sys::msg::totals().map_err(Error::Errno)
}

/// The versioned fabric snapshot (stats ABI v3): every subsystem in one block.
/// The snapshot buffer is sized so the kernel always serves the full block.
///
/// Allocates the snapshot buffer per call; a polling loop should use
/// [`fabric_stats_with`] and reuse one buffer.
pub fn fabric_stats() -> Result<FabricStats> {
    let mut buf = vec![0u8; FabricStats::SIZE];
    fabric_stats_with(&mut buf)
}

/// [`fabric_stats`] with a caller-owned buffer of at least
/// [`FabricStats::SIZE`] bytes.
pub fn fabric_stats_with(buf: &mut [u8]) -> Result<FabricStats> {
    lazyos_sys::msg::fabric_stats_into(buf).map_err(Error::Errno)
}

/// The compact channel counters of `handle` (`0`: every live channel).
fn stats_call(handle: u64) -> Result<Stats> {
    lazyos_sys::msg::stats(handle).map_err(Error::Errno)
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
            self.endpoint.reply_or_drop(txn, &reply)?;
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
///
/// `pub(super)` because the topics/registry/display protocol modules issue
/// the odd raw syscall directly (stats fetches, the topics broker's own
/// `AUTHORIZE_TOPIC` call) instead of going through [`Endpoint`].
pub(super) fn syscall(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<()> {
    // SAFETY: every `MsgArgs` this crate builds points at parcels, buffers
    // and wait sets it owns for the call, with their real lengths.
    unsafe { lazyos_sys::msg::messenger(op, args, result) }.map_err(Error::Errno)
}

/// Encode a parcel for the wire.
pub(super) fn encode(parcel: &Parcel) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}
