//! The WP1 spike (docs/wifi-prerequisites-plan.md section 3.1 and risk 1):
//! what the pinned smoltcp does when two `Interface`s poll one `SocketSet`.
//!
//! The finding these tests keep true: smoltcp has **no binding of a socket to
//! an interface**. Whichever interface polls first takes a ready socket's
//! packet (`Interface::socket_egress` runs `socket.dispatch`, which advances
//! the socket's state, before the interface looks at its own routes), routes it
//! with its own table, and never checks that the source address is one of its
//! own. So a shared set puts one NIC's traffic on the other, and a single DHCP
//! socket serves only one of two interfaces. `netstack::Net` therefore gives
//! every interface its own set and owns the choice of interface itself. If a
//! smoltcp upgrade changes this, these tests fail and the design may simplify.

use std::collections::VecDeque;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address,
};

/// A device that records what it transmits and replays what it is given.
#[derive(Default)]
struct Wire {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        self.0.push(frame);
        result
    }
}

impl Device for Wire {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.rx.pop_front()?;
        Some((Rx(frame), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

const MAC_A: [u8; 6] = [2, 0, 0, 0, 0, 0xa];
const MAC_B: [u8; 6] = [2, 0, 0, 0, 0, 0xb];
const GW_B_MAC: [u8; 6] = [2, 0, 0, 0, 1, 0xb];

/// An interface with `addr`/24 and a default route through `gateway`.
fn interface(wire: &mut Wire, mac: [u8; 6], addr: [u8; 4], gateway: [u8; 4]) -> Interface {
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = 7;
    let mut iface = Interface::new(config, wire, Instant::from_millis(0));
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(
            IpAddress::v4(addr[0], addr[1], addr[2], addr[3]),
            24,
        ));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(
        gateway[0], gateway[1], gateway[2], gateway[3],
    ));
    iface
}

fn udp_socket(local: [u8; 4]) -> udp::Socket<'static> {
    let buffer = || udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]);
    let mut socket = udp::Socket::new(buffer(), buffer());
    socket
        .bind(IpListenEndpoint {
            addr: Some(IpAddress::v4(local[0], local[1], local[2], local[3])),
            port: 5000,
        })
        .unwrap();
    socket
        .send_slice(b"hello", IpEndpoint::new(IpAddress::v4(8, 8, 8, 8), 53))
        .unwrap();
    socket
}

fn ethertype(frame: &[u8]) -> u16 {
    u16::from_be_bytes([frame[12], frame[13]])
}

/// `(source address, destination address, destination port)` of a UDP frame.
fn udp_of(frame: &[u8]) -> Option<([u8; 4], [u8; 4], u16)> {
    if ethertype(frame) != 0x0800 || frame[23] != 17 {
        return None;
    }
    let ihl = usize::from(frame[14] & 0x0f) * 4;
    let port = u16::from_be_bytes([frame[14 + ihl + 2], frame[14 + ihl + 3]]);
    Some((
        frame[26..30].try_into().unwrap(),
        frame[30..34].try_into().unwrap(),
        port,
    ))
}

/// An ARP request's `(target address)`.
fn arp_request_target(frame: &[u8]) -> Option<[u8; 4]> {
    (ethertype(frame) == 0x0806 && frame[20..22] == [0, 1])
        .then(|| frame[38..42].try_into().unwrap())
}

/// An ARP reply from `from_mac`/`from_ip` to `to_mac`/`to_ip`.
fn arp_reply(from_mac: [u8; 6], from_ip: [u8; 4], to_mac: [u8; 6], to_ip: [u8; 4]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&to_mac);
    frame.extend_from_slice(&from_mac);
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 8, 0, 6, 4, 0, 2]);
    frame.extend_from_slice(&from_mac);
    frame.extend_from_slice(&from_ip);
    frame.extend_from_slice(&to_mac);
    frame.extend_from_slice(&to_ip);
    frame
}

#[test]
fn shared_set_sends_one_interfaces_socket_out_of_the_other() {
    let (mut wire_a, mut wire_b) = (Wire::default(), Wire::default());
    let mut a = interface(&mut wire_a, MAC_A, [10, 0, 0, 1], [10, 0, 0, 254]);
    let mut b = interface(&mut wire_b, MAC_B, [10, 0, 1, 1], [10, 0, 1, 254]);
    let mut sockets = SocketSet::new(Vec::new());
    // Bound to A's address: it belongs on A.
    sockets.add(udp_socket([10, 0, 0, 1]));

    // B polls first and takes the socket: it starts resolving *its* gateway.
    let t = Instant::from_millis(10);
    b.poll(t, &mut wire_b, &mut sockets);
    a.poll(t, &mut wire_a, &mut sockets);
    assert_eq!(
        wire_b.tx.iter().find_map(|f| arp_request_target(f)),
        Some([10, 0, 1, 254]),
        "B asked for its own gateway on behalf of A's socket"
    );
    assert!(wire_a.tx.is_empty(), "A never saw its own socket's packet");

    // B's gateway answers and the datagram leaves B with A's source address.
    wire_b
        .rx
        .push_back(arp_reply(GW_B_MAC, [10, 0, 1, 254], MAC_B, [10, 0, 1, 1]));
    b.poll(Instant::from_millis(20), &mut wire_b, &mut sockets);
    let leaked = wire_b.tx.iter().find_map(|f| udp_of(f));
    assert_eq!(leaked, Some(([10, 0, 0, 1], [8, 8, 8, 8], 53)));
}

#[test]
fn separate_sets_keep_each_socket_on_its_interface() {
    let (mut wire_a, mut wire_b) = (Wire::default(), Wire::default());
    let mut a = interface(&mut wire_a, MAC_A, [10, 0, 0, 1], [10, 0, 0, 254]);
    let mut b = interface(&mut wire_b, MAC_B, [10, 0, 1, 1], [10, 0, 1, 254]);
    let (mut set_a, mut set_b) = (SocketSet::new(Vec::new()), SocketSet::new(Vec::new()));
    set_a.add(udp_socket([10, 0, 0, 1]));

    let t = Instant::from_millis(10);
    b.poll(t, &mut wire_b, &mut set_b);
    a.poll(t, &mut wire_a, &mut set_a);
    assert!(wire_b.tx.is_empty());
    assert_eq!(
        wire_a.tx.iter().find_map(|f| arp_request_target(f)),
        Some([10, 0, 0, 254])
    );
}

#[test]
fn one_dhcp_socket_in_a_shared_set_serves_one_interface() {
    let (mut wire_a, mut wire_b) = (Wire::default(), Wire::default());
    let mut a = interface(&mut wire_a, MAC_A, [10, 0, 0, 1], [10, 0, 0, 254]);
    let mut b = interface(&mut wire_b, MAC_B, [10, 0, 1, 1], [10, 0, 1, 254]);
    let mut sockets = SocketSet::new(Vec::new());
    sockets.add(dhcpv4::Socket::new());

    let discovers = |wire: &Wire| {
        wire.tx
            .iter()
            .filter(|f| udp_of(f).is_some_and(|(_, _, port)| port == 67))
            .count()
    };
    let t = Instant::from_millis(10);
    a.poll(t, &mut wire_a, &mut sockets);
    b.poll(t, &mut wire_b, &mut sockets);
    assert_eq!(discovers(&wire_a), 1);
    assert_eq!(
        discovers(&wire_b),
        0,
        "the second interface never got a DISCOVER"
    );
}
