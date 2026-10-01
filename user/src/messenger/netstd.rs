//! `TcpStream`, `TcpListener` and `UdpSocket`, named after `std::net`, over
//! the socket service ([`netsock`](super::netsock)). They give the tools
//! (`nc`, `ftp`) conventional shapes and a future native `std` port an obvious
//! mapping.
//!
//! Every type owns one socket id and closes it on drop, so a tool that returns
//! early never leaks a slot in `netd`'s table (and `netd` reclaims whatever a
//! crashed tool leaves behind).
//!
//! Timeouts are explicit milliseconds: a read that times out is
//! `Err(Errno(-ETIMEDOUT))`, which a caller treats as "nothing yet", not a
//! failure of the socket.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::netsock::{wire, Addr, Client, MAX_CHUNK};
use super::{errno, Error, Result};

/// Whether `error` is only a wait that ran out.
pub fn is_timeout(error: &Error) -> bool {
    matches!(error, Error::Errno(code) if *code == -errno::ETIMEDOUT)
}

/// Parse `a.b.c.d`.
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse::<u16>().ok().filter(|n| *n <= 255)? as u8;
    }
    parts.next().is_none().then_some(octets)
}

/// A connected TCP socket.
pub struct TcpStream {
    client: Rc<Client>,
    sock: u32,
}

impl TcpStream {
    /// Connect to `addr`, waiting at most `timeout_ms`.
    pub fn connect(client: &Rc<Client>, addr: Addr, timeout_ms: u32) -> Result<TcpStream> {
        let sock = client.open(wire::SOCK_KIND_STREAM)?;
        let stream = TcpStream {
            client: client.clone(),
            sock,
        };
        client.connect_to(sock, addr, timeout_ms)?;
        Ok(stream)
    }

    fn adopt(client: &Rc<Client>, sock: u32) -> TcpStream {
        TcpStream {
            client: client.clone(),
            sock,
        }
    }

    pub fn id(&self) -> u32 {
        self.sock
    }

    /// Read up to `max` bytes; empty means the peer closed its side.
    pub fn read(&self, max: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        self.client.recv(self.sock, max, timeout_ms)
    }

    /// Queue as much of `data` as fits right now (at most one chunk): how many
    /// bytes, waiting at most `timeout_ms` for room.
    pub fn write_some(&self, data: &[u8], timeout_ms: u32) -> Result<usize> {
        self.client.send(self.sock, data, timeout_ms)
    }

    /// Queue every byte of `data`, in chunks, waiting at most `timeout_ms`
    /// for room each time.
    pub fn write_all(&self, data: &[u8], timeout_ms: u32) -> Result<()> {
        let mut rest = data;
        while !rest.is_empty() {
            let n = self
                .client
                .send(self.sock, &rest[..rest.len().min(MAX_CHUNK)], timeout_ms)?;
            if n == 0 {
                return Err(Error::Errno(-errno::EIO));
            }
            rest = &rest[n..];
        }
        Ok(())
    }

    /// End the sending direction (the peer reads the end of the stream).
    pub fn shutdown_write(&self) -> Result<()> {
        self.client.shutdown(self.sock, wire::SHUTDOWN_WRITE)
    }

    pub fn local_addr(&self) -> Result<Addr> {
        self.client.local_addr(self.sock)
    }

    pub fn peer_addr(&self) -> Result<Addr> {
        self.client.peer_addr(self.sock)
    }

    /// Wait until the socket is readable or closed (or `timeout_ms` passes).
    pub fn wait_readable(&self, timeout_ms: u32) -> Result<bool> {
        let ready = self
            .client
            .poll(self.sock, 1 << wire::READY_READABLE, timeout_ms)?;
        Ok(ready != 0)
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let _ = self.client.close(self.sock);
    }
}

/// A listening TCP socket.
pub struct TcpListener {
    client: Rc<Client>,
    sock: u32,
}

impl TcpListener {
    /// Listen on `port` (0: an ephemeral one) with room for `backlog`
    /// connections.
    pub fn bind(client: &Rc<Client>, port: u16, backlog: u32) -> Result<TcpListener> {
        let sock = client.open(wire::SOCK_KIND_STREAM)?;
        let listener = TcpListener {
            client: client.clone(),
            sock,
        };
        client.bind(sock, Addr::new([0; 4], port))?;
        client.listen(sock, backlog)?;
        Ok(listener)
    }

    pub fn local_addr(&self) -> Result<Addr> {
        self.client.local_addr(self.sock)
    }

    /// The next connection, waiting at most `timeout_ms`.
    pub fn accept(&self, timeout_ms: u32) -> Result<(TcpStream, Addr)> {
        let (sock, peer) = self.client.accept(self.sock, timeout_ms)?;
        Ok((TcpStream::adopt(&self.client, sock), peer))
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let _ = self.client.close(self.sock);
    }
}

/// A UDP socket.
pub struct UdpSocket {
    client: Rc<Client>,
    sock: u32,
}

impl UdpSocket {
    /// A socket bound to `port` (0: an ephemeral one).
    pub fn bind(client: &Rc<Client>, port: u16) -> Result<UdpSocket> {
        let sock = client.open(wire::SOCK_KIND_DATAGRAM)?;
        let socket = UdpSocket {
            client: client.clone(),
            sock,
        };
        client.bind(sock, Addr::new([0; 4], port))?;
        Ok(socket)
    }

    pub fn local_addr(&self) -> Result<Addr> {
        self.client.local_addr(self.sock)
    }

    pub fn send_to(&self, data: &[u8], to: Addr) -> Result<usize> {
        self.client.send_to(self.sock, to, data)
    }

    pub fn recv_from(&self, max: usize, timeout_ms: u32) -> Result<(Vec<u8>, Addr)> {
        self.client.recv_from(self.sock, max, timeout_ms)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let _ = self.client.close(self.sock);
    }
}
