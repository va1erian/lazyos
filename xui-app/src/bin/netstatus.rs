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

use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_net_nic_v1 as nic_wire;
use messenger_generated::os_lazy_net_stack_v1 as stack_wire;
use messenger_generated::os_lazy_net_wifi_v1 as wifi_wire;
use trayclient::{item, lucide, wire};
use xui_app::net::wifimodel::{self, Conn};
use xui_app::platform::topic_feed::{TopicEvent, TopicFeed};
use xui_app::resident::{Resident, Wake};

/// Ticks between looks at the topics (a second).
const LOOK_TICKS: u64 = 100;
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

/// Fold one event into `net`.
fn apply(net: &mut Net, links: &mut Vec<(String, bool)>, event: &TopicEvent) {
    if event.topic == stack_wire::TOPIC_SYSTEM_NET_INTERFACES {
        if let Ok(list) = stack_wire::decode_system_net_interfaces(&event.payload) {
            net.primary = list.list.iter().find(|i| i.primary).map(|i| i.name.clone());
        }
        return;
    }
    let Some(name) = segment(&event.topic) else {
        return;
    };
    if event.topic.ends_with("/wifi/state") {
        if let Ok(state) = wifi_wire::decode_system_net_wifi_state(&event.payload) {
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
        return;
    }
    if event.topic.ends_with("/addr") {
        let Ok(addr) = stack_wire::decode_system_net_addr(&event.payload) else {
            return;
        };
        let octets: Option<[u8; 4]> = addr.addr.as_slice().try_into().ok();
        net.addresses.retain(|(known, _)| known != name);
        if let Some(octets) = octets.filter(|octets| *octets != [0; 4]) {
            if net.addresses.len() < MAX_LINKS {
                net.addresses.push((name.to_owned(), octets));
            }
        }
    } else if let Ok(link) = nic_wire::decode_system_net_link(&event.payload) {
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
}

fn main() {
    let res = Resident::windowless("NETSTATUS");
    let mut feeds = [
        TopicFeed::new(
            String::from(wifi_wire::TOPIC_SYSTEM_NET_WIFI_STATE),
            topics::QOS_LATEST,
            1,
            LOOK_TICKS,
        ),
        TopicFeed::new(
            String::from(stack_wire::TOPIC_SYSTEM_NET_ADDR),
            topics::QOS_LATEST,
            1,
            LOOK_TICKS,
        ),
        TopicFeed::new(
            String::from(nic_wire::TOPIC_SYSTEM_NET_LINK),
            topics::QOS_LATEST,
            1,
            LOOK_TICKS,
        ),
        TopicFeed::new(
            String::from(stack_wire::TOPIC_SYSTEM_NET_INTERFACES),
            topics::QOS_LATEST,
            1,
            LOOK_TICKS,
        ),
    ];
    let mut net = Net::default();
    let mut links = Vec::new();
    let mut first = item(lucide("link"), &net.tooltip());
    first.status = net.status();
    // Before the shell serves the tray the item is set when it announces
    // itself (`xui_app::tray`), so a refusal here is not fatal.
    if let Err(code) = res.tray.borrow_mut().set(first) {
        println!("NETSTATUS:TRAY:LATER err={}", -code);
    }
    println!("NETSTATUS:UP:PASS");
    let mut shown = (net.tooltip(), net.bars());
    loop {
        for wake in res.idle(500) {
            if let Wake::Quit(_) = wake {
                let _ = res.tray.borrow_mut().clear();
                println!("NETSTATUS:QUIT:PASS");
                std::process::exit(0);
            }
        }
        for feed in feeds.iter_mut() {
            for event in feed.poll() {
                apply(&mut net, &mut links, &event);
            }
        }
        let now = (net.tooltip(), net.bars());
        if now != shown {
            println!("NETSTATUS:STATE {}", now.0);
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
