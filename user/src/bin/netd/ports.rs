//! The cards `netd` drives: one [`Port`] per `os.lazy.net.nic/<ifname>` service
//! in the registry, found by listing it, so cards appear and vanish while the
//! stack runs (docs/wifi-prerequisites-plan.md section 3.1).
//!
//! A port has a [`Nic`] (the driver client and the rings) and, once the card
//! has been read, an interface in the [`Net`]. A driver that restarts keeps
//! its interface (and lease): only the rings are rebuilt. A name that stays
//! out of the registry for [`GONE_TICKS`] is a card that was removed, and its
//! interface goes with it; that is a normal event, not an error.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use netstack::config::Mode;
use netstack::net::MAX_INTERFACES;
use netstack::{Net, RingDevice, Stack};
use user::messenger::net::{self as nic_api, wire as nic_wire};
use user::sys;

use super::config::Config;
use super::nic::Nic;

/// Ticks between looks at the registry.
const SCAN_TICKS: u64 = 100;
/// Ticks a card's name may be missing before its interface is removed. A
/// driver `init` restarts is back well within this.
const GONE_TICKS: u64 = 300;
/// The largest frame the stack builds for a card (`Info` carries the real
/// one, which the ring clamps anyway).
const MAX_FRAME: usize = 1514;

pub(super) struct Port {
    pub(super) ifname: String,
    pub(super) nic: Nic,
    pub(super) mode: Mode,
    /// The interface in the `Net`, once the card has been read.
    pub(super) slot: Option<usize>,
    /// The driver's task (the sender of its `Notify` messages).
    driver: u64,
    missing_since: Option<u64>,
}

pub(super) struct Ports {
    pub(super) list: Vec<Port>,
    next_scan: u64,
    empty_logged: u32,
    list_errors: u32,
    /// Times a ring attachment was dropped and made again.
    pub(super) resets: u64,
}

impl Ports {
    pub(super) fn new() -> Ports {
        Ports {
            list: Vec::new(),
            next_scan: 0,
            empty_logged: 0,
            list_errors: 0,
            resets: 0,
        }
    }

    pub(super) fn by_slot(&self, slot: usize) -> Option<&Port> {
        self.list.iter().find(|port| port.slot == Some(slot))
    }

    /// Look at the registry: new cards get a port, cards that stayed away too
    /// long lose theirs and their interface.
    pub(super) fn scan(&mut self, net: &mut Net, config: &mut Config, tick: u64) {
        if tick < self.next_scan {
            return;
        }
        self.next_scan = tick + SCAN_TICKS;
        let present = match nic_api::present() {
            Ok(present) => present,
            Err(error) => {
                // Said once, then every tenth time; the next scan tries again.
                if self.list_errors.is_multiple_of(10) {
                    sys::write_str(&format!(
                        "NETD:NIC:LIST:FAIL the registry cannot be listed: {}\n",
                        error.message()
                    ));
                }
                self.list_errors += 1;
                return;
            }
        };
        for card in &present {
            let full = self.list.len() >= MAX_INTERFACES;
            match self.list.iter_mut().find(|port| port.ifname == card.ifname) {
                Some(port) => {
                    port.missing_since = None;
                    port.driver = card.driver;
                }
                // More names than interfaces the stack can hold are ignored
                // (the registry is shared: anyone may register a name).
                None if full => {}
                None => {
                    sys::write_str(&format!("NETD:NIC:FOUND if={}\n", card.ifname));
                    self.list.push(Port {
                        mode: config.read(&card.ifname).unwrap_or(Mode::Dhcp),
                        nic: Nic::new(&card.ifname),
                        ifname: card.ifname.clone(),
                        slot: None,
                        driver: card.driver,
                        missing_since: None,
                    });
                }
            }
        }
        for port in &mut self.list {
            if present.iter().all(|card| card.ifname != port.ifname) {
                port.missing_since.get_or_insert(tick);
            }
        }
        self.drop_gone(net, tick);
        if self.list.is_empty() {
            // Say it once, then every tenth time: no card is a normal state.
            if self.empty_logged.is_multiple_of(10) {
                sys::write_str("NETD:NIC:WAIT no NIC driver is registered\n");
            }
            self.empty_logged += 1;
        }
    }

    fn drop_gone(&mut self, net: &mut Net, tick: u64) {
        self.list.retain_mut(|port| {
            let gone = port
                .missing_since
                .is_some_and(|since| tick.saturating_sub(since) >= GONE_TICKS);
            if gone {
                if let Some(slot) = port.slot.take() {
                    net.remove_interface(slot);
                }
                port.nic.forget_attachment("card removed");
                port.nic.release();
                sys::write_str(&format!("NETD:NIC:GONE if={}\n", port.ifname));
            }
            !gone
        });
    }

    /// Bring every port to where it should be: an interface for a card that
    /// has been read, rings for an interface whose driver is there, and no
    /// rings for a driver that went quiet, broke its ring, or was asked to
    /// start over (`rebuild`).
    pub(super) fn maintain(
        &mut self,
        net: &mut Net,
        seed: fn() -> u64,
        tick: u64,
        now_ms: i64,
        rebuild: bool,
    ) {
        for port in &mut self.list {
            if port.slot.is_none() && !port.create(net, seed, now_ms, tick) {
                continue;
            }
            let Some(slot) = port.slot else { continue };
            let Some(unit) = net.unit_mut(slot) else {
                port.slot = None;
                continue;
            };
            if port.nic.attached() {
                let why = if rebuild {
                    Some("reattach requested")
                } else if unit.stack.device().is_poisoned() {
                    Some("ring corrupt")
                } else if port.nic.silent(tick) {
                    Some("driver silent")
                } else {
                    None
                };
                if let Some(why) = why {
                    port.nic.detach(&mut unit.stack, why);
                    self.resets += 1;
                }
            }
            if !port.nic.attached() && port.nic.should_try(tick) {
                match port.nic.attach(&mut unit.stack, tick) {
                    Ok(()) => {
                        if let Some(card) = port.nic.card {
                            sys::write_str(&format!(
                                "NETD:NIC:ATTACHED if={} mac={} mtu={} link={}\n",
                                port.ifname,
                                super::mac_text(&card.mac),
                                card.mtu,
                                card.link
                            ));
                            net.set_attached(slot, true);
                            net.set_link(slot, card.link);
                        }
                    }
                    Err(message) => {
                        sys::write_str(&format!("NETD:NIC:WAIT if={} {message}\n", port.ifname));
                    }
                }
            }
            if !port.nic.attached() {
                net.set_attached(slot, false);
            }
        }
    }

    /// A message from a driver arrived: it is alive, and a link change is
    /// worth a look at its card. `sender` is the driver's task; a sender no
    /// port knows (a driver restarted since the scan) counts for all of them.
    pub(super) fn heard(&mut self, net: &mut Net, sender: u64, link_change: bool, tick: u64) {
        let known = self.list.iter().any(|port| port.driver == sender);
        for port in &mut self.list {
            if known && port.driver != sender {
                continue;
            }
            port.nic.heard(tick);
            if !link_change {
                continue;
            }
            if let (Some(link), Some(slot)) = (port.nic.refresh_link(), port.slot) {
                net.set_link(slot, link);
                sys::write_str(&format!("NETD:LINK if={} up={link}\n", port.ifname));
            }
        }
    }

    /// After a poll: tell each driver whose transmit ring has frames, and ask
    /// every driver to wake us. Whether any receive ring already holds frames.
    pub(super) fn pump(&mut self, net: &mut Net) -> bool {
        let mut pending = false;
        for port in &mut self.list {
            let Some(unit) = port.slot.and_then(|slot| net.unit_mut(slot)) else {
                continue;
            };
            if unit.stack.device_mut().take_tx_notify() {
                port.nic.kick(&mut unit.stack);
            }
            // Ask the driver to wake us, then look once more so a frame that
            // landed in between is not missed.
            unit.stack.device_mut().arm_rx();
            pending |= unit.stack.device_mut().rx_pending();
        }
        pending
    }

    /// Read every interface's configuration again; one that changed is
    /// rebuilt (its sockets end) with the new mode.
    pub(super) fn refresh_config(&mut self, net: &mut Net, config: &mut Config) {
        for port in &mut self.list {
            let Some(mode) = config.read(&port.ifname) else {
                return;
            };
            if mode == port.mode {
                continue;
            }
            sys::write_str(&format!(
                "NETD:RESTART if={} the configuration changed\n",
                port.ifname
            ));
            port.mode = mode;
            // The interface goes first: its device stops reading the rings.
            if let Some(slot) = port.slot.take() {
                net.remove_interface(slot);
            }
            port.nic.forget_attachment("configuration changed");
        }
    }
}

impl Port {
    /// Read the card and add its interface; whether there is one now.
    fn create(&mut self, net: &mut Net, seed: fn() -> u64, now_ms: i64, tick: u64) -> bool {
        if !self.nic.should_try(tick) {
            return false;
        }
        let card = match self.nic.try_probe(tick) {
            Ok(card) => card,
            Err(message) => {
                sys::write_str(&format!("NETD:NIC:WAIT if={} {message}\n", self.ifname));
                return false;
            }
        };
        let stack = Stack::new(
            RingDevice::detached(MAX_FRAME),
            card.mac,
            seed(),
            now_ms,
            &self.mode,
        );
        match net.add_interface(&self.ifname, card.kind, stack, card.link) {
            Ok(slot) => {
                self.slot = Some(slot);
                true
            }
            Err(error) => {
                sys::write_str(&format!(
                    "NETD:NIC:WAIT if={} cannot add the interface: {error:?}\n",
                    self.ifname
                ));
                false
            }
        }
    }
}

/// The `NotifyBit` of a link change, from the driver's interface.
pub(super) const LINK_CHANGE: u32 = 1 << nic_wire::NOTIFY_BIT_LINK_CHANGE;
