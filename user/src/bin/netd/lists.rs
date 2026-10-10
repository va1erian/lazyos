//! The read-only calls of `os.lazy.net.stack.v1` that list what the stack
//! holds: `Interfaces`, `Addresses`, `Routes`, `Stats`. Every one covers all
//! interfaces; `primary` marks the one carrying new traffic.

use alloc::string::String;
use alloc::vec::Vec;

use netstack::config::Mode;
use netstack::{DhcpState, IfKind, Net, Source};
use user::messenger::net::wire as nic_wire;
use user::messenger::netstack::wire;
use user::messenger::Error as MsgError;
use user::sys;

use super::ports::Ports;
use super::service::{Netd, Result};

/// Every interface as the wire describes it.
pub(super) fn interface_infos(net: &Net, ports: &Ports) -> Vec<wire::InterfaceInfo> {
    let primary = net.primary();
    net.units()
        .map(|(slot, unit)| {
            let port = ports.by_slot(slot);
            wire::InterfaceInfo {
                name: unit.name.clone(),
                mac: unit.stack.mac().to_vec(),
                mtu: port.and_then(|p| p.nic.card).map_or(0, |card| card.mtu),
                link: unit.up(),
                mode: match port.map(|p| p.mode) {
                    Some(Mode::Static(_)) => wire::CONFIG_MODE_STATIC,
                    _ => wire::CONFIG_MODE_DHCP,
                },
                dhcp: match unit.stack.state().dhcp {
                    DhcpState::Off => wire::DHCP_STATE_OFF,
                    DhcpState::Discovering => wire::DHCP_STATE_DISCOVERING,
                    DhcpState::Bound => wire::DHCP_STATE_BOUND,
                },
                kind: match unit.kind {
                    IfKind::Wired => nic_wire::NIC_KIND_WIRED,
                    IfKind::Wireless => nic_wire::NIC_KIND_WIRELESS,
                },
                metric: unit.metric,
                primary: primary == Some(slot),
            }
        })
        .collect()
}

impl Netd {
    pub(super) fn interfaces(&self) -> Result<Vec<u8>> {
        wire::encode_interfaces_reply(&wire::InterfacesReply {
            list: interface_infos(&self.net, &self.ports),
        })
        .map_err(MsgError::Parcel)
    }

    pub(super) fn addresses(&self) -> Result<Vec<u8>> {
        let now_ms = sys::monotonic_ms() as i64;
        let list = self
            .net
            .units()
            .filter_map(|(_, unit)| {
                let state = unit.stack.state();
                Some(wire::AddressInfo {
                    interface: unit.name.clone(),
                    addr: state.addr?.to_vec(),
                    prefix_len: u32::from(state.prefix_len),
                    source: match state.source {
                        Source::Dhcp => wire::ADDR_SOURCE_DHCP,
                        Source::Static => wire::ADDR_SOURCE_STATIC,
                    },
                    lease_secs: state
                        .lease_ends_ms
                        .map_or(0, |end| ((end - now_ms).max(0) / 1000) as u32),
                })
            })
            .collect();
        wire::encode_addresses_reply(&wire::AddressesReply { list }).map_err(MsgError::Parcel)
    }

    pub(super) fn routes(&self) -> Result<Vec<u8>> {
        let list = self
            .net
            .routes()
            .into_iter()
            .filter_map(|route| {
                Some(wire::RouteInfo {
                    interface: self.net.unit(route.slot).map(|u| u.name.clone())?,
                    dest: route.dest.to_vec(),
                    prefix_len: u32::from(route.prefix_len),
                    gateway: route.gateway.to_vec(),
                    metric: route.metric,
                })
            })
            .collect();
        wire::encode_routes_reply(&wire::RoutesReply { list }).map_err(MsgError::Parcel)
    }

    pub(super) fn stats(&self) -> Result<Vec<u8>> {
        let d = self.net.device_stats();
        let c = self.net.counters();
        wire::encode_stats_reply(&wire::StatsReply {
            stats: wire::StackStats {
                rx_frames: d.rx_frames,
                tx_frames: d.tx_frames,
                rx_bytes: d.rx_bytes,
                tx_bytes: d.tx_bytes,
                tx_dropped: d.tx_dropped,
                rx_bad_length: d.rx_bad_length,
                nic_resets: self.ports.resets,
                leases: c.leases,
                lease_losses: c.lease_losses,
                pings_sent: c.pings_sent,
                pings_answered: c.pings_answered,
                pings_timed_out: c.pings_timed_out,
                lookups_sent: c.lookups_sent,
                lookups_answered: c.lookups_answered,
                lookups_failed: c.lookups_failed,
            },
        })
        .map_err(MsgError::Parcel)
    }
}

/// `192.168.1.5`.
pub(super) fn text(addr: [u8; 4]) -> String {
    alloc::format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
}
