//! The control surface of the host (`os.lazy.bt.v1`, docs/bluetooth-plan.md
//! B5): the device list, discovery, pairing driven by a caller instead of a
//! boot argument, and forgetting a device. The shell (`user/src/bin/btd/`)
//! checks who is calling (`btpolicy`) and maps these calls and the
//! [`Effect::Device`] / [`Effect::Pairing`] notices to Messenger.
//!
//! **No silent pairing.** `pair` opens a window of [`UI_WINDOW_MS`] for one
//! address. While it is open the host answers that device's pairing requests
//! and, on BR/EDR, is discoverable (inquiry scan on) so a device that wants
//! to connect to the host can find it; when the pairing ends, fails or the
//! window passes, both go back to bonded devices only. Discovery lists
//! devices and never admits them.
//!
//! The discovery half is `discover.rs`; the hooks the pairing code calls
//! (passkey shown, number to compare, paired, failed) are `ctl_pair.rs`.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use btbond::InputClass;
use bthci::{BdAddr, Command};

use crate::ctl_types::*;
use crate::effect::Effect;
use crate::host::Host;
use crate::paging::Job;

/// How long one `Pair` keeps the window open.
pub const UI_WINDOW_MS: u64 = 60_000;
/// The longest discovery one call may ask for, in seconds.
pub const MAX_DISCOVERY_S: u32 = 60;
/// Scan results the host keeps.
pub const MAX_FOUND: usize = 32;
/// Page attempts at a pairing target before giving up.
const PAIR_ATTEMPTS: u8 = 3;
/// The longest name shown.
const MAX_NAME_CHARS: usize = 64;

/// What the BR/EDR inquiry is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inquiry {
    Idle,
    Running,
    /// `Inquiry_Cancel` was sent; its completion ends it.
    Cancelling,
}

/// A device a scan saw.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub addr: BdAddr,
    pub name: String,
    pub transport: Transport,
    pub input: InputKind,
    pub rssi: Option<i8>,
    /// A name request was made (answered or not): never repeated.
    pub name_tried: bool,
}

/// The one device the window is open for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target {
    pub addr: BdAddr,
    pub transport: Transport,
}

pub(crate) struct Ctl {
    pub found: Vec<Found>,
    pub discovery_until: Option<u64>,
    pub inquiry: Inquiry,
    pub inquiry_mode_set: bool,
    pub names: VecDeque<BdAddr>,
    /// The name request in flight and when to give up on it.
    pub name_inflight: Option<(BdAddr, u64)>,
    pub pairing: PairingState,
    pub target: Option<Target>,
    /// Inquiry scan is on (the host can be found).
    pub discoverable: bool,
    /// Harness builds: a passkey shown before the controller's version was
    /// known, to tell a simulated controller once it is (`le_sec.rs`).
    pub harness_passkey: Option<u32>,
}

impl Ctl {
    pub fn new() -> Ctl {
        Ctl {
            found: Vec::new(),
            discovery_until: None,
            inquiry: Inquiry::Idle,
            inquiry_mode_set: false,
            names: VecDeque::new(),
            name_inflight: None,
            pairing: PairingState::IDLE,
            target: None,
            discoverable: false,
            harness_passkey: None,
        }
    }
}

/// A name from the air, made safe to show: printable characters only, cut
/// to [`MAX_NAME_CHARS`].
pub(crate) fn clean_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{FFFD}')
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .into()
}

fn input_kind(class: InputClass) -> InputKind {
    match class {
        InputClass::Keyboard | InputClass::Combo => InputKind::Keyboard,
        InputClass::Pointer => InputKind::Pointer,
    }
}

impl Host {
    // ---- what the shell asks ---------------------------------------------------

    /// Bonded devices and unbonded scan results, one row per address.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let mut out: Vec<DeviceInfo> = self
            .store
            .iter()
            .filter_map(|b| self.device_info(b.addr))
            .collect();
        for found in &self.ctl.found {
            if !out.iter().any(|d| d.addr == found.addr) {
                if let Some(info) = self.device_info(found.addr) {
                    out.push(info);
                }
            }
        }
        out
    }

    /// The row of one device, if it is bonded or was found.
    pub(crate) fn device_info(&self, addr: BdAddr) -> Option<DeviceInfo> {
        let found = self.ctl.found.iter().find(|f| f.addr == addr);
        let bond = self.store.iter().find(|b| b.addr == addr);
        if found.is_none() && bond.is_none() {
            return None;
        }
        let name = match bond {
            Some(b) if !b.name.is_empty() => clean_name(b.name.as_bytes()),
            _ => found.map(|f| f.name.clone()).unwrap_or_default(),
        };
        Some(DeviceInfo {
            addr,
            name,
            transport: match bond {
                Some(b) if b.kind.is_le() => Transport::Le,
                Some(_) => Transport::BrEdr,
                None => found.map_or(Transport::BrEdr, |f| f.transport),
            },
            input: bond
                .map(|b| input_kind(b.input))
                .or(found.map(|f| f.input))
                .unwrap_or(InputKind::Unknown),
            rssi: found.and_then(|f| f.rssi),
            bonded: bond.is_some(),
            connected: self.is_typing(addr),
        })
    }

    pub fn is_discovering(&self) -> bool {
        self.ctl.discovery_until.is_some()
    }

    /// The pairing in progress, or the last one's outcome.
    pub fn pairing_state(&self) -> PairingState {
        self.ctl.pairing
    }

    /// Pair with a device the list shows. Opens the window for that address
    /// alone; the outcome arrives as [`Effect::Pairing`] notices.
    pub fn pair(&mut self, now: u64, addr: BdAddr) -> Result<(), CtlError> {
        if !self.init.is_armed() {
            return Err(CtlError::NoController);
        }
        if self.ctl.pairing.phase.active() {
            return Err(CtlError::Busy);
        }
        let transport = self
            .device_info(addr)
            .map(|d| d.transport)
            .ok_or(CtlError::NotFound)?;
        if transport == Transport::Le && !self.controller().le_supported {
            return Err(CtlError::NotFound);
        }
        self.stop_discovery(now);
        self.ctl.target = Some(Target { addr, transport });
        self.window_until = Some(now.saturating_add(UI_WINDOW_MS));
        self.le.denied.retain(|a| *a != addr);
        self.set_pairing(PairingState {
            phase: Phase::Connecting,
            addr: Some(addr),
            ..PairingState::IDLE
        });
        match transport {
            Transport::BrEdr => {
                self.set_discoverable(true);
                self.pager.add(Job::pairing(addr, PAIR_ATTEMPTS, now));
            }
            Transport::Le => self.le_pump(now),
        }
        Ok(())
    }

    /// Answer a number to compare.
    pub fn confirm_number(&mut self, now: u64, addr: BdAddr, accept: bool) -> Result<(), CtlError> {
        let state = self.ctl.pairing;
        if state.phase != Phase::ConfirmNumber || state.addr != Some(addr) {
            return Err(CtlError::Invalid);
        }
        let idx = self.link_idx_by_addr(addr).ok_or(CtlError::Invalid)?;
        let (handle, le) = (self.links[idx].handle, self.links[idx].le.is_some());
        if le {
            self.smp_input(now, handle, btsmp::Input::UserConfirm(accept));
        } else if accept {
            self.cmd(Command::UserConfirmationRequestReply { addr });
        } else {
            self.cmd(Command::UserConfirmationRequestNegativeReply { addr });
        }
        if accept {
            self.set_pairing(PairingState {
                phase: Phase::Connecting,
                number: 0,
                ..state
            });
        } else {
            self.ctl_failed(addr, FailReason::Rejected);
        }
        Ok(())
    }

    /// Give up the pairing in progress.
    pub fn cancel_pairing(&mut self, _now: u64) {
        let state = self.ctl.pairing;
        let Some(addr) = state.addr.filter(|_| state.phase.active()) else {
            return;
        };
        self.ctl_failed(addr, FailReason::Cancelled);
        if let Some(idx) = self.link_idx_by_addr(addr) {
            let link = &self.links[idx];
            if link.hid.is_none() {
                let handle = link.handle;
                self.disconnect(
                    handle,
                    bthci::opcode::status::CONNECTION_TERMINATED_BY_LOCAL_HOST,
                );
            }
        }
    }

    /// Remove a device: its bond, its link and its source.
    pub fn forget_device(&mut self, now: u64, addr: BdAddr) -> Result<(), CtlError> {
        let known = self.device_info(addr).ok_or(CtlError::NotFound)?;
        if self.ctl.pairing.phase.active() && self.ctl.pairing.addr == Some(addr) {
            self.cancel_pairing(now);
        }
        self.forget(now, addr);
        self.ctl.found.retain(|f| f.addr != addr);
        self.effect(Effect::Device(DeviceChange::Removed, known));
        Ok(())
    }

    // ---- shared plumbing ---------------------------------------------------------

    /// Record and announce the pairing state.
    pub(crate) fn set_pairing(&mut self, state: PairingState) {
        self.ctl.pairing = state;
        self.effect(Effect::Pairing(state));
    }

    /// The window closes: back to bonded devices only.
    pub(crate) fn end_pairing_window(&mut self, now: u64) {
        let target = self.ctl.target.take();
        self.window_until = None;
        self.set_discoverable(false);
        if let Some(target) = target {
            self.pager_forget(target.addr);
        }
        self.le_pump(now);
    }

    /// Inquiry scan on only while a pairing needs the host to be found.
    pub(crate) fn set_discoverable(&mut self, on: bool) {
        if self.ctl.discoverable != on && self.init.is_armed() {
            self.ctl.discoverable = on;
            self.cmd(Command::WriteScanEnable(if on { 0x03 } else { 0x02 }));
        }
    }

    /// Whether the window is open for `addr` by a `Pair` call.
    pub(crate) fn ctl_allows(&self, now: u64, addr: BdAddr) -> bool {
        self.window_open(now) && self.ctl.target.is_some_and(|t| t.addr == addr)
    }

    /// An LE pairing was asked for and the window is open.
    pub(crate) fn ctl_le_active(&self, now: u64) -> bool {
        self.window_open(now)
            && self
                .ctl
                .target
                .is_some_and(|t| t.transport == Transport::Le)
    }

    /// Deadlines of the control surface.
    pub(crate) fn ctl_deadline(&self) -> Option<u64> {
        let window = self.ctl.target.and(self.window_until);
        [
            self.ctl.discovery_until,
            window,
            self.ctl.name_inflight.map(|n| n.1),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn ctl_tick(&mut self, now: u64) {
        if self.ctl.discovery_until.is_some_and(|t| t <= now) {
            self.stop_discovery(now);
        }
        if self.ctl.name_inflight.is_some_and(|n| n.1 <= now) {
            self.ctl.name_inflight = None;
            self.pump_names(now);
        }
        let expired = self.ctl.target.is_some() && !self.window_open(now);
        if let (true, Some(addr)) = (expired, self.ctl.pairing.addr) {
            self.ctl_failed(addr, FailReason::Timeout);
        }
    }

    /// Device state changed (connected, bonded): tell the subscribers.
    pub(crate) fn ctl_changed(&mut self, addr: BdAddr) {
        if let Some(info) = self.device_info(addr) {
            self.effect(Effect::Device(DeviceChange::Changed, info));
        }
    }

    /// An input source opened or closed on a link.
    pub(crate) fn ctl_link_state(&mut self, handle: u16) {
        if let Some(addr) = self.link_idx(handle).map(|i| self.links[i].addr) {
            self.ctl_changed(addr);
        }
    }
}
