//! What `netd` announces: each interface's retained address
//! (`system/net/<if>/addr`), the retained list of interfaces
//! (`system/net/interfaces`) and the network-up event, republished only when
//! something changed.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use netstack::{Net, Source};
use user::central;
use user::messenger::netstack::wire;
use user::sys;

use super::lists::{interface_infos, text};
use super::ports::Ports;

/// What was published for one interface slot.
struct Seen {
    name: String,
    epoch: u64,
}

pub(super) struct Publisher {
    bus: Option<central::Bus>,
    seen: Vec<Option<Seen>>,
    list_signature: u64,
    had_address: bool,
}

fn octets_or_zero(value: Option<[u8; 4]>) -> Vec<u8> {
    value.unwrap_or([0; 4]).to_vec()
}

/// A cheap fingerprint of everything the interface list shows, so the list is
/// built and encoded only when one of those changed.
fn signature(net: &Net) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64 ^ net.generation();
    let mut mix = |value: u64| hash = (hash ^ value).wrapping_mul(0x0000_0100_0000_01B3);
    mix(net.primary().map_or(u64::MAX, |slot| slot as u64));
    for (slot, unit) in net.units() {
        mix(slot as u64);
        mix(unit.stack.epoch());
        mix(unit.stack.state().dhcp as u64);
        mix(u64::from(unit.metric));
    }
    hash
}

impl Publisher {
    pub(super) fn new() -> Publisher {
        Publisher {
            bus: None,
            seen: Vec::new(),
            list_signature: 0,
            had_address: false,
        }
    }

    /// Publish whatever changed since the last call.
    pub(super) fn sync(&mut self, net: &Net, ports: &Ports) {
        if self.bus.is_none() {
            self.bus = central::Bus::connect().ok();
        }
        self.retire_removed(net);
        for (slot, unit) in net.units() {
            let epoch = unit.stack.epoch();
            let current = self.seen.get(slot).and_then(Option::as_ref);
            if current.is_some_and(|seen| seen.name == unit.name && seen.epoch == epoch) {
                continue;
            }
            self.announce(net, slot);
            if self.seen.len() <= slot {
                self.seen.resize_with(slot + 1, || None);
            }
            self.seen[slot] = Some(Seen {
                name: unit.name.clone(),
                epoch,
            });
        }
        let any_address = net.units().any(|(_, u)| u.stack.state().addr.is_some());
        if any_address && !self.had_address {
            self.announce_up(net);
        }
        self.had_address = any_address;
        let sig = signature(net);
        if sig != self.list_signature {
            self.list_signature = sig;
            if let Some(bus) = self.bus.as_mut() {
                let list = wire::InterfaceList {
                    list: interface_infos(net, ports),
                };
                let _ = wire::publish_system_net_interfaces(bus, &list);
            }
        }
    }

    /// An interface that is gone: its retained address must not outlive it.
    fn retire_removed(&mut self, net: &Net) {
        for slot in 0..self.seen.len() {
            let Some(seen) = &self.seen[slot] else { continue };
            if net.unit(slot).is_some_and(|unit| unit.name == seen.name) {
                continue;
            }
            let event = wire::AddressEvent {
                interface: seen.name.clone(),
                addr: octets_or_zero(None),
                prefix_len: 0,
                gateway: octets_or_zero(None),
            };
            sys::write_str(&format!("NETD:ADDR none if={}\n", seen.name));
            if let Some(bus) = self.bus.as_mut() {
                let _ = wire::publish_system_net_addr(bus, &seen.name, &event);
            }
            self.seen[slot] = None;
        }
    }

    fn event(net: &Net, slot: usize) -> Option<wire::AddressEvent> {
        let unit = net.unit(slot)?;
        let state = unit.stack.state();
        Some(wire::AddressEvent {
            interface: unit.name.clone(),
            addr: octets_or_zero(state.addr),
            prefix_len: u32::from(state.prefix_len),
            gateway: octets_or_zero(state.gateway),
        })
    }

    fn announce(&mut self, net: &Net, slot: usize) {
        let (Some(event), Some(unit)) = (Publisher::event(net, slot), net.unit(slot)) else {
            return;
        };
        let state = unit.stack.state();
        if let Some(addr) = state.addr {
            sys::write_str(&format!(
                "NETD:ADDR {}/{} gw={} dns={} source={} if={}\n",
                text(addr),
                state.prefix_len,
                state.gateway.map_or(String::from("none"), text),
                state.dns.first().map_or(String::from("none"), |d| text(*d)),
                if state.source == Source::Dhcp {
                    "dhcp"
                } else {
                    "static"
                },
                unit.name
            ));
        } else {
            sys::write_str(&format!("NETD:ADDR none if={}\n", unit.name));
        }
        if let Some(bus) = self.bus.as_mut() {
            let _ = wire::publish_system_net_addr(bus, &unit.name, &event);
        }
    }

    /// The first address after none: announce the network is up, with the
    /// primary interface's address (any interface that has one otherwise).
    fn announce_up(&mut self, net: &Net) {
        let slot = net.primary().or_else(|| {
            net.units()
                .find(|(_, u)| u.stack.state().addr.is_some())
                .map(|(slot, _)| slot)
        });
        let (Some(slot), Some(bus)) = (slot, self.bus.as_mut()) else {
            return;
        };
        if let Some(event) = Publisher::event(net, slot) {
            let _ = wire::publish_system_events_network_up(bus, &event);
        }
    }
}
