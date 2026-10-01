//! Datagram sockets: bind, connect (a default peer), `SendTo` and `RecvFrom`.
//!
//! Like the stream operations in `tcp.rs`, nothing blocks: a full send queue
//! is [`SockError::WouldBlock`] and an empty receive queue is `Ok(None)`, and
//! `netd` parks the call. A datagram longer than `max` is cut to `max`, as
//! `recvfrom` does.

use alloc::vec::Vec;

use smoltcp::socket::udp::{self, UdpMetadata};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};

use super::sockets::{Inner, Kind, SockAddr, SockError, UDP_PAYLOAD};
use super::tcp::sock_addr;
use super::Stack;

fn metadata(addr: SockAddr) -> UdpMetadata {
    UdpMetadata::from(IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::from(addr.addr)),
        addr.port,
    ))
}

impl Stack {
    /// The smoltcp handle behind datagram socket `id`, and its default peer.
    fn udp_parts(
        &mut self,
        id: u32,
        owner: u64,
    ) -> Result<(smoltcp::iface::SocketHandle, Option<SockAddr>), SockError> {
        match &self.socks.entry(id, owner)?.inner {
            Inner::Udp { handle, peer } => Ok((*handle, *peer)),
            _ => Err(SockError::InvalidState),
        }
    }

    /// Bind to `port` (`None`: pick an ephemeral one) unless already bound.
    fn udp_ensure_bound(&mut self, id: u32, owner: u64) -> Result<(), SockError> {
        let (handle, _) = self.udp_parts(id, owner)?;
        if self.sockets.get::<udp::Socket>(handle).is_open() {
            return Ok(());
        }
        let port = self
            .socks
            .pick_port(&self.sockets, Kind::Datagram)
            .ok_or(SockError::AddrInUse)?;
        self.udp_listen(handle, port)
    }

    fn udp_listen(
        &mut self,
        handle: smoltcp::iface::SocketHandle,
        port: u16,
    ) -> Result<(), SockError> {
        self.sockets
            .get_mut::<udp::Socket>(handle)
            .bind(IpListenEndpoint { addr: None, port })
            .map_err(|_| SockError::InvalidState)
    }

    /// `Bind` on a datagram socket; `port` is already checked (`None`: pick).
    pub(super) fn udp_bind(
        &mut self,
        id: u32,
        owner: u64,
        port: Option<u16>,
    ) -> Result<(), SockError> {
        let (handle, _) = self.udp_parts(id, owner)?;
        if self.sockets.get::<udp::Socket>(handle).is_open() {
            return Err(SockError::InvalidState);
        }
        let port = match port {
            Some(port) => port,
            None => self
                .socks
                .pick_port(&self.sockets, Kind::Datagram)
                .ok_or(SockError::AddrInUse)?,
        };
        self.udp_listen(handle, port)
    }

    /// `Connect` on a datagram socket: remember the default peer.
    pub(super) fn udp_connect(
        &mut self,
        id: u32,
        owner: u64,
        peer: SockAddr,
    ) -> Result<(), SockError> {
        self.udp_ensure_bound(id, owner)?;
        let Inner::Udp { peer: slot, .. } = &mut self.socks.entry(id, owner)?.inner else {
            return Err(SockError::InvalidState);
        };
        *slot = Some(peer);
        Ok(())
    }

    /// `Send` on a connected datagram socket.
    pub(super) fn udp_send(
        &mut self,
        id: u32,
        owner: u64,
        data: &[u8],
    ) -> Result<usize, SockError> {
        let (_, peer) = self.udp_parts(id, owner)?;
        let peer = peer.ok_or(SockError::NotConnected)?;
        self.socket_sendto(id, owner, peer, data)
    }

    /// `SendTo`: one datagram to `to`. An unbound socket takes an ephemeral
    /// port first.
    pub fn socket_sendto(
        &mut self,
        id: u32,
        owner: u64,
        to: SockAddr,
        data: &[u8],
    ) -> Result<usize, SockError> {
        if data.len() > UDP_PAYLOAD {
            return Err(SockError::MessageSize);
        }
        if to.port == 0 {
            return Err(SockError::BadAddress);
        }
        let (handle, _) = self.udp_parts(id, owner)?;
        self.route_check(to.addr)?;
        self.udp_ensure_bound(id, owner)?;
        match self
            .sockets
            .get_mut::<udp::Socket>(handle)
            .send_slice(data, metadata(to))
        {
            Ok(()) => {
                self.socks.counters.tx_datagrams += 1;
                self.socks.counters.tx_bytes += data.len() as u64;
                Ok(data.len())
            }
            Err(udp::SendError::BufferFull) => Err(SockError::WouldBlock),
            Err(udp::SendError::Unaddressable) => Err(SockError::BadAddress),
        }
    }

    /// `RecvFrom`: the oldest datagram (cut to `max`) and who sent it, or
    /// `Ok(None)` when none is queued. On a connected socket datagrams from
    /// anyone else are dropped.
    pub fn socket_recvfrom(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<(Vec<u8>, SockAddr)>, SockError> {
        if max == 0 || max > UDP_PAYLOAD * 11 {
            return Err(SockError::BadAddress);
        }
        self.udp_recv(id, owner, max)
    }

    pub(super) fn udp_recv(
        &mut self,
        id: u32,
        owner: u64,
        max: usize,
    ) -> Result<Option<(Vec<u8>, SockAddr)>, SockError> {
        let (handle, peer) = self.udp_parts(id, owner)?;
        let socket = self.sockets.get_mut::<udp::Socket>(handle);
        if !socket.is_open() {
            // Nothing can arrive on a socket that has no port.
            return Ok(None);
        }
        loop {
            // `recv` hands back the payload in place, so a datagram longer
            // than `max` is cut here instead of being dropped by the socket.
            let (payload, meta) = match socket.recv() {
                Ok(got) => got,
                Err(_) => return Ok(None),
            };
            let mut buf = payload[..payload.len().min(max)].to_vec();
            let from = sock_addr(meta.endpoint);
            if peer.is_some_and(|p| p != from) {
                continue;
            }
            let buf = core::mem::take(&mut buf);
            self.socks.counters.rx_datagrams += 1;
            self.socks.counters.rx_bytes += buf.len() as u64;
            return Ok(Some((buf, from)));
        }
    }
}
