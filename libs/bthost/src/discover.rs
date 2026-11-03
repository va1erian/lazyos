//! Discovery: BR/EDR Inquiry (with RSSI and extended results) followed by
//! Remote Name Requests, and an LE scan that lists devices advertising as a
//! keyboard or mouse (docs/bluetooth-plan.md B5).
//!
//! Discovery only lists. A device found here is never connected to and never
//! admitted: that takes a `pair` call (and its window), so a stranger that
//! shows up in a scan cannot type. Only Peripheral-class devices (BR/EDR) and
//! HID advertisers (LE) are listed; both come from a stranger's packets and
//! are bounded ([`MAX_FOUND`] entries, names cut and cleaned).

use alloc::vec::Vec;

use bthci::event::AdvReport;
use bthci::inquiry::{self, Entry, InquiryEvent, Mode};
use bthci::opcode::status;
use bthci::{BdAddr, Event};

use crate::ctl::{clean_name, Found, Inquiry, MAX_DISCOVERY_S, MAX_FOUND};
use crate::ctl_types::*;
use crate::effect::Effect;
use crate::host::Host;
use crate::le_link::is_hid_adv;

/// One inquiry round: 3 units of 1.28 s, so names follow quickly.
const INQUIRY_UNITS: u8 = 3;
/// A name request that has not answered after this long is given up.
const NAME_TIMEOUT_MS: u64 = 12_000;
/// A change in signal strength worth telling a subscriber about.
const RSSI_STEP: i16 = 10;
/// Class of device bits 6 and 7 of the minor class for a peripheral:
/// keyboard, pointing device, both.
const MINOR_KEYBOARD: u32 = 0x40;
const MINOR_POINTER: u32 = 0x80;
/// LE appearance subcategories of the HID category.
const APPEARANCE_KEYBOARD: u16 = 1;
const APPEARANCE_MOUSE: u16 = 2;

fn class_input(class_of_device: u32) -> InputKind {
    match class_of_device & 0xC0 {
        MINOR_KEYBOARD | 0xC0 => InputKind::Keyboard,
        MINOR_POINTER => InputKind::Pointer,
        _ => InputKind::Unknown,
    }
}

/// What an advertisement says the device is, from its appearance.
fn adv_input(data: &[u8]) -> InputKind {
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len);
        if len == 0 || len > tail.len() {
            break;
        }
        let (field, next) = tail.split_at(len);
        rest = next;
        if field[0] == 0x19 && field.len() >= 3 {
            let appearance = u16::from_le_bytes([field[1], field[2]]);
            return match (appearance >> 6 == 0x0F).then_some(appearance & 0x3F) {
                Some(APPEARANCE_KEYBOARD) => InputKind::Keyboard,
                Some(APPEARANCE_MOUSE) => InputKind::Pointer,
                _ => InputKind::Unknown,
            };
        }
    }
    InputKind::Unknown
}

impl Host {
    /// Scan for `seconds` (clamped to 1..=60). A running scan is extended.
    pub fn start_discovery(&mut self, now: u64, seconds: u32) -> Result<(), CtlError> {
        if !self.init.is_armed() {
            return Err(CtlError::NoController);
        }
        if self.ctl.pairing.phase.active() {
            return Err(CtlError::Busy);
        }
        let seconds = u64::from(seconds.clamp(1, MAX_DISCOVERY_S));
        self.ctl.discovery_until = Some(now.saturating_add(seconds * 1000));
        self.ctl_start_inquiry();
        self.le_pump(now);
        Ok(())
    }

    /// End discovery: the inquiry is cancelled and the LE scan stops.
    pub fn stop_discovery(&mut self, now: u64) {
        self.ctl.discovery_until = None;
        self.ctl.names.clear();
        if self.ctl.inquiry == Inquiry::Running {
            self.ctl.inquiry = Inquiry::Cancelling;
            self.cmd(inquiry::inquiry_cancel());
        }
        self.le_pump(now);
    }

    fn ctl_start_inquiry(&mut self) {
        if self.ctl.inquiry != Inquiry::Idle || self.ctl.name_inflight.is_some() {
            return;
        }
        if !self.ctl.inquiry_mode_set {
            self.ctl.inquiry_mode_set = true;
            self.cmd(inquiry::write_inquiry_mode(Mode::Extended));
        }
        self.ctl.inquiry = Inquiry::Running;
        self.cmd(inquiry::inquiry(INQUIRY_UNITS, 0));
    }

    /// The radio is busy with discovery work a page cannot interleave with.
    pub(crate) fn ctl_radio_busy(&self) -> bool {
        self.ctl.inquiry != Inquiry::Idle || self.ctl.name_inflight.is_some()
    }

    /// Whether an LE scan is wanted for discovery.
    pub(crate) fn ctl_wants_le_scan(&self) -> bool {
        self.ctl.discovery_until.is_some()
    }

    // ---- BR/EDR events -------------------------------------------------------

    /// Inquiry events arrive as unknown events to the main codec.
    pub(crate) fn ctl_on_event(&mut self, now: u64, event: &Event) {
        match inquiry::parse(event) {
            Some(Ok(InquiryEvent::Results(entries))) => {
                for entry in &entries {
                    self.ctl_on_result(entry);
                }
            }
            Some(Ok(InquiryEvent::Complete { .. })) => {
                if self.ctl.inquiry == Inquiry::Running {
                    self.ctl.inquiry = Inquiry::Idle;
                    self.queue_names();
                    self.pump_names(now);
                }
            }
            Some(Ok(InquiryEvent::Name { status, addr, name })) => {
                self.ctl_on_name(now, status, addr, &name);
            }
            Some(Err(_)) => self.stats.malformed += 1,
            None => {}
        }
    }

    /// A command the controller answered: the cancel of an inquiry ends it.
    pub(crate) fn ctl_on_command(&mut self, now: u64, event: &Event) {
        match event {
            Event::CommandComplete { opcode, .. } if *opcode == inquiry::INQUIRY_CANCEL => {
                self.ctl.inquiry = Inquiry::Idle;
                self.pump_names(now);
            }
            _ => {}
        }
    }

    /// A refused inquiry or name request is over.
    pub(crate) fn ctl_command_refused(&mut self, now: u64, op: u16) {
        match op {
            inquiry::INQUIRY => {
                self.ctl.inquiry = Inquiry::Idle;
                self.ctl.discovery_until = None;
            }
            inquiry::REMOTE_NAME_REQUEST => {
                self.ctl.name_inflight = None;
                self.pump_names(now);
            }
            _ => {}
        }
    }

    fn ctl_on_result(&mut self, entry: &Entry) {
        if self.ctl.inquiry == Inquiry::Idle || !inquiry::is_peripheral(entry.class_of_device) {
            return;
        }
        let name =
            inquiry::ad_local_name(&entry.eir).map_or_else(Default::default, |n| clean_name(&n));
        self.record(Found {
            addr: entry.addr,
            name_tried: !name.is_empty(),
            name,
            transport: Transport::BrEdr,
            input: class_input(entry.class_of_device),
            rssi: entry.rssi,
        });
    }

    /// Ask for the names the results did not carry, one at a time.
    fn queue_names(&mut self) {
        let wanted: Vec<BdAddr> = self
            .ctl
            .found
            .iter()
            .filter(|f| f.transport == Transport::BrEdr && !f.name_tried)
            .map(|f| f.addr)
            .collect();
        self.ctl.names.extend(wanted);
    }

    /// Start the next name request, or carry on scanning once they are done.
    pub(crate) fn pump_names(&mut self, now: u64) {
        if self.ctl.inquiry != Inquiry::Idle || self.ctl.name_inflight.is_some() {
            return;
        }
        // A pairing page goes first: names wait for it.
        if self.ctl.pairing.phase.active() {
            self.ctl.names.clear();
            return;
        }
        while let Some(addr) = self.ctl.names.pop_front() {
            let Some(found) = self.ctl.found.iter_mut().find(|f| f.addr == addr) else {
                continue;
            };
            found.name_tried = true;
            self.ctl.name_inflight = Some((addr, now.saturating_add(NAME_TIMEOUT_MS)));
            self.cmd(inquiry::remote_name_request(addr));
            return;
        }
        // Names done: another round if time is left.
        if self.ctl.discovery_until.is_some_and(|until| now < until) {
            self.ctl_start_inquiry();
        }
    }

    fn ctl_on_name(&mut self, now: u64, code: u8, addr: BdAddr, raw: &[u8]) {
        if self.ctl.name_inflight.map(|n| n.0) == Some(addr) {
            self.ctl.name_inflight = None;
        }
        let name = clean_name(raw);
        if code == status::SUCCESS && !name.is_empty() {
            if let Some(found) = self.ctl.found.iter_mut().find(|f| f.addr == addr) {
                found.name = name;
                self.ctl_changed(addr);
            }
        }
        self.pump_names(now);
    }

    // ---- LE advertisements -------------------------------------------------------

    /// An advertisement while scanning: list HID advertisers that are not
    /// bonded devices coming back.
    pub(crate) fn ctl_on_adv(&mut self, report: &AdvReport) {
        if (self.ctl.discovery_until.is_none() && self.ctl.target.is_none())
            || report.address_type > 1
            || !is_hid_adv(&report.data)
            || self.le_bond_for(report.address_type, report.addr).is_some()
        {
            return;
        }
        let name =
            inquiry::ad_local_name(&report.data).map_or_else(Default::default, |n| clean_name(&n));
        self.record(Found {
            addr: report.addr,
            name,
            transport: Transport::Le,
            input: adv_input(&report.data),
            rssi: Some(report.rssi),
            name_tried: true,
        });
    }

    // ---- the list ------------------------------------------------------------------

    /// Add or update a result, and tell subscribers what is new or changed.
    fn record(&mut self, new: Found) {
        let addr = new.addr;
        let Some(old) = self.ctl.found.iter_mut().find(|f| f.addr == addr) else {
            if self.ctl.found.len() >= MAX_FOUND {
                self.stats.overflow += 1;
                return;
            }
            self.ctl.found.push(new);
            if let Some(info) = self.device_info(addr) {
                self.effect(Effect::Device(DeviceChange::Found, info));
            }
            return;
        };
        let mut changed = false;
        if !new.name.is_empty() && old.name != new.name {
            old.name = new.name;
            changed = true;
        }
        if new.input != InputKind::Unknown && old.input != new.input {
            old.input = new.input;
            changed = true;
        }
        let moved = match (old.rssi, new.rssi) {
            (Some(a), Some(b)) => (i16::from(a) - i16::from(b)).abs() >= RSSI_STEP,
            (None, Some(_)) => true,
            _ => false,
        };
        if moved {
            old.rssi = new.rssi;
            changed = true;
        }
        old.name_tried |= new.name_tried;
        if changed {
            self.ctl_changed(addr);
        }
    }
}
