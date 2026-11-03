//! L2CAP channels of a link: SDP (to learn what a freshly paired device is),
//! the HID control and interrupt channels, and the moment a link becomes a
//! typing device.
//!
//! The gate (docs/bluetooth-plan.md decision 6) is here and in `btl2cap`: the
//! PSMs `0x11` and `0x13` are admitted only after `set_allowed`, which
//! `conn.rs` calls for a bonded device on an authenticated, encrypted link;
//! and a source is registered only when both channels are open on such a
//! link, with a class that follows the bond's record, and a keyboard half only
//! for an authenticated key.

use alloc::format;
use alloc::vec::Vec;

use btbond::{AddrKind, Bond, InputClass};
use bthci::opcode::status;
use bthid::decode::{Decoder, Event as HidEvent, Plan};
use bthid::hidp::{self, Control, FromControl, Handshake};
use bthid::{PSM_CONTROL, PSM_INTERRUPT};
use btl2cap::{Action, CloseReason, Event as L2Event};
use btsdp::{Client, Progress, PSM_SDP};

use crate::effect::{Effect, SourceKind};
use crate::host::{red, Host};
use crate::link::Hid;
use crate::pair::key_authenticated;

/// `class_of_device` the host stores for a HID device: the Peripheral major
/// class with the SDP subclass as the minor class.
const COD_PERIPHERAL: u32 = 0x0500;

impl Host {
    /// Carry out L2CAP actions: queue the PDUs, handle the events.
    pub(crate) fn apply(&mut self, now: u64, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send { handle, pdu } => self.queue_pdu(handle, &pdu),
                Action::Event(event) => self.on_l2_event(now, event),
            }
        }
    }

    fn connect_channel(&mut self, now: u64, handle: u16, psm: u16) {
        match self.l2.connect(now, handle, psm) {
            Ok((_, actions)) => self.apply(now, actions),
            Err(_) => self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST),
        }
    }

    /// The link is encrypted and bonded: learn what the device is (SDP) if
    /// the bond does not say, and otherwise go straight to its HID channels.
    pub(crate) fn after_encrypted(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        let known = self
            .store
            .find(addr, AddrKind::BrEdr)
            .is_some_and(|b| !b.report_descriptor.is_empty());
        if !known {
            self.links[idx].sdp = Some(Client::new());
            self.connect_channel(now, handle, PSM_SDP);
        } else if self.links[idx].initiated {
            self.connect_channel(now, handle, PSM_CONTROL);
        } else {
            // The device opens its channels itself; a silent one is helped.
            self.links[idx].open_at = Some(now.saturating_add(crate::link::OPEN_GRACE_MS));
        }
    }

    /// The deadline for a device to open its own channels passed.
    pub(crate) fn open_control_if_idle(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &self.links[idx];
        if link.encrypted && link.control.is_none() && link.sdp_cid.is_none() {
            self.connect_channel(now, handle, PSM_CONTROL);
        }
    }

    fn on_l2_event(&mut self, now: u64, event: L2Event) {
        match event {
            L2Event::Opened {
                handle,
                cid,
                psm,
                incoming,
                ..
            } => self.on_opened(now, handle, cid, psm, incoming),
            L2Event::Data { handle, cid, data } => self.on_data(now, handle, cid, &data),
            L2Event::Closed {
                handle,
                cid,
                reason,
            } => self.on_closed(handle, cid, reason),
            L2Event::ConnectFailed { handle, psm, .. } => {
                if let Some(idx) = self.link_idx(handle) {
                    let addr = self.links[idx].addr;
                    self.note(format!("L2CAP:FAIL addr={} psm={psm:#06x}", red(addr)));
                }
                self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
            }
            L2Event::Refused { handle, psm } => {
                // A HID connection before the link earned it (decision 6).
                if let Some(idx) = self.link_idx(handle) {
                    let addr = self.links[idx].addr;
                    self.stats.refused += 1;
                    self.note(format!(
                        "REFUSE reason=psm psm={psm:#06x} addr={}",
                        red(addr)
                    ));
                }
            }
            L2Event::Misbehaved { handle } => {
                self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST)
            }
        }
    }

    fn on_opened(&mut self, now: u64, handle: u16, cid: u16, psm: u16, incoming: bool) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        // Belt and braces: `btl2cap` already refuses HID before `set_allowed`.
        if psm != PSM_SDP && !self.links[idx].encrypted {
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
            return;
        }
        match psm {
            PSM_SDP => {
                self.links[idx].sdp_cid = Some(cid);
                let request = match &mut self.links[idx].sdp {
                    Some(client) => client.start(),
                    None => return,
                };
                self.send_on(handle, cid, &request);
            }
            PSM_CONTROL => {
                let duplicate = self.links[idx].control.replace(cid).is_some();
                if duplicate {
                    return self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
                }
                self.links[idx].open_at = None;
                // We opened it (our link, or the grace expired): the
                // interrupt channel follows.
                if !incoming && self.links[idx].interrupt.is_none() {
                    self.connect_channel(now, handle, PSM_INTERRUPT);
                }
            }
            PSM_INTERRUPT => {
                let duplicate = self.links[idx].interrupt.replace(cid).is_some();
                if duplicate {
                    return self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
                }
            }
            _ => {}
        }
        self.try_ready(handle);
    }

    fn on_closed(&mut self, handle: u16, cid: u16, _reason: CloseReason) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &mut self.links[idx];
        let was_sdp = link.sdp_cid == Some(cid);
        let hid_channel = link.control == Some(cid) || link.interrupt == Some(cid);
        for slot in [&mut link.sdp_cid, &mut link.control, &mut link.interrupt] {
            if *slot == Some(cid) {
                *slot = None;
            }
        }
        // A HID channel closing ends the device: what it held is released.
        if hid_channel {
            self.release_hid(handle);
            self.disconnect(handle, status::REMOTE_USER_TERMINATED);
        } else if was_sdp && self.links[idx].sdp.is_some() && !self.links[idx].fresh_bond {
            self.links[idx].sdp = None;
        }
    }

    fn on_data(&mut self, now: u64, handle: u16, cid: u16, data: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if self.links[idx].le.is_some() {
            return self.on_le_data(now, handle, cid, data);
        }
        let link = &self.links[idx];
        if Some(cid) == link.sdp_cid {
            self.on_sdp_data(now, handle, data);
        } else if Some(cid) == link.control {
            self.on_control_data(handle, data);
        } else if Some(cid) == link.interrupt {
            self.on_interrupt_data(now, handle, data);
        }
    }

    pub(crate) fn send_on(&mut self, handle: u16, cid: u16, data: &[u8]) {
        match self.l2.send(handle, cid, data) {
            Ok(actions) => {
                for action in actions {
                    if let Action::Send { handle, pdu } = action {
                        self.queue_pdu(handle, &pdu);
                    }
                }
            }
            Err(_) => self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST),
        }
    }

    // ---- SDP ----------------------------------------------------------------

    fn on_sdp_data(&mut self, now: u64, handle: u16, data: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        let cid = self.links[idx].sdp_cid;
        let progress = match &mut self.links[idx].sdp {
            Some(client) => client.on_pdu(data),
            None => return,
        };
        match (progress, cid) {
            (Ok(Progress::Request(pdu)), Some(cid)) => self.send_on(handle, cid, &pdu),
            (Ok(Progress::Done(record)), _) => self.sdp_done(now, handle, record),
            (error, _) => {
                self.note(format!(
                    "PAIR:FAIL addr={} reason=sdp-{:?}",
                    red(addr),
                    error.err()
                ));
                self.abandon_bond(handle, false);
            }
        }
    }

    /// The device told us what it is. Store it, applying decision 6: the
    /// class follows the record, and a keyboard needs an authenticated key.
    fn sdp_done(&mut self, now: u64, handle: u16, record: btsdp::HidRecord) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        let Some(mut bond) = self.store.find(addr, AddrKind::BrEdr).cloned() else {
            return self.abandon_bond(handle, false);
        };
        let Some(plan) = Plan::new(&record.descriptor, record.subclass) else {
            self.refuse("no-hid-descriptor", addr);
            return self.abandon_bond(handle, true);
        };
        let Some(plan) = self.admit_plan(addr, plan, bond.authenticated) else {
            return self.abandon_bond(handle, true);
        };
        bond.input = match (plan.keyboard, plan.pointer) {
            (true, true) => InputClass::Combo,
            (true, false) => InputClass::Keyboard,
            _ => InputClass::Pointer,
        };
        bond.report_descriptor = record.descriptor;
        bond.hid_flags = u8::from(record.reconnect_initiate)
            | u8::from(record.normally_connectable) << 1
            | u8::from(record.boot_device) << 2;
        bond.name = record.name;
        bond.class_of_device = COD_PERIPHERAL | u32::from(record.subclass);
        if self.store.upsert(bond.clone()).is_err() {
            return self.abandon_bond(handle, false);
        }
        self.save_bonds();
        let link = &mut self.links[idx];
        let fresh = core::mem::take(&mut link.fresh_bond);
        let sdp_cid = link.sdp_cid;
        link.sdp = None;
        if fresh || link.pairing {
            self.note(format!(
                "PAIR:PASS addr={} auth={} class={}",
                red(addr),
                u8::from(bond.authenticated),
                class_word(bond.input)
            ));
            self.ctl_paired(now, addr);
        }
        if let Some(cid) = sdp_cid {
            if let Ok(actions) = self.l2.disconnect(now, handle, cid) {
                self.apply(now, actions);
            }
        }
        // A device that opened its channels while SDP ran has control open
        // already; it is registered now that the record is known.
        if self.links[idx].control.is_some() {
            self.try_ready(handle);
        } else {
            self.connect_channel(now, handle, PSM_CONTROL);
        }
    }

    /// Decision 6 at the door: a keyboard half only for an authenticated key.
    /// A combo device keeps its pointer; a plain keyboard is refused.
    fn admit_plan(&mut self, addr: bthci::BdAddr, plan: Plan, authenticated: bool) -> Option<Plan> {
        if !plan.keyboard || authenticated {
            return Some(plan);
        }
        self.refuse("keyboard-unauthenticated", addr);
        plan.without_keyboard()
    }

    /// Give up on a link that was meant to be paired: a bond made on it and
    /// not completed goes (a half-made bond would be retried at every boot).
    /// `forget`: the device itself is unacceptable (it is no HID device, or a
    /// keyboard without an authenticated key), so its bond goes even if it
    /// was made on an earlier connection.
    fn abandon_bond(&mut self, handle: u16, forget: bool) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let (addr, fresh) = (self.links[idx].addr, self.links[idx].fresh_bond);
        if (fresh || forget) && self.store.remove(addr, AddrKind::BrEdr) {
            self.save_bonds();
            self.note(format!("PAIR:FAIL addr={} reason=bond-dropped", red(addr)));
        }
        self.pager_forget(addr);
        self.ctl_failed(addr, crate::ctl_types::FailReason::Unsupported);
        self.disconnect(handle, status::AUTHENTICATION_FAILURE);
    }

    // ---- HID --------------------------------------------------------------------

    /// Both channels are open: register the device if the bond allows it.
    fn try_ready(&mut self, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &self.links[idx];
        if link.hid.is_some() || link.control.is_none() || link.interrupt.is_none() {
            return;
        }
        // A device that opens its channels while SDP still runs is not yet
        // known (empty descriptor, class 0): `sdp_done` calls back.
        if link.sdp.is_some() {
            return;
        }
        let (addr, control, ok) = (
            link.addr,
            link.control,
            link.encrypted && link.authenticated,
        );
        let bond = self.store.find(addr, AddrKind::BrEdr).cloned();
        let (Some(bond), true) = (bond, ok) else {
            self.refuse("not-bonded", addr);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        };
        let Some(plan) = self.plan_for(&bond) else {
            self.refuse("no-input-class", addr);
            // A bond that never learned its descriptor is of no use: drop it
            // rather than refuse the device again at every boot.
            if bond.report_descriptor.is_empty() {
                return self.abandon_bond(handle, true);
            }
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        };
        if plan.needs_boot_protocol() {
            if let Some(cid) = control {
                self.send_on(handle, cid, &hidp::set_protocol(true));
            }
        }
        let decoder = Decoder::new(plan, &bond.report_descriptor);
        self.links[idx].hid = Some(Hid { decoder });
        if plan.keyboard {
            self.effect(Effect::OpenSource {
                handle,
                kind: SourceKind::Keyboard,
            });
            self.note(format!(
                "HID:KBD addr={} auth={}",
                red(addr),
                u8::from(bond.authenticated)
            ));
        }
        if plan.pointer {
            let kind = if plan.absolute {
                SourceKind::Tablet
            } else {
                SourceKind::Pointer
            };
            self.effect(Effect::OpenSource { handle, kind });
            self.note(format!("HID:MOUSE addr={}", red(addr)));
        }
    }

    /// What a bonded device may register as: the plan its stored descriptor
    /// gives, cut down to the class of the bond's record, and to a pointer
    /// when the key is not authenticated.
    pub(crate) fn plan_for(&mut self, bond: &Bond) -> Option<Plan> {
        let subclass = (bond.class_of_device & 0xFF) as u8;
        let mut plan = Plan::new(&bond.report_descriptor, subclass)?;
        plan.keyboard &= matches!(bond.input, InputClass::Keyboard | InputClass::Combo);
        plan.pointer &= matches!(bond.input, InputClass::Pointer | InputClass::Combo);
        let authenticated = bond
            .link_key
            .as_ref()
            .is_some_and(|k| key_authenticated(k.key_type));
        if plan.keyboard && !(bond.authenticated && authenticated) {
            self.refuse("keyboard-unauthenticated", bond.addr);
            plan.keyboard = false;
        }
        (plan.keyboard || plan.pointer).then_some(plan)
    }

    fn on_control_data(&mut self, handle: u16, data: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        match hidp::parse_control(data) {
            Ok(FromControl::Handshake(Handshake::Success)) => {}
            Ok(FromControl::Handshake(other)) => {
                self.note(format!("HID:HANDSHAKE addr={} result={other:?}", red(addr)));
                self.release_hid(handle);
                self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
            }
            Ok(FromControl::Control(Control::VirtualCableUnplug)) => {
                // The device is unpairing itself; it is only heard on a link
                // that earned the control channel.
                self.forget(0, addr);
            }
            Ok(_) | Err(_) => {}
        }
    }

    fn on_interrupt_data(&mut self, now: u64, handle: u16, data: &[u8]) {
        match hidp::parse_interrupt(data) {
            Ok(report) => self.feed_report(now, handle, report),
            Err(_) => self.stats.malformed += 1,
        }
    }

    /// One input report of a registered device (BR/EDR: the interrupt
    /// channel; LE: a notification with the report id put back).
    pub(crate) fn feed_report(&mut self, now: u64, handle: u16, report: &[u8]) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &mut self.links[idx];
        let Some(hid) = &mut link.hid else {
            self.stats.dropped_reports += 1;
            return;
        };
        let mut events: Vec<HidEvent> = Vec::new();
        hid.decoder.feed(report, |event| events.push(event));
        let (addr, first) = (link.addr, !link.reported && !events.is_empty());
        let (reconnect, up_at) = (link.reconnect, link.up_at);
        if first {
            link.reported = true;
        }
        if self.cfg.trace_reports {
            let hex: alloc::string::String = report.iter().map(|b| format!("{b:02x}")).collect();
            self.note(format!("REPORT {hex}"));
        }
        for event in events {
            self.effect(Effect::Input { handle, event });
        }
        if first && reconnect {
            self.note(format!(
                "RECONNECT ms={} addr={} link_at={up_at} report_at={now}",
                now.saturating_sub(up_at),
                red(addr)
            ));
        }
    }

    /// Let go of everything the link's device holds and close its sources.
    pub(crate) fn release_hid(&mut self, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let Some(mut hid) = self.links[idx].hid.take() else {
            return;
        };
        let mut events: Vec<HidEvent> = Vec::new();
        hid.decoder.release_all(|event| events.push(event));
        for event in events {
            self.effect(Effect::Input { handle, event });
        }
        self.effect(Effect::CloseSources { handle });
    }
}

fn class_word(class: InputClass) -> &'static str {
    match class {
        InputClass::Keyboard => "keyboard",
        InputClass::Pointer => "pointer",
        InputClass::Combo => "combo",
    }
}
