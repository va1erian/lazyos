//! The `netd` side of the `AF_INET` seam: fetch requests, answer them, move
//! bytes through side A of each socket's data path, and hand in accepted
//! connections. Everything here is non-blocking; `netd` calls it on its own
//! tick (native syscall 27, `process/inetsys.rs`). Pure kernel functions, so
//! the in-kernel suite plays `netd` with them directly.

use alloc::sync::Arc;

use crate::ipc::pipe::{Error as PipeError, Side};

use super::errno::*;
use super::{Addr, InetSock, Kind, Op, Request, Slot, State, TABLE};

/// Size of a request in the wire form [`encode`] produces.
pub const REQUEST_BYTES: usize = 24;

/// Request codes of the wire form.
pub mod code {
    pub const BIND: u32 = 1;
    pub const CONNECT: u32 = 2;
    pub const LISTEN: u32 = 3;
    pub const CLOSE: u32 = 4;
}

/// The wire form of a request: `u32` code, `u32` id, four address octets,
/// `u16` port (little endian), `u16` kind (0 stream, 1 datagram), `u32` aux
/// (the backlog of a `Listen`), four reserved bytes.
pub fn encode(request: &Request) -> [u8; REQUEST_BYTES] {
    let (op, addr, aux) = match request.op {
        Op::Bind(addr) => (code::BIND, addr, 0),
        Op::Connect(addr) => (code::CONNECT, addr, 0),
        Op::Listen(backlog) => (code::LISTEN, Addr::ANY, backlog),
        Op::Close => (code::CLOSE, Addr::ANY, 0),
    };
    let mut out = [0u8; REQUEST_BYTES];
    out[0..4].copy_from_slice(&op.to_le_bytes());
    out[4..8].copy_from_slice(&request.id.to_le_bytes());
    out[8..12].copy_from_slice(&addr.ip);
    out[12..14].copy_from_slice(&addr.port.to_le_bytes());
    out[14..16].copy_from_slice(&(request.kind as u16).to_le_bytes());
    out[16..20].copy_from_slice(&aux.to_le_bytes());
    out
}

/// What a byte-moving call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    /// This many bytes moved (never zero).
    Data(usize),
    /// Nothing to read, or no room to write, right now.
    Empty,
    /// Read: the application will send no more (it shut down or closed).
    Eof,
    /// Write: the application closed its side; stop sending.
    Gone,
}

/// `netd` announces itself (task `slot`). Whatever an earlier `netd` left is
/// forgotten: its sockets could not be served any more.
pub fn attach(slot: usize) {
    super::reset();
    TABLE.lock().netd = Some(slot);
}

/// Whether `slot` is the attached `netd`.
pub fn is_netd(slot: usize) -> bool {
    TABLE.lock().netd == Some(slot)
}

/// The next request worth acting on. A bind, connect or listen for a socket
/// whose descriptors are all gone is dropped here; its close still arrives.
pub fn next_request() -> Option<Request> {
    let mut table = TABLE.lock();
    while let Some(request) = table.queue.pop_front() {
        let closed = table.get(request.id).is_none_or(|slot| slot.closed);
        if request.op == Op::Close || !closed {
            return Some(request);
        }
    }
    None
}

fn sock_of(id: u32) -> Option<Arc<InetSock>> {
    TABLE.lock().get(id)?.sock.upgrade()
}

/// `netd` answers the request it fetched for `id`: `status` 0 on success, an
/// errno otherwise; `local` and `peer` are the addresses the stack ended up
/// with (the ephemeral port a bind or connect was given).
pub fn complete(id: u32, status: i32, local: Addr, peer: Addr) -> Result<(), i32> {
    let sock = sock_of(id).ok_or(EBADF)?;
    let mut inner = sock.inner.lock();
    let Some(op) = inner.pending.take() else {
        return Err(EINVAL);
    };
    let outcome = if status != 0 {
        Err(status)
    } else {
        match op {
            Op::Bind(_) => {
                // Only a datagram socket has its data path from the bind; a
                // stream gets one when it connects.
                let pair = if sock.kind() == Kind::Dgram {
                    prepare_pair(&sock, &mut inner, id)
                } else {
                    Ok(())
                };
                if pair.is_ok() {
                    inner.state = State::Bound;
                    inner.local = local;
                }
                pair
            }
            Op::Connect(_) => match prepare_pair(&sock, &mut inner, id) {
                Ok(()) => {
                    inner.state = State::Connected;
                    inner.local = local;
                    inner.peer = peer;
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Op::Listen(_) => {
                inner.state = State::Listening;
                // A bound socket keeps its address; an unbound one learns the
                // port the stack gave it.
                if local.port != 0 {
                    inner.local = local;
                }
                Ok(())
            }
            Op::Close => Ok(()),
        }
    };
    // The stack said yes but the kernel could not follow (out of rings): `netd`
    // is told, so it releases what it made.
    let kernel_failed = outcome.err().filter(|_| status == 0);
    if outcome.is_err() && inner.state == State::Connecting {
        inner.state = State::Fresh;
    }
    // A `connect` whose caller already left has nobody waiting: the answer is
    // `SO_ERROR` (and `poll`).
    if let Err(e) = outcome {
        if inner.detached && matches!(op, Op::Connect(_)) {
            inner.so_error = e;
        }
    }
    if matches!(op, Op::Connect(_)) {
        inner.detached = false;
    }
    inner.done = Some(outcome);
    drop(inner);
    sock.bump();
    kernel_failed.map_or(Ok(()), Err)
}

/// Give socket `id` its data path: a fresh small pair, the application's side
/// acquired here and `netd`'s kept in the table.
fn prepare_pair(sock: &InetSock, inner: &mut super::sock::Inner, id: u32) -> Result<(), i32> {
    if inner.pair.is_some() {
        return Ok(());
    }
    let pair = sock.new_pair().ok_or(ENOBUFS)?;
    pair.acquire(Side::A);
    pair.acquire(Side::B);
    pair.set_nonblock(Side::B, sock.nonblock());
    let mut table = TABLE.lock();
    match table.get_mut(id) {
        Some(Slot { net_pair, .. }) => {
            *net_pair = Some(Arc::clone(&pair));
            inner.pair = Some(pair);
            Ok(())
        }
        None => {
            pair.close(Side::A);
            pair.close(Side::B);
            Err(EBADF)
        }
    }
}

/// `netd` hands in a connection it accepted for listener `listener`; the new
/// socket's id. `ENOBUFS` when the listener's queue or the socket table is
/// full (`netd` should drop the connection).
pub fn accepted(listener: u32, peer: Addr, local: Addr) -> Result<u32, i32> {
    let listening = sock_of(listener).ok_or(EBADF)?;
    if listening.state() != State::Listening {
        return Err(EINVAL);
    }
    let conn = super::create(Kind::Stream).ok_or(ENOBUFS)?;
    {
        let mut inner = conn.inner.lock();
        prepare_pair(&conn, &mut inner, conn.id)?;
        inner.state = State::Connected;
        inner.local = local;
        inner.peer = peer;
    }
    let id = conn.id;
    if !listening.offer(conn) {
        return Err(ENOBUFS);
    }
    Ok(id)
}

fn net_pair(id: u32) -> Option<Arc<crate::ipc::pipe::SocketPair>> {
    TABLE.lock().get(id)?.net_pair.clone()
}

/// Take bytes the application wrote: from a stream, whatever is queued (up to
/// `dst`); from a datagram socket, one message (header first).
pub fn net_read(id: u32, dst: &mut [u8]) -> Result<Io, i32> {
    let pair = net_pair(id).ok_or(EBADF)?;
    match pair.read(Side::A, dst, true) {
        Ok(0) => Ok(Io::Eof),
        Ok(n) => Ok(Io::Data(n)),
        Err(PipeError::WouldBlock) => Ok(Io::Empty),
        Err(PipeError::MessageTooLong) => Err(EINVAL),
        Err(_) => Ok(Io::Eof),
    }
}

/// Give the application bytes from the network. A stream takes what fits; a
/// datagram message is all or nothing.
pub fn net_write(id: u32, src: &[u8]) -> Result<Io, i32> {
    let pair = net_pair(id).ok_or(EBADF)?;
    match pair.write(Side::A, src, true) {
        Ok(0) => Ok(Io::Empty),
        Ok(n) => Ok(Io::Data(n)),
        Err(PipeError::WouldBlock) => Ok(Io::Empty),
        Err(PipeError::BrokenPipe) => Ok(Io::Gone),
        Err(_) => Err(EINVAL),
    }
}

/// The network side finished sending: the application reads end of stream.
pub fn net_eof(id: u32) -> Result<(), i32> {
    let pair = net_pair(id).ok_or(EBADF)?;
    pair.shutdown(Side::A, 1);
    if let Some(sock) = sock_of(id) {
        sock.bump();
    }
    Ok(())
}

/// The connection failed (reset, timed out): the application sees end of
/// stream and `SO_ERROR`.
pub fn net_error(id: u32, errno: i32) -> Result<(), i32> {
    let pair = net_pair(id).ok_or(EBADF)?;
    pair.shutdown(Side::A, 2);
    if let Some(sock) = sock_of(id) {
        sock.inner.lock().so_error = errno;
        sock.bump();
    }
    Ok(())
}

/// `netd` released the stack's socket for `id`: free the slot.
pub fn close_ack(id: u32) -> Result<(), i32> {
    let mut table = TABLE.lock();
    let index = super::Table::slot_of(id);
    let known = table.get(id).is_some();
    if !known {
        return Err(EBADF);
    }
    let slot = table.slots[index].take();
    drop(table);
    if let Some(Slot {
        net_pair: Some(pair),
        ..
    }) = slot
    {
        pair.close(Side::A);
    }
    Ok(())
}
