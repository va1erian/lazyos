//! Closing and reclaim: releasing a socket (or every socket of an owner that
//! exited), and keeping closed streams until their close handshake is over,
//! bounded by [`MAX_CLOSING`] and [`CLOSING_MS`].

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp;

use super::{Inner, SockError, Sockets, CLOSING_MS, MAX_CLOSING, MAX_SOCKETS};

impl Sockets {
    /// Release the socket `id` of `owner`.
    pub(in crate::stack) fn close(
        &mut self,
        sockets: &mut SocketSet<'static>,
        id: u32,
        owner: u64,
        now_ms: i64,
    ) -> Result<(), SockError> {
        self.entry(id, owner)?;
        let entry = self.slots[Sockets::slot_of(id)].take().expect("checked");
        self.release(sockets, entry.inner, now_ms);
        self.counters.closed += 1;
        Ok(())
    }

    /// Release every socket `owner` holds (it exited); how many there were.
    pub fn close_owner(
        &mut self,
        sockets: &mut SocketSet<'static>,
        owner: u64,
        now_ms: i64,
    ) -> usize {
        let mut reclaimed = 0;
        for slot in 0..MAX_SOCKETS {
            if self.slots[slot].as_ref().is_some_and(|e| e.owner == owner) {
                let entry = self.slots[slot].take().expect("checked");
                self.release(sockets, entry.inner, now_ms);
                reclaimed += 1;
            }
        }
        self.counters.reclaimed += reclaimed as u64;
        reclaimed
    }

    /// Give the smoltcp sockets behind `inner` back: a stream that is on the
    /// wire closes gracefully in the background, everything else goes now.
    fn release(&mut self, sockets: &mut SocketSet<'static>, inner: Inner, now_ms: i64) {
        match inner {
            Inner::Tcp { handle, .. } => self.retire(sockets, handle, now_ms),
            Inner::Listener { backlog, .. } => {
                for handle in backlog {
                    // A connection the owner never accepted is reset, not
                    // left half-open for the peer to wait on.
                    let socket = sockets.get_mut::<tcp::Socket>(handle);
                    if socket.state() != tcp::State::Listen {
                        socket.abort();
                    }
                    drop(sockets.remove(handle));
                }
            }
            Inner::Udp { handle, .. } => drop(sockets.remove(handle)),
        }
    }

    /// Close a stream socket and keep it until the wire is done with it.
    pub(in crate::stack) fn retire(
        &mut self,
        sockets: &mut SocketSet<'static>,
        handle: SocketHandle,
        now_ms: i64,
    ) {
        let socket = sockets.get_mut::<tcp::Socket>(handle);
        match socket.state() {
            tcp::State::Closed | tcp::State::Listen | tcp::State::TimeWait => {
                drop(sockets.remove(handle));
            }
            _ => {
                socket.close();
                if self.closing.len() >= MAX_CLOSING {
                    let (oldest, _) = self.closing.remove(0);
                    sockets.get_mut::<tcp::Socket>(oldest).abort();
                    drop(sockets.remove(oldest));
                }
                self.closing.push((handle, now_ms));
            }
        }
    }

    /// Drop the closing streams the wire has finished with, and abort the
    /// ones a peer keeps waiting.
    pub(in crate::stack) fn reap_closing(&mut self, sockets: &mut SocketSet<'static>, now_ms: i64) {
        let mut i = 0;
        while i < self.closing.len() {
            let (handle, since) = self.closing[i];
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            // TIME-WAIT is over for our purposes: `retire` does not hold a
            // socket there either, and a late segment is answered with a reset.
            let done = matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait);
            let stale = now_ms - since > CLOSING_MS;
            if done || stale {
                if stale && !done {
                    socket.abort();
                }
                drop(sockets.remove(handle));
                self.closing.remove(i);
            } else {
                i += 1;
            }
        }
    }
}
