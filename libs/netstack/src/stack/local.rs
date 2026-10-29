//! Addresses: what a socket reports as its own and its peer's, and the
//! loopback stack (`Net`'s `lo`), which holds 127.0.0.1 and, as /32s, the
//! addresses of the machine's other interfaces, so that a connection to this
//! machine's own address never leaves it.
//!
//! The loopback stack is an ordinary [`Stack`] over a
//! [`RingDevice::loopback`]: an IP interface (no Ethernet header, no ARP,
//! whose requests smoltcp would hold to one a second) that receives what it
//! sends, so TCP and UDP run through the same smoltcp code as on a card.

use alloc::vec::Vec;

use smoltcp::iface::Interface;
use smoltcp::socket::tcp;
use smoltcp::socket::udp as udp_socket;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};

use super::sockets::{Inner, SockAddr, SockError};
use super::tcp::sock_addr;
use super::Stack;
use crate::config::{Mode, StaticConfig};
use crate::device::RingDevice;
use framering::MAX_FRAME;

/// The loopback address. The rest of 127/8 is not served: `lo` holds this
/// one address of it (smoltcp matches destinations exactly).
pub const LOOPBACK_ADDR: [u8; 4] = [127, 0, 0, 1];

/// Addresses smoltcp lets one interface hold (`iface-max-addr-count-8`):
/// 127.0.0.1 and up to seven of the cards'.
pub const MAX_LOCAL_ADDRS: usize = 8;

impl Stack {
    /// The loopback stack: 127.0.0.1/8 over a device that receives what it
    /// sends. The largest frame a ring slot could hold keeps segments big.
    pub fn loopback(seed: u64, now_ms: i64) -> Stack {
        let mode = Mode::Static(StaticConfig {
            addr: LOOPBACK_ADDR,
            prefix_len: 8,
            gateway: None,
            dns: None,
        });
        Stack::with_hardware(
            RingDevice::loopback(MAX_FRAME),
            HardwareAddress::Ip,
            seed,
            now_ms,
            &mode,
        )
    }

    /// Whether this is the loopback stack.
    pub fn is_loopback(&self) -> bool {
        self.device.is_loopback()
    }

    /// Frames the loopback stack sent itself and has yet to take in: it
    /// needs another `poll` at once.
    pub fn looped_frames(&self) -> usize {
        self.device.looped_frames()
    }

    /// Make the loopback stack answer for `addrs` (this machine's interface
    /// addresses) besides 127.0.0.1; only the first [`MAX_LOCAL_ADDRS`] - 1
    /// fit. Nothing changes when the set is the same.
    pub fn set_local_addrs(&mut self, addrs: &[[u8; 4]]) {
        let mut wanted: Vec<IpCidr> = Vec::with_capacity(MAX_LOCAL_ADDRS);
        wanted.push(cidr(LOOPBACK_ADDR, 8));
        for addr in addrs.iter().filter(|a| **a != LOOPBACK_ADDR) {
            if wanted.len() == MAX_LOCAL_ADDRS {
                break;
            }
            let addr = cidr(*addr, 32);
            if !wanted.contains(&addr) {
                wanted.push(addr);
            }
        }
        if self.iface.ip_addrs() == wanted.as_slice() {
            return;
        }
        set_addrs(&mut self.iface, &wanted);
    }

    /// Close `id` with a reset rather than a FIN (its address is gone, so a
    /// FIN could never be answered).
    pub fn socket_abort(&mut self, id: u32, owner: u64) -> Result<(), SockError> {
        if let Inner::Tcp { handle, .. } = &self.socks.entry(id, owner)?.inner {
            self.sockets.get_mut::<tcp::Socket>(*handle).abort();
        }
        self.socket_close(id, owner, 0)
    }

    /// Whether `addr` is one this loopback stack answers for.
    pub(super) fn local_check(&self, addr: [u8; 4]) -> Result<(), SockError> {
        if self.is_loopback() && self.iface.has_ip_addr(Ipv4Address::from(addr)) {
            Ok(())
        } else {
            Err(SockError::Unreachable)
        }
    }

    /// `LocalAddr`: our address and the socket's port.
    pub fn socket_local_addr(&mut self, id: u32, owner: u64) -> Result<SockAddr, SockError> {
        let ours = self.state.addr.unwrap_or([0; 4]);
        let entry = self.socks.entry(id, owner)?;
        let (addr, port) = match &entry.inner {
            Inner::Tcp { handle, state } => {
                match self.sockets.get::<tcp::Socket>(*handle).local_endpoint() {
                    Some(ep) => return Ok(sock_addr(ep)),
                    None => (
                        state.bound_addr,
                        state.bound_port.ok_or(SockError::InvalidState)?,
                    ),
                }
            }
            Inner::Listener { port, addr, .. } => (*addr, *port),
            Inner::Udp { handle, peer } => {
                let ep = self.sockets.get::<udp_socket::Socket>(*handle).endpoint();
                if ep.port == 0 {
                    return Err(SockError::InvalidState);
                }
                let addr = ep.addr.map(|IpAddress::Ipv4(addr)| addr.octets());
                // On `lo` a datagram leaves from the address its peer is:
                // smoltcp picks the one whose /32 holds the destination.
                let from_peer = peer
                    .map(|p| p.addr)
                    .filter(|a| self.is_loopback() && self.local_check(*a).is_ok());
                (addr.or(from_peer), ep.port)
            }
        };
        Ok(SockAddr {
            addr: addr.unwrap_or(ours),
            port,
        })
    }

    /// `PeerAddr`: who the socket is connected to.
    pub fn socket_peer_addr(&mut self, id: u32, owner: u64) -> Result<SockAddr, SockError> {
        let entry = self.socks.entry(id, owner)?;
        match &entry.inner {
            Inner::Tcp { handle, .. } => self
                .sockets
                .get::<tcp::Socket>(*handle)
                .remote_endpoint()
                .map(sock_addr)
                .ok_or(SockError::NotConnected),
            Inner::Udp { peer, .. } => peer.ok_or(SockError::NotConnected),
            Inner::Listener { .. } => Err(SockError::NotConnected),
        }
    }
}

fn cidr(addr: [u8; 4], prefix_len: u8) -> IpCidr {
    IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::from(addr), prefix_len))
}

fn set_addrs(iface: &mut Interface, wanted: &[IpCidr]) {
    iface.update_ip_addrs(|addrs| {
        addrs.clear();
        for addr in wanted {
            let _ = addrs.push(*addr);
        }
    });
}
