//! `netstatus` (`os.lazy.netstatus`): the network status in the taskbar tray
//! (docs/tray-plan.md T3). A resident applet with no window, shipped with
//! the network stack (`LAZYOS_NETD=1`): a Lucide `link` icon whose tooltip
//! says whether the machine is connected and with which address, from the
//! retained topics `netd` keeps (`system/net/<if>/addr`) and the NIC driver
//! publishes (`system/net/<nic>/link`). Without an address the item is
//! `Passive` (it overflows first). It opens with every session.
//!
//! Serial evidence: `NETSTATUS:UP:PASS` once it started,
//! `NETSTATUS:STATE <tooltip>` on every change, `NETSTATUS:QUIT:PASS`.

use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_net_nic_v1 as nic_wire;
use messenger_generated::os_lazy_net_stack_v1 as stack_wire;
use trayclient::{item, lucide, wire};
use xui_app::platform::topic_feed::{TopicEvent, TopicFeed};
use xui_app::resident::{Resident, Wake};

/// Ticks between looks at the topics (a second).
const LOOK_TICKS: u64 = 100;
/// Most interfaces remembered.
const MAX_LINKS: usize = 8;

/// What the topics said.
#[derive(Default, PartialEq, Eq)]
struct Net {
    /// The first interface with an address, and the address.
    address: Option<(String, [u8; 4])>,
    /// Whether any card reported its link up.
    link: bool,
}

impl Net {
    fn tooltip(&self) -> String {
        match &self.address {
            Some((name, [a, b, c, d])) => format!("Connected on {name}: {a}.{b}.{c}.{d}"),
            None if self.link => String::from("Link up, no address yet"),
            None => String::from("Not connected"),
        }
    }

    fn status(&self) -> u32 {
        if self.address.is_some() {
            wire::STATUS_ACTIVE
        } else {
            wire::STATUS_PASSIVE
        }
    }
}

/// The interface name in `system/net/<name>/...`.
fn segment(topic: &str) -> Option<&str> {
    topic.split('/').nth(2).filter(|name| !name.is_empty())
}

/// Fold one event into `net`.
fn apply(net: &mut Net, links: &mut Vec<(String, bool)>, event: &TopicEvent) {
    let Some(name) = segment(&event.topic) else {
        return;
    };
    if event.topic.ends_with("/addr") {
        let Ok(addr) = stack_wire::decode_system_net_addr(&event.payload) else {
            return;
        };
        let octets: Option<[u8; 4]> = addr.addr.as_slice().try_into().ok();
        match octets.filter(|octets| *octets != [0; 4]) {
            Some(octets) => net.address = Some((name.to_owned(), octets)),
            None if net.address.as_ref().is_some_and(|(known, _)| known == name) => {
                net.address = None
            }
            None => {}
        }
    } else if let Ok(link) = nic_wire::decode_system_net_link(&event.payload) {
        links.retain(|(known, _)| known != name);
        if links.len() < MAX_LINKS {
            links.push((name.to_owned(), link.up));
        }
        net.link = links.iter().any(|(_, up)| *up);
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
    let mut shown = net.tooltip();
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
        let tooltip = net.tooltip();
        if tooltip != shown {
            println!("NETSTATUS:STATE {tooltip}");
            let patch = wire::UpdateArgs {
                tooltip: Some(tooltip.clone()),
                status: Some(net.status()),
                ..wire::UpdateArgs::default()
            };
            let _ = res.tray.borrow_mut().update(patch);
            shown = tooltip;
        }
    }
}
