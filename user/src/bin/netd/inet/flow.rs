//! The byte-moving half of the `AF_INET` pump: streams, datagrams and the
//! flush of a closing socket. See the parent module for the whole picture.

use alloc::vec;
use alloc::vec::Vec;

use netstack::{Kind, SockAddr, SockError, Stack};
use user::sys::{self, InetIo};

use super::{net_errno, Entry, Inet, CHUNK, CLOSE_LINGER_MS, FRAME};

impl Inet {
    /// Move stream bytes both ways for the entry at `at`.
    pub(super) fn service_stream(&mut self, stack: &mut Stack, at: usize) {
        let entry = &mut self.entries[at];
        let (id, owner) = (entry.id, entry.owner());
        let Some(sid) = entry.stack else { return };
        // network to application
        if entry.rx.is_empty() && !entry.net_end {
            match stack.socket_recv(sid, owner, CHUNK) {
                Ok(Some(data)) if data.is_empty() => entry.net_end = true,
                Ok(Some(data)) => {
                    self.stats.from_stack += data.len() as u64;
                    entry.rx = data;
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = sys::inet_error(id, net_errno(e));
                    entry.net_end = true;
                    entry.net_end_told = true;
                }
            }
        }
        if !entry.rx.is_empty() {
            match sys::inet_write(id, &entry.rx) {
                Ok(InetIo::Data(n)) => {
                    self.stats.to_app += n as u64;
                    entry.rx.drain(..n);
                }
                Ok(InetIo::End) => entry.rx.clear(),
                _ => {}
            }
        }
        if entry.net_end && entry.rx.is_empty() && !entry.net_end_told {
            let _ = sys::inet_eof(id);
            entry.net_end_told = true;
        }
        // application to network
        if entry.tx.is_empty() && !entry.app_end {
            let mut buf = vec![0u8; CHUNK];
            match sys::inet_read(id, &mut buf) {
                Ok(InetIo::Data(n)) => {
                    self.stats.from_app += n as u64;
                    buf.truncate(n);
                    entry.tx = buf;
                }
                Ok(InetIo::End) => entry.app_end = true,
                _ => {}
            }
        }
        if !entry.tx.is_empty() {
            match stack.socket_send(sid, owner, &entry.tx) {
                Ok(n) => {
                    self.stats.to_stack += n as u64;
                    entry.tx.drain(..n);
                }
                Err(SockError::WouldBlock) => {}
                Err(e) => {
                    let _ = sys::inet_error(id, net_errno(e));
                    entry.tx.clear();
                    entry.net_end = true;
                    entry.net_end_told = true;
                }
            }
        }
        if entry.tx.is_empty() && entry.app_end && !entry.shut {
            let _ = stack.socket_shutdown(sid, owner, false, true);
            entry.shut = true;
        }
    }

    /// Move datagrams both ways for the entry at `at`.
    pub(super) fn service_datagrams(&mut self, stack: &mut Stack, at: usize) {
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
                Ok(InetIo::Data(_)) | Ok(InetIo::End) => entry.rx.clear(),
                _ => {}
            }
        }
        if entry.tx.is_empty() {
            let mut buf = vec![0u8; FRAME + 2];
            if let Ok(InetIo::Data(n)) = sys::inet_read(id, &mut buf) {
                buf.truncate(n);
                entry.tx = buf;
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
        stack: &mut Stack,
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
        let mut drained = entry.tx.is_empty();
        if entry.kind == Kind::Stream && entry.phase_is_flushable() {
            if !entry.tx.is_empty() {
                if let Ok(n) = stack.socket_send(sid, owner, &entry.tx) {
                    entry.tx.drain(..n);
                }
                drained = entry.tx.is_empty();
            }
            while drained && !entry.app_end {
                let mut buf = vec![0u8; CHUNK];
                match sys::inet_read(id, &mut buf) {
                    Ok(InetIo::Data(n)) => {
                        buf.truncate(n);
                        entry.tx = buf;
                        if let Ok(sent) = stack.socket_send(sid, owner, &entry.tx) {
                            entry.tx.drain(..sent);
                        }
                        drained = entry.tx.is_empty();
                    }
                    Ok(InetIo::End) => entry.app_end = true,
                    _ => break,
                }
            }
            drained = drained && entry.tx.is_empty();
        }
        let done = (drained && (entry.app_end || entry.kind == Kind::Datagram))
            || now_ms - since > CLOSE_LINGER_MS;
        if !done {
            return false;
        }
        let _ = stack.socket_close(sid, owner, now_ms);
        let _ = sys::inet_close_ack(id);
        true
    }
}

impl Entry {
    /// Whether the stack socket can still take the application's last bytes.
    fn phase_is_flushable(&self) -> bool {
        self.stack.is_some()
    }
}
