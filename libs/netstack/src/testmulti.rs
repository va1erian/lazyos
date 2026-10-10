//! Test networks for [`Net`](crate::net::Net): several interfaces, each wired
//! to something in host memory.
//!
//! * [`MultiLan`]: every interface sits on its own network (10.0.N.0/24, N
//!   from 2) with a scripted gateway that answers ARP, DHCP, echo and DNS, so
//!   the tests judge routes, metrics, link changes and lookups.
//! * [`MultiPair`]: every interface has a real [`Stack`] peer on its subnet
//!   (10.0.N.1 is the `Net`, 10.0.N.2 the peer), so TCP and UDP run through
//!   the real smoltcp on both ends.

use std::string::String;
use std::vec::Vec;

use framering::fuzz::Mem;
use framering::{ring_bytes, Consumer, Producer, Ring, MAX_FRAME};

use crate::config::{Mode, StaticConfig};
use crate::device::RingDevice;
use crate::net::{IfKind, Net};
use crate::stack::Stack;
use crate::testnet::Gateway;

const SLOTS: u32 = 64;

/// The two ring pairs of one wire, in one block of memory.
struct Wire {
    mem: Mem,
    /// What the far end consumes and the near end produced.
    near_rx: Consumer,
    near_tx: Producer,
    far_rx: Consumer,
    far_tx: Producer,
}

fn wire() -> Wire {
    let one = ring_bytes(SLOTS);
    let mut mem = Mem::with_len(one * 2);
    let base = mem.base();
    // SAFETY: both rings lie inside `mem`, which the owner keeps alive.
    let (a_to_b, b_to_a) = unsafe {
        (
            Ring::create(base, one, SLOTS).expect("ring"),
            Ring::create(base.add(one), one, SLOTS).expect("ring"),
        )
    };
    Wire {
        mem,
        near_rx: b_to_a.consumer(),
        near_tx: a_to_b.producer(),
        far_rx: a_to_b.consumer(),
        far_tx: b_to_a.producer(),
    }
}

fn mac(index: usize) -> [u8; 6] {
    [0x52, 0x54, 0x00, 0x12, 0x34, 0x50 + index as u8]
}

/// Interfaces on separate networks with scripted gateways.
pub struct MultiLan {
    pub net: Net,
    pub gateways: Vec<Gateway>,
    pub now: i64,
    ports: Vec<LanPort>,
}

struct LanPort {
    _mem: Mem,
    to_net: Producer,
    from_net: Consumer,
}

impl MultiLan {
    /// `kinds` interfaces named eth0.. (or wlan0.. for wireless), each on
    /// 10.0.(2+N).0/24 with its gateway at .2, DHCP.
    pub fn new(kinds: &[IfKind]) -> MultiLan {
        let mut lan = MultiLan {
            net: Net::new(0x5EED),
            gateways: Vec::new(),
            now: 1000,
            ports: Vec::new(),
        };
        for kind in kinds {
            lan.add(*kind);
        }
        lan
    }

    /// Add one more interface; its slot.
    pub fn add(&mut self, kind: IfKind) -> usize {
        let n = self.ports.len();
        let w = wire();
        let device = RingDevice::new(w.near_rx, w.near_tx, 1514);
        let stack = Stack::new(device, mac(n), 0x1000 + n as u64, self.now, &Mode::Dhcp);
        let name = std::format!("{}{n}", if kind == IfKind::Wired { "eth" } else { "wlan" });
        let net_n = 2 + n as u8;
        let gateway = Gateway {
            offer_ip: [10, 0, net_n, 15],
            router: Some([10, 0, net_n, 2]),
            dns: std::vec![[10, 0, net_n, 2]],
            gw_ip: [10, 0, net_n, 2],
            gw_mac: [0x52, 0x55, 0x0a, 0x00, net_n, 0x02],
            stack_mac: mac(n),
            ..Gateway::default()
        };
        self.gateways.push(gateway);
        self.ports.push(LanPort {
            _mem: w.mem,
            to_net: w.far_tx,
            from_net: w.far_rx,
        });
        self.net
            .add_interface(&name, kind, stack, true)
            .expect("room for the interface")
    }

    /// Hand each gateway what its interface sent and queue the answers.
    fn exchange(&mut self) {
        let mut buf = [0u8; MAX_FRAME];
        for (port, gateway) in self.ports.iter_mut().zip(self.gateways.iter_mut()) {
            while let Ok(Some(n)) = port.from_net.pop(&mut buf) {
                for answer in gateway.react(&buf[..n]) {
                    let _ = port.to_net.push(&answer);
                }
            }
        }
    }

    pub fn step(&mut self, ms: i64) {
        self.now += ms;
        for _ in 0..4 {
            self.net.poll(self.now);
            self.exchange();
        }
        self.net.poll(self.now);
    }

    pub fn run_until(&mut self, max_ms: i64, mut done: impl FnMut(&mut MultiLan) -> bool) -> bool {
        let end = self.now + max_ms;
        while self.now < end {
            self.step(10);
            if done(self) {
                return true;
            }
        }
        false
    }

    /// Run until every interface has an address.
    pub fn configure(&mut self) {
        assert!(
            self.run_until(5000, |lan| lan
                .net
                .units()
                .all(|(_, u)| u.stack.state().addr.is_some())),
            "no lease on every interface"
        );
    }

    /// The names of the requests gateway `n` has seen.
    pub fn seen(&self, n: usize) -> &[&'static str] {
        &self.gateways[n].requests_seen
    }
}

/// Interfaces each wired to a real peer stack.
pub struct MultiPair {
    pub net: Net,
    pub peers: Vec<Stack>,
    pub now: i64,
    _mems: Vec<Mem>,
}

fn static_mode(addr: [u8; 4], gateway: Option<[u8; 4]>) -> Mode {
    Mode::Static(StaticConfig {
        addr,
        prefix_len: 24,
        gateway,
        dns: None,
    })
}

impl MultiPair {
    /// One interface and peer per entry of `kinds`; interface N is
    /// 10.0.N.1/24 (`eth`N), its peer 10.0.N.2.
    pub fn new(kinds: &[IfKind]) -> MultiPair {
        let mut pair = MultiPair {
            net: Net::new(0x77),
            peers: Vec::new(),
            now: 1000,
            _mems: Vec::new(),
        };
        for kind in kinds {
            pair.add(*kind);
        }
        pair
    }

    /// The address of interface `n`.
    pub fn net_ip(n: usize) -> [u8; 4] {
        [10, 0, n as u8, 1]
    }

    /// The address of peer `n`.
    pub fn peer_ip(n: usize) -> [u8; 4] {
        [10, 0, n as u8, 2]
    }

    /// Add interface N and its peer; the slot. The peer is the interface's
    /// default gateway, so any other destination goes to it too.
    pub fn add(&mut self, kind: IfKind) -> usize {
        let n = self.peers.len();
        let w = wire();
        let net_dev = RingDevice::new(w.near_rx, w.near_tx, 1514);
        let peer_dev = RingDevice::new(w.far_rx, w.far_tx, 1514);
        let stack = Stack::new(
            net_dev,
            mac(n),
            0x10 + n as u64,
            self.now,
            &static_mode(MultiPair::net_ip(n), Some(MultiPair::peer_ip(n))),
        );
        self.peers.push(Stack::new(
            peer_dev,
            mac(n + 4),
            0x20 + n as u64,
            self.now,
            &static_mode(MultiPair::peer_ip(n), None),
        ));
        self._mems.push(w.mem);
        let name: String = std::format!("{}{n}", if kind == IfKind::Wired { "eth" } else { "wlan" });
        self.net
            .add_interface(&name, kind, stack, true)
            .expect("room for the interface")
    }

    pub fn step(&mut self, ms: i64) {
        self.now += ms;
        for _ in 0..8 {
            self.net.poll(self.now);
            for peer in &mut self.peers {
                peer.poll(self.now);
            }
        }
    }

    pub fn run_until(&mut self, max_ms: i64, mut done: impl FnMut(&mut MultiPair) -> bool) -> bool {
        let end = self.now + max_ms;
        while self.now < end {
            self.step(10);
            if done(self) {
                return true;
            }
        }
        false
    }
}
