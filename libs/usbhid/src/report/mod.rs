//! HID report protocol (`docs/usb-hid-plan.md` U4, `docs/usb-gamepad-plan.md`
//! G0): just enough of the report descriptor to find the values of a pointer
//! or a gamepad, and to read them out of a report.
//!
//! A tablet (QEMU's `usb-tablet`) and every gamepad have no boot protocol:
//! their reports only make sense through their report descriptor. The parser
//! walks the item stream once, keeps the global state (with Push/Pop), the
//! local usages of the next main item, the open collections and a bit offset
//! per report id, and records each Input field whose usage it knows.
//! Everything is bounded by the input and by small fixed limits; anything it
//! cannot place is skipped, never trusted.
//!
//! * [`pointer`] finds a pointer's X, Y, wheel and buttons.
//! * [`gamepad`] finds a gamepad's axes, hat switch and buttons, inside a
//!   Game Pad, Joystick or Multi-axis Controller application collection.

use crate::Error;

pub mod gamepad;
pub mod pointer;

pub use pointer::{parse_pointer, Decoder, Out, Pointer, PointerReport, MAX_BUTTONS, MAX_WHEEL};

/// Largest report this module reads fields from, in bytes (with the id byte).
pub const MAX_REPORT: usize = 64;
/// Usages one main item may name before the rest are ignored.
const MAX_USAGES: usize = 16;
/// Push depth: real descriptors use one or two levels.
const MAX_STACK: usize = 4;
/// Collections kept open at once (an application, then physical/logical ones).
const MAX_COLLECTIONS: usize = 4;

/// Usage pages and the usages this module looks for.
pub mod usage {
    pub const GENERIC_DESKTOP: u16 = 0x01;
    pub const BUTTON: u16 = 0x09;
    /// The application collections a gamepad sits in.
    pub const JOYSTICK: u16 = 0x04;
    pub const GAME_PAD: u16 = 0x05;
    pub const MULTI_AXIS: u16 = 0x08;
    /// The Generic Desktop axis usages.
    pub const X: u16 = 0x30;
    pub const Y: u16 = 0x31;
    pub const Z: u16 = 0x32;
    pub const RX: u16 = 0x33;
    pub const RY: u16 = 0x34;
    pub const RZ: u16 = 0x35;
    pub const SLIDER: u16 = 0x36;
    pub const DIAL: u16 = 0x37;
    pub const WHEEL: u16 = 0x38;
    pub const HAT: u16 = 0x39;
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

    /// A gamepad axis value scaled to `-32768..=32767` for a signed logical
    /// range (sticks, which rest at 0) or `0..=32767` for an unsigned one
    /// (triggers, which rest at their logical minimum).
    pub fn normalize_axis(&self, value: i32) -> i32 {
        let (min, max) = (i64::from(self.logical_min), i64::from(self.logical_max));
        if max <= min {
            return 0;
        }
        let clamped = i64::from(value).clamp(min, max);
        if self.logical_min < 0 {
            ((clamped - min) * 0xFFFF / (max - min) - 0x8000) as i32
        } else {
            ((clamped - min) * 0x7FFF / (max - min)) as i32
        }
    }

    /// A hat switch's direction: `0..=7` clockwise from up (a 4-way hat is
    /// spread over the same eight directions), or [`gamepad::HAT_CENTRED`]
    /// for a value outside the logical range — the null state a hat rests in.
    pub fn normalize_hat(&self, value: i32) -> i32 {
        // A hostile descriptor can name the full `i32` range: widen before
        // any arithmetic, as `normalize` and `normalize_axis` do.
        let (min, max, value) = (
            i64::from(self.logical_min),
            i64::from(self.logical_max),
            i64::from(value),
        );
        if max < min || value < min || value > max {
            return gamepad::HAT_CENTRED;
        }
        let span = max - min + 1;
        (((value - min) * 8) / span) as i32
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

/// The collections open at this point in the walk, outermost first.
///
/// `depth` is the true nesting depth; only the outermost [`MAX_COLLECTIONS`]
/// are recorded, so a hostile descriptor that nests more cannot unbalance the
/// application collection's identity (which is all the layouts below use).
#[derive(Clone, Copy, Default)]
struct Collections {
    open: [(u16, u16); MAX_COLLECTIONS],
    depth: usize,
}

impl Collections {
    /// The application collection this item sits in: the outermost one.
    fn application(&self) -> Option<(u16, u16)> {
        (self.depth > 0).then(|| self.open[0])
    }

    fn push(&mut self, usage: (u16, u16)) {
        if self.depth < MAX_COLLECTIONS {
            self.open[self.depth] = usage;
        }
        self.depth += 1;
    }

    fn pop(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// One Input item as the walk saw it.
struct Item<'a> {
    globals: &'a Globals,
    locals: &'a Locals,
    flags: u32,
    /// Bit offset of the item's first field within its report.
    start: u32,
    /// The application collection's usage this item sits in, if any.
    app: Option<(u16, u16)>,
    /// Whether a Report ID item appeared before this one: the layout's
    /// reports carry an id byte only if they do (a descriptor whose first
    /// report has none and a later one has one must not demand a zero byte).
    ids_used: bool,
}

/// Walk the item stream, calling `visit` for every Input item with the
/// global and local state in force and its offset in its report.
fn walk(descriptor: &[u8], mut visit: impl FnMut(&Item)) -> Result<(), Error> {
    let mut globals = Globals::default();
    let mut stack = [Globals::default(); MAX_STACK];
    let mut depth = 0;
    let mut locals = Locals::default();
    let mut collections = Collections::default();
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
            // Main items: Input places fields, Collection opens one, every
            // main item ends the local state.
            (0, tag) => {
                match tag {
                    0x8 => {
                        let id = usize::from(globals.report_id);
                        let start = offsets[id];
                        let bits = globals.size.saturating_mul(globals.count.min(255));
                        offsets[id] = start.saturating_add(bits);
                        visit(&Item {
                            globals: &globals,
                            locals: &locals,
                            flags: unsigned,
                            start,
                            app: collections.application(),
                            ids_used,
                        });
                    }
                    0xA => collections.push(locals.nth(0, globals.page).unwrap_or((0, 0))),
                    0xC => collections.pop(),
                    _ => {}
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
    Ok(())
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
