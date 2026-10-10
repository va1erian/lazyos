//! The byte-moving half of the `AF_INET` pump: streams, datagrams and the
//! flush of a closing socket. See the parent module for the whole picture.
//!
//! **Streams drain in place** (docs/performance-plan.md P4.2). The stack's
//! receive queue is written straight into the kernel ring, and the kernel
//! ring is read straight into the stack's free send space
//! (`Stack::socket_recv_with` / `socket_send_with`), so nothing is staged,
//! nothing is allocated, and bytes leave one side only when the other took
//! them. Each direction loops until one side is exhausted or the socket's
//! [`BUDGET`] for the pass is spent; a pass that moved bytes is followed by
//! another at once (`Inet::pump`), so the budget only orders the sockets,
//! it never stalls one.

use alloc::vec::Vec;

use netstack::{Kind, Net, Received, SockAddr, SockError};
use user::sys::{self, InetIo};

use super::{net_errno, Inet, CLOSE_LINGER_MS, FRAME};

/// Most bytes one stream moves per direction per pass, so a bulk transfer
/// cannot hold the others off for long.
const BUDGET: usize = 256 * 1024;

/// Read the application's bytes into `buf` (the stack's free space); `end`
/// is set when it will send no more.
fn from_app(id: u32, buf: &mut [u8], end: &mut bool) -> usize {
    if buf.is_empty() {
        return 0;
    }
    match sys::inet_read(id, buf) {
        Ok(InetIo::Data(n)) => n,
        Ok(InetIo::End) => {
            *end = true;
            0
        }
        _ => 0,
    }
}

impl Inet {
    /// Move stream bytes both ways for the entry at `at`.
    pub(super) fn service_stream(&mut self, stack: &mut Net, at: usize) {
        let entry = &mut self.entries[at];
        let (id, owner) = (entry.id, entry.owner());
        let Some(sid) = entry.stack else { return };
        // network to application
        let mut moved = 0;
        while !entry.net_end && moved < BUDGET {
            let received = stack.socket_recv_with(sid, owner, BUDGET - moved, |data| {
                match sys::inet_write(id, data) {
                    Ok(InetIo::Data(n)) => n,
                    // The application closed its side: what still arrives is
                    // dropped, as the kernel would for a closed socket.
                    Ok(InetIo::End) => data.len(),
                    _ => 0,
                }
            });
            match received {
                Ok(Received::Data(0)) | Ok(Received::Empty) => break,
                Ok(Received::Data(n)) => moved += n,
                Ok(Received::End) => entry.net_end = true,
                Err(e) => {
                    let _ = sys::inet_error(id, net_errno(e));
                    entry.net_end = true;
                    entry.net_end_told = true;
                }
            }
        }
        self.stats.from_stack += moved as u64;
        self.stats.to_app += moved as u64;
        if entry.net_end && !entry.net_end_told {
            let _ = sys::inet_eof(id);
            entry.net_end_told = true;
        }
        // application to network
        let mut moved = 0;
        while !entry.app_end && moved < BUDGET {
            let mut end = false;
            let sent = stack.socket_send_with(sid, owner, BUDGET - moved, |buf| {
                from_app(id, buf, &mut end)
            });
            entry.app_end |= end;
            match sent {
                Ok(0) | Err(SockError::WouldBlock) => break,
                Ok(n) => moved += n,
                Err(e) => {
                    let _ = sys::inet_error(id, net_errno(e));
                    entry.net_end = true;
                    entry.net_end_told = true;
                    break;
                }
            }
        }
        self.stats.from_app += moved as u64;
        self.stats.to_stack += moved as u64;
        if entry.app_end && !entry.shut {
            let _ = stack.socket_shutdown(sid, owner, false, true);
            entry.shut = true;
        }
    }

    /// Move datagrams both ways for the entry at `at`.
    pub(super) fn service_datagrams(&mut self, stack: &mut Net, at: usize) {
        let entry = &mut self.entries[at];
        let (id, owner) = (entry.id, entry.owner());
        let Some(sid) = entry.stack else { return };
        if entry.rx.is_empty() {
            if let Ok(Some((data, from))) = stack.socket_recvfrom(sid, owner, FRAME) {
                let mut frame = Vec::with_capacity(6 + data.len());
                frame.extend_from_slice(&from.addr);
                frame.extend_from_slice(&from.port.to_be_bytes());
                frame.extend_from_slice(&data);
                self.stats.from_stack += data.len() as u64;
                entry.rx = frame;
            }
        }
        if !entry.rx.is_empty() {
            match sys::inet_write(id, &entry.rx) {
                // A message is delivered whole or not at all.
                Ok(InetIo::Data(_)) | Ok(InetIo::End) => {
                    self.stats.to_app += entry.rx.len() as u64;
                    entry.rx.clear();
                }
                _ => {}
            }
        }
        if entry.tx.is_empty() {
            let buf = &mut self.scratch[..FRAME + 2];
            if let Ok(InetIo::Data(n)) = sys::inet_read(id, buf) {
                self.stats.from_app += n as u64;
                entry.tx = buf[..n].to_vec();
            }
        }
        if !entry.tx.is_empty() {
            let frame = core::mem::take(&mut entry.tx);
            if frame.len() >= 6 {
                let to = SockAddr {
                    addr: [frame[0], frame[1], frame[2], frame[3]],
                    port: u16::from_be_bytes([frame[4], frame[5]]),
                };
                match stack.socket_sendto(sid, owner, to, &frame[6..]) {
                    Ok(n) => self.stats.to_stack += n as u64,
                    Err(SockError::WouldBlock) => entry.tx = frame,
                    // UDP: an undeliverable datagram is dropped.
                    Err(_) => {}
                }
            }
        }
    }

    /// Flush what the application wrote, close the stack socket, acknowledge.
    pub(super) fn service_closing(
        &mut self,
        stack: &mut Net,
        at: usize,
        since: i64,
        now_ms: i64,
    ) -> bool {
        let entry = &mut self.entries[at];
        let (id, owner) = (entry.id, entry.owner());
        if since == i64::MIN {
            // A listener or an unconnected socket: closed already.
            let _ = sys::inet_close_ack(id);
            return true;
        }
        let Some(sid) = entry.stack else {
            let _ = sys::inet_close_ack(id);
            return true;
        };
        if entry.kind == Kind::Stream {
            // The application's last bytes, straight into the stack, until
            // its end of stream; a stack that cannot take more (reset, shut)
            // ends the flush.
            while !entry.app_end {
                let mut end = false;
                let sent =
                    stack.socket_send_with(sid, owner, BUDGET, |buf| from_app(id, buf, &mut end));
                entry.app_end |= end;
                match sent {
                    Ok(n) if n > 0 => self.stats.to_stack += n as u64,
                    Ok(_) => break,
                    // Full, or never connected: a connection still being
                    // made has nothing to flush and is simply closed.
                    Err(SockError::WouldBlock) => {
                        if stack.socket_connect_status(sid, owner) != Ok(true) {
                            entry.app_end = true;
                        }
                        break;
                    }
                    Err(_) => entry.app_end = true,
                }
            }
        }
        let flushed = entry.tx.is_empty() && (entry.app_end || entry.kind == Kind::Datagram);
        if !flushed && now_ms - since <= CLOSE_LINGER_MS {
            return false;
        }
        let _ = stack.socket_close(sid, owner, now_ms);
        let _ = sys::inet_close_ack(id);
        true
    }
}
