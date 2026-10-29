//! Stream sockets: connect, listen and accept, send and receive, shutdown.
//!
//! Every operation names a socket id and the caller (the kernel-stamped
//! sender `netd` passes through); the table refuses anyone but the owner.
//! Nothing here blocks: an operation that cannot finish yet returns
//! [`SockError::WouldBlock`] (or `Ok(None)`) and `netd` parks the call and
//! asks again after the next poll.
//!
//! A listener holds `backlog` smoltcp sockets listening on the same port, so
//! that many connections can complete before `Accept` collects them; each
//! accepted one becomes a socket of its own and a fresh listening socket takes
//! its place (smoltcp has one connection per listening socket).

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::socket::tcp;
use smoltcp::socket::udp as udp_socket;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};

use super::sockets::{
    new_tcp_socket, ready, Inner, Kind, SockAddr, SockError, MAX_BACKLOG, MAX_CHUNK,
};
use super::Stack;
use crate::config::{is_usable_unicast, same_subnet};

fn endpoint(addr: SockAddr) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(addr.addr)), addr.port)
}

/// What a socket bound to `addr` (`None`: any) and `port` listens on.
pub(super) fn listen_endpoint(addr: Option<[u8; 4]>, port: u16) -> IpListenEndpoint {
    IpListenEndpoint {
        addr: addr.map(|addr| IpAddress::Ipv4(Ipv4Address::from(addr))),
        port,
    }
}

pub(super) fn sock_addr(ep: IpEndpoint) -> SockAddr {
    let IpAddress::Ipv4(addr) = ep.addr;
    SockAddr {
        addr: addr.octets(),
        port: ep.port,
    }
}

/// Whether a stream is done receiving: the peer's FIN arrived and the
/// receive queue is drained, or it was reset.
fn at_end(socket: &tcp::Socket) -> bool {
    !socket.can_recv() && !socket.may_recv()
}

/// A backlog socket holding a connection a client may take.
fn connection_ready(socket: &tcp::Socket) -> bool {
    !matches!(
        socket.state(),
        tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed
    )
}

impl Stack {
    /// Check a destination is somewhere we can send: a unicast host, and
    /// either on-link or behind the gateway.
    pub(super) fn route_check(&self, dst: [u8; 4]) -> Result<(), SockError> {
        if self.is_loopback() {
            return self.local_check(dst);
        }
        if !is_usable_unicast(dst) || dst == [255; 4] {
            return Err(SockError::BadAddress);
        }
        let Some(addr) = self.state.addr else {
            return Err(SockError::Unreachable);
        };
        if !same_subnet(dst, addr, self.state.prefix_len) && self.state.gateway.is_none() {
            return Err(SockError::Unreachable);
        }
        Ok(())
    }

    /// `Open`: a new socket of `kind` for `owner`.
    pub fn socket_open(&mut self, owner: u64, kind: Kind) -> Result<u32, SockError> {
        self.socks.open(&mut self.sockets, owner, kind)
    }

    /// `Bind`: choose the local port (and check the address is ours or any).
    pub fn socket_bind(&mut self, id: u32, owner: u64, local: SockAddr) -> Result<(), SockError> {
        let bound_addr = (local.addr != [0; 4]).then_some(local.addr);
        if let Some(addr) = bound_addr {
            if Some(addr) != self.state.addr && self.local_check(addr).is_err() {
                return Err(SockError::BadAddress);
            }
        }
        let kind = self.socks.entry(id, owner)?.kind();
        let port = self.socks.check_port(&self.sockets, kind, local.port)?;
        let entry = self.socks.entry(id, owner)?;
        match &mut entry.inner {
            Inner::Tcp { state, .. } => {
                if state.connecting || state.established || state.bound_port.is_some() {
                    return Err(SockError::InvalidState);
                }
                let port = match port {
                    Some(port) => port,
                    None => self
                        .socks
                        .pick_port(&self.sockets, Kind::Stream)
                        .ok_or(SockError::AddrInUse)?,
                };
                let Inner::Tcp { state, .. } = &mut self.socks.entry(id, owner)?.inner else {
                    unreachable!()
                };
                state.bound_port = Some(port);
                state.bound_addr = bound_addr;
                Ok(())
            }
            Inner::Listener { .. } => Err(SockError::InvalidState),
            Inner::Udp { .. } => self.udp_bind(id, owner, port, bound_addr),
        }
    }

    /// `Connect` on a stream: start the handshake. Poll
    /// [`Stack::socket_connect_status`] for the outcome.
    pub fn socket_connect(&mut self, id: u32, owner: u64, peer: SockAddr) -> Result<(), SockError> {
        if peer.port == 0 {
            return Err(SockError::BadAddress);
        }
        self.route_check(peer.addr)?;
        let entry = self.socks.entry(id, owner)?;
        let (handle, bound, bound_addr) = match &entry.inner {
            Inner::Tcp { handle, state } => {
                if state.connecting || state.established || state.refused {
                    return Err(SockError::InvalidState);
                }
                (*handle, state.bound_port, state.bound_addr)
            }
            Inner::Udp { .. } => return self.udp_connect(id, owner, peer),
            Inner::Listener { .. } => return Err(SockError::InvalidState),
        };
        let local = match bound {
            Some(port) => port,
            None => self
                .socks
                .pick_port(&self.sockets, Kind::Stream)
                .ok_or(SockError::AddrInUse)?,
        };
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        socket
            .connect(
                self.iface.context(),
                endpoint(peer),
                listen_endpoint(bound_addr, local),
            )
            .map_err(|_| SockError::InvalidState)?;
        let Inner::Tcp { state, .. } = &mut self.socks.entry(id, owner)?.inner else {
            unreachable!()
        };
        state.connecting = true;
        state.bound_port = Some(local);
        Ok(())
    }

    /// Whether a `Connect` finished: `Ok(true)` established, `Ok(false)` still
    /// trying, `Err(Refused)` reset by the peer.
    pub fn socket_connect_status(&mut self, id: u32, owner: u64) -> Result<bool, SockError> {
        let entry = self.socks.entry(id, owner)?;
        match &entry.inner {
            Inner::Tcp { state, .. } => {
                if state.established {
                    Ok(true)
                } else if state.refused {
                    Err(SockError::Refused)
                } else if state.connecting {
                    Ok(false)
                } else {
                    Err(SockError::NotConnected)
                }
            }
            Inner::Udp { peer, .. } => peer.map(|_| true).ok_or(SockError::NotConnected),
            Inner::Listener { .. } => Err(SockError::InvalidState),
        }
    }

    /// `Listen`: turn a bound (or fresh) stream socket into a listener.
    pub fn socket_listen(&mut self, id: u32, owner: u64, backlog: u32) -> Result<(), SockError> {
        if !(1..=MAX_BACKLOG as u32).contains(&backlog) {
            return Err(SockError::BadAddress);
        }
        let entry = self.socks.entry(id, owner)?;
        let (handle, bound, addr) = match &entry.inner {
            Inner::Tcp { handle, state } => {
                if state.connecting || state.established || state.refused {
                    return Err(SockError::InvalidState);
                }
                (*handle, state.bound_port, state.bound_addr)
            }
            _ => return Err(SockError::InvalidState),
        };
        let port = match bound {
            Some(port) => port,
            None => self
                .socks
                .pick_port(&self.sockets, Kind::Stream)
                .ok_or(SockError::AddrInUse)?,
        };
        let listen_on = listen_endpoint(addr, port);
        self.sockets
            .get_mut::<tcp::Socket>(handle)
            .listen(listen_on)
            .map_err(|_| SockError::InvalidState)?;
        let mut handles = vec![handle];
        for _ in 1..backlog {
            let mut socket = new_tcp_socket();
            let _ = socket.listen(listen_on);
            handles.push(self.sockets.add(socket));
        }
        self.socks.entry(id, owner)?.inner = Inner::Listener {
            port,
            addr,
            backlog: handles,
        };
        Ok(())
    }

    /// `Accept`: one established connection as a new socket of `owner`, or
    /// `Ok(None)` when none is waiting.
    pub fn socket_accept(
        &mut self,
        id: u32,
        owner: u64,
    ) -> Result<Option<(u32, SockAddr)>, SockError> {
        self.socket_accept_as(id, owner, owner)
    }

    /// [`Stack::socket_accept`] with the new socket given to `new_owner`
    /// (the kernel's `AF_INET` pump keeps one owner per socket, so a busy
    /// listener's connections do not share its quota).
    pub fn socket_accept_as(
        &mut self,
        id: u32,
        owner: u64,
        new_owner: u64,
    ) -> Result<Option<(u32, SockAddr)>, SockError> {
        let entry = self.socks.entry(id, owner)?;
        let Inner::Listener {
            port,
            addr,
            backlog,
        } = &entry.inner
        else {
            return Err(SockError::InvalidState);
        };
        let (port, addr) = (*port, *addr);
        let Some((at, handle)) = backlog
            .iter()
            .copied()
            .enumerate()
            .find(|(_, h)| connection_ready(self.sockets.get::<tcp::Socket>(*h)))
        else {
            return Ok(None);
        };
        // The connection stays in the backlog until the caller has room.
        self.socks.check_quota(new_owner)?;
        let peer = self
            .sockets
            .get::<tcp::Socket>(handle)
            .remote_endpoint()
            .map(sock_addr)
            .ok_or(SockError::InvalidState)?;
        let mut fresh = new_tcp_socket();
        let _ = fresh.listen(listen_endpoint(addr, port));
        let replacement = self.sockets.add(fresh);
        let Inner::Listener { backlog, .. } = &mut self.socks.entry(id, owner)?.inner else {
            unreachable!()
        };
        backlog[at] = replacement;
        let state = super::sockets::StreamState {
            bound_port: Some(port),
            established: true,
            ..Default::default()
        };
        let conn = self.socks.insert(new_owner, Inner::Tcp { handle, state });
        self.socks.counters.accepted += 1;
        Ok(Some((conn, peer)))
    }

    /// `Send` on a stream: how many bytes were queued (`Ok(0)` never happens:
    /// a full buffer is `WouldBlock`).
    pub fn socket_send(&mut self, id: u32, owner: u64, data: &[u8]) -> Result<usize, SockError> {
        if data.is_empty() || data.len() > MAX_CHUNK {
            return Err(SockError::BadAddress);
        }
        let entry = self.socks.entry(id, owner)?;
        let handle = match &entry.inner {
            Inner::Tcp { handle, state } => {
                Stack::stream_io_check(state)?;
                if state.shut_write {
                    return Err(SockError::Pipe);
                }
                *handle
            }
            Inner::Udp { .. } => return self.udp_send(id, owner, data),
            Inner::Listener { .. } => return Err(SockError::InvalidState),
        };
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        if !socket.may_send() {
            return Err(SockError::Pipe);
        }
        match socket.send_slice(data) {
            Ok(0) => Err(SockError::WouldBlock),
            Ok(n) => {
                self.socks.counters.tx_bytes += n as u64;
                Ok(n)
            }
            Err(_) => Err(SockError::Pipe),
        }
    }

    /// What every stream I/O call checks first.
    pub(super) fn stream_io_check(state: &super::sockets::StreamState) -> Result<(), SockError> {
        if state.reset {
            return Err(SockError::Reset);
        }
        if state.refused {
            return Err(SockError::Refused);
        }
        if !state.established {
            return Err(if state.connecting {
                SockError::WouldBlock
            } else {
                SockError::NotConnected
            });
        }
        Ok(())
    }

    /// `Recv` on a stream: up to `max` bytes, an empty vector at the end of
    /// the stream, `Ok(None)` when nothing is there yet.
    pub fn socket_recv(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<Vec<u8>>, SockError> {
        if max == 0 || max > MAX_CHUNK {
            return Err(SockError::BadAddress);
        }
        let entry = self.socks.entry(id, owner)?;
        let (handle, shut_read) = match &entry.inner {
            Inner::Tcp { handle, state } => {
                if state.reset && !state.fin_seen {
                    return Err(SockError::Reset);
                }
                Stack::stream_io_check(state)?;
                (*handle, state.shut_read)
            }
            Inner::Udp { .. } => {
                return Ok(self.udp_recv(id, owner, max)?.map(|(data, _)| data));
            }
            Inner::Listener { .. } => return Err(SockError::InvalidState),
        };
        if shut_read {
            return Ok(Some(Vec::new()));
        }
        let socket = self.sockets.get_mut::<tcp::Socket>(handle);
        if socket.can_recv() {
            let mut buf = vec![0u8; max];
            let n = socket.recv_slice(&mut buf).map_err(|_| SockError::Reset)?;
            buf.truncate(n);
            self.socks.counters.rx_bytes += n as u64;
            return Ok(Some(buf));
        }
        if at_end(socket) {
            return Ok(Some(Vec::new()));
        }
        Ok(None)
    }

    /// `Shutdown`: end reading (`read`), writing (`write`) or both.
    pub fn socket_shutdown(
        &mut self,
        id: u32,
        owner: u64,
        read: bool,
        write: bool,
    ) -> Result<(), SockError> {
        let entry = self.socks.entry(id, owner)?;
        let Inner::Tcp { handle, state } = &mut entry.inner else {
            return Err(SockError::InvalidState);
        };
        if !state.established {
            return Err(SockError::NotConnected);
        }
        state.shut_read |= read;
        if write && !state.shut_write {
            state.shut_write = true;
            self.sockets.get_mut::<tcp::Socket>(*handle).close();
        }
        Ok(())
    }

    /// `Poll`: the readiness bits of a socket right now.
    pub fn socket_readiness(&mut self, id: u32, owner: u64) -> Result<u32, SockError> {
        let entry = self.socks.entry(id, owner)?;
        let mut bits = 0;
        match &entry.inner {
            Inner::Tcp { handle, state } => {
                let socket = self.sockets.get::<tcp::Socket>(*handle);
                if state.refused {
                    bits |= ready::ERROR | ready::CLOSED;
                }
                if state.reset {
                    bits |= ready::CLOSED | ready::READABLE;
                }
                if state.established {
                    if socket.can_recv() || at_end(socket) || state.shut_read {
                        bits |= ready::READABLE;
                    }
                    if socket.can_send() && !state.shut_write {
                        bits |= ready::WRITABLE;
                    }
                    if socket.state() == tcp::State::Closed {
                        bits |= ready::CLOSED;
                    }
                }
            }
            Inner::Listener { backlog, .. } => {
                if backlog
                    .iter()
                    .any(|h| connection_ready(self.sockets.get::<tcp::Socket>(*h)))
                {
                    bits |= ready::ACCEPTABLE;
                }
            }
            Inner::Udp { handle, .. } => {
                let socket = self.sockets.get::<udp_socket::Socket>(*handle);
                if socket.can_recv() {
                    bits |= ready::READABLE;
                }
                if socket.can_send() {
                    bits |= ready::WRITABLE;
                }
            }
        }
        Ok(bits)
    }

    /// `Close`.
    pub fn socket_close(&mut self, id: u32, owner: u64, now_ms: i64) -> Result<(), SockError> {
        self.socks.close(&mut self.sockets, id, owner, now_ms)
    }

    /// Reclaim everything `owner` holds (it exited); how many sockets.
    pub fn sockets_close_owner(&mut self, owner: u64, now_ms: i64) -> usize {
        self.socks.close_owner(&mut self.sockets, owner, now_ms)
    }
}
