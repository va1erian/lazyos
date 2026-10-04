//! Stream I/O in place (docs/performance-plan.md P4.2): the caller fills the
//! socket's send buffer, or drains its receive buffer, directly.
//!
//! `socket_send` and `socket_recv` copy through a caller's slice and a fresh
//! vector, which suits a Messenger request of one chunk. `netd`'s pump for
//! Linux programs moves bytes between a kernel ring and the stack instead, and
//! with these calls it reads the ring straight into the stack's free space and
//! writes the stack's queued bytes straight into the ring: no staging buffer,
//! no allocation, and no bytes taken from one side that the other cannot hold
//! (the closure says how many it moved, and only those are queued or
//! dequeued).

use smoltcp::socket::tcp;

use super::sockets::{Inner, SockError};
use super::Stack;

/// What [`Stack::socket_recv_with`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// The closure took this many bytes (it may take none: then the bytes
    /// stay queued).
    Data(usize),
    /// Nothing is queued yet.
    Empty,
    /// The peer finished sending and everything was taken.
    End,
}

impl Stack {
    /// The stream socket `id` of `owner`, checked as `socket_send` and
    /// `socket_recv` check it; `read` selects the receive rules.
    fn stream_handle(
        &mut self,
        id: u32,
        owner: u64,
        read: bool,
    ) -> Result<(smoltcp::iface::SocketHandle, bool), SockError> {
        let entry = self.socks.entry(id, owner)?;
        let Inner::Tcp { handle, state } = &entry.inner else {
            return Err(SockError::InvalidState);
        };
        if read && state.reset && !state.fin_seen {
            return Err(SockError::Reset);
        }
        Stack::stream_io_check(state)?;
        if !read && state.shut_write {
            return Err(SockError::Pipe);
        }
        Ok((*handle, state.shut_read))
    }

    /// Offer the free part of stream `id`'s send buffer to `fill`, which
    /// writes bytes at the start of the slice and returns how many; those are
    /// queued. At most `max` bytes are offered. `Ok(0)` when the buffer is
    /// full or `fill` wrote nothing. Errors as `socket_send`.
    pub fn socket_send_with(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<usize, SockError> {
        let (handle, _) = self.stream_handle(id, owner, false)?;
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        if !socket.may_send() {
            return Err(SockError::Pipe);
        }
        let n = socket
            .send(|free| {
                let room = free.len().min(max);
                let n = fill(&mut free[..room]).min(room);
                (n, n)
            })
            .map_err(|_| SockError::Pipe)?;
        self.socks.counters.tx_bytes += n as u64;
        Ok(n)
    }

    /// Offer the queued part of stream `id`'s receive buffer (one contiguous
    /// run of it, at most `max` bytes) to `take`, which returns how many it
    /// consumed; those are dequeued. Errors as `socket_recv`.
    pub fn socket_recv_with(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
        take: impl FnOnce(&[u8]) -> usize,
    ) -> Result<Received, SockError> {
        let (handle, shut_read) = self.stream_handle(id, owner, true)?;
        if shut_read {
            return Ok(Received::End);
        }
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        if socket.can_recv() {
            let n = socket
                .recv(|queued| {
                    let offer = queued.len().min(max);
                    let n = take(&queued[..offer]).min(offer);
                    (n, n)
                })
                .map_err(|_| SockError::Reset)?;
            self.socks.counters.rx_bytes += n as u64;
            return Ok(Received::Data(n));
        }
        if !socket.may_recv() {
            return Ok(Received::End);
        }
        Ok(Received::Empty)
    }
}
