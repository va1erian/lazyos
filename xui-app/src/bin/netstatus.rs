//! `netstatus` (`os.lazy.netstatus`): the network status in the taskbar tray
//! (docs/tray-plan.md T3). A resident applet with no window, shipped with
//! the network stack (`LAZYOS_NETD=1`): a Lucide `link` icon whose tooltip
//! says whether the machine is connected and with which address, from the
//! retained topics `netd` keeps (`system/net/<if>/addr`, and
//! `system/net/interfaces` for which interface is primary) and the NIC driver
//! publishes (`system/net/<if>/link`). A card whose link is down does not
//! count as connected, even while `netd` keeps its lease. Without a usable
//! address the item is `Passive` (it overflows first). It opens with every session.
//!
//! A wireless interface adds `wlanmd`'s retained `system/net/<if>/wifi/state`
//! (docs/wifi-prerequisites-plan.md WP5): while the shown interface is
//! connected the tooltip names the network and signal and the icon is
//! signal bars (an alpha mask the shell tints, `wifimodel::signal_mask`), and
//! while a join is under way or failed the tooltip says so.
//!
//! Serial evidence: `NETSTATUS:UP:PASS` once it started,
//! `NETSTATUS:STATE <tooltip>` on every change, `NETSTATUS:QUIT:PASS`.

use messenger_generated::os_lazy_net_nic_v1 as nic_wire;
use messenger_generated::os_lazy_net_stack_v1 as stack_wire;
use messenger_generated::os_lazy_net_wifi_v1 as wifi_wire;
use std::rc::Rc;
use trayclient::{item, lucide, wire};
use xui_app::fabric::{pull, Watch};
use xui_app::net::wifimodel::{self, Conn};
use xui_app::resident::{Resident, Wake};
use xui_app::trace::{self, Channel};

/// Most interfaces remembered.
const MAX_LINKS: usize = 8;

/// What `wlanmd` said about one wireless interface.
#[derive(Clone, PartialEq, Eq)]
struct Wifi {
    name: String,
    conn: Conn,
    ssid: String,
    rssi_dbm: i32,
}

/// What the topics said.
#[derive(Default, PartialEq, Eq)]
struct Net {
    /// Every wireless interface `wlanmd` published a state for.
    wifi: Vec<Wifi>,
    /// Every interface with an address, and the address.
    addresses: Vec<(String, [u8; 4])>,
    /// The interface carrying new traffic (`system/net/interfaces`).
    primary: Option<String>,
    /// Whether any card reported its link up.
    link: bool,
    /// The interfaces whose card reported its link down.
    down: Vec<String>,
}

impl Net {
    /// The addresses of interfaces whose link is not down.
    fn usable(&self) -> impl Iterator<Item = &(String, [u8; 4])> {
        self.addresses
            .iter()
            .filter(|(name, _)| !self.down.contains(name))
    }

    /// The address to show: the primary interface's, else the first usable
    /// one.
    fn shown(&self) -> Option<&(String, [u8; 4])> {
        self.primary
            .as_ref()
            .and_then(|name| self.usable().find(|(known, _)| known == name))
            .or_else(|| self.usable().next())
    }

    /// `wlanmd`'s state for the interface called `name`.
    fn wifi_of(&self, name: &str) -> Option<&Wifi> {
        self.wifi.iter().find(|wifi| wifi.name == name)
    }

    /// A wireless interface that is joining (or failed to) and has no usable
    /// address yet, for a tooltip that says what it is doing.
    fn wifi_in_progress(&self) -> Option<&Wifi> {
        self.wifi.iter().find(|wifi| {
            !matches!(wifi.conn, Conn::Disabled | Conn::Idle | Conn::Connected)
                && self.usable().all(|(name, _)| *name != wifi.name)
        })
    }

    fn tooltip(&self) -> String {
        match self.shown() {
            Some((name, [a, b, c, d])) => {
                let more = match self.usable().count() - 1 {
                    0 => String::new(),
                    n => format!(" (+{n} more)"),
                };
                match self.wifi_of(name).filter(|w| w.conn == Conn::Connected) {
                    Some(wifi) => format!(
                        "Connected to {:?} on {name} ({} dBm): {a}.{b}.{c}.{d}{more}",
                        wifi.ssid, wifi.rssi_dbm
                    ),
                    None => format!("Connected on {name}: {a}.{b}.{c}.{d}{more}"),
                }
            }
            None => match self.wifi_in_progress() {
                Some(wifi) if wifi.conn == Conn::Failed => {
                    format!("Wi-Fi: could not join {:?}", wifi.ssid)
                }
                Some(wifi) if wifi.ssid.is_empty() => String::from("Wi-Fi: looking for networks"),
                Some(wifi) => format!("Wi-Fi: joining {:?}...", wifi.ssid),
                None if self.link => String::from("Link up, no address yet"),
                None => String::from("Not connected"),
            },
        }
    }

    /// Signal bars to draw (0 to 4) while the shown interface is a joined
    /// wireless one; `None` keeps the wired icon.
    fn bars(&self) -> Option<u8> {
        let (name, _) = self.shown()?;
        let wifi = self.wifi_of(name).filter(|w| w.conn == Conn::Connected)?;
        Some(wifimodel::bars(wifi.rssi_dbm))
    }

    /// The tray picture for the current state.
    fn icon(&self) -> wire::Icon {
        match self.bars() {
            Some(lit) => wire::Icon {
                mask: Some(wire::Image {
                    width: wifimodel::MASK_SIDE,
                    height: wifimodel::MASK_SIDE,
                    data: wifimodel::signal_mask(lit),
                }),
                ..wire::Icon::default()
            },
            None => lucide("link"),
        }
    }

    fn status(&self) -> u32 {
        if self.shown().is_none() {
            wire::STATUS_PASSIVE
        } else {
            wire::STATUS_ACTIVE
        }
    }
}

/// The interface name in `system/net/<name>/...`.
fn segment(topic: &str) -> Option<&str> {
    topic.split('/').nth(2).filter(|name| !name.is_empty())
}

/// Fold one interface list into `net`.
fn apply_interfaces(net: &mut Net, list: &stack_wire::InterfaceList) {
    net.primary = list.list.iter().find(|i| i.primary).map(|i| i.name.clone());
}

/// Fold one `system/net/<name>/wifi/state` value into `net`.
fn apply_wifi(net: &mut Net, name: &str, state: &wifi_wire::WifiState) {
    net.wifi.retain(|known| known.name != name);
    if net.wifi.len() < MAX_LINKS {
        net.wifi.push(Wifi {
            name: name.to_owned(),
            conn: Conn::from_wire(state.state),
            ssid: wifimodel::clean(&state.ssid, wifimodel::MAX_SSID),
            rssi_dbm: state.rssi_dbm.clamp(-127, 0),
        });
    }
}

/// Fold one `system/net/<name>/addr` value into `net`.
fn apply_addr(net: &mut Net, name: &str, addr: &stack_wire::AddressEvent) {
    let octets: Option<[u8; 4]> = addr.addr.as_slice().try_into().ok();
    net.addresses.retain(|(known, _)| known != name);
    if let Some(octets) = octets.filter(|octets| *octets != [0; 4]) {
        if net.addresses.len() < MAX_LINKS {
            net.addresses.push((name.to_owned(), octets));
        }
    }
}

/// Fold one `system/net/<name>/link` value into `net`.
fn apply_link(
    net: &mut Net,
    links: &mut Vec<(String, bool)>,
    name: &str,
    link: &nic_wire::LinkEvent,
) {
    links.retain(|(known, _)| known != name);
    if links.len() < MAX_LINKS {
        links.push((name.to_owned(), link.up));
    }
    net.link = links.iter().any(|(_, up)| *up);
    net.down = links
        .iter()
        .filter(|(_, up)| !*up)
        .map(|(name, _)| name.clone())
        .collect();
}

fn main() {
    xui_app::app(Channel::Netstatus, "Network status")
        .windowless()
        .run(run)
}

fn run(res: &Rc<Resident>) {
    // The hub drains a topic when its doorbell rang; this loop parks on their
    // doorbells with the tray's, so it wakes for a change and for nothing else.
    let mut wifi = wifi_wire::subscribe_system_net_wifi_state(&mut Watch, "+").ok();
    let mut addr = stack_wire::subscribe_system_net_addr(&mut Watch, "+").ok();
    let mut link = nic_wire::subscribe_system_net_link(&mut Watch, "+").ok();
    let mut interfaces = stack_wire::subscribe_system_net_interfaces(&mut Watch).ok();
    let bells: Vec<u64> = [
        wifi.as_ref().and_then(|stream| stream.handle().bell()),
        addr.as_ref().and_then(|stream| stream.handle().bell()),
        link.as_ref().and_then(|stream| stream.handle().bell()),
        interfaces
            .as_ref()
            .and_then(|stream| stream.handle().bell()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut net = Net::default();
    let mut links = Vec::new();
    let mut first = item(lucide("link"), &net.tooltip());
    first.status = net.status();
    // Before the shell serves the tray the item is set when it announces
    // itself (`xui_app::tray`), so a refusal here is not fatal.
    if let Err(code) = res.tray.borrow_mut().set(first) {
        trace::line!(Channel::Netstatus, "TRAY:LATER err={}", -code);
    }
    trace::pass!(Channel::Netstatus, "UP");
    let mut shown = (net.tooltip(), net.bars());
    loop {
        for wake in res.idle_with(&bells, 500) {
            if let Wake::Quit(_) = wake {
                return;
            }
        }
        if let Some(stream) = interfaces.as_mut() {
            pull(stream, |event| apply_interfaces(&mut net, &event.value));
        }
        if let Some(stream) = wifi.as_mut() {
            pull(stream, |event| {
                if let Some(name) = segment(&event.topic) {
                    apply_wifi(&mut net, name, &event.value);
                }
            });
        }
        if let Some(stream) = addr.as_mut() {
            pull(stream, |event| {
                if let Some(name) = segment(&event.topic) {
                    apply_addr(&mut net, name, &event.value);
                }
            });
        }
        if let Some(stream) = link.as_mut() {
            pull(stream, |event| {
                if let Some(name) = segment(&event.topic) {
                    apply_link(&mut net, &mut links, name, &event.value);
                }
            });
        }
        let now = (net.tooltip(), net.bars());
        if now != shown {
            trace::line!(Channel::Netstatus, "STATE {}", now.0);
            let patch = wire::UpdateArgs {
                icon: Some(net.icon()),
                tooltip: Some(now.0.clone()),
                status: Some(net.status()),
                ..wire::UpdateArgs::default()
            };
            let _ = res.tray.borrow_mut().update(patch);
            shown = now;
        }
    }
}
