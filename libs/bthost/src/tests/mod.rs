//! Host tests: `btd`'s logic against the simulated controller.
//!
//! [`Rig`] is the I/O shell in miniature: it feeds the host what the
//! simulator sends, drains the host's queues into the simulator, and records
//! the effects the way the real shell would act on them (sources opened and
//! closed, the bond file saved, the kernel releasing keys a source still
//! held when it closes).

mod config;
mod firmware;
mod fw_images;
mod fw_rig;
mod hostile;
mod le;
mod le_flows;
mod le_hostile;
mod pairing;
mod reconnect;
mod refusal;
mod store;

use std::collections::BTreeSet;
pub use std::string::{String, ToString};
pub use std::vec::Vec;
pub use std::{format, vec};

use btbond::BondStore;
use bthci::BdAddr;
use bthid::decode::Event as HidEvent;
use btsim::{BrPeer, Controller, HciIn, LePeer, PeerId, SimConfig, SimController};
use usbhid::boot::KeyEdge;

use crate::{Config, Effect, Host, PairPolicy, SourceKind};

pub const KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x01];
pub const MOUSE: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x02];
pub const KEY_A: u8 = 0x04;
pub const LE_KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x01, 0x01];
pub const LE_MOUSE: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x01, 0x02];

/// A controller with an LE keyboard (Passkey Entry) and an LE mouse (Just
/// Works), both in pairing mode.
pub fn le_world() -> (SimController, PeerId, PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_le_peer(LePeer::keyboard(LE_KBD));
    let mouse = sim.add_le_peer(LePeer::mouse(LE_MOUSE));
    for id in [kbd, mouse] {
        sim.enter_pairing_mode(id, 0);
    }
    (sim, kbd, mouse)
}

/// The harness configuration that pairs these LE addresses.
pub fn le_pair_with(targets: &[[u8; 6]]) -> Config {
    Config {
        trace_reports: true,
        le_targets: targets.iter().map(|a| addr(*a)).collect(),
        ..Config::default()
    }
}

pub fn addr(msb: [u8; 6]) -> BdAddr {
    BdAddr::from_msb(msb)
}

pub fn world() -> (SimController, PeerId, PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    let mouse = sim.add_br_peer(BrPeer::mouse(MOUSE));
    (sim, kbd, mouse)
}

pub fn pair_with(targets: &[[u8; 6]]) -> Config {
    Config {
        trace_reports: true,
        pair: PairPolicy::Targets(targets.iter().map(|a| addr(*a)).collect()),
        ..Config::default()
    }
}

pub struct Rig {
    pub sim: SimController,
    pub host: Host,
    pub now: u64,
    pub started_at: u64,
    pub traces: Vec<String>,
    pub saved: Option<Vec<u8>>,
    pub saves: u32,
    /// `(handle, kind)` of every source currently open.
    pub sources: Vec<(u16, SourceKind)>,
    pub opened: Vec<(u16, SourceKind)>,
    /// Key edges published, in order.
    pub keys: Vec<(u16, bool)>,
    pub pointer_events: Vec<HidEvent>,
    /// What the kernel would have to release itself when a source closed.
    pub kernel_released: Vec<u16>,
    held: BTreeSet<u16>,
    pub failed: Option<String>,
    /// While set, the host's ACL goes here instead of to the simulator.
    capture: Option<Vec<Vec<u8>>>,
    /// Everything the simulator sent the host, in order.
    pub recorded: Vec<HciIn>,
    /// Deliver each `Encryption_Change` after the next ACL packet, as two USB
    /// endpoints may (no order is promised between events and data).
    pub encryption_late: bool,
    late_events: Vec<HciIn>,
}

impl Rig {
    pub fn boot(sim: SimController, store: BondStore, cfg: Config, now: u64) -> Rig {
        let mut host = Host::new(cfg, store, now);
        host.seed_entropy(b"bthost test entropy");
        Rig {
            sim,
            host,
            now,
            started_at: now,
            traces: Vec::new(),
            saved: None,
            saves: 0,
            sources: Vec::new(),
            opened: Vec::new(),
            keys: Vec::new(),
            pointer_events: Vec::new(),
            kernel_released: Vec::new(),
            held: BTreeSet::new(),
            failed: None,
            capture: None,
            recorded: Vec::new(),
            encryption_late: false,
            late_events: Vec::new(),
        }
    }

    /// A fresh host with no bonds, brought up and idle.
    pub fn fresh(sim: SimController, cfg: Config) -> Rig {
        let mut rig = Rig::boot(sim, BondStore::new(), cfg, 0);
        rig.run_for(300);
        rig
    }

    /// Power the host off and boot a new one on the same controller with the
    /// bonds as the file layer would hold them.
    pub fn reboot(self, cfg: Config, at: u64) -> Rig {
        let bytes = self.host.store().encode().expect("bonds encode");
        let mut sim = self.sim;
        sim.power_cycle();
        Rig::boot(
            sim,
            BondStore::decode(&bytes).expect("bonds decode"),
            cfg,
            at,
        )
    }

    pub fn run_for(&mut self, ms: u64) {
        let end = self.now + ms;
        for _ in 0..1_000_000 {
            self.settle();
            let due = [self.sim.next_due(), self.host.next_deadline()]
                .into_iter()
                .flatten()
                .min();
            match due {
                Some(t) if t <= end => {
                    self.now = self.now.max(t);
                    self.host.on_tick(self.now);
                }
                _ => break,
            }
        }
        self.now = end;
        self.host.on_tick(self.now);
        self.settle();
    }

    pub fn settle(&mut self) {
        loop {
            let mut progress = false;
            while let Some(packet) = self.host.poll_command(self.now) {
                self.sim.command(self.now, &packet);
                progress = true;
            }
            while let Some(packet) = self.host.poll_acl() {
                match &mut self.capture {
                    Some(held) => held.push(packet),
                    None => self.sim.acl_out(self.now, &packet),
                }
                progress = true;
            }
            while let Some(incoming) = self.sim.poll_in(self.now) {
                self.recorded.push(incoming.clone());
                let late = self.encryption_late
                    && matches!(&incoming, HciIn::Event(b) if b.first() == Some(&0x08));
                if late {
                    self.late_events.push(incoming);
                } else {
                    let acl = matches!(incoming, HciIn::Acl(_));
                    self.deliver(incoming);
                    if acl {
                        self.release_held();
                    }
                }
                progress = true;
            }
            if !self.late_events.is_empty() {
                self.release_held();
                progress = true;
            }
            while let Some(effect) = self.host.poll_effect() {
                self.act(effect);
                progress = true;
            }
            if !progress {
                return;
            }
        }
    }

    fn deliver(&mut self, incoming: HciIn) {
        match incoming {
            HciIn::Event(bytes) => self.host.on_event(self.now, &bytes),
            HciIn::Acl(bytes) => self.host.on_acl(self.now, &bytes),
        }
    }

    fn release_held(&mut self) {
        for incoming in core::mem::take(&mut self.late_events) {
            self.deliver(incoming);
        }
    }

    fn act(&mut self, effect: Effect) {
        match effect {
            Effect::OpenSource { handle, kind } => {
                self.sources.push((handle, kind));
                self.opened.push((handle, kind));
            }
            Effect::Input { event, .. } => match event {
                HidEvent::Key(KeyEdge { usage, pressed }) => {
                    self.keys.push((usage, pressed));
                    if pressed {
                        self.held.insert(usage);
                    } else {
                        self.held.remove(&usage);
                    }
                }
                other => self.pointer_events.push(other),
            },
            Effect::CloseSources { handle } => {
                self.sources.retain(|(h, _)| *h != handle);
                // Whatever is still held when a source closes, the kernel
                // releases on its own; the host must have done it first.
                self.kernel_released.extend(self.held.iter().copied());
                self.held.clear();
            }
            Effect::SaveBonds(bytes) => {
                self.saved = Some(bytes);
                self.saves += 1;
            }
            Effect::Trace(text) => self.traces.push(text),
            Effect::Failed(why) => self.failed = Some(why),
            Effect::LoadFirmware { .. } => {}
        }
    }

    /// Hand the host an HCI event the simulator did not send (a hostile or
    /// buggy controller, or a state the simulator cannot be put in).
    pub fn inject_event(&mut self, event: bthci::Event) {
        self.capture = Some(Vec::new());
        self.host.on_event(self.now, &event.encode());
        // Let the simulator answer, so the next command has its credit.
        self.run_for(5);
        self.capture = None;
    }

    /// Hand the host one ACL PDU on `handle` and return the ACL packets it
    /// answers with (kept from the simulator, which knows no such link).
    pub fn inject_pdu(&mut self, handle: u16, pdu: &[u8]) -> Vec<Vec<u8>> {
        let packet = bthci::acl::encode(handle, bthci::Boundary::FirstNonFlushable, pdu).unwrap();
        self.capture = Some(Vec::new());
        self.host.on_acl(self.now, &packet);
        self.settle();
        self.capture.take().unwrap_or_default()
    }

    pub fn traced(&self, needle: &str) -> bool {
        self.traces.iter().any(|t| t.contains(needle))
    }

    pub fn trace_line(&self, needle: &str) -> Option<&String> {
        self.traces.iter().find(|t| t.contains(needle))
    }

    pub fn assert_clean(&self) {
        assert!(
            self.sim.violations().is_empty(),
            "{:?}",
            self.sim.violations()
        );
        assert_eq!(self.failed, None);
    }

    /// The keys typed so far: press edges as usages.
    pub fn typed(&self) -> Vec<u16> {
        self.keys
            .iter()
            .filter(|(_, d)| *d)
            .map(|(u, _)| *u)
            .collect()
    }
}
