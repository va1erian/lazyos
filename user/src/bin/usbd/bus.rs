//! One controller's device tree: root ports, hubs and the devices below
//! them, attached and detached as ports change (xHCI 4.3, 4.19).
//!
//! A root port change and a hub's status-change report take the same path:
//! a disconnect (or a replug seen as a connect change) detaches whatever
//! hung there, children first; a connect is enabled (debounce, reset,
//! recovery), enumerated and bound to its classes (`class.rs`). A device
//! that fails to enumerate is retried twice with a fresh reset, as real
//! devices sometimes need; one with nothing to drive is left alone until it
//! is unplugged.

use alloc::format;
use alloc::vec::Vec;

use user::sys;
use xhci::regs::portsc;
use xhci::route::Location;
use xhci::trb::{kind, Trb};

use alloc::string::String;

use super::class::{self, Function};
use super::device::Device;
use super::hc::Hc;
use super::pipe::{Report, MAX_REPORT};
use super::{port, Error, Settings};

/// Enumeration attempts per connect (Linux tries several too).
const ATTEMPTS: u32 = 3;

/// A device in the tree and what is bound to it.
struct Node {
    device: Device,
    functions: Vec<Function>,
    /// The hub slot and hub port it hangs off; `None` on a root port.
    parent: Option<(u8, u8)>,
}

pub(super) struct Controller {
    pub(super) hc: Hc,
    nodes: Vec<Node>,
    /// Places whose device is not driven (nothing to bind, or it failed):
    /// left alone until unplugged.
    skipped: Vec<Location>,
    /// The controller reported a fatal error and is no longer driven.
    dead: bool,
}

impl Controller {
    pub(super) fn new(hc: Hc) -> Controller {
        Controller {
            hc,
            nodes: Vec::new(),
            skipped: Vec::new(),
            dead: false,
        }
    }

    /// The controller's registers, ports and every enumerated device, as
    /// `USBD:DUMP:*` lines.
    pub(super) fn dump_lines(&mut self) -> Vec<String> {
        let mut lines = self.hc.dump_lines();
        for node in &mut self.nodes {
            lines.push(node.device.dump_line(&self.hc));
        }
        if self.dead {
            lines.push(format!("USBD:DUMP:DEAD hc={}", self.hc.index));
        }
        lines
    }

    /// Devices with at least one function bound (hubs included).
    pub(super) fn devices(&self) -> usize {
        self.nodes.len()
    }

    /// Look at every root port once (a device present at boot is handled
    /// like one plugged in later).
    pub(super) fn scan(&mut self, settings: &Settings) {
        port::power_on(&mut self.hc);
        for root in 1..=self.hc.info.ports {
            self.root_changed(root, settings);
        }
    }

    /// Handle every pending event; returns whether there was any.
    pub(super) fn poll(&mut self, settings: &Settings) -> bool {
        if self.dead {
            return false;
        }
        if self.hc.failed() {
            sys::write_str(&format!(
                "USBD:XHCI:ERROR hc={} the controller stopped (HSE/HCE); its devices are gone\n",
                self.hc.index
            ));
            while let Some(node) = self.nodes.pop() {
                for function in node.functions {
                    function.close(settings.trace);
                }
            }
            self.dead = true;
            // The snapshot changed (`USBD:DUMP:DEAD`): report it as an event.
            return true;
        }
        let mut busy = false;
        while let Some(event) = self.hc.next_event() {
            busy = true;
            match event.kind() {
                kind::PORT_STATUS_CHANGE => self.root_changed(event.port(), settings),
                kind::TRANSFER_EVENT => self.transfer(&event, settings),
                _ => {}
            }
        }
        busy
    }

    /// Serve the kernel's pending requests to this controller's sticks
    /// (`msc.rs`), one per stick; returns whether any was served.
    pub(super) fn serve_storage(&mut self) -> bool {
        let mut served = false;
        for node in &mut self.nodes {
            for function in &mut node.functions {
                if let Function::Msc(msc) = function {
                    served |= msc.serve(&mut self.hc, &mut node.device, 0);
                }
            }
        }
        served
    }

    /// Idle: wait up to a tick for a request to this controller's first
    /// live stick (instead of a plain nap, so it is served at once).
    /// Returns whether there was a stick to wait on.
    pub(super) fn wait_storage(&mut self) -> bool {
        for node in &mut self.nodes {
            for function in &mut node.functions {
                if let Function::Msc(msc) = function {
                    if msc.live() {
                        msc.serve(&mut self.hc, &mut node.device, sys::clock() + 1);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Bring root `port` in line with what is plugged into it.
    fn root_changed(&mut self, root: u8, settings: &Settings) {
        if root == 0 || root > self.hc.info.ports {
            return;
        }
        let status = self.hc.portsc(root);
        self.hc.set_portsc(root, portsc::ack_changes(status));
        let connected = status & portsc::CCS != 0;
        let here = |at: &Location| at.root_port == root && at.depth == 0;
        let bound = self.nodes.iter().position(|n| here(&n.device.at));
        // A disconnect, or a connect change while bound (unplugged and
        // replugged between two looks), ends the current device.
        if !connected || status & portsc::CSC != 0 {
            self.skipped.retain(|at| at.root_port != root);
            if let Some(index) = bound {
                self.detach(index, "unplugged", settings);
            }
        } else if bound.is_some() {
            return;
        }
        if !connected || self.skipped.iter().any(here) {
            return;
        }
        // A port that does not enable yet (a USB 3 link still training) is
        // not skipped: its next change event tries again.
        let Some(speed) = port::enable(&mut self.hc, root) else {
            return;
        };
        self.attach(Location::root(root, speed), None, settings);
    }

    /// Enumerate the device at `at` and bind it, retrying with a fresh
    /// reset; a hub then has its ports looked at.
    fn attach(&mut self, mut at: Location, parent: Option<(u8, u8)>, settings: &Settings) {
        let name = format!("{}-{}", self.hc.index, at);
        sys::write_str(&format!("USBD:PORT port={name} speed={:?}\n", at.speed));
        for attempt in 1..=ATTEMPTS {
            match self.enumerate(at) {
                Ok(Some(node)) => {
                    let slot = node.device.slot;
                    let hub = node.functions.iter().any(|f| matches!(f, Function::Hub(_)));
                    self.nodes.push(Node { parent, ..node });
                    if hub {
                        self.service_hub(slot, settings);
                    }
                    return;
                }
                Ok(None) => {
                    sys::write_str(&format!(
                        "USBD:PORT:SKIP port={name} nothing this driver binds\n"
                    ));
                    break;
                }
                Err(error) if attempt < ATTEMPTS => {
                    sys::write_str(&format!(
                        "USBD:PORT:RETRY port={name} attempt={attempt} {error}\n"
                    ));
                    match self.reset_again(at, parent) {
                        Some(again) => at = again,
                        None => return,
                    }
                }
                Err(error) => {
                    sys::write_str(&format!("USBD:PORT:FAIL port={name} {error}\n"));
                }
            }
        }
        self.skipped.push(at);
    }

    /// Address, read and bind; `Ok(None)` when nothing binds (the slot is
    /// given back). Any failure gives the slot back too.
    fn enumerate(&mut self, at: Location) -> Result<Option<Node>, Error> {
        let mut device = Device::enable(&mut self.hc, at)?;
        let bound = device
            .read_config(&mut self.hc)
            .and_then(|config| class::bind(&mut self.hc, &mut device, &config));
        match bound {
            Ok(functions) if !functions.is_empty() => Ok(Some(Node {
                device,
                functions,
                parent: None,
            })),
            Ok(_) => {
                device.release(&mut self.hc);
                Ok(None)
            }
            Err(error) => {
                device.release(&mut self.hc);
                Err(error)
            }
        }
    }

    /// Reset the port of `at` again before another attempt; the location
    /// it now has (the speed may differ), `None` when it is gone.
    fn reset_again(&mut self, at: Location, parent: Option<(u8, u8)>) -> Option<Location> {
        let Some((hub_slot, hub_port)) = parent else {
            let speed = port::enable(&mut self.hc, at.root_port)?;
            return Some(Location::root(at.root_port, speed));
        };
        let index = self.nodes.iter().position(|n| n.device.slot == hub_slot)?;
        let node = &mut self.nodes[index];
        let hub = node.functions.iter().find_map(|f| match f {
            Function::Hub(hub) => Some(hub),
            _ => None,
        })?;
        let multi_tt = hub.multi_tt;
        let speed = hub
            .enable(&mut self.hc, &mut node.device, hub_port)
            .ok()??;
        node.device
            .at
            .child(hub_slot, multi_tt, hub_port, speed)
            .ok()
    }

    /// Look at every port the hub in `slot` has queued: detach what left,
    /// attach what arrived (which may be another hub, serviced in turn).
    fn service_hub(&mut self, slot: u8, settings: &Settings) {
        // Each pass examines one port; the queue is bounded by the hub's
        // port count, and a port is only queued again by a new report.
        while let Some(index) = self.nodes.iter().position(|n| n.device.slot == slot) {
            let node = &mut self.nodes[index];
            let Some(hub) = node.functions.iter_mut().find_map(|f| match f {
                Function::Hub(hub) => Some(hub),
                _ => None,
            }) else {
                return;
            };
            let Some(hub_port) = hub.pending.pop_front() else {
                return;
            };
            let multi_tt = hub.multi_tt;
            let state = match hub.check(&mut self.hc, &mut node.device, hub_port) {
                Ok(state) => state,
                Err(error) => {
                    // A hub that cannot answer is as good as unplugged.
                    let why = format!("hub stopped answering: {error}");
                    self.detach(index, &why, settings);
                    return;
                }
            };
            let parent = Some((slot, hub_port));
            let child = self.nodes.iter().position(|n| n.parent == parent);
            let hub_at = self.nodes[index].device.at;
            if !state.connected || state.changed {
                self.skipped
                    .retain(|at| !below(at, &hub_at, hub_port, false));
                if let Some(child) = child {
                    self.detach(child, "unplugged", settings);
                }
            } else if child.is_some() {
                continue;
            }
            if !state.connected
                || self
                    .skipped
                    .iter()
                    .any(|at| below(at, &hub_at, hub_port, true))
            {
                continue;
            }
            let Some(index) = self.nodes.iter().position(|n| n.device.slot == slot) else {
                return;
            };
            let node = &mut self.nodes[index];
            let hub = node.functions.iter().find_map(|f| match f {
                Function::Hub(hub) => Some(hub),
                _ => None,
            });
            let Some(hub) = hub else { return };
            let speed = match hub.enable(&mut self.hc, &mut node.device, hub_port) {
                Ok(Some(speed)) => speed,
                Ok(None) => continue,
                Err(error) => {
                    let why = format!("hub stopped answering: {error}");
                    self.detach(index, &why, settings);
                    return;
                }
            };
            match hub_at.child(slot, multi_tt, hub_port, speed) {
                Ok(at) => self.attach(at, parent, settings),
                Err(_) => sys::write_str(&format!(
                    "USBD:PORT:SKIP port={}-{}.{hub_port} too deep or wrong speed ({speed:?})\n",
                    self.hc.index, hub_at
                )),
            }
        }
    }

    /// A transfer event: a report for a HID function or a hub, or an event
    /// of a pipe a class drives itself.
    fn transfer(&mut self, event: &Trb, settings: &Settings) {
        // Events of a detached slot are already dropped (`Device::release`).
        let Some(index) = self
            .nodes
            .iter()
            .position(|n| n.device.slot == event.slot())
        else {
            return;
        };
        let dci = event.endpoint();
        let node = &mut self.nodes[index];
        if !node.device.has_reports(dci) {
            if let Some(function) = node.functions.iter_mut().find(|f| f.owns(dci)) {
                if let Err(error) = function.on_transfer(&mut self.hc, &mut node.device, event) {
                    let why = format!("transfer: {error}");
                    self.detach(index, &why, settings);
                }
            }
            return;
        }
        let mut report = [0u8; MAX_REPORT];
        let len = match node.device.take_report(&mut self.hc, event, &mut report) {
            Report::Data(len) => len,
            Report::Stale => return,
            Report::Failed(code) => {
                let why = format!("transfer code={code}");
                self.detach(index, &why, settings);
                return;
            }
        };
        let slot = node.device.slot;
        let mut hub_changed = false;
        if let Some(function) = node.functions.iter_mut().find(|f| f.owns(dci)) {
            match function {
                Function::Hid { hid, .. } => {
                    let pressed = hid.report(&report[..len], settings.trace);
                    if pressed && settings.crash_on_key {
                        // The kernel releases the key on our death; `init`
                        // restarts us, and every controller is reset and
                        // every device re-enumerated.
                        sys::write_str("USBD:CRASH:TEST exiting with a key held\n");
                        sys::exit(3);
                    }
                }
                Function::Hub(hub) => {
                    hub.on_report(&report[..len]);
                    hub_changed = true;
                }
                // Its pipes are not report pipes: never reached.
                Function::Msc(_) => {}
            }
        }
        if hub_changed {
            self.service_hub(slot, settings);
        }
    }

    /// Detach node `index` and, first, everything below it: release what
    /// each held (keys and buttons), then give its slot and memory back.
    fn detach(&mut self, index: usize, why: &str, settings: &Settings) {
        let slot = self.nodes[index].device.slot;
        while let Some(child) = self
            .nodes
            .iter()
            .position(|n| n.parent.is_some_and(|(s, _)| s == slot))
        {
            self.detach(child, "its hub went away", settings);
        }
        let Some(index) = self.nodes.iter().position(|n| n.device.slot == slot) else {
            return;
        };
        let gone = self.nodes.swap_remove(index);
        let name = gone.device.name.clone();
        let kinds: Vec<&str> = gone
            .functions
            .iter()
            .map(|f| match f {
                Function::Hid { .. } => "hid",
                Function::Hub(_) => "hub",
                Function::Msc(_) => "msc",
            })
            .collect();
        for function in gone.functions {
            function.close(settings.trace);
        }
        let at = gone.device.at;
        self.skipped.retain(|skipped| !skipped.is_within(&at));
        gone.device.release(&mut self.hc);
        sys::write_str(&format!(
            "USBD:DETACH port={name} slot={slot} regions={} functions={} ({why})\n",
            self.hc.regions,
            kinds.join(",")
        ));
    }
}

/// Whether `at` is on `hub_port` of the hub at `hub_at` (`exact`), or
/// anywhere below that port.
fn below(at: &Location, hub_at: &Location, hub_port: u8, exact: bool) -> bool {
    at.is_within(hub_at)
        && at.depth > hub_at.depth
        && at.hub_port(hub_at.depth + 1) == Some(hub_port)
        && (!exact || at.depth == hub_at.depth + 1)
}
