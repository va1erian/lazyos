//! `netstatus` (`os.lazy.netstatus`): the network status in the taskbar tray
//! (docs/tray-plan.md T3). A resident applet with no window, shipped with
//! the network stack (`LAZYOS_NETD=1`). Its tooltip says whether the machine
//! is connected and with which address, from the retained topics `netd`
//! keeps (`system/net/<if>/addr`, and `system/net/interfaces` for which
//! interface is primary) and the NIC driver publishes
//! (`system/net/<if>/link`). A card whose link is down does not count as
//! connected, even while `netd` keeps its lease. Without a usable address
//! the item is `Passive` (it overflows first). It opens with every session.
//!
//! **Wi-Fi.** When `wlanmd` publishes a wireless interface
//! (`system/net/<if>/wifi/state`, docs/architecture/wifi.md) the item shows
//! the radio instead of the link icon (unless a cable carries the connection
//! and the radio is idle): a Wi-Fi fan with 1 to 4 bars lit, a faded one
//! when not connected, an animated one while joining, a struck-through one
//! when the radio is unavailable (`wifitray::glyph`), with the network and
//! signal in the tooltip. A click opens a menu of the networks heard (a
//! saved or open one connects, a secured new one opens the Network app on
//! its Wi-Fi page), Disconnect and Network settings. `radio.rs` follows the
//! radio; `crates/wifi-tray` decides what is shown. A wired-only machine
//! sees none of it.
//!
//! Serial evidence: `NETSTATUS:UP:PASS` once it started,
//! `NETSTATUS:STATE <tooltip>` on every change, `NETSTATUS:ICON <name>` when
//! the picture changes (`link`, `bars3`, ...), `NETSTATUS:WIFI:UP <if>`,
//! `NETSTATUS:WIFI:NETWORKS`, `NETSTATUS:WIFI:ACTION <what>` with its
//! `:PASS`/`:FAIL`, `NETSTATUS:QUIT:PASS`.

#[path = "netstatus/radio.rs"]
mod radio;

use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_net_nic_v1 as nic_wire;
use messenger_generated::os_lazy_net_stack_v1 as stack_wire;
use messenger_generated::os_lazy_net_wifi_v1 as wifi_wire;
use radio::Radio;
use trayclient::{item, lucide, wire, Event};
use wifitray::model::{prefer_wifi, show};
use xui_app::platform::topic_feed::{TopicEvent, TopicFeed};
use xui_app::resident::{Resident, Wake};
use xui_app::sys;

/// Ticks between looks at the topics (a second).
const LOOK_TICKS: u64 = 100;
/// Ticks between looks at the radio's state topic.
const WIFI_LOOK_TICKS: u64 = 50;
/// Most interfaces remembered.
const MAX_LINKS: usize = 8;

/// What the topics said.
#[derive(Default, PartialEq, Eq)]
struct Net {
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

    fn tooltip(&self) -> String {
        match self.shown() {
            Some((name, [a, b, c, d])) => {
                let more = match self.usable().count() - 1 {
                    0 => String::new(),
                    n => format!(" (+{n} more)"),
                };
                format!("Connected on {name}: {a}.{b}.{c}.{d}{more}")
            }
            None if self.link => String::from("Link up, no address yet"),
            None => String::from("Not connected"),
        }
    }

    fn status(&self) -> u32 {
        if self.shown().is_none() {
            wire::STATUS_PASSIVE
        } else {
            wire::STATUS_ACTIVE
        }
    }

    /// The address of interface `name`, if it has a usable one.
    fn address_of(&self, name: &str) -> Option<[u8; 4]> {
        self.usable()
            .find(|(known, _)| known == name)
            .map(|(_, address)| *address)
    }

    /// Whether an interface other than the radio `wireless` is usable.
    fn wired_up(&self, wireless: Option<&str>) -> bool {
        self.usable().any(|(name, _)| Some(name.as_str()) != wireless)
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

/// Fold one radio event (`wifi/state`, `wifi/scan`) into `radio`; the first
/// interface that publishes a state is the one followed.
fn apply_wifi(radio: &mut Option<Radio>, event: &TopicEvent) {
    let Some(name) = segment(&event.topic) else {
        return;
    };
    if event.topic.ends_with("/wifi/state") {
        let Ok(state) = wifi_wire::decode_system_net_wifi_state(&event.payload) else {
            return;
        };
        let followed = radio.get_or_insert_with(|| {
            println!("NETSTATUS:WIFI:UP {name}");
            Radio::new(name)
        });
        if followed.ifname == name {
            followed.apply_topic(&state);
        }
    } else if event.topic.ends_with("/wifi/scan") {
        if let Some(followed) = radio.as_mut().filter(|r| r.ifname == name) {
            followed.scan_done();
        }
    }
}

/// What the shell was last told, so only changes are sent.
#[derive(Default)]
struct Sent {
    tooltip: String,
    status: u32,
    /// The Wi-Fi glyph's name; `None` is the wired link icon.
    glyph: Option<String>,
    menu: Vec<wire::MenuItem>,
    /// Whether a click opens the menu (a radio exists).
    menu_mode: bool,
}

/// Show what the topics and the radio say. The first sight of a radio
/// replaces the item (a click opens the menu from then on); afterwards only
/// the parts that changed are updated.
fn refresh(res: &Resident, net: &Net, radio: Option<&Radio>, phase: u8, sent: &mut Sent) {
    let link = radio.map(|r| &r.link);
    let wireless = radio.map(|r| r.ifname.as_str());
    let shown = link
        .filter(|link| prefer_wifi(Some(link), net.wired_up(wireless)))
        .map(|link| show(link, phase));
    let (tooltip, status, glyph) = match &shown {
        Some(shown) => (
            shown.tooltip.clone(),
            shown.status,
            Some(shown.glyph),
        ),
        None => (net.tooltip(), net.status(), None),
    };
    let glyph_name = glyph.map(|glyph| glyph.name());
    let menu = radio.map_or(Vec::new(), |r| r.menu.rows.clone());
    let menu_mode = radio.is_some();
    if tooltip != sent.tooltip {
        println!("NETSTATUS:STATE {tooltip}");
    }
    if glyph_name != sent.glyph {
        println!("NETSTATUS:ICON {}", glyph_name.as_deref().unwrap_or("link"));
    }
    let mut tray = res.tray.borrow_mut();
    if menu_mode != sent.menu_mode {
        let mut full = item(icon_of(glyph), &tooltip);
        full.status = status;
        full.menu = menu.clone();
        if menu_mode {
            full.activate = wire::ACTIVATION_MENU;
        }
        let _ = tray.set(full);
    } else if tooltip != sent.tooltip
        || status != sent.status
        || glyph_name != sent.glyph
        || menu != sent.menu
    {
        let patch = wire::UpdateArgs {
            icon: (glyph_name != sent.glyph).then(|| icon_of(glyph)),
            tooltip: (tooltip != sent.tooltip).then(|| tooltip.clone()),
            status: (status != sent.status).then_some(status),
            menu: (menu != sent.menu).then(|| wire::Menu { rows: menu.clone() }),
            ..wire::UpdateArgs::default()
        };
        let _ = tray.update(patch);
    }
    *sent = Sent {
        tooltip,
        status,
        glyph: glyph_name,
        menu,
        menu_mode,
    };
}

/// The picture for `glyph`: the Wi-Fi mask, or the wired link outline.
fn icon_of(glyph: Option<wifitray::Glyph>) -> wire::Icon {
    match glyph {
        Some(glyph) => glyph.mask().icon(),
        None => lucide("link"),
    }
}

fn main() {
    let res = Resident::windowless("NETSTATUS");
    let mut feeds = [
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
    // The radio's state is retained (a late start sees it at once); a scan
    // is an edge, so a few are queued.
    let mut wifi_feeds = [
        TopicFeed::new(
            String::from(wifi_wire::TOPIC_SYSTEM_NET_WIFI_STATE),
            topics::QOS_LATEST,
            1,
            WIFI_LOOK_TICKS,
        ),
        TopicFeed::new(
            String::from(wifi_wire::TOPIC_SYSTEM_NET_WIFI_SCAN),
            topics::QOS_LATEST,
            2,
            LOOK_TICKS,
        ),
    ];
    let mut net = Net::default();
    let mut links = Vec::new();
    let mut radio: Option<Radio> = None;
    let mut first = item(lucide("link"), &net.tooltip());
    first.status = net.status();
    // Before the shell serves the tray the item is set when it announces
    // itself (`xui_app::tray`), so a refusal here is not fatal.
    if let Err(code) = res.tray.borrow_mut().set(first) {
        println!("NETSTATUS:TRAY:LATER err={}", -code);
    }
    println!("NETSTATUS:UP:PASS");
    let mut sent = Sent {
        tooltip: net.tooltip(),
        status: net.status(),
        ..Sent::default()
    };
    let mut phase = 0u8;
    loop {
        for wake in res.idle(500) {
            match wake {
                Wake::Quit(_) => {
                    let _ = res.tray.borrow_mut().clear();
                    println!("NETSTATUS:QUIT:PASS");
                    std::process::exit(0);
                }
                Wake::Tray(Event::MenuItem { id, .. }) => {
                    if let Some(radio) = radio.as_mut() {
                        radio.act(id, sys::clock_ticks());
                    }
                }
                _ => {}
            }
        }
        for feed in feeds.iter_mut() {
            for event in feed.poll() {
                apply(&mut net, &mut links, &event);
            }
        }
        for feed in wifi_feeds.iter_mut() {
            for event in feed.poll() {
                apply_wifi(&mut radio, &event);
            }
        }
        if let Some(radio) = radio.as_mut() {
            radio.set_address(net.address_of(&radio.ifname));
            radio.tick(sys::clock_ticks());
        }
        phase = phase.wrapping_add(1);
        refresh(&res, &net, radio.as_ref(), phase, &mut sent);
    }
}
