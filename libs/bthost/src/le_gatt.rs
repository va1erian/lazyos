//! ATT and GATT on an encrypted LE link: the first-pairing discovery, the
//! admission decision, and the subscription that makes a bonded device type.
//!
//! The gates (docs/bluetooth-plan.md decision 6): ATT data is read only on a
//! link encrypted with the bond's key; a source is registered only for a
//! device whose bond says what it is; and a keyboard half only for an
//! authenticated key. SMP cannot know the class of the device before GATT
//! tells it, so a Just Works pairing is admitted as far as discovery, and a
//! device with a keyboard report is then dropped, its bond forgotten, with
//! `BT:REFUSE reason=keyboard-unauthenticated`.

use alloc::format;
use alloc::vec::Vec;

use btbond::bond::hid_flag;
use btbond::bond::MAX_REPORT_HANDLES;
use btbond::{Bond, Gatt, InputClass, Ltk, ReportHandle};
use bthci::opcode::status;
use bthid::decode::{Decoder, Plan};
use btgatt::hogp::{kind, HidService};
use btgatt::{Error as GattError, Hogp, HogpEvent, Step, CID_ATT};

use crate::effect::{Effect, SourceKind};
use crate::host::{red, Host};
use crate::le::Fresh;
use crate::link::Hid;

/// The ATT MTU the host offers (a Report Map then takes few reads).
const RX_MTU: u16 = 247;
/// `HIDDeviceSubclass` bits for the boot keyboard and pointing devices.
const SUBCLASS_KEYBOARD: u8 = 0x40;
const SUBCLASS_POINTING: u8 = 0x80;
/// HID Information flag: the device is normally connectable.
const INFO_NORMALLY_CONNECTABLE: u8 = 0x02;

impl Host {
    /// Start first-pairing discovery on an encrypted link.
    pub(crate) fn le_discover(&mut self, now: u64, handle: u16) {
        let mut hogp = Hogp::new(RX_MTU);
        let step = hogp.discover(now);
        self.le_set_gatt(handle, hogp);
        self.gatt_apply(now, handle, step);
    }

    /// A bonded link is encrypted: register the device from the bond and
    /// subscribe, with no discovery (plan decision 5).
    pub(crate) fn le_resume(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        let bond = self
            .store
            .iter()
            .find(|b| b.addr == addr && b.kind.is_le())
            .cloned();
        let Some(bond) = bond else {
            self.refuse("not-bonded", addr);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        };
        // Defence in depth: a keyboard bond that is not authenticated (a
        // file edited by hand, an older stack) does not type.
        if bond.input != InputClass::Pointer && !bond.authenticated {
            self.refuse("keyboard-unauthenticated", addr);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
        let Some(plan) = le_plan(&bond) else {
            self.refuse("no-input-class", addr);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        };
        self.links[idx].authenticated = true;
        self.le_set_gatt(handle, Hogp::new(RX_MTU));
        self.le_register(now, handle, plan, &bond);
    }

    fn le_set_gatt(&mut self, handle: u16, hogp: Hogp) {
        if let Some(le) = self.link_idx(handle).and_then(|i| self.links[i].le.as_mut()) {
            le.gatt = Some(hogp);
        }
    }

    /// ATT and SMP data of an LE link (`chan.rs` hands it over).
    pub(crate) fn on_le_data(&mut self, now: u64, handle: u16, cid: u16, data: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        match cid {
            CID_ATT if !self.links[idx].encrypted || self.replaying(idx) => {
                // Decision 6: a notification, or anything, sent before the
                // link was encrypted with the bond's key is not heard, even
                // if it reaches us after the Encryption Change (a plaintext
                // PDU can slip in while the procedure starts).
                self.stats.dropped_reports += 1;
                self.refuse("att-unencrypted", addr);
            }
            CID_ATT => {
                let step = match self.links[idx].le.as_mut().and_then(|le| le.gatt.as_mut()) {
                    Some(gatt) => gatt.on_pdu(now, data),
                    None => return,
                };
                self.gatt_apply(now, handle, step);
            }
            6 => self.smp_input(now, handle, btsmp::Input::Pdu(data)),
            _ => {}
        }
    }

    fn replaying(&self, idx: usize) -> bool {
        self.links[idx].le.as_ref().is_some_and(|le| le.replaying)
    }

    pub(crate) fn le_att_tick(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let Some(gatt) = self.links[idx].le.as_mut().and_then(|le| le.gatt.as_mut()) else {
            return;
        };
        let step = gatt.tick(now);
        self.gatt_apply(now, handle, step);
    }

    fn gatt_apply(&mut self, now: u64, handle: u16, step: Step) {
        for pdu in &step.send {
            self.send_on(handle, CID_ATT, pdu);
        }
        for event in step.events {
            match event {
                HogpEvent::Discovered(service) => self.le_admit(now, handle, service),
                HogpEvent::Ready => {}
                HogpEvent::Unsolicited { .. } => self.unsolicited(handle),
                HogpEvent::Notification { handle: value, value: bytes } => {
                    self.on_notification(now, handle, value, &bytes)
                }
                HogpEvent::Failed(error) => self.gatt_failed(handle, error),
            }
        }
    }

    /// A notification nobody subscribed to (decision 6): counted, not heard.
    fn unsolicited(&mut self, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        self.stats.dropped_reports += 1;
        self.refuse("notification-unsolicited", addr);
    }

    fn gatt_failed(&mut self, handle: u16, error: GattError) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        self.note(format!(
            "GATT:FAIL addr={} reason={}",
            red(addr),
            gatt_reason(error)
        ));
        if self.links[idx].le.as_ref().is_some_and(|le| le.fresh.is_some()) {
            self.le_deny(addr);
        }
        self.release_hid(handle);
        self.disconnect(handle, status::AUTHENTICATION_FAILURE);
    }

    /// A notification: the report id comes from the characteristic, not the
    /// payload (HOGP), and only a registered device's input reports count.
    fn on_notification(&mut self, now: u64, handle: u16, value: u16, bytes: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &self.links[idx];
        if link.hid.is_none() {
            self.stats.dropped_reports += 1;
            return;
        }
        let report = link
            .le
            .as_ref()
            .and_then(|le| le.reports.iter().find(|r| r.value == value));
        let Some(report) = report else { return };
        let mut buf = Vec::with_capacity(bytes.len() + 1);
        if report.report_id != 0 {
            buf.push(report.report_id);
        }
        buf.extend_from_slice(bytes);
        self.feed_report(now, handle, &buf);
    }

    /// Discovery found the HID service. Decide what the device is; the bond
    /// is stored only if it is acceptable.
    fn le_admit(&mut self, now: u64, handle: u16, service: HidService) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        let Some(fresh) = self.links[idx].le.as_mut().and_then(|le| le.fresh.take()) else {
            return;
        };
        let plan = Plan::new(&service.report_map, boot_subclass(&service));
        let Some(plan) = plan else {
            self.refuse("no-hid-descriptor", addr);
            return self.le_reject(handle, addr);
        };
        if plan.keyboard && !fresh.key.authenticated {
            // Plan Security: a Just Works key may not type. The bond that
            // would have been made is never made, and an older one goes.
            self.refuse("keyboard-unauthenticated", addr);
            if self.store.remove(addr, fresh.kind) {
                self.save_bonds();
            }
            return self.le_reject(handle, addr);
        }
        let bond = bond_of(addr, &fresh, plan, service);
        if self.store.upsert(bond.clone()).is_err() {
            self.note(format!("PAIR:FAIL addr={} reason=store", red(addr)));
            return self.le_reject(handle, addr);
        }
        self.save_bonds();
        self.note(format!(
            "PAIR:PASS addr={} auth={} class={}",
            red(addr),
            u8::from(bond.authenticated),
            class_word(bond.input)
        ));
        self.le_register(now, handle, plan, &bond);
    }

    fn le_reject(&mut self, handle: u16, addr: bthci::BdAddr) {
        self.le_deny(addr);
        self.disconnect(handle, status::AUTHENTICATION_FAILURE);
    }

    /// Open the device's sources and subscribe to the reports its plan reads.
    fn le_register(&mut self, now: u64, handle: u16, plan: Plan, bond: &Bond) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let (cccds, listened, boot) = subscriptions(plan, &bond.gatt);
        if cccds.is_empty() {
            self.refuse("no-input-report", bond.addr);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
        let link = &mut self.links[idx];
        link.hid = Some(Hid {
            decoder: Decoder::new(plan, &bond.report_descriptor),
        });
        let step = match link.le.as_mut() {
            Some(le) => {
                le.reports = listened;
                le.gatt.as_mut().map(|g| g.enable(now, &cccds, boot))
            }
            None => None,
        };
        self.open_sources(handle, bond.addr, plan, bond.authenticated);
        if let Some(step) = step {
            self.gatt_apply(now, handle, step);
        }
    }

    /// Register the sources a plan calls for (the same lines as BR/EDR).
    pub(crate) fn open_sources(&mut self, handle: u16, addr: bthci::BdAddr, plan: Plan, auth: bool) {
        if plan.keyboard {
            self.effect(Effect::OpenSource {
                handle,
                kind: SourceKind::Keyboard,
            });
            self.note(format!("HID:KBD addr={} auth={}", red(addr), u8::from(auth)));
        }
        if plan.pointer {
            let source = if plan.absolute {
                SourceKind::Tablet
            } else {
                SourceKind::Pointer
            };
            self.effect(Effect::OpenSource {
                handle,
                kind: source,
            });
            self.note(format!("HID:MOUSE addr={}", red(addr)));
        }
    }
}

/// The boot-device bits of the subclass a HID Information-less device gets.
fn boot_subclass(service: &HidService) -> u8 {
    if service.boot_keyboard() {
        SUBCLASS_KEYBOARD
    } else if service.boot_mouse() {
        SUBCLASS_POINTING
    } else {
        0
    }
}

/// What a stored bond may register as, cut to the class of the bond.
fn le_plan(bond: &Bond) -> Option<Plan> {
    let mut subclass = 0;
    for r in &bond.gatt.reports {
        match r.report_type {
            kind::BOOT_KEYBOARD_INPUT => subclass |= SUBCLASS_KEYBOARD,
            kind::BOOT_MOUSE_INPUT => subclass |= SUBCLASS_POINTING,
            _ => {}
        }
    }
    let mut plan = Plan::new(&bond.report_descriptor, subclass)?;
    plan.keyboard &= matches!(bond.input, InputClass::Keyboard | InputClass::Combo);
    plan.pointer &= matches!(bond.input, InputClass::Pointer | InputClass::Combo);
    (plan.keyboard || plan.pointer).then_some(plan)
}

/// The CCCD handles to enable, the reports to listen to, and the Protocol
/// Mode handle when the boot protocol is to be selected.
fn subscriptions(plan: Plan, gatt: &Gatt) -> (Vec<u16>, Vec<ReportHandle>, Option<u16>) {
    let boot = plan.needs_boot_protocol();
    let wanted = |r: &ReportHandle| match r.report_type {
        kind::INPUT => !boot,
        kind::BOOT_KEYBOARD_INPUT => boot && plan.keyboard,
        kind::BOOT_MOUSE_INPUT => boot && plan.pointer,
        _ => false,
    };
    let listened: Vec<ReportHandle> = gatt.reports.iter().filter(|r| wanted(r)).copied().collect();
    let cccds = listened.iter().map(|r| r.cccd).filter(|&c| c != 0).collect();
    let mode = (boot && gatt.protocol_mode != 0).then_some(gatt.protocol_mode);
    (cccds, listened, mode)
}

/// The bond for a device that passed admission.
fn bond_of(addr: bthci::BdAddr, fresh: &Fresh, plan: Plan, service: HidService) -> Bond {
    let mut reports: Vec<ReportHandle> = service
        .reports
        .iter()
        .map(|r| ReportHandle {
            value: r.value,
            cccd: r.cccd,
            report_id: r.report_id,
            report_type: r.kind,
        })
        .collect();
    // The inputs matter most: if a device has more reports than a bond keeps,
    // the others (outputs, features) are the ones left out.
    reports.sort_by_key(|r| u8::from(r.report_type != kind::INPUT));
    reports.truncate(MAX_REPORT_HANDLES);
    let mut hid_flags = 0;
    if service.info_flags & INFO_NORMALLY_CONNECTABLE != 0 {
        hid_flags |= hid_flag::NORMALLY_CONNECTABLE;
    }
    if service.boot_keyboard() || service.boot_mouse() {
        hid_flags |= hid_flag::BOOT_DEVICE;
    }
    Bond {
        addr,
        kind: fresh.kind,
        authenticated: fresh.key.authenticated,
        secure_connections: true,
        key_size: fresh.key.key_size,
        link_key: None,
        ltk: Some(Ltk {
            key: fresh.key.ltk,
            ediv: 0,
            rand: 0,
        }),
        irk: fresh.irk,
        class_of_device: 0,
        input: match (plan.keyboard, plan.pointer) {
            (true, true) => InputClass::Combo,
            (true, false) => InputClass::Keyboard,
            _ => InputClass::Pointer,
        },
        hid_flags,
        name: alloc::string::String::new(),
        report_descriptor: service.report_map,
        gatt: Gatt {
            report_map: service.report_map_handle,
            protocol_mode: service.protocol_mode_handle,
            reports,
        },
    }
}

fn class_word(class: InputClass) -> &'static str {
    match class {
        InputClass::Keyboard => "keyboard",
        InputClass::Pointer => "pointer",
        InputClass::Combo => "combo",
    }
}

fn gatt_reason(error: GattError) -> &'static str {
    match error {
        GattError::Malformed => "malformed",
        GattError::Unexpected => "unexpected",
        GattError::Busy => "busy",
        GattError::Dead => "dead",
        GattError::TooLong => "too-long",
        GattError::Timeout => "timeout",
        GattError::Att(_) => "att-error",
        GattError::NoHidService => "no-hid-service",
        GattError::NoReportMap => "no-report-map",
        GattError::TooLarge => "too-large",
        GattError::TooManyRounds => "too-many-rounds",
    }
}
