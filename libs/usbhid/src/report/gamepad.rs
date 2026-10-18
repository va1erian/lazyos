//! The report layout of a gamepad (`docs/usb-gamepad-plan.md` G0): where its
//! axes, hat switch and buttons live in its reports.
//!
//! A gamepad has no boot protocol, so its layout is read from its report
//! descriptor. It is the first report whose X axis sits inside a **Game Pad**,
//! **Joystick** or **Multi-axis Controller** application collection: that
//! scoping is what keeps a gamepad from being mistaken for a pointer (a pad's
//! absolute X/Y would otherwise look like a tablet). Axes keep their Generic
//! Desktop usages, which is the vocabulary the bus carries; the driver
//! normalizes the values, `inputd` decides what they mean.

use super::{fields, usage, walk, Field};
use crate::Error;

/// Axes a gamepad layout holds, in the canonical order of [`AXES`].
pub const AXIS_COUNT: usize = 9;
/// The axis usages, in the canonical order: X, Y, Z, Rx, Ry, Rz, Slider,
/// Dial and the Hat switch.
pub const AXES: [u16; AXIS_COUNT] = [
    usage::X,
    usage::Y,
    usage::Z,
    usage::RX,
    usage::RY,
    usage::RZ,
    usage::SLIDER,
    usage::DIAL,
    usage::HAT,
];
/// Buttons a gamepad reports (usages 1..=32 of the Button page).
pub const MAX_BUTTONS: u8 = 32;
/// The hat's centred (null) value: its eight directions are `0..=7`,
/// clockwise from up.
pub const HAT_CENTRED: i32 = 8;

/// Where a gamepad's axes and buttons live in its reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gamepad {
    /// The report id these fields belong to (`None`: reports carry no id).
    pub report_id: Option<u8>,
    /// Axis slot `n` is [`AXES`]`[n]`.
    pub axes: [Option<Field>; AXIS_COUNT],
    /// Button `n` (usage `n`, `1..=MAX_BUTTONS`) at index `n - 1`.
    pub buttons: [Option<Field>; MAX_BUTTONS as usize],
}

impl Gamepad {
    /// Whether the layout has a hat switch.
    pub fn hat(&self) -> bool {
        self.axes[AXIS_COUNT - 1].is_some()
    }

    /// Read `report` (as the device sent it, id byte included when the
    /// layout has one). `None` for another report id or a report too short
    /// for a field the layout holds.
    pub fn read(&self, report: &[u8]) -> Option<GamepadReport> {
        let data = match self.report_id {
            Some(id) => match report.split_first() {
                Some((&first, rest)) if first == id => rest,
                _ => return None,
            },
            None => report,
        };
        let mut out = GamepadReport::default();
        for (slot, field) in self.axes.iter().enumerate() {
            if let Some(field) = field {
                out.axes[slot] = field.read(data)?;
            }
        }
        for (n, field) in self.buttons.iter().enumerate() {
            if let Some(field) = field {
                if field.read(data)? != 0 {
                    out.buttons |= 1 << n;
                }
            }
        }
        Some(out)
    }
}

/// A report read through a [`Gamepad`] layout; axes and buttons the layout
/// does not hold read 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GamepadReport {
    /// Raw values, by axis slot.
    pub axes: [i32; AXIS_COUNT],
    /// Bit `n - 1` is button `n`.
    pub buttons: u32,
}

/// What one report of a gamepad means for the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadOut {
    /// A changed axis: its Generic Desktop usage and its normalized value —
    /// `-32768..=32767` for a signed range, `0..=32767` for an unsigned one,
    /// and `0..=7` or [`HAT_CENTRED`] for the hat.
    Axis { usage: u16, value: i32 },
    /// A button edge, usage `1..=MAX_BUTTONS`.
    Button { usage: u16, pressed: bool },
}

/// Turns successive reports of a report-protocol gamepad into bus records:
/// one `Axis` per changed axis (in layout order), then one `Button` edge per
/// changed button. A report is a *state*, the bus carries changes.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    layout: Gamepad,
    /// Last normalized value per axis; `None` until the first report.
    axes: [Option<i32>; AXIS_COUNT],
    buttons: u32,
}

impl Decoder {
    pub fn new(layout: Gamepad) -> Decoder {
        Decoder {
            layout,
            axes: [None; AXIS_COUNT],
            buttons: 0,
        }
    }

    /// The layout this decoder reads reports through.
    pub fn layout(&self) -> &Gamepad {
        &self.layout
    }

    /// Decode `report`; `false` when it is not this pad's (another report id,
    /// or too short), which changes nothing.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(PadOut)) -> bool {
        let Some(read) = self.layout.read(report) else {
            return false;
        };
        for (slot, field) in self.layout.axes.iter().enumerate() {
            let Some(field) = field else { continue };
            let value = if AXES[slot] == usage::HAT {
                field.normalize_hat(read.axes[slot])
            } else {
                field.normalize_axis(read.axes[slot])
            };
            if self.axes[slot] != Some(value) {
                self.axes[slot] = Some(value);
                emit(PadOut::Axis {
                    usage: AXES[slot],
                    value,
                });
            }
        }
        self.apply_buttons(read.buttons, &mut emit);
        true
    }

    /// The pad is going away: every axis it reported returns to its rest
    /// value (a stick must not stay deflected) and every held button is
    /// released. Axes already at rest emit nothing.
    pub fn release_all(&mut self, mut emit: impl FnMut(PadOut)) {
        for (slot, last) in self.axes.iter_mut().enumerate() {
            let Some(previous) = *last else { continue };
            let rest = if AXES[slot] == usage::HAT {
                HAT_CENTRED
            } else {
                0
            };
            if previous != rest {
                *last = Some(rest);
                emit(PadOut::Axis {
                    usage: AXES[slot],
                    value: rest,
                });
            }
        }
        self.apply_buttons(0, &mut emit);
    }

    /// Emit one edge per changed button, in usage order.
    fn apply_buttons(&mut self, next: u32, emit: &mut impl FnMut(PadOut)) {
        for n in 0..u32::from(MAX_BUTTONS) {
            let mask = 1u32 << n;
            if (self.buttons ^ next) & mask != 0 {
                emit(PadOut::Button {
                    usage: (n + 1) as u16,
                    pressed: next & mask != 0,
                });
            }
        }
        self.buttons = next;
    }
}

/// Whether `app` is an application collection a gamepad may sit in.
fn is_pad_app(app: Option<(u16, u16)>) -> bool {
    matches!(
        app,
        Some((
            usage::GENERIC_DESKTOP,
            usage::JOYSTICK | usage::GAME_PAD | usage::MULTI_AXIS
        ))
    )
}

/// Find the first gamepad in a report descriptor: the report that carries an
/// X axis inside a pad application collection, and that report's other axes,
/// hat and buttons. Relative fields are not placed: the bus's `AXIS` is
/// absolute state.
pub fn parse_gamepad(descriptor: &[u8]) -> Result<Gamepad, Error> {
    let mut id = None;
    let ids_used = walk(descriptor, |item| {
        if id.is_none()
            && is_pad_app(item.app)
            && fields(item).any(|(u, _)| u == (usage::GENERIC_DESKTOP, usage::X))
        {
            id = Some(item.globals.report_id);
        }
    })?;
    let id = id.ok_or(Error::WrongType)?;
    let mut pad = Gamepad {
        report_id: ids_used.then_some(id),
        ..Gamepad::default()
    };
    walk(descriptor, |item| {
        if item.globals.report_id != id || !is_pad_app(item.app) {
            return;
        }
        for ((page, use_), field) in fields(item) {
            let slot = match (page, use_) {
                (usage::GENERIC_DESKTOP, u) if !field.relative => {
                    match AXES.iter().position(|&axis| axis == u) {
                        Some(slot) => &mut pad.axes[slot],
                        None => continue,
                    }
                }
                (usage::BUTTON, n @ 1..=32) => &mut pad.buttons[usize::from(n - 1)],
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(field);
            }
        }
    })?;
    if pad.axes[0].is_none() || pad.axes[1].is_none() {
        return Err(Error::WrongType);
    }
    Ok(pad)
}
