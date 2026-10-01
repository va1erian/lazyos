//! Pointer policy for `inputd` (`docs/usb-hid-plan.md`, phase P1): one cursor
//! and one button state for every pointing device on the raw bus.
//!
//! [`Pointer`] consumes raw pointer records (the bus encoding, decision 2 of
//! the plan) and produces [`PointerOut`]s for the compositor. It owns what is
//! device-independent and stateful: the cursor position and its clamping to
//! the screen, absolute-to-pixel scaling, the held-button set across devices,
//! and wheel accumulation. Routing to windows is the compositor's job.
//!
//! Motion is coalesced: records only update state, and an output is emitted
//! when a button changes (so every edge is its own output, in order) and on
//! [`Pointer::flush`], which the service calls once per bus drain. An
//! output's motion and wheel happened *before* its button change, and an
//! output that would repeat the last one (motion the clamp swallowed) is not
//! emitted.
//!
//! Records come from the kernel, but a USB driver feeds the kernel, so they
//! are validated here too: unknown kinds and codes, and edges that are not
//! `0`/`1`, are dropped and counted, and the cursor is always clamped.

/// Raw bus kinds this module consumes (the kernel's `bus::kind` values).
pub mod raw {
    pub const REL_MOTION: u8 = 2;
    pub const ABS_MOTION: u8 = 3;
    pub const BUTTON: u8 = 4;
    pub const SCROLL: u8 = 5;

    /// Whether `kind` is a pointer record.
    pub fn is_pointer(kind: u8) -> bool {
        (REL_MOTION..=SCROLL).contains(&kind)
    }
}

/// Button bits in [`PointerOut::buttons`]: bit `usage - 1` for HID button
/// usage `usage` (page 0x09).
pub mod buttons {
    pub const LEFT: u32 = 1 << 0;
    pub const RIGHT: u32 = 1 << 1;
    pub const MIDDLE: u32 = 1 << 2;
    pub const BACK: u32 = 1 << 3;
    pub const FORWARD: u32 = 1 << 4;
}

/// HID button usages the bus may carry (`1..=BUTTON_COUNT`).
const BUTTON_COUNT: usize = 5;

/// The largest screen side accepted by [`Pointer::set_bounds`].
pub const MAX_SIDE: u32 = 16_384;

/// The full scale of an `ABS_MOTION` coordinate.
const ABS_SCALE: i64 = 0xFFFF;

/// One raw pointer record, as the bus delivered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawPointer {
    pub seq: u64,
    pub ts_ns: u64,
    pub device: u8,
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

/// The pointer state the compositor should act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerOut {
    /// Screen pixels, inside the bounds.
    pub x: i32,
    pub y: i32,
    /// [`buttons`] bits held after this output.
    pub buttons: u32,
    /// Notches since the previous output; positive is up / right.
    pub wheel_v: i32,
    pub wheel_h: i32,
    /// Timestamp and raw sequence number of the newest record it covers.
    pub ts_ns: u64,
    pub seq: u64,
}

/// The cursor, the buttons and the wheel, across every pointing device.
pub struct Pointer {
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    /// Per button, one bit per device id holding it: a button is held while
    /// any device holds it, so two mice cannot release each other's press.
    holders: [[u64; 4]; BUTTON_COUNT],
    wheel_v: i32,
    wheel_h: i32,
    /// A record arrived since the last output.
    dirty: bool,
    /// Position and buttons of the last output: motion that the clamp
    /// swallowed (pushing against an edge) emits nothing.
    reported: (i32, i32, u32),
    ts_ns: u64,
    seq: u64,
    rejected: u64,
}

impl Pointer {
    /// A pointer centred on a `width` x `height` screen.
    pub fn new(width: u32, height: u32) -> Pointer {
        let (width, height) = (side(width), side(height));
        let (x, y) = (width / 2, height / 2);
        Pointer {
            width,
            height,
            x,
            y,
            holders: [[0; 4]; BUTTON_COUNT],
            wheel_v: 0,
            wheel_h: 0,
            dirty: false,
            reported: (x, y, 0),
            ts_ns: 0,
            seq: 0,
            rejected: 0,
        }
    }

    /// The cursor position.
    pub fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// The [`buttons`] bits held now.
    pub fn buttons(&self) -> u32 {
        (0..BUTTON_COUNT)
            .filter(|&index| self.holders[index].iter().any(|&word| word != 0))
            .fold(0, |mask, index| mask | 1 << index)
    }

    /// Records dropped as malformed so far.
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// The screen size, `(width, height)`.
    pub fn bounds(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    /// Adopt a new screen size (clamped to `1..=MAX_SIDE`) and re-clamp the
    /// cursor. Returns whether the cursor moved.
    pub fn set_bounds(&mut self, width: u32, height: u32) -> bool {
        self.width = side(width);
        self.height = side(height);
        let before = (self.x, self.y);
        self.clamp();
        let moved = before != (self.x, self.y);
        self.dirty |= moved;
        moved
    }

    /// Apply one record. A button edge that changes the held set emits an
    /// output (carrying any motion and wheel queued before it).
    pub fn apply(&mut self, record: RawPointer, out: &mut alloc::vec::Vec<PointerOut>) {
        let accepted = match record.kind {
            raw::REL_MOTION if record.code == 0 => {
                let (dx, dy) = (record.value as i16, (record.value >> 16) as i16);
                self.x = self.x.saturating_add(i32::from(dx));
                self.y = self.y.saturating_add(i32::from(dy));
                self.clamp();
                self.dirty = true;
                true
            }
            raw::ABS_MOTION if record.code == 0 => {
                let (x, y) = (record.value as u16, (record.value as u32 >> 16) as u16);
                self.x = scale(x, self.width);
                self.y = scale(y, self.height);
                self.dirty = true;
                true
            }
            raw::SCROLL if record.code <= 1 => {
                let wheel = if record.code == 0 {
                    &mut self.wheel_v
                } else {
                    &mut self.wheel_h
                };
                *wheel = wheel.saturating_add(record.value);
                self.dirty = true;
                true
            }
            raw::BUTTON => self.button(record, out),
            _ => false,
        };
        if accepted {
            self.ts_ns = record.ts_ns;
            self.seq = record.seq;
        } else {
            self.rejected += 1;
        }
    }

    /// The bus lost records (`Dropped`): a release may have been among them,
    /// and button records are edges, so every button is released. The
    /// position is state and stays.
    pub fn resync(&mut self, ts_ns: u64, seq: u64, out: &mut alloc::vec::Vec<PointerOut>) {
        self.ts_ns = ts_ns;
        self.seq = seq;
        if self.buttons() != 0 {
            self.holders = [[0; 4]; BUTTON_COUNT];
            self.dirty = true;
        }
        self.flush(out);
    }

    /// Emit the coalesced motion and wheel, if anything changed.
    pub fn flush(&mut self, out: &mut alloc::vec::Vec<PointerOut>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let now = (self.x, self.y, self.buttons());
        if now == self.reported && self.wheel_v == 0 && self.wheel_h == 0 {
            return;
        }
        self.reported = now;
        out.push(PointerOut {
            x: self.x,
            y: self.y,
            buttons: now.2,
            wheel_v: self.wheel_v,
            wheel_h: self.wheel_h,
            ts_ns: self.ts_ns,
            seq: self.seq,
        });
        self.wheel_v = 0;
        self.wheel_h = 0;
    }

    /// A button edge from one device. Returns whether it was well formed.
    fn button(&mut self, record: RawPointer, out: &mut alloc::vec::Vec<PointerOut>) -> bool {
        let index = usize::from(record.code).wrapping_sub(1);
        if index >= BUTTON_COUNT || !matches!(record.value, 0 | 1) {
            return false;
        }
        let before = self.buttons();
        let (word, bit) = (
            usize::from(record.device >> 6),
            1u64 << (record.device & 63),
        );
        if record.value == 1 {
            self.holders[index][word] |= bit;
        } else {
            self.holders[index][word] &= !bit;
        }
        if self.buttons() != before {
            self.ts_ns = record.ts_ns;
            self.seq = record.seq;
            self.dirty = true;
            self.flush(out);
        }
        true
    }

    fn clamp(&mut self) {
        self.x = self.x.clamp(0, self.width - 1);
        self.y = self.y.clamp(0, self.height - 1);
    }
}

/// A screen side as a non-zero pixel count.
fn side(pixels: u32) -> i32 {
    pixels.clamp(1, MAX_SIDE) as i32
}

/// `0..=0xFFFF` onto `0..pixels`, both ends inclusive of the screen edges.
fn scale(coordinate: u16, pixels: i32) -> i32 {
    (i64::from(coordinate) * i64::from(pixels - 1) / ABS_SCALE) as i32
}
