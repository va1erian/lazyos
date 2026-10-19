//! Gamepad policy for `inputd` (`docs/usb-gamepad-plan.md`, phase G3): the
//! device-independent state of up to [`MAX_PADS`] gamepads on the raw bus.
//!
//! [`Gamepad`] consumes `AXIS` and `BUTTON` records of the devices it was told
//! are pads and produces [`PadOut`] events for the focused session. It owns
//! what the plan's decision 6 gives `inputd`: the deadzone (applied around 0
//! and rescaled to the full range), the hat turned into d-pad buttons, one
//! slot per device, coalescing, and the resync that fails safe.
//!
//! Which records are a pad's is not decided here but by [`route::Classifier`]
//! from the kernel's `SOURCE` records and source table; [`pads::Pads`] ties
//! the two together and is what `inputd` drives. The shared page a game polls
//! is [`page`].
//!
//! **Events.** Button edges (and the d-pad edges a hat change makes) are
//! emitted at once, in order, releases of a change before its presses; an axis
//! only updates state, and [`Gamepad::flush`] (once per bus drain) emits one
//! event per axis that ended up somewhere new. [`Gamepad::resync`] (the bus
//! lost records) and [`Gamepad::detach`] (the pad is gone) release every button
//! and centre every axis, so a lost release or an unplug sticks nothing. A
//! stick held still across a resync reads as centred until it moves: that
//! fails safe, as the keyboard's "release what you cannot prove is held".
//!
//! **Hostile input.** Records come from a USB driver through the kernel, which
//! already gates them; they are validated here too. An unknown usage, a value
//! out of range, a button outside `1..=32`, or a record for a device with no
//! slot changes nothing but [`Gamepad::rejected`].

use alloc::vec::Vec;

pub mod pads;
pub mod page;
pub mod route;

/// Pads tracked at once; a fifth is refused ([`Gamepad::attach`]).
pub const MAX_PADS: usize = 4;

/// Axes a pad slot holds: Generic Desktop usages `0x30..=0x37`.
pub const AXES: usize = 8;

/// The deadzone: 0.15 of full scale. Values within it read as 0; the rest is
/// rescaled so the full range stays reachable.
pub const DEADZONE: i32 = 4915;

/// The hat value for "no direction".
pub const HAT_CENTRED: u8 = 8;

/// Raw bus kinds this module consumes (the kernel's `bus::kind` values).
pub mod raw {
    pub const BUTTON: u8 = 4;
    pub const AXIS: u8 = 8;
    pub const DEVICE: u8 = 9;
    pub const SOURCE: u8 = 10;
}

/// Generic Desktop usages of an `AXIS` record's `code`.
pub mod usage {
    /// X, the first axis; the slot index of axis `u` is `u - X`.
    pub const X: u16 = 0x30;
    /// Dial, the last of the eight plain axes.
    pub const DIAL: u16 = 0x37;
    /// The hat switch: delivered as d-pad buttons, never as an axis.
    pub const HAT: u16 = 0x39;
}

/// `GamepadButton` codes: HID button usages `1..=32`, then the d-pad.
pub mod code {
    /// The highest HID button usage a pad may publish.
    pub const LAST_BUTTON: u16 = 32;
    pub const DPAD_UP: u16 = 33;
    pub const DPAD_DOWN: u16 = 34;
    pub const DPAD_LEFT: u16 = 35;
    pub const DPAD_RIGHT: u16 = 36;
}

/// Bits of [`PadState::dpad`].
pub mod dpad {
    pub const UP: u8 = 1 << 0;
    pub const DOWN: u8 = 1 << 1;
    pub const LEFT: u8 = 1 << 2;
    pub const RIGHT: u8 = 1 << 3;
}

/// `GamepadButton` code per [`dpad`] bit, in bit order.
const DPAD_CODES: [(u8, u16); 4] = [
    (dpad::UP, code::DPAD_UP),
    (dpad::DOWN, code::DPAD_DOWN),
    (dpad::LEFT, code::DPAD_LEFT),
    (dpad::RIGHT, code::DPAD_RIGHT),
];

/// One raw bus record, as the bus delivered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawPad {
    pub seq: u64,
    pub ts_ns: u64,
    pub device: u8,
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

impl From<crate::RawPointer> for RawPad {
    fn from(record: crate::RawPointer) -> RawPad {
        RawPad {
            seq: record.seq,
            ts_ns: record.ts_ns,
            device: record.device,
            kind: record.kind,
            code: record.code,
            value: record.value,
        }
    }
}

/// What one pad is doing now (deadzone already applied).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadState {
    /// The pad's device id (a kernel source slot's).
    pub device: u8,
    /// Bit `usage - 1` for HID button usage `usage` (`1..=32`).
    pub buttons: u32,
    /// `0..=7` clockwise from up, [`HAT_CENTRED`] for none.
    pub hat: u8,
    /// X, Y, Z, Rx, Ry, Rz, Slider, Dial; `0` is the rest, Y and Ry are
    /// positive down.
    pub axes: [i16; AXES],
}

impl Default for PadState {
    fn default() -> PadState {
        PadState::new(0)
    }
}

impl PadState {
    /// A pad at rest.
    pub const fn new(device: u8) -> PadState {
        PadState {
            device,
            buttons: 0,
            hat: HAT_CENTRED,
            axes: [0; AXES],
        }
    }

    /// The hat as [`dpad`] bits; a diagonal sets two.
    pub fn dpad(&self) -> u8 {
        dpad_of(self.hat)
    }

    /// Whether HID button usage `usage` (`1..=32`) is held.
    pub fn is_down(&self, usage: u16) -> bool {
        (1..=code::LAST_BUTTON).contains(&usage) && self.buttons & (1 << (usage - 1)) != 0
    }
}

/// [`dpad`] bits of hat value `hat` (anything but `0..=7` is centred).
pub const fn dpad_of(hat: u8) -> u8 {
    match hat {
        0 => dpad::UP,
        1 => dpad::UP | dpad::RIGHT,
        2 => dpad::RIGHT,
        3 => dpad::RIGHT | dpad::DOWN,
        4 => dpad::DOWN,
        5 => dpad::DOWN | dpad::LEFT,
        6 => dpad::LEFT,
        7 => dpad::LEFT | dpad::UP,
        _ => 0,
    }
}

/// One change for the focused session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadOut {
    /// An axis moved: `code` is its usage `0x30..=0x37`.
    Axis {
        device: u8,
        code: u16,
        value: i16,
        ts_ns: u64,
        seq: u64,
    },
    /// A button (or d-pad direction) changed: see [`code`].
    Button {
        device: u8,
        code: u16,
        pressed: bool,
        ts_ns: u64,
        seq: u64,
    },
}

impl PadOut {
    /// The pad the change is about.
    pub fn device(&self) -> u8 {
        match *self {
            PadOut::Axis { device, .. } | PadOut::Button { device, .. } => device,
        }
    }

    /// The raw sequence number of the record it covers.
    pub fn seq(&self) -> u64 {
        match *self {
            PadOut::Axis { seq, .. } | PadOut::Button { seq, .. } => seq,
        }
    }
}

/// The deadzone applied to a raw axis value and the rest rescaled to the full
/// range: `0` within [`DEADZONE`], `-32768` and `32767` still reachable.
pub fn deadzone(value: i16) -> i16 {
    let magnitude = i32::from(value).abs();
    if magnitude <= DEADZONE {
        return 0;
    }
    // Each side has its own full scale (-32768 and +32767).
    let full = if value < 0 { 32768 } else { 32767 };
    let scaled = (magnitude - DEADZONE) * full / (full - DEADZONE);
    let scaled = scaled.min(full);
    (if value < 0 { -scaled } else { scaled }) as i16
}

/// The stamp of the newest record behind a change.
#[derive(Clone, Copy)]
struct Stamp {
    ts_ns: u64,
    seq: u64,
}

struct Slot {
    state: PadState,
    /// What the session was last told for each axis.
    sent: [i16; AXES],
    /// Axes that moved since the last [`Gamepad::flush`].
    pending: u8,
    stamp: Stamp,
}

impl Slot {
    fn new(device: u8) -> Slot {
        Slot {
            state: PadState::new(device),
            sent: [0; AXES],
            pending: 0,
            stamp: Stamp { ts_ns: 0, seq: 0 },
        }
    }

    fn button(&self, code: u16, pressed: bool, stamp: Stamp) -> PadOut {
        PadOut::Button {
            device: self.state.device,
            code,
            pressed,
            ts_ns: stamp.ts_ns,
            seq: stamp.seq,
        }
    }

    /// Move the hat, emitting the d-pad edges it makes: releases first.
    fn set_hat(&mut self, hat: u8, stamp: Stamp, out: &mut Vec<PadOut>) {
        let (before, after) = (dpad_of(self.state.hat), dpad_of(hat));
        self.state.hat = hat;
        for (bit, code) in DPAD_CODES {
            if before & bit != 0 && after & bit == 0 {
                out.push(self.button(code, false, stamp));
            }
        }
        for (bit, code) in DPAD_CODES {
            if after & bit != 0 && before & bit == 0 {
                out.push(self.button(code, true, stamp));
            }
        }
    }

    /// Release every button and centre every axis, telling the session only
    /// what it believes is away from rest.
    fn centre(&mut self, stamp: Stamp, out: &mut Vec<PadOut>) {
        self.stamp = stamp;
        let held = self.state.buttons;
        self.state.buttons = 0;
        for usage in 1..=code::LAST_BUTTON {
            if held & (1 << (usage - 1)) != 0 {
                out.push(self.button(usage, false, stamp));
            }
        }
        self.set_hat(HAT_CENTRED, stamp, out);
        for (index, sent) in self.sent.iter_mut().enumerate() {
            if *sent != 0 {
                out.push(PadOut::Axis {
                    device: self.state.device,
                    code: usage::X + index as u16,
                    value: 0,
                    ts_ns: stamp.ts_ns,
                    seq: stamp.seq,
                });
            }
            *sent = 0;
        }
        self.state.axes = [0; AXES];
        self.pending = 0;
    }
}

/// The state of every attached pad.
pub struct Gamepad {
    slots: [Option<Slot>; MAX_PADS],
    last_seq: u64,
    rejected: u64,
}

impl Default for Gamepad {
    fn default() -> Gamepad {
        Gamepad::new()
    }
}

impl Gamepad {
    pub const fn new() -> Gamepad {
        Gamepad {
            slots: [const { None }; MAX_PADS],
            last_seq: 0,
            rejected: 0,
        }
    }

    /// Give device `device` a slot. `false` when all [`MAX_PADS`] are taken
    /// (its records are then rejected and counted); attaching a device that
    /// has a slot leaves it as it is.
    pub fn attach(&mut self, device: u8) -> bool {
        if self.slot(device).is_some() {
            return true;
        }
        match self.slots.iter_mut().find(|slot| slot.is_none()) {
            Some(free) => {
                *free = Some(Slot::new(device));
                true
            }
            None => false,
        }
    }

    /// Free `device`'s slot after releasing every button and centring every
    /// axis (events for the session, which has not seen it go).
    pub fn detach(&mut self, device: u8, ts_ns: u64, seq: u64, out: &mut Vec<PadOut>) {
        let stamp = Stamp { ts_ns, seq };
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|s| s.state.device == device) {
                if let Some(mut gone) = slot.take() {
                    gone.centre(stamp, out);
                }
                self.last_seq = self.last_seq.max(seq);
            }
        }
    }

    /// The pad on `device`, if it has a slot.
    pub fn pad(&self, device: u8) -> Option<&PadState> {
        self.slot(device).map(|slot| &slot.state)
    }

    /// The attached pads, in slot order.
    pub fn pads(&self) -> impl Iterator<Item = &PadState> {
        self.slots.iter().flatten().map(|slot| &slot.state)
    }

    /// How many pads are attached.
    pub fn count(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Records dropped as malformed (or for a device without a slot).
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// The raw sequence number of the newest record or resync reflected.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Apply one `BUTTON` or `AXIS` record. Returns whether it was accepted.
    pub fn apply(&mut self, record: RawPad, out: &mut Vec<PadOut>) -> bool {
        let stamp = Stamp {
            ts_ns: record.ts_ns,
            seq: record.seq,
        };
        let accepted = match self.slot_mut(record.device) {
            Some(slot) => {
                let ok = match record.kind {
                    raw::BUTTON => button(slot, &record, stamp, out),
                    raw::AXIS => axis(slot, &record, stamp, out),
                    _ => false,
                };
                if ok {
                    slot.stamp = stamp;
                }
                ok
            }
            None => false,
        };
        if accepted {
            self.last_seq = self.last_seq.max(record.seq);
        } else {
            self.rejected += 1;
        }
        accepted
    }

    /// Emit one event per axis that moved since the last flush (the bus
    /// drain's end): its newest value, none if it came back to where the
    /// session last saw it.
    pub fn flush(&mut self, out: &mut Vec<PadOut>) {
        for slot in self.slots.iter_mut().flatten() {
            let pending = core::mem::take(&mut slot.pending);
            for index in 0..AXES {
                let value = slot.state.axes[index];
                if pending & (1 << index) != 0 && value != slot.sent[index] {
                    slot.sent[index] = value;
                    out.push(PadOut::Axis {
                        device: slot.state.device,
                        code: usage::X + index as u16,
                        value,
                        ts_ns: slot.stamp.ts_ns,
                        seq: slot.stamp.seq,
                    });
                }
            }
        }
    }

    /// The bus lost records (`Dropped`): a release may have been among them
    /// and a stick's last position is unknown, so every pad is released and
    /// centred. The pads stay attached.
    pub fn resync(&mut self, ts_ns: u64, seq: u64, out: &mut Vec<PadOut>) {
        let stamp = Stamp { ts_ns, seq };
        self.last_seq = self.last_seq.max(seq);
        for slot in self.slots.iter_mut().flatten() {
            slot.centre(stamp, out);
        }
    }

    fn slot(&self, device: u8) -> Option<&Slot> {
        self.slots
            .iter()
            .flatten()
            .find(|slot| slot.state.device == device)
    }

    fn slot_mut(&mut self, device: u8) -> Option<&mut Slot> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|slot| slot.state.device == device)
    }
}

/// A button edge: usage `1..=32`, value `0`/`1`.
fn button(slot: &mut Slot, record: &RawPad, stamp: Stamp, out: &mut Vec<PadOut>) -> bool {
    if !(1..=code::LAST_BUTTON).contains(&record.code) || !matches!(record.value, 0 | 1) {
        return false;
    }
    let bit = 1u32 << (record.code - 1);
    let pressed = record.value == 1;
    if (slot.state.buttons & bit != 0) != pressed {
        slot.state.buttons ^= bit;
        out.push(slot.button(record.code, pressed, stamp));
    }
    true
}

/// An axis: the hat (`0..=8`, as d-pad edges) or one of the eight plain axes
/// (`i16`, deadzoned, emitted at the next flush).
fn axis(slot: &mut Slot, record: &RawPad, stamp: Stamp, out: &mut Vec<PadOut>) -> bool {
    if record.code == usage::HAT {
        let Ok(hat @ 0..=HAT_CENTRED) = u8::try_from(record.value) else {
            return false;
        };
        slot.set_hat(hat, stamp, out);
        return true;
    }
    if !(usage::X..=usage::DIAL).contains(&record.code) {
        return false;
    }
    let Ok(value) = i16::try_from(record.value) else {
        return false;
    };
    let index = usize::from(record.code - usage::X);
    let value = deadzone(value);
    if slot.state.axes[index] != value {
        slot.state.axes[index] = value;
        slot.pending |= 1 << index;
    }
    true
}
