//! A scripted network for host tests and fuzzing: a stack wired to a gateway
//! that answers ARP, DHCP and ICMP echo, all in host memory.
//!
//! The gateway is built from smoltcp's own wire representations, so what it
//! sends is what a real peer would send; its behaviour can be bent (a hostile
//! lease, no echo replies, a forged reply) to test the stack's defences.

use std::vec::Vec;

use framering::fuzz::Mem;
use framering::{ring_bytes, Consumer, Producer, Ring, MAX_FRAME};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
    EthernetRepr, Icmpv4Packet, Icmpv4Repr, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr,
    UdpPacket, UdpRepr,
};

use crate::config::Mode;
use crate::device::RingDevice;
use crate::stack::Stack;

pub const STACK_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
pub const GW_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
pub const GW_IP: [u8; 4] = [10, 0, 2, 2];
pub const LEASE_IP: [u8; 4] = [10, 0, 2, 15];
pub const SLOTS: u32 = 64;

/// What the gateway offers in a lease, and how it misbehaves.
#[derive(Clone)]
pub struct Gateway {
    pub offer_ip: [u8; 4],
    pub mask: [u8; 4],
    pub router: Option<[u8; 4]>,
    pub dns: Vec<[u8; 4]>,
    pub lease_secs: u32,
    pub answer_arp: bool,
    pub answer_dhcp: bool,
    pub answer_echo: bool,
    /// Corrupt the payload of echo replies.
    pub mangle_echo: bool,
    /// Answer echo requests with this identifier instead of the request's.
    pub echo_ident_override: Option<u16>,
    /// Answer from another address than the one pinged.
    pub echo_source_override: Option<[u8; 4]>,
    /// Answer queries to port 53 from `dns_records` (NXDOMAIN for the rest).
    pub answer_dns: bool,
    pub dns_records: Vec<(&'static str, [u8; 4])>,
    pub requests_seen: Vec<&'static str>,
}

impl Default for Gateway {
    fn default() -> Gateway {
        Gateway {
            offer_ip: LEASE_IP,
            mask: [255, 255, 255, 0],
            router: Some(GW_IP),
            dns: std::vec![[10, 0, 2, 3]],
            lease_secs: 86_400,
            answer_arp: true,
            answer_dhcp: true,
            answer_echo: true,
            mangle_echo: false,
            echo_ident_override: None,
            echo_source_override: None,
            answer_dns: true,
            dns_records: Vec::new(),
            requests_seen: Vec::new(),
        }
    }
}

fn eth(dst: [u8; 6], src: [u8; 6], ethertype: EthernetProtocol, payload: &[u8]) -> Vec<u8> {
    let repr = EthernetRepr {
        src_addr: EthernetAddress(src),
        dst_addr: EthernetAddress(dst),
        ethertype,
    };
    let mut frame = std::vec![0u8; 14 + payload.len()];
    repr.emit(&mut EthernetFrame::new_unchecked(&mut frame[..]));
    frame[14..].copy_from_slice(payload);
    frame
}

fn ipv4(src: [u8; 4], dst: [u8; 4], proto: IpProtocol, payload: &[u8]) -> Vec<u8> {
    let repr = Ipv4Repr {
        src_addr: Ipv4Address::from(src),
        dst_addr: Ipv4Address::from(dst),
        next_header: proto,
        payload_len: payload.len(),
        hop_limit: 64,
    };
    let mut packet = std::vec![0u8; 20 + payload.len()];
    repr.emit(
        &mut Ipv4Packet::new_unchecked(&mut packet[..]),
        &ChecksumCapabilities::default(),
    );
    packet[20..].copy_from_slice(payload);
    packet
}

/// A full Ethernet/IPv4/ICMP echo frame (request or reply).
#[allow(clippy::too_many_arguments)] // a frame builder: each argument is one header field
pub fn echo_frame(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src: [u8; 4],
    dst: [u8; 4],
    reply: bool,
    ident: u16,
    seq: u16,
    data: &[u8],
) -> Vec<u8> {
    let repr = if reply {
        Icmpv4Repr::EchoReply {
            ident,
            seq_no: seq,
            data,
        }
    } else {
        Icmpv4Repr::EchoRequest {
            ident,
            seq_no: seq,
            data,
        }
    };
    let mut icmp = std::vec![0u8; repr.buffer_len()];
    repr.emit(
        &mut Icmpv4Packet::new_unchecked(&mut icmp[..]),
        &ChecksumCapabilities::default(),
    );
    eth(
        dst_mac,
        src_mac,
        EthernetProtocol::Ipv4,
        &ipv4(src, dst, IpProtocol::Icmp, &icmp),
    )
}

fn udp_frame(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src: [u8; 4],
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    payload: &[u8],
) -> Vec<u8> {
    let repr = UdpRepr {
        src_port: sport,
        dst_port: dport,
    };
    let mut udp = std::vec![0u8; 8 + payload.len()];
    repr.emit(
        &mut UdpPacket::new_unchecked(&mut udp[..]),
        &Ipv4Address::from(src).into(),
        &Ipv4Address::from(dst).into(),
        payload.len(),
        |buf| buf.copy_from_slice(payload),
        &ChecksumCapabilities::default(),
    );
    eth(
        dst_mac,
        src_mac,
        EthernetProtocol::Ipv4,
        &ipv4(src, dst, IpProtocol::Udp, &udp),
    )
}

/// An ARP reply frame.
pub fn arp_reply(to_mac: [u8; 6], from_mac: [u8; 6], from_ip: [u8; 4], to_ip: [u8; 4]) -> Vec<u8> {
    let mut arp = std::vec![0u8; 28];
    arp[0..2].copy_from_slice(&[0, 1]);
    arp[2..4].copy_from_slice(&[0x08, 0x00]);
    arp[4] = 6;
    arp[5] = 4;
    arp[6..8].copy_from_slice(&[0, 2]);
    arp[8..14].copy_from_slice(&from_mac);
    arp[14..18].copy_from_slice(&from_ip);
    arp[18..24].copy_from_slice(&to_mac);
    arp[24..28].copy_from_slice(&to_ip);
    eth(to_mac, from_mac, EthernetProtocol::Arp, &arp)
}

impl Gateway {
    fn dhcp_reply(&self, kind: DhcpMessageType, request: &DhcpRepr) -> Vec<u8> {
        let dns: smoltcp::wire::DhcpRepr = DhcpRepr {
            message_type: kind,
            transaction_id: request.transaction_id,
            secs: 0,
            client_hardware_address: request.client_hardware_address,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: Ipv4Address::from(self.offer_ip),
            server_ip: Ipv4Address::from(GW_IP),
            router: self.router.map(Ipv4Address::from),
            subnet_mask: Some(Ipv4Address::from(self.mask)),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(Ipv4Address::from(GW_IP)),
            parameter_request_list: None,
            dns_servers: Some(
                self.dns
                    .iter()
                    .copied()
                    .take(3)
                    .map(Ipv4Address::from)
                    .collect(),
            ),
            max_size: None,
            lease_duration: Some(self.lease_secs),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &[],
        };
        let mut payload = std::vec![0u8; dns.buffer_len()];
        dns.emit(&mut DhcpPacket::new_unchecked(&mut payload[..]))
            .expect("emit");
        udp_frame([0xFF; 6], GW_MAC, GW_IP, [255; 4], 67, 68, &payload)
    }

    /// The frames the gateway sends in answer to one frame from the stack.
    pub fn react(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return Vec::new();
        };
        match eth_frame.ethertype() {
            EthernetProtocol::Arp if frame.len() >= 42 => {
                self.requests_seen.push("arp");
                let asks_for_gateway = frame[38..42] == GW_IP;
                if self.answer_arp && frame[20..22] == [0, 1] && asks_for_gateway {
                    let mut mac = [0u8; 6];
                    mac.copy_from_slice(&frame[22..28]);
                    let mut ip = [0u8; 4];
                    ip.copy_from_slice(&frame[28..32]);
                    return std::vec![arp_reply(mac, GW_MAC, GW_IP, ip)];
                }
                Vec::new()
            }
            EthernetProtocol::Ipv4 => {
                let Ok(packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
                    return Vec::new();
                };
                match packet.next_header() {
                    IpProtocol::Udp => self.react_udp(&packet),
                    IpProtocol::Icmp => self.react_icmp(&eth_frame, &packet),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn react_udp(&mut self, packet: &Ipv4Packet<&[u8]>) -> Vec<Vec<u8>> {
        let Ok(udp) = UdpPacket::new_checked(packet.payload()) else {
            return Vec::new();
        };
        if udp.dst_port() == 53 {
            return self.react_dns(packet, &udp);
        }
        if udp.dst_port() != 67 {
            return Vec::new();
        }
        let Ok(dhcp) = DhcpPacket::new_checked(udp.payload()) else {
            return Vec::new();
        };
        let Ok(request) = DhcpRepr::parse(&dhcp) else {
            return Vec::new();
        };
        let (name, answer) = match request.message_type {
            DhcpMessageType::Discover => ("discover", DhcpMessageType::Offer),
            DhcpMessageType::Request => ("request", DhcpMessageType::Ack),
            _ => return Vec::new(),
        };
        self.requests_seen.push(name);
        if !self.answer_dhcp {
            return Vec::new();
        }
        std::vec![self.dhcp_reply(answer, &request)]
    }

    fn react_dns(&mut self, packet: &Ipv4Packet<&[u8]>, udp: &UdpPacket<&[u8]>) -> Vec<Vec<u8>> {
        self.requests_seen.push("dns");
        let Some(response) = self
            .answer_dns
            .then(|| crate::testdns::answer(udp.payload(), &self.dns_records))
            .flatten()
        else {
            return Vec::new();
        };
        let mut to = [0u8; 4];
        to.copy_from_slice(&packet.src_addr().octets());
        let mut from = [0u8; 4];
        from.copy_from_slice(&packet.dst_addr().octets());
        // The stack learned our MAC from the ARP exchange; unicast back to it.
        std::vec![udp_frame(
            STACK_MAC,
            GW_MAC,
            from,
            to,
            53,
            udp.src_port(),
            &response
        )]
    }

    fn react_icmp(
        &mut self,
        eth_frame: &EthernetFrame<&[u8]>,
        packet: &Ipv4Packet<&[u8]>,
    ) -> Vec<Vec<u8>> {
        let Ok(icmp) = Icmpv4Packet::new_checked(packet.payload()) else {
            return Vec::new();
        };
        let Ok(Icmpv4Repr::EchoRequest {
            ident,
            seq_no,
            data,
        }) = Icmpv4Repr::parse(&icmp, &ChecksumCapabilities::default())
        else {
            return Vec::new();
        };
        self.requests_seen.push("echo");
        if !self.answer_echo {
            return Vec::new();
        }
        let mut data = data.to_vec();
        if self.mangle_echo {
            data.iter_mut().for_each(|b| *b ^= 0xFF);
        }
        let mut stack_mac = [0u8; 6];
        stack_mac.copy_from_slice(eth_frame.src_addr().as_bytes());
        let mut stack_ip = [0u8; 4];
        stack_ip.copy_from_slice(&packet.src_addr().octets());
        let from = self
            .echo_source_override
            .unwrap_or_else(|| packet.dst_addr().octets());
        std::vec![echo_frame(
            stack_mac,
            GW_MAC,
            from,
            stack_ip,
            true,
            self.echo_ident_override.unwrap_or(ident),
            seq_no,
            &data
        )]
    }
}

/// A stack wired to a gateway through two rings in host memory.
pub struct Lan {
    pub mem: Mem,
    pub stack: Stack,
    /// The gateway's side: produces into the stack's receive ring.
    pub to_stack: Producer,
    /// The gateway's side: consumes what the stack transmitted.
    pub from_stack: Consumer,
    pub gateway: Gateway,
    pub now: i64,
    /// Every frame the stack transmitted, in order.
    pub sent: Vec<Vec<u8>>,
}

impl Lan {
    pub fn new(mode: &Mode) -> Lan {
        Lan::with_seed(mode, 0x1234_5678_9ABC_DEF0)
    }

    pub fn with_seed(mode: &Mode, seed: u64) -> Lan {
        let one = ring_bytes(SLOTS);
        let mut mem = Mem::with_len(one * 2);
        let base = mem.base();
        // SAFETY: both rings lie inside `mem`, which lives in the Lan next to
        // the endpoints and is never moved.
        let (rx, tx) = unsafe {
            (
                Ring::create(base, one, SLOTS).expect("rx ring"),
                Ring::create(base.add(one), one, SLOTS).expect("tx ring"),
            )
        };
        let device = RingDevice::new(rx.consumer(), tx.producer(), 1514);
        let stack = Stack::new(device, STACK_MAC, seed, 1000, mode);
        Lan {
            mem,
            stack,
            to_stack: rx.producer(),
            from_stack: tx.consumer(),
            gateway: Gateway::default(),
            now: 1000,
            sent: Vec::new(),
        }
    }

    /// Replace the rings with a fresh pair in new memory (the NIC driver came
    /// back): the device is re-attached, the stack above it is untouched.
    pub fn rewire(&mut self) {
        let one = ring_bytes(SLOTS);
        let mut mem = Mem::with_len(one * 2);
        let base = mem.base();
        // SAFETY: both rings lie inside `mem`, which replaces the old one here.
        let (rx, tx) = unsafe {
            (
                Ring::create(base, one, SLOTS).expect("rx ring"),
                Ring::create(base.add(one), one, SLOTS).expect("tx ring"),
            )
        };
        self.stack.device_mut().attach(rx.consumer(), tx.producer());
        self.to_stack = rx.producer();
        self.from_stack = tx.consumer();
        self.mem = mem;
    }

    /// Hand the gateway everything the stack has sent, and queue its answers.
    pub fn exchange(&mut self) {
        let mut buf = [0u8; MAX_FRAME];
        while let Ok(Some(n)) = self.from_stack.pop(&mut buf) {
            let frame = buf[..n].to_vec();
            for answer in self.gateway.react(&frame) {
                let _ = self.to_stack.push(&answer);
            }
            self.sent.push(frame);
        }
    }

    /// Advance the clock by `ms` and run the stack and the gateway to quiet.
    pub fn step(&mut self, ms: i64) {
        self.now += ms;
        for _ in 0..4 {
            self.stack.poll(self.now);
            self.exchange();
        }
        self.stack.poll(self.now);
    }

    /// Step in 10 ms ticks (the OS's clock) until `done` holds or `max_ms` pass.
    pub fn run_until(&mut self, max_ms: i64, mut done: impl FnMut(&mut Lan) -> bool) -> bool {
        let end = self.now + max_ms;
        while self.now < end {
            self.step(10);
            if done(self) {
                return true;
            }
        }
        false
    }

    /// Run until DHCP has configured the stack.
    pub fn configure(&mut self) {
        assert!(
            self.run_until(5000, |lan| lan.stack.state().addr.is_some()),
            "no lease: {:?}",
            self.gateway.requests_seen
        );
    }

    /// Inject a frame as if the wire delivered it.
    pub fn deliver(&mut self, frame: &[u8]) -> bool {
        self.to_stack.push(frame).is_ok()
    }
}
