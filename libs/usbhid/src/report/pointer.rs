//! The report layout of a pointer (`docs/usb-hid-plan.md` U4): where a
//! mouse's or tablet's X, Y, wheel and buttons live in its reports.
//!
//! A pointer is the first report in the descriptor that carries X, whatever
//! collection it sits in: that is what QEMU's tablet and every report-protocol
//! mouse present.

use super::{fields, usage, walk, Field};
use crate::boot::{BootMouse, MouseOut, MouseReport};
use crate::Error;

/// Buttons a pointer reports (usages 1..=8 of the Button page).
pub const MAX_BUTTONS: u8 = 8;

/// Largest wheel step the bus accepts per record (`kernel/src/input/sources.rs`).
pub const MAX_WHEEL: i32 = 127;

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
