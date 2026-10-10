//! The stack: a smoltcp [`Interface`] over a [`RingDevice`], a DHCP client, and
//! the ICMP echo path (answering echo requests to us, and sending our own for
//! `ping`).
//!
//! **Hostile network.** Every byte the wire delivers is parsed by smoltcp, which
//! validates as it goes; this layer adds what smoltcp leaves to the caller: an
//! address from DHCP is used only if it is a sane unicast address and subnet, a
//! router only if it is a sane unicast address, an echo reply only if it
//! answers a request we sent (same identifier, a sequence number we are waiting
//! for, from the address we asked, with the payload we sent). Work per `poll` is
//! bounded (a fixed number of ingress and egress steps) whatever arrives.
//! Memory is bounded: a fixed socket set, at most [`MAX_PINGS`] outstanding
//! pings, at most [`MAX_DNS`] resolvers.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{
    Config, Interface, PollIngressSingleResult, PollResult, SocketHandle, SocketSet,
};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address, Ipv4Cidr};

use crate::config::{is_usable_unicast, Mode};
use crate::device::{DeviceStats, RingDevice};

/// Ingress packets processed per `poll`, so a flood cannot hold the loop.
pub const INGRESS_BUDGET: u32 = 256;
/// Egress steps per `poll`.
pub const EGRESS_BUDGET: u32 = 64;
/// Pings outstanding at once.
pub const MAX_PINGS: usize = 8;
/// Resolvers kept from a DHCP lease.
pub const MAX_DNS: usize = 3;
/// Largest echo payload `ping` sends.
pub const MAX_PING_PAYLOAD: usize = 1400;

/// Where the current address came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Dhcp,
    Static,
}

/// The DHCP client as far as the stack can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpState {
    /// Static configuration: no client runs.
    Off,
    /// Looking for, or asking for, a lease.
    Discovering,
    /// A lease is held.
    Bound,
}

/// What the stack holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub addr: Option<[u8; 4]>,
    pub prefix_len: u8,
    pub gateway: Option<[u8; 4]>,
    pub dns: Vec<[u8; 4]>,
    pub source: Source,
    pub dhcp: DhcpState,
    /// Milliseconds (stack clock) when the lease ends, if DHCP told us.
    pub lease_ends_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub leases: u64,
    pub lease_losses: u64,
    pub pings_sent: u64,
    pub pings_answered: u64,
    pub pings_timed_out: u64,
    pub lookups_sent: u64,
    pub lookups_answered: u64,
    pub lookups_failed: u64,
}

mod dns;
mod observe;
mod ping;
mod sockets;
mod route;
mod stream_io;
mod tcp;
mod udp;

use dns::{dns_socket, Lookup};
pub use dns::{valid_host_name, LookupOutcome, LookupResult, ResolveError, MAX_LOOKUPS};
use ping::{icmp_socket, Pending};
pub use ping::{PingError, PingOutcome, PingResult};
pub use sockets::{ready, Kind, SockAddr, SockError, SocketCounters, Sockets};
pub use sockets::{
    EPHEMERAL_FIRST, MAX_BACKLOG, MAX_CHUNK, MAX_CLOSING, MAX_PER_OWNER, MAX_SOCKETS,
    PRIVILEGED_PORTS, TCP_BUFFER, UDP_PAYLOAD,
};
pub use route::RouteClass;
pub use stream_io::Received;

pub struct Stack {
    device: RingDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: Option<SocketHandle>,
    icmp: SocketHandle,
    dns: SocketHandle,
    lookups: Vec<Lookup>,
    lookup_results: Vec<LookupResult>,
    next_lookup: u32,
    socks: Sockets,
    state: State,
    ident: u16,
    next_seq: u16,
    pending: Vec<Pending>,
    results: Vec<PingResult>,
    counters: Counters,
    /// Bumped whenever the address state changes, so a caller can publish it.
    epoch: u64,
    /// The DHCP socket's reply buffer, owned as a raw pointer because the socket
    /// (in `sockets`) holds a `'static` borrow of it; `Drop` removes the socket
    /// and then frees the buffer. See `Stack::new`.
    dhcp_reply: Option<core::ptr::NonNull<[u8]>>,
}

fn octets(address: Ipv4Address) -> [u8; 4] {
    address.octets()
}

impl Drop for Stack {
    fn drop(&mut self) {
        // The socket borrows the reply buffer: it goes first.
        if let Some(handle) = self.dhcp.take() {
            drop(self.sockets.remove(handle));
        }
        if let Some(reply) = self.dhcp_reply.take() {
            // SAFETY: `reply` is the pointer `Box::into_raw` returned in `new`,
            // freed exactly once (`take`), after its only borrower is gone.
            drop(unsafe { alloc::boxed::Box::from_raw(reply.as_ptr()) });
        }
    }
}

impl Stack {
    /// Build the stack over `device`. `seed` seeds smoltcp's initial sequence
    /// numbers and transaction ids (the caller draws it from the kernel
    /// CSPRNG); `now_ms` is the stack clock, milliseconds since any fixed
    /// moment.
    pub fn new(mut device: RingDevice, mac: [u8; 6], seed: u64, now_ms: i64, mode: &Mode) -> Stack {
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed = seed;
        let iface = Interface::new(config, &mut device, Instant::from_millis(now_ms));
        let mut sockets = SocketSet::new(Vec::new());

        // The identifier is ours alone: clients cannot choose it, so they
        // cannot forge another caller's echo.
        let ident = (seed >> 16) as u16 | 1;
        let icmp = sockets.add(icmp_socket(ident));
        let sockets_dns = sockets.add(dns_socket());

        let mut stack = Stack {
            device,
            iface,
            sockets,
            dhcp: None,
            icmp,
            dns: sockets_dns,
            lookups: Vec::new(),
            lookup_results: Vec::new(),
            next_lookup: 1,
            socks: Sockets::new(seed),
            state: State {
                addr: None,
                prefix_len: 0,
                gateway: None,
                dns: Vec::new(),
                source: Source::Dhcp,
                dhcp: DhcpState::Discovering,
                lease_ends_ms: None,
            },
            ident,
            next_seq: (seed >> 32) as u16,
            pending: Vec::new(),
            results: Vec::new(),
            counters: Counters::default(),
            epoch: 0,
            dhcp_reply: None,
        };
        match mode {
            Mode::Dhcp => {
                let mut socket = dhcpv4::Socket::new();
                // Without a buffer to keep the last reply in, the socket cannot
                // tell us the lease length. The stack owns the buffer.
                let reply: *mut [u8] =
                    alloc::boxed::Box::into_raw(vec![0u8; 1024].into_boxed_slice());
                // SAFETY: `reply` came from `Box::into_raw`, so it is valid and
                // nothing owns it as a `Box` (no retag can invalidate this
                // borrow when the `Stack` moves). The socket is the only user
                // and `Stack::drop` removes the socket before freeing the
                // buffer, so the `&mut` is unique and outlives every use.
                let borrowed: &'static mut [u8] = unsafe { &mut *reply };
                socket.set_receive_packet_buffer(borrowed);
                stack.dhcp_reply = core::ptr::NonNull::new(reply);
                stack.dhcp = Some(stack.sockets.add(socket));
            }
            Mode::Static(config) => {
                stack.state.source = Source::Static;
                stack.state.dhcp = DhcpState::Off;
                stack.apply(
                    config.addr,
                    config.prefix_len,
                    config.gateway,
                    config.dns.into_iter().collect(),
                    None,
                );
            }
        }
        stack
    }

    pub fn device(&self) -> &RingDevice {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut RingDevice {
        &mut self.device
    }

    pub fn device_stats(&self) -> &DeviceStats {
        self.device.stats()
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Changes whenever the address state does.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Use another hardware address (the NIC driver came back reporting a
    /// different one). Existing neighbour entries stay; they age out.
    pub fn set_mac(&mut self, mac: [u8; 6]) {
        self.iface
            .set_hardware_addr(HardwareAddress::Ethernet(EthernetAddress(mac)));
    }

    pub fn mac(&self) -> [u8; 6] {
        match self.iface.hardware_addr() {
            HardwareAddress::Ethernet(EthernetAddress(mac)) => mac,
        }
    }

    /// Install (or replace) the address, route and resolvers.
    fn apply(
        &mut self,
        addr: [u8; 4],
        prefix_len: u8,
        gateway: Option<[u8; 4]>,
        dns: Vec<[u8; 4]>,
        lease_ends_ms: Option<i64>,
    ) {
        let cidr = Ipv4Cidr::new(Ipv4Address::from(addr), prefix_len);
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            let _ = addrs.push(IpCidr::Ipv4(cidr));
        });
        match gateway {
            Some(gw) => {
                let _ = self
                    .iface
                    .routes_mut()
                    .add_default_ipv4_route(Ipv4Address::from(gw));
            }
            None => {
                self.iface.routes_mut().remove_default_ipv4_route();
            }
        }
        self.state.addr = Some(addr);
        self.state.prefix_len = prefix_len;
        self.state.gateway = gateway;
        self.state.dns = dns;
        self.state.lease_ends_ms = lease_ends_ms;
        self.epoch += 1;
        self.sync_resolvers();
    }

    /// Forget the address (a lease ended).
    fn clear(&mut self) {
        self.iface.update_ip_addrs(|addrs| addrs.clear());
        self.iface.routes_mut().remove_default_ipv4_route();
        self.state.addr = None;
        self.state.prefix_len = 0;
        self.state.gateway = None;
        self.state.dns.clear();
        self.state.lease_ends_ms = None;
        self.epoch += 1;
        self.sync_resolvers();
    }

    /// Ask for a fresh lease: the current one is dropped.
    pub fn renew(&mut self) {
        if let Some(handle) = self.dhcp {
            self.sockets.get_mut::<dhcpv4::Socket>(handle).reset();
            if self.state.addr.is_some() {
                self.counters.lease_losses += 1;
                self.clear();
            }
            self.state.dhcp = DhcpState::Discovering;
        }
    }

    /// One round of work at stack time `now_ms`: maintenance, ingress and egress
    /// (each bounded), then DHCP events and echo replies.
    pub fn poll(&mut self, now_ms: i64) {
        let now = Instant::from_millis(now_ms);
        self.iface.poll_maintenance(now);
        for _ in 0..INGRESS_BUDGET {
            if self
                .iface
                .poll_ingress_single(now, &mut self.device, &mut self.sockets)
                == PollIngressSingleResult::None
            {
                break;
            }
        }
        for _ in 0..EGRESS_BUDGET {
            if self
                .iface
                .poll_egress(now, &mut self.device, &mut self.sockets)
                == PollResult::None
            {
                break;
            }
        }
        self.handle_dhcp(now_ms);
        self.handle_icmp(now_ms);
        self.handle_dns(now_ms);
        self.observe_streams(now_ms);
    }

    /// Milliseconds until the stack next needs a `poll` even if nothing
    /// arrives (`None`: nothing scheduled).
    pub fn poll_delay_ms(&mut self, now_ms: i64) -> Option<u64> {
        let now = Instant::from_millis(now_ms);
        let stack = self
            .iface
            .poll_delay(now, &self.sockets)
            .map(|d| d.total_millis());
        let ping = self
            .pending
            .iter()
            .map(|p| (p.deadline_ms - now_ms).max(0) as u64)
            .min();
        let lookup = self.lookup_delay_ms(now_ms);
        [stack, ping, lookup].into_iter().flatten().min()
    }

    fn handle_dhcp(&mut self, now_ms: i64) {
        let Some(handle) = self.dhcp else { return };
        let event = match self.sockets.get_mut::<dhcpv4::Socket>(handle).poll() {
            Some(dhcpv4::Event::Configured(config)) => {
                let address = config.address;
                let router = config.router;
                let dns: Vec<Ipv4Address> = config.dns_servers.iter().copied().collect();
                let lease = config
                    .packet
                    .as_ref()
                    .and_then(|packet| smoltcp::wire::DhcpRepr::parse(packet).ok())
                    .and_then(|repr| repr.lease_duration);
                Some(Some((address, router, dns, lease)))
            }
            Some(dhcpv4::Event::Deconfigured) => Some(None),
            None => None,
        };
        match event {
            Some(Some((address, router, dns, lease))) => {
                self.accept_lease(address, router, dns, lease, now_ms)
            }
            Some(None) => {
                if self.state.addr.is_some() {
                    self.counters.lease_losses += 1;
                    self.clear();
                }
                self.state.dhcp = DhcpState::Discovering;
            }
            None => {}
        }
    }

    /// Use a lease from the server only if it is sane.
    fn accept_lease(
        &mut self,
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        dns: Vec<Ipv4Address>,
        lease: Option<u32>,
        now_ms: i64,
    ) {
        let addr = octets(address.address());
        let prefix = address.prefix_len();
        if !is_usable_unicast(addr) || !(1..=30).contains(&prefix) {
            // A hostile or broken server: drop whatever we held (the socket
            // already considers itself configured, so the old address would
            // otherwise outlive the server's say-so) and look again.
            if self.state.addr.is_some() {
                self.counters.lease_losses += 1;
                self.clear();
            }
            if let Some(handle) = self.dhcp {
                self.sockets.get_mut::<dhcpv4::Socket>(handle).reset();
            }
            self.state.dhcp = DhcpState::Discovering;
            return;
        }
        let gateway = router
            .map(octets)
            .filter(|gw| is_usable_unicast(*gw) && *gw != addr);
        let dns: Vec<[u8; 4]> = dns
            .into_iter()
            .map(octets)
            .filter(|d| is_usable_unicast(*d))
            .take(MAX_DNS)
            .collect();
        let lease_ends = lease.map(|secs| now_ms + i64::from(secs) * 1000);
        if self.state.addr.is_none() {
            self.counters.leases += 1;
        }
        self.apply(addr, prefix, gateway, dns, lease_ends);
        self.state.dhcp = DhcpState::Bound;
    }
}
