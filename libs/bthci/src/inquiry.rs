//! BR/EDR discovery: Inquiry, Remote Name Request and their events
//! (docs/bluetooth-plan.md B5).
//!
//! The commands are built as [`Command::Unknown`] and the events read from
//! [`Event::Unknown`], so the main codecs stay as small as they are; this
//! module is the one place that knows these layouts. Everything an inquiry
//! returns is a stranger's: lengths are checked before use and an entry that
//! does not fit is an error, never a panic.

use alloc::vec::Vec;

use crate::opcode::{opcode, OGF_BASEBAND, OGF_LINK};
use crate::wire::{put_u16, put_u24, Reader};
use crate::{BdAddr, Command, Error, Event};

pub const INQUIRY: u16 = opcode(OGF_LINK, 0x01);
pub const INQUIRY_CANCEL: u16 = opcode(OGF_LINK, 0x02);
pub const REMOTE_NAME_REQUEST: u16 = opcode(OGF_LINK, 0x19);
pub const REMOTE_NAME_REQUEST_CANCEL: u16 = opcode(OGF_LINK, 0x1A);
pub const WRITE_INQUIRY_MODE: u16 = opcode(OGF_BASEBAND, 0x45);

pub const EV_INQUIRY_COMPLETE: u8 = 0x01;
pub const EV_INQUIRY_RESULT: u8 = 0x02;
pub const EV_REMOTE_NAME_COMPLETE: u8 = 0x07;
pub const EV_INQUIRY_RESULT_RSSI: u8 = 0x22;
pub const EV_EXTENDED_INQUIRY_RESULT: u8 = 0x2F;

/// The General Inquiry Access Code.
pub const GIAC: u32 = 0x009E_8B33;
/// The EIR block of an extended inquiry result.
pub const EIR_LEN: usize = 240;
/// A remote name is at most this many octets.
pub const NAME_LEN: usize = 248;

/// How results are reported (`Write_Inquiry_Mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Standard = 0,
    Rssi = 1,
    Extended = 2,
}

/// One device an inquiry found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub addr: BdAddr,
    pub class_of_device: u32,
    /// Absent in the standard format.
    pub rssi: Option<i8>,
    /// Extended inquiry response data (AD structures), empty when none.
    pub eir: Vec<u8>,
}

/// What the three inquiry-related events say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InquiryEvent {
    Complete {
        status: u8,
    },
    Results(Vec<Entry>),
    Name {
        status: u8,
        addr: BdAddr,
        /// Up to the first NUL.
        name: Vec<u8>,
    },
}

/// `Inquiry`: `length` in units of 1.28 s, at most `max_responses` results
/// (0: unlimited).
pub fn inquiry(length: u8, max_responses: u8) -> Command {
    let mut params = Vec::with_capacity(5);
    put_u24(&mut params, GIAC);
    params.extend_from_slice(&[length, max_responses]);
    Command::Unknown {
        opcode: INQUIRY,
        params,
    }
}

pub fn inquiry_cancel() -> Command {
    Command::Unknown {
        opcode: INQUIRY_CANCEL,
        params: Vec::new(),
    }
}

pub fn write_inquiry_mode(mode: Mode) -> Command {
    Command::Unknown {
        opcode: WRITE_INQUIRY_MODE,
        params: alloc::vec![mode as u8],
    }
}

/// `Remote_Name_Request` with page scan repetition mode R2 and no clock
/// offset estimate.
pub fn remote_name_request(addr: BdAddr) -> Command {
    let mut params = Vec::with_capacity(10);
    params.extend_from_slice(&addr.0);
    params.extend_from_slice(&[2, 0]);
    put_u16(&mut params, 0);
    Command::Unknown {
        opcode: REMOTE_NAME_REQUEST,
        params,
    }
}

pub fn remote_name_request_cancel(addr: BdAddr) -> Command {
    Command::Unknown {
        opcode: REMOTE_NAME_REQUEST_CANCEL,
        params: addr.0.to_vec(),
    }
}

/// Read an inquiry event; `None` when `event` is something else.
pub fn parse(event: &Event) -> Option<Result<InquiryEvent, Error>> {
    let Event::Unknown { code, params } = event else {
        return None;
    };
    let mut r = Reader::new(params);
    Some(match *code {
        EV_INQUIRY_COMPLETE => r.u8().map(|status| InquiryEvent::Complete { status }),
        EV_INQUIRY_RESULT => parse_list(&mut r, Mode::Standard),
        EV_INQUIRY_RESULT_RSSI => parse_list(&mut r, Mode::Rssi),
        EV_EXTENDED_INQUIRY_RESULT => parse_extended(&mut r),
        EV_REMOTE_NAME_COMPLETE => parse_name(&mut r),
        _ => return None,
    })
}

fn parse_name(r: &mut Reader<'_>) -> Result<InquiryEvent, Error> {
    let status = r.u8()?;
    let addr = r.addr()?;
    let raw = r.rest();
    let raw = &raw[..raw.len().min(NAME_LEN)];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    Ok(InquiryEvent::Name {
        status,
        addr,
        name: raw[..end].to_vec(),
    })
}

/// The array-of-fields layout of the standard and RSSI formats.
fn parse_list(r: &mut Reader<'_>, mode: Mode) -> Result<InquiryEvent, Error> {
    let n = usize::from(r.u8()?);
    // Per entry: address 6, repetition mode 1, reserved 2 (1 in the RSSI
    // format), class 3, clock offset 2, and the RSSI octet in that format.
    let (reserved, per) = match mode {
        Mode::Rssi => (1, 6 + 1 + 1 + 3 + 2 + 1),
        _ => (2, 6 + 1 + 2 + 3 + 2),
    };
    if r.remaining() < n * per {
        return Err(Error::Truncated);
    }
    let mut addrs = Vec::with_capacity(n);
    for _ in 0..n {
        addrs.push(r.addr()?);
    }
    r.take(n)?; // page scan repetition modes
    r.take(n * reserved)?;
    let mut classes = Vec::with_capacity(n);
    for _ in 0..n {
        classes.push(r.u24()?);
    }
    r.take(n * 2)?; // clock offsets
    let rssi = r.rest();
    let mut out = Vec::with_capacity(n);
    for (i, (addr, class)) in addrs.into_iter().zip(classes).enumerate() {
        let rssi = (mode == Mode::Rssi).then(|| rssi.get(i).copied().unwrap_or(0) as i8);
        out.push(Entry {
            addr,
            class_of_device: class,
            rssi,
            eir: Vec::new(),
        });
    }
    Ok(InquiryEvent::Results(out))
}

fn parse_extended(r: &mut Reader<'_>) -> Result<InquiryEvent, Error> {
    let n = r.u8()?;
    if n != 1 {
        return Err(Error::BadLength);
    }
    let addr = r.addr()?;
    r.take(1 + 1)?; // repetition mode, reserved
    let class_of_device = r.u24()?;
    r.take(2)?; // clock offset
    let rssi = r.u8()? as i8;
    let eir = r.rest();
    Ok(InquiryEvent::Results(alloc::vec![Entry {
        addr,
        class_of_device,
        rssi: Some(rssi),
        eir: eir[..eir.len().min(EIR_LEN)].to_vec(),
    }]))
}

impl InquiryEvent {
    /// The event as a controller sends it in `mode` (the simulator's side).
    /// A result list is cut to what one event can hold; the extended format
    /// carries one entry per event, so only the first is encoded.
    pub fn to_event(&self, mode: Mode) -> Event {
        let mut p = Vec::new();
        let code = match self {
            InquiryEvent::Complete { status } => {
                p.push(*status);
                EV_INQUIRY_COMPLETE
            }
            InquiryEvent::Name { status, addr, name } => {
                p.push(*status);
                p.extend_from_slice(&addr.0);
                let n = name.len().min(NAME_LEN);
                p.extend_from_slice(&name[..n]);
                p.resize(1 + 6 + NAME_LEN, 0);
                EV_REMOTE_NAME_COMPLETE
            }
            InquiryEvent::Results(entries) => return put_results(entries, mode),
        };
        Event::Unknown { code, params: p }
    }
}

fn put_results(entries: &[Entry], mode: Mode) -> Event {
    let mut p = Vec::new();
    if mode == Mode::Extended {
        let e = entries.first();
        p.push(u8::from(e.is_some()));
        if let Some(e) = e {
            p.extend_from_slice(&e.addr.0);
            p.extend_from_slice(&[2, 0]);
            put_u24(&mut p, e.class_of_device);
            put_u16(&mut p, 0);
            p.push(e.rssi.unwrap_or(0) as u8);
            p.extend_from_slice(&e.eir[..e.eir.len().min(EIR_LEN)]);
            p.resize(1 + 6 + 2 + 3 + 2 + 1 + EIR_LEN, 0);
        }
        return Event::Unknown {
            code: EV_EXTENDED_INQUIRY_RESULT,
            params: p,
        };
    }
    let rssi_mode = mode == Mode::Rssi;
    let per = if rssi_mode { 15 } else { 14 };
    let entries = &entries[..entries.len().min(254 / per)];
    p.push(entries.len() as u8);
    for e in entries {
        p.extend_from_slice(&e.addr.0);
    }
    p.extend(entries.iter().map(|_| 2));
    for _ in entries {
        p.extend_from_slice(if rssi_mode { &[0] } else { &[0, 0] });
    }
    for e in entries {
        put_u24(&mut p, e.class_of_device);
    }
    p.extend(entries.iter().flat_map(|_| [0u8, 0]));
    if rssi_mode {
        p.extend(entries.iter().map(|e| e.rssi.unwrap_or(0) as u8));
    }
    let code = if rssi_mode {
        EV_INQUIRY_RESULT_RSSI
    } else {
        EV_INQUIRY_RESULT
    };
    Event::Unknown { code, params: p }
}

/// The device name in a block of AD structures (an EIR or an advertisement):
/// the complete local name (type 9) if present, else the shortened one (8).
pub fn ad_local_name(data: &[u8]) -> Option<Vec<u8>> {
    let mut shortened = None;
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len);
        if len == 0 || len > tail.len() {
            break;
        }
        let (field, next) = tail.split_at(len);
        match field[0] {
            0x09 => return Some(field[1..].to_vec()),
            0x08 => shortened = Some(field[1..].to_vec()),
            _ => {}
        }
        rest = next;
    }
    shortened
}

/// Whether a class of device is a Peripheral (major class 5): keyboards,
/// mice, gamepads. The only devices a HID host lists.
pub const fn is_peripheral(class_of_device: u32) -> bool {
    (class_of_device >> 8) & 0x1F == 0x05
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn entry(last: u8, rssi: Option<i8>, eir: &[u8]) -> Entry {
        Entry {
            addr: BdAddr::from_msb([1, 2, 3, 4, 5, last]),
            class_of_device: 0x0005C0,
            rssi,
            eir: eir.to_vec(),
        }
    }

    fn roundtrip(mode: Mode, entries: Vec<Entry>) -> Vec<Entry> {
        let event = InquiryEvent::Results(entries).to_event(mode);
        let wire = event.encode();
        match parse(&Event::parse(&wire).unwrap()) {
            Some(Ok(InquiryEvent::Results(out))) => out,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn standard_and_rssi_results_roundtrip() {
        let std = vec![entry(1, None, &[]), entry(2, None, &[])];
        assert_eq!(roundtrip(Mode::Standard, std.clone()), std);
        let rssi = vec![entry(1, Some(-40), &[]), entry(2, Some(-77), &[])];
        assert_eq!(roundtrip(Mode::Rssi, rssi.clone()), rssi);
    }

    #[test]
    fn extended_result_keeps_the_eir() {
        let eir = [5, 0x09, b'K', b'e', b'y', b's'];
        let out = roundtrip(Mode::Extended, vec![entry(3, Some(-60), &eir)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rssi, Some(-60));
        assert_eq!(ad_local_name(&out[0].eir).as_deref(), Some(&b"Keys"[..]));
    }

    #[test]
    fn name_and_complete_roundtrip() {
        let addr = BdAddr::from_msb([9, 8, 7, 6, 5, 4]);
        let name = InquiryEvent::Name {
            status: 0,
            addr,
            name: b"Mouse".to_vec(),
        };
        let wire = name.to_event(Mode::Standard).encode();
        assert_eq!(parse(&Event::parse(&wire).unwrap()), Some(Ok(name)));
        let done = InquiryEvent::Complete { status: 0 };
        let wire = done.to_event(Mode::Standard).encode();
        assert_eq!(parse(&Event::parse(&wire).unwrap()), Some(Ok(done)));
    }

    #[test]
    fn hostile_counts_are_refused() {
        // Claims 200 entries and carries none.
        let event = Event::Unknown {
            code: EV_INQUIRY_RESULT,
            params: vec![200, 1, 2, 3],
        };
        assert_eq!(parse(&event), Some(Err(Error::Truncated)));
        let event = Event::Unknown {
            code: EV_EXTENDED_INQUIRY_RESULT,
            params: vec![2],
        };
        assert_eq!(parse(&event), Some(Err(Error::BadLength)));
        // A name without its terminator is cut at the field's end.
        let mut params = vec![0, 1, 2, 3, 4, 5, 6];
        params.extend(core::iter::repeat_n(b'x', 400));
        let event = Event::Unknown {
            code: EV_REMOTE_NAME_COMPLETE,
            params,
        };
        match parse(&event) {
            Some(Ok(InquiryEvent::Name { name, .. })) => assert_eq!(name.len(), NAME_LEN),
            other => panic!("{other:?}"),
        }
        let other = Event::Unknown {
            code: 0x77,
            params: vec![],
        };
        assert_eq!(parse(&other), None);
    }

    #[test]
    fn commands_have_the_specified_layout() {
        assert_eq!(
            inquiry(8, 20).encode(),
            [0x01, 0x04, 5, 0x33, 0x8B, 0x9E, 8, 20]
        );
        assert_eq!(inquiry_cancel().encode(), [0x02, 0x04, 0]);
        assert_eq!(
            write_inquiry_mode(Mode::Extended).encode(),
            [0x45, 0x0C, 1, 2]
        );
        let a = BdAddr::from_msb([1, 2, 3, 4, 5, 6]);
        assert_eq!(
            remote_name_request(a).encode(),
            [0x19, 0x04, 10, 6, 5, 4, 3, 2, 1, 2, 0, 0, 0]
        );
    }

    #[test]
    fn local_name_prefers_the_complete_one() {
        let ad = [3, 0x08, b'a', b'b', 4, 0x09, b'a', b'b', b'c'];
        assert_eq!(ad_local_name(&ad).as_deref(), Some(&b"abc"[..]));
        assert_eq!(ad_local_name(&[9, 0x09, 1]), None);
        assert!(is_peripheral(0x002540));
        assert!(!is_peripheral(0x5A020C));
    }
}
