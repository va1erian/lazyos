//! HID report protocol for pointers (`docs/usb-hid-plan.md` U4, HID 1.11
//! 6.2.2): just enough of the report descriptor to find a pointer's X, Y,
//! wheel and buttons, and to read them out of a report.
//!
//! A tablet (QEMU's `usb-tablet`) has no boot protocol: its reports only make
//! sense through its report descriptor. The parser walks the item stream once,
//! keeps the global state (with Push/Pop), the local usages of the next main
//! item and a bit offset per report id, and records each Input field whose
//! usage it knows. Everything is bounded by the input and by small fixed
//! limits; anything it cannot place is skipped, never trusted.

use crate::boot::{BootMouse, MouseOut, MouseReport};
use crate::Error;

/// Largest report this module reads fields from, in bytes (with the id byte).
pub const MAX_REPORT: usize = 64;
/// Usages one main item may name before the rest are ignored.
const MAX_USAGES: usize = 16;
/// Push depth: real descriptors use one or two levels.
const MAX_STACK: usize = 4;
/// Buttons a pointer reports (usages 1..=8 of the Button page).
pub const MAX_BUTTONS: u8 = 8;

/// Usage pages and the usages this module looks for.
pub mod usage {
    pub const GENERIC_DESKTOP: u16 = 0x01;
    pub const BUTTON: u16 = 0x09;
    pub const X: u16 = 0x30;
    pub const Y: u16 = 0x31;
    pub const WHEEL: u16 = 0x38;
}

/// One value inside a report: where it is and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Offset in bits from the start of the report data (after the id byte).
    pub bit: u32,
    /// Width in bits, `1..=32`.
    pub bits: u8,
    pub logical_min: i32,
    pub logical_max: i32,
    /// Relative (a delta) rather than absolute (a position).
    pub relative: bool,
}

impl Field {
    /// The field's value in `data` (the report without its id byte),
    /// sign-extended when the logical range is signed; `None` if the report
    /// is too short to hold it.
    pub fn read(&self, data: &[u8]) -> Option<i32> {
        let end = self.bit.checked_add(u32::from(self.bits))?;
        if end as usize > data.len() * 8 {
            return None;
        }
        let mut raw = 0u64;
        for n in 0..u32::from(self.bits) {
            let bit = self.bit + n;
            let byte = data[(bit / 8) as usize];
            raw |= u64::from((byte >> (bit % 8)) & 1) << n;
        }
        let bits = u32::from(self.bits);
        if self.logical_min < 0 && bits < 64 && raw & (1 << (bits - 1)) != 0 {
            raw |= u64::MAX << bits;
        }
        Some(raw as i64 as i32)
    }

    /// An absolute value scaled to `0..=0xFFFF` over the logical range.
    pub fn normalize(&self, value: i32) -> u16 {
        let (min, max) = (i64::from(self.logical_min), i64::from(self.logical_max));
        if max <= min {
            return 0;
        }
        let clamped = i64::from(value).clamp(min, max);
        ((clamped - min) * 0xFFFF / (max - min)) as u16
    }
}

/// Where a pointer's values live in its reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    /// The report id these fields belong to (`None`: reports carry no id).
    pub report_id: Option<u8>,
    pub x: Option<Field>,
    pub y: Option<Field>,
    pub wheel: Option<Field>,
    /// Button `n` (usage `n`, `1..=MAX_BUTTONS`) at index `n - 1`.
    pub buttons: [Option<Field>; MAX_BUTTONS as usize],
}

impl Pointer {
    /// Whether X and Y are positions (a tablet) rather than deltas.
    pub fn absolute(&self) -> bool {
        matches!((self.x, self.y), (Some(x), Some(y)) if !x.relative && !y.relative)
    }

    /// Whether this is a relative pointer that reports a wheel: a boot
    /// mouse with one must run in report protocol, whose boot report
    /// (buttons, dx, dy) has no wheel byte.
    pub fn has_wheel(&self) -> bool {
        self.wheel.is_some() && !self.absolute()
    }
}

/// A report read through a [`Pointer`] layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerReport {
    pub x: i32,
    pub y: i32,
    pub wheel: i32,
    /// Bit `n - 1` is button `n`.
    pub buttons: u8,
}

impl Pointer {
    /// Read `report` (as the device sent it, id byte included when the
    /// layout has one). `None` for another report id or a short report.
    pub fn read(&self, report: &[u8]) -> Option<PointerReport> {
        let data = match self.report_id {
            Some(id) => match report.split_first() {
                Some((&first, rest)) if first == id => rest,
                _ => return None,
            },
            None => report,
        };
        let value = |field: Option<Field>| field.map_or(Some(0), |f| f.read(data));
        let mut buttons = 0u8;
        for (n, field) in self.buttons.iter().enumerate() {
            if let Some(field) = field {
                if field.read(data)? != 0 {
                    buttons |= 1 << n;
                }
            }
        }
        Some(PointerReport {
            x: value(self.x)?,
            y: value(self.y)?,
            wheel: value(self.wheel)?,
            buttons,
        })
    }
}

/// Largest wheel step the bus accepts per record (`kernel/src/input/sources.rs`).
pub const MAX_WHEEL: i32 = 127;

/// What one report of a report-protocol pointer means for the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    /// A tablet's position, scaled to `0..=0xFFFF` on both axes.
    Position { x: u16, y: u16 },
    /// Relative motion, wheel and button edges, as a boot mouse has them.
    Mouse(MouseOut),
}

/// Turns successive reports of a report-protocol pointer into bus events:
/// the position (only when it changed) or the motion, then the wheel, then
/// one edge per changed button.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    layout: Pointer,
    buttons: BootMouse,
    last: Option<(u16, u16)>,
}

impl Decoder {
    pub fn new(layout: Pointer) -> Decoder {
        Decoder {
            layout,
            buttons: BootMouse::new(),
            last: None,
        }
    }

    /// Whether this is an absolute pointer (a tablet).
    pub fn absolute(&self) -> bool {
        self.layout.absolute()
    }

    /// Decode `report`; `false` when it is not this pointer's (another
    /// report id, or too short), which changes nothing.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(Out)) -> bool {
        let Some(read) = self.layout.read(report) else {
            return false;
        };
        if let (true, Some(x), Some(y)) = (self.absolute(), self.layout.x, self.layout.y) {
            let at = (x.normalize(read.x), y.normalize(read.y));
            if self.last != Some(at) {
                self.last = Some(at);
                emit(Out::Position { x: at.0, y: at.1 });
            }
        } else {
            let clamp = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            let (dx, dy) = (clamp(read.x), clamp(read.y));
            if dx != 0 || dy != 0 {
                emit(Out::Mouse(MouseOut::Motion { dx, dy }));
            }
        }
        let wheel = read.wheel.clamp(-MAX_WHEEL, MAX_WHEEL);
        if wheel != 0 {
            emit(Out::Mouse(MouseOut::Wheel(wheel)));
        }
        let buttons = MouseReport {
            buttons: read.buttons,
            dx: 0,
            dy: 0,
            wheel: 0,
        };
        self.buttons.feed(&buttons, |out| emit(Out::Mouse(out)));
        true
    }

    /// Release every held button (the device went away).
    pub fn release_all(&mut self, mut emit: impl FnMut(Out)) {
        self.buttons.release_all(|out| emit(Out::Mouse(out)));
    }
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

/// Local usages of the next main item.
#[derive(Default)]
struct Locals {
    usages: [u32; MAX_USAGES],
    len: usize,
    min: Option<u32>,
    max: Option<u32>,
}

impl Locals {
    /// A usage item: four bytes carry their own page, shorter ones do not.
    fn push(&mut self, usage: u32) {
        if self.len < MAX_USAGES {
            self.usages[self.len] = usage;
            self.len += 1;
        }
    }

    /// The (page, usage) of field `n` of the current main item.
    fn nth(&self, n: u32, page: u16) -> Option<(u16, u16)> {
        let full = if self.len > 0 {
            *self
                .usages
                .get(n as usize)
                .unwrap_or(&self.usages[self.len - 1])
        } else {
            let (min, max) = (self.min?, self.max?);
            let usage = min.checked_add(n)?;
            if usage > max {
                return None;
            }
            usage
        };
        // A four-byte usage carries its own page in the high half.
        let page = if full > 0xFFFF {
            (full >> 16) as u16
        } else {
            page
        };
        Some((page, full as u16))
    }
}

/// One Input item as the walk saw it.
struct Item<'a> {
    globals: &'a Globals,
    locals: &'a Locals,
    flags: u32,
    /// Bit offset of the item's first field within its report.
    start: u32,
}

/// Walk the item stream, calling `visit` for every Input item with the
/// global and local state in force and its offset in its report. Returns
/// whether the descriptor uses report ids at all.
fn walk(descriptor: &[u8], mut visit: impl FnMut(&Item)) -> Result<bool, Error> {
    let mut globals = Globals::default();
    let mut stack = [Globals::default(); MAX_STACK];
    let mut depth = 0;
    let mut locals = Locals::default();
    let mut offsets = [0u32; 256];
    let mut ids_used = false;
    let mut at = 0;
    while at < descriptor.len() {
        let prefix = descriptor[at];
        if prefix == 0xFE {
            // Long item: size, tag, data. None are defined; skip it.
            let size = *descriptor.get(at + 1).ok_or(Error::Short)? as usize;
            at = at.checked_add(3 + size).ok_or(Error::BadLength)?;
            continue;
        }
        let size = [0, 1, 2, 4][usize::from(prefix & 0b11)];
        let data = descriptor.get(at + 1..at + 1 + size).ok_or(Error::Short)?;
        at += 1 + size;
        let unsigned = data
            .iter()
            .rev()
            .fold(0u32, |acc, &b| acc << 8 | u32::from(b));
        let signed = match size {
            1 => i32::from(data[0] as i8),
            2 => i32::from(i16::from_le_bytes([data[0], data[1]])),
            4 => unsigned as i32,
            _ => 0,
        };
        match ((prefix >> 2) & 0b11, prefix >> 4) {
            // Main items: Input places fields; every main item ends the
            // local state.
            (0, tag) => {
                if tag == 0x8 {
                    let id = usize::from(globals.report_id);
                    let start = offsets[id];
                    let bits = globals.size.saturating_mul(globals.count.min(255));
                    offsets[id] = start.saturating_add(bits);
                    visit(&Item {
                        globals: &globals,
                        locals: &locals,
                        flags: unsigned,
                        start,
                    });
                }
                locals = Locals::default();
            }
            (1, 0x0) => globals.page = unsigned as u16,
            (1, 0x1) => globals.logical_min = signed,
            // A logical maximum is unsigned when the minimum is not negative.
            (1, 0x2) => {
                globals.logical_max = if globals.logical_min >= 0 {
                    unsigned as i32
                } else {
                    signed
                }
            }
            (1, 0x7) => globals.size = unsigned,
            (1, 0x8) => {
                globals.report_id = unsigned as u8;
                ids_used = true;
            }
            (1, 0x9) => globals.count = unsigned,
            (1, 0xA) if depth < MAX_STACK => {
                stack[depth] = globals;
                depth += 1;
            }
            (1, 0xB) if depth > 0 => {
                depth -= 1;
                globals = stack[depth];
            }
            (2, 0x0) => locals.push(unsigned),
            (2, 0x1) => locals.min = Some(unsigned),
            (2, 0x2) => locals.max = Some(unsigned),
            _ => {}
        }
    }
    Ok(ids_used)
}

/// The value fields of an Input item with their (page, usage): constant
/// (padding) items and arrays carry none.
fn fields<'a>(item: &'a Item) -> impl Iterator<Item = ((u16, u16), Field)> + 'a {
    let size = item.globals.size;
    let usable = item.flags & 1 == 0 && item.flags & 2 != 0 && (1..=32).contains(&size);
    let count = if usable {
        item.globals.count.min(255)
    } else {
        0
    };
    (0..count).filter_map(move |n| {
        let bit = item.start.checked_add(n.checked_mul(size)?)?;
        if bit.checked_add(size)? as usize > (MAX_REPORT - 1) * 8 {
            return None;
        }
        let usage = item.locals.nth(n, item.globals.page)?;
        Some((
            usage,
            Field {
                bit,
                bits: size as u8,
                logical_min: item.globals.logical_min,
                logical_max: item.globals.logical_max,
                relative: item.flags & 4 != 0,
            },
        ))
    })
}

/// Find the first pointer (X and Y on the Generic Desktop page) in a report
/// descriptor: the report that carries the first X field, and that report's
/// Y, wheel and buttons. Fields of other reports are ignored.
pub fn parse_pointer(descriptor: &[u8]) -> Result<Pointer, Error> {
    let mut id = None;
    let ids_used = walk(descriptor, |item| {
        if id.is_none() && fields(item).any(|(u, _)| u == (usage::GENERIC_DESKTOP, usage::X)) {
            id = Some(item.globals.report_id);
        }
    })?;
    let id = id.ok_or(Error::WrongType)?;
    let mut pointer = Pointer {
        report_id: ids_used.then_some(id),
        ..Pointer::default()
    };
    walk(descriptor, |item| {
        if item.globals.report_id != id {
            return;
        }
        for ((page, use_), field) in fields(item) {
            let slot = match (page, use_) {
                (usage::GENERIC_DESKTOP, usage::X) => &mut pointer.x,
                (usage::GENERIC_DESKTOP, usage::Y) => &mut pointer.y,
                (usage::GENERIC_DESKTOP, usage::WHEEL) => &mut pointer.wheel,
                (usage::BUTTON, n @ 1..=8) => &mut pointer.buttons[usize::from(n - 1)],
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(field);
            }
        }
    })?;
    if pointer.y.is_none() {
        return Err(Error::WrongType);
    }
    Ok(pointer)
}
