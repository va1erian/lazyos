//! HID over GATT discovery and subscription, driven over a [`Client`].
//!
//! Discovery (first pairing only, plan decision 5) walks: Exchange MTU, the
//! primary services (Read By Group Type) to find `0x1812`, its
//! characteristics (Read By Type `0x2803`), its descriptors (Find
//! Information) to find each characteristic's CCCD and Report Reference, the
//! Report References (report id and type), HID Information, and the Report
//! Map with Read Blob for the part that does not fit one response. The result
//! is a [`HidService`] the host stores in the bond. A reconnect skips all of
//! it and calls [`Hogp::enable`] with the stored CCCD handles.

use alloc::vec::Vec;

use crate::att::{err, Request, Response};
use crate::client::{Client, Event};
use crate::uuid::{known, Uuid};
use crate::Error;

/// Requests one driver may send in its life: a hostile server that answers
/// one entry at a time cannot keep a discovery loop going past this.
pub const MAX_ROUNDS: u32 = 400;
/// Characteristics kept from the HID service.
pub const MAX_CHARS: usize = 48;
/// The largest Report Map read (the bond store's bound).
pub const MAX_REPORT_MAP: usize = 4096;

/// What a report characteristic is.
pub mod kind {
    pub const INPUT: u8 = 1;
    pub const OUTPUT: u8 = 2;
    pub const FEATURE: u8 = 3;
    /// Boot Keyboard Input Report (no Report Reference; no report id).
    pub const BOOT_KEYBOARD_INPUT: u8 = 0x10;
    pub const BOOT_MOUSE_INPUT: u8 = 0x11;
}

/// One report characteristic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// The value handle (notifications carry it).
    pub value: u16,
    /// The CCCD handle; 0 when there is none.
    pub cccd: u16,
    /// The report id from the Report Reference; 0 for a boot report.
    pub report_id: u8,
    /// A [`kind`] value.
    pub kind: u8,
}

/// What discovery learned about the device's HID service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidService {
    pub report_map: Vec<u8>,
    pub report_map_handle: u16,
    /// The Protocol Mode value handle; 0 when the device has none.
    pub protocol_mode_handle: u16,
    /// HID Information flags (bit 0 remote wake, bit 1 normally connectable).
    pub info_flags: u8,
    pub reports: Vec<Report>,
}

impl HidService {
    pub fn boot_keyboard(&self) -> bool {
        self.reports
            .iter()
            .any(|r| r.kind == kind::BOOT_KEYBOARD_INPUT)
    }

    pub fn boot_mouse(&self) -> bool {
        self.reports.iter().any(|r| r.kind == kind::BOOT_MOUSE_INPUT)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HogpEvent {
    Discovered(HidService),
    /// Every CCCD write was answered: the device will notify.
    Ready,
    Notification { handle: u16, value: Vec<u8> },
    /// A notification that arrived before any CCCD write was answered: the
    /// device cannot have been asked for it, so it is not reported as input.
    Unsolicited { handle: u16 },
    Failed(Error),
}

/// PDUs to send, in order, and what happened.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub send: Vec<Vec<u8>>,
    pub events: Vec<HogpEvent>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Mtu,
    Services(u16),
    Chars(u16),
    Descriptors(u16),
    Refs(usize),
    Info,
    Map,
    Enable(usize),
    Done,
    Failed,
}

#[derive(Clone, Debug)]
struct Char {
    value: u16,
    uuid: Uuid,
    cccd: u16,
    reference: u16,
    /// The Report Reference's `(id, type)` once read.
    id_kind: Option<(u8, u8)>,
}

pub struct Hogp {
    client: Client,
    phase: Phase,
    rounds: u32,
    range: Option<(u16, u16)>,
    chars: Vec<Char>,
    ref_reads: Vec<usize>,
    flags: u8,
    map: Vec<u8>,
    to_enable: Vec<u16>,
    /// A CCCD write was answered: from here the device may notify.
    subscribed: bool,
}

impl Hogp {
    /// A driver whose ATT client offers `rx_mtu`.
    pub fn new(rx_mtu: u16) -> Hogp {
        Hogp {
            client: Client::new(rx_mtu),
            phase: Phase::Idle,
            rounds: 0,
            range: None,
            chars: Vec::new(),
            ref_reads: Vec::new(),
            flags: 0,
            map: Vec::new(),
            to_enable: Vec::new(),
            subscribed: false,
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.client.deadline()
    }

    pub fn is_failed(&self) -> bool {
        self.phase == Phase::Failed
    }

    /// Begin discovery.
    pub fn discover(&mut self, now: u64) -> Step {
        let mut step = Step::default();
        if self.phase == Phase::Idle {
            self.phase = Phase::Mtu;
            let mtu = self.client.rx_mtu();
            self.request(now, Request::ExchangeMtu { mtu }, &mut step);
        }
        step
    }

    /// Subscribe to notifications on the CCCDs `cccds`. With `boot_protocol`
    /// (a Protocol Mode value handle) the device is first switched to the
    /// boot protocol with a Write Command.
    pub fn enable(&mut self, now: u64, cccds: &[u16], boot_protocol: Option<u16>) -> Step {
        let mut step = Step::default();
        if !matches!(self.phase, Phase::Idle | Phase::Done) {
            return step;
        }
        if let Some(handle) = boot_protocol {
            match self.client.write_command(handle, &[0]) {
                Ok(pdu) => step.send.push(pdu),
                Err(e) => return self.fail(e, step),
            }
        }
        self.to_enable = cccds.to_vec();
        self.phase = Phase::Enable(0);
        self.next(now, &mut step);
        step
    }

    pub fn tick(&mut self, now: u64) -> Step {
        let step = Step::default();
        match self.client.tick(now) {
            Some(e) => self.fail(e, step),
            None => step,
        }
    }

    /// One PDU from the server.
    pub fn on_pdu(&mut self, now: u64, pdu: &[u8]) -> Step {
        let mut step = Step::default();
        let received = self.client.on_pdu(pdu);
        step.send.extend(received.reply);
        match received.event {
            Event::Ignored => {}
            Event::Notification { handle, value } if self.subscribed => {
                step.events.push(HogpEvent::Notification { handle, value })
            }
            Event::Notification { handle, .. } => {
                step.events.push(HogpEvent::Unsolicited { handle })
            }
            Event::Failed(e) => return self.fail(e, step),
            Event::Response { request, response } => {
                if let Err(e) = self.on_response(&request, response) {
                    return self.fail(e, step);
                }
                self.next(now, &mut step);
            }
            Event::Error {
                request,
                handle: _,
                code,
            } => {
                if let Err(e) = self.on_error(&request, code) {
                    return self.fail(e, step);
                }
                self.next(now, &mut step);
            }
        }
        step
    }

    fn fail(&mut self, error: Error, mut step: Step) -> Step {
        self.phase = Phase::Failed;
        step.events.push(HogpEvent::Failed(error));
        step
    }

    fn request(&mut self, now: u64, request: Request, step: &mut Step) {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            self.phase = Phase::Failed;
            step.events.push(HogpEvent::Failed(Error::TooManyRounds));
            return;
        }
        match self.client.send(now, request) {
            Ok(pdu) => step.send.push(pdu),
            Err(e) => {
                self.phase = Phase::Failed;
                step.events.push(HogpEvent::Failed(e));
            }
        }
    }

    // ---- responses ---------------------------------------------------------

    fn on_response(&mut self, request: &Request, response: Response) -> Result<(), Error> {
        match (request, response) {
            (Request::ExchangeMtu { .. }, Response::Mtu(_)) if self.phase == Phase::Mtu => {
                self.phase = Phase::Services(1);
            }
            (Request::ReadByGroupType { .. }, Response::ReadByGroupType(groups)) => {
                self.on_groups(groups)?;
            }
            (Request::ReadByType { .. }, Response::ReadByType(entries)) => {
                self.on_chars(entries)?;
            }
            (Request::FindInformation { .. }, Response::FindInformation(entries)) => {
                self.on_descriptors(entries);
            }
            (Request::Read { .. }, Response::Read(value)) => self.on_read(value)?,
            (Request::ReadBlob { .. }, Response::ReadBlob(value)) => self.on_map(value)?,
            (Request::Write { .. }, Response::Write) => {
                if let Phase::Enable(i) = self.phase {
                    self.phase = Phase::Enable(i + 1);
                    self.subscribed = true;
                }
            }
            _ => return Err(Error::Unexpected),
        }
        Ok(())
    }

    fn on_error(&mut self, request: &Request, code: u8) -> Result<(), Error> {
        let not_found = code == err::ATTRIBUTE_NOT_FOUND;
        match (self.phase, request) {
            // A server without Exchange MTU stays at the default.
            (Phase::Mtu, _) => self.phase = Phase::Services(1),
            // The end of a listing.
            (Phase::Services(_), _) if not_found => {
                if self.range.is_none() {
                    return Err(Error::NoHidService);
                }
                self.phase = Phase::Chars(self.range.map_or(1, |r| r.0));
            }
            (Phase::Chars(_), _) if not_found => self.phase = self.descriptors_phase(),
            (Phase::Descriptors(_), _) if not_found => self.finish_descriptors(),
            // A Report Reference or HID Information that cannot be read: the
            // report is left out, the flags stay zero.
            (Phase::Refs(i), _) => self.phase = Phase::Refs(i + 1),
            (Phase::Info, _) => self.phase = Phase::Map,
            // The Report Map ended exactly at a full response.
            (Phase::Map, Request::ReadBlob { .. })
                if matches!(code, err::ATTRIBUTE_NOT_LONG | err::INVALID_OFFSET)
                    && !self.map.is_empty() =>
            {
                self.phase = Phase::Done;
            }
            _ => return Err(Error::Att(code)),
        }
        Ok(())
    }

    fn on_groups(&mut self, groups: Vec<crate::att::Group>) -> Result<(), Error> {
        let Phase::Services(_) = self.phase else {
            return Err(Error::Unexpected);
        };
        let mut last_end = 0;
        for group in &groups {
            last_end = group.end;
            let hid = Uuid::from_wire(&group.value) == Some(Uuid::Short(known::SERVICE_HID));
            if hid && self.range.is_none() {
                self.range = Some((group.start, group.end));
            }
        }
        self.phase = match self.range {
            Some((start, _)) => Phase::Chars(start),
            None if last_end == 0xFFFF => return Err(Error::NoHidService),
            None => Phase::Services(last_end + 1),
        };
        Ok(())
    }

    fn on_chars(&mut self, entries: Vec<(u16, Vec<u8>)>) -> Result<(), Error> {
        let (_, end) = self.range.ok_or(Error::Unexpected)?;
        let mut last = 0;
        for (decl, value) in entries {
            // properties(1) value handle(2) uuid(2 or 16)
            let uuid = value.get(3..).and_then(Uuid::from_wire);
            let (Some(uuid), Some(handle)) = (uuid, value.get(1..3)) else {
                return Err(Error::Malformed);
            };
            let handle = u16::from_le_bytes([handle[0], handle[1]]);
            if handle <= decl || handle > end {
                return Err(Error::Malformed);
            }
            if self.chars.len() >= MAX_CHARS {
                return Err(Error::TooLarge);
            }
            last = decl;
            self.chars.push(Char {
                value: handle,
                uuid,
                cccd: 0,
                reference: 0,
                id_kind: None,
            });
        }
        self.phase = if last >= end {
            self.descriptors_phase()
        } else {
            Phase::Chars(last + 1)
        };
        Ok(())
    }

    fn descriptors_phase(&self) -> Phase {
        Phase::Descriptors(self.range.map_or(1, |r| r.0))
    }

    fn on_descriptors(&mut self, entries: Vec<(u16, Uuid)>) {
        let end = self.range.map_or(0, |r| r.1);
        let last = entries.last().map_or(0, |e| e.0);
        for (handle, uuid) in entries {
            let Some(short) = uuid.short() else { continue };
            // The owner is the last characteristic declared before `handle`.
            let owner = self.chars.iter_mut().rev().find(|c| c.value < handle);
            match (short, owner) {
                (known::CCCD, Some(c)) if c.cccd == 0 => c.cccd = handle,
                (known::REPORT_REFERENCE, Some(c)) if c.reference == 0 => c.reference = handle,
                _ => {}
            }
        }
        if last >= end {
            self.finish_descriptors();
        } else {
            self.phase = Phase::Descriptors(last + 1);
        }
    }

    fn finish_descriptors(&mut self) {
        self.ref_reads = (0..self.chars.len())
            .filter(|&i| {
                let c = &self.chars[i];
                c.uuid == Uuid::Short(known::REPORT) && c.reference != 0
            })
            .collect();
        self.phase = Phase::Refs(0);
    }

    fn on_read(&mut self, value: Vec<u8>) -> Result<(), Error> {
        match self.phase {
            Phase::Refs(i) => {
                if let (Some(&index), [id, kind, ..]) = (self.ref_reads.get(i), value.as_slice()) {
                    self.chars[index].id_kind = Some((*id, *kind));
                }
                self.phase = Phase::Refs(i + 1);
            }
            Phase::Info => {
                self.flags = value.get(3).copied().unwrap_or(0);
                self.phase = Phase::Map;
            }
            Phase::Map => self.on_map(value)?,
            _ => {}
        }
        Ok(())
    }

    /// A piece of the Report Map: a full response means more may follow.
    fn on_map(&mut self, value: Vec<u8>) -> Result<(), Error> {
        let full = value.len() + 1 >= usize::from(self.client.mtu());
        if self.map.len() + value.len() > MAX_REPORT_MAP {
            return Err(Error::TooLarge);
        }
        self.map.extend_from_slice(&value);
        if !full {
            self.phase = Phase::Done;
        }
        Ok(())
    }

    // ---- what to ask next --------------------------------------------------

    fn next(&mut self, now: u64, step: &mut Step) {
        if self.client.is_busy() || self.phase == Phase::Failed {
            return;
        }
        let uuid = |v: u16| Uuid::Short(v);
        let end = self.range.map_or(0xFFFF, |r| r.1);
        let request = match self.phase {
            Phase::Services(start) => Request::ReadByGroupType {
                start,
                end: 0xFFFF,
                uuid: uuid(known::PRIMARY_SERVICE),
            },
            Phase::Chars(start) => Request::ReadByType {
                start,
                end,
                uuid: uuid(known::CHARACTERISTIC),
            },
            Phase::Descriptors(start) => Request::FindInformation { start, end },
            Phase::Refs(i) => match self.ref_reads.get(i) {
                Some(&index) => Request::Read {
                    handle: self.chars[index].reference,
                },
                None => return self.after_refs(now, step),
            },
            Phase::Info => match self.find(known::HID_INFORMATION) {
                Some(handle) => Request::Read { handle },
                None => return self.after_info(now, step),
            },
            Phase::Map => self.map_request(step),
            Phase::Enable(i) => match self.to_enable.get(i) {
                Some(&handle) => Request::Write {
                    handle,
                    value: alloc::vec![1, 0],
                },
                None => {
                    self.phase = Phase::Idle;
                    step.events.push(HogpEvent::Ready);
                    return;
                }
            },
            Phase::Done => return self.complete(step),
            Phase::Idle | Phase::Mtu | Phase::Failed => return,
        };
        if self.phase != Phase::Failed {
            self.request(now, request, step);
        }
    }

    fn after_refs(&mut self, now: u64, step: &mut Step) {
        self.phase = Phase::Info;
        self.next(now, step);
    }

    fn after_info(&mut self, now: u64, step: &mut Step) {
        self.phase = Phase::Map;
        self.next(now, step);
    }

    fn find(&self, uuid: u16) -> Option<u16> {
        self.chars
            .iter()
            .find(|c| c.uuid == Uuid::Short(uuid))
            .map(|c| c.value)
    }

    fn map_request(&mut self, step: &mut Step) -> Request {
        let Some(handle) = self.find(known::REPORT_MAP) else {
            self.phase = Phase::Failed;
            step.events.push(HogpEvent::Failed(Error::NoReportMap));
            // Never sent: the caller sees `Failed` and the phase.
            return Request::Read { handle: 0 };
        };
        if self.map.is_empty() {
            Request::Read { handle }
        } else {
            Request::ReadBlob {
                handle,
                offset: self.map.len() as u16,
            }
        }
    }

    /// The map is read: build the result.
    fn complete(&mut self, step: &mut Step) {
        let Some(handle) = self.find(known::REPORT_MAP) else {
            return;
        };
        if self.map.is_empty() {
            self.phase = Phase::Failed;
            step.events.push(HogpEvent::Failed(Error::NoReportMap));
            return;
        }
        let reports = self.chars.iter().filter_map(report_of).collect();
        let service = HidService {
            report_map: core::mem::take(&mut self.map),
            report_map_handle: handle,
            protocol_mode_handle: self.find(known::PROTOCOL_MODE).unwrap_or(0),
            info_flags: self.flags,
            reports,
        };
        self.phase = Phase::Idle;
        step.events.push(HogpEvent::Discovered(service));
    }
}

fn report_of(c: &Char) -> Option<Report> {
    let (report_id, kind) = match c.uuid.short()? {
        known::REPORT => c.id_kind.filter(|(_, k)| (1..=3).contains(k))?,
        known::BOOT_KEYBOARD_INPUT => (0, kind::BOOT_KEYBOARD_INPUT),
        known::BOOT_MOUSE_INPUT => (0, kind::BOOT_MOUSE_INPUT),
        _ => return None,
    };
    Some(Report {
        value: c.value,
        cccd: c.cccd,
        report_id,
        kind,
    })
}
