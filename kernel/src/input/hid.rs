//! PS/2 scancode set 1 to USB HID usage translation (`docs/input-plan.md`,
//! layer 1).
//!
//! The raw event bus speaks one vocabulary for every keyboard: USB HID usage
//! page 0x07 (Keyboard/Keypad). The PS/2 driver translates at its boundary with
//! one table, so USB HID and virtio-input drivers can later feed the same bus
//! without any consumer noticing the difference.
//!
//! Set 1 has three quirks a byte-at-a-time table cannot express, so
//! [`Set1Decoder`] is a small state machine:
//!
//! * `E0` prefixes a second key bank (navigation cluster, right-hand
//!   modifiers, keypad Enter and `/`).
//! * PrintScreen is sent as `E0 2A E0 37` (make) and `E0 B7 E0 AA` (break):
//!   the `E0 2A`/`E0 AA` halves are *fake shifts* the keyboard injects, not
//!   keys, and are swallowed (NumLock-dependent variants use `E0 36`/`E0 B6`).
//! * Pause has no break code at all: `E1 1D 45 E1 9D C5` arrives on press
//!   only. The decoder reports it as a [`Step::Tap`] (press then release) so
//!   consumers never see it stuck down.

/// HID usage codes (page 0x07) the table produces. Only the ones other code
/// names are listed; the rest are reachable through [`translate`].
#[allow(dead_code)] // a vocabulary for consumers; not every name has a user yet
pub mod usage {
    pub const A: u16 = 0x04;
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const SPACE: u16 = 0x2C;
    pub const PRINT_SCREEN: u16 = 0x46;
    pub const PAUSE: u16 = 0x48;
    pub const RIGHT_ARROW: u16 = 0x4F;
    pub const KEYPAD_ENTER: u16 = 0x58;
    pub const LEFT_CTRL: u16 = 0xE0;
    pub const LEFT_SHIFT: u16 = 0xE1;
    pub const LEFT_ALT: u16 = 0xE2;
    pub const LEFT_GUI: u16 = 0xE3;
    pub const RIGHT_CTRL: u16 = 0xE4;
    pub const RIGHT_SHIFT: u16 = 0xE5;
    pub const RIGHT_ALT: u16 = 0xE6;
    pub const RIGHT_GUI: u16 = 0xE7;
}

/// One decoded byte of the set-1 stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A prefix or the middle of a multi-byte sequence: nothing to report yet.
    Pending,
    /// A byte that is deliberately not a key (fake shift, the Pause break half).
    Ignored,
    /// A byte or sequence with no HID mapping (multimedia keys, ACPI keys,
    /// corrupt input). The caller counts these; they are never forwarded.
    Unknown,
    /// A key edge: `(usage, pressed)`.
    Key(u16, bool),
    /// A key with no break code (Pause): press followed by release.
    Tap(u16),
}

/// Where in a multi-byte sequence the decoder is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// After `E0`.
    Extended,
    /// After `E1`, expecting `first` of the two-byte Pause payload.
    Pause1,
    /// After `E1 first`, expecting the second payload byte.
    Pause2(u8),
}

/// A byte-at-a-time set-1 to HID decoder. Owned by the PS/2 driver; not
/// thread-safe by itself (the IRQ1 handler is its only caller).
pub struct Set1Decoder {
    state: State,
}

impl Set1Decoder {
    pub const fn new() -> Self {
        Set1Decoder { state: State::Idle }
    }

    /// Forget any half-received sequence (test hook).
    #[cfg(lazyos_tests)]
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// Consume one scancode byte.
    pub fn feed(&mut self, byte: u8) -> Step {
        match (self.state, byte) {
            (State::Idle, 0xE0) => {
                self.state = State::Extended;
                Step::Pending
            }
            (State::Idle, 0xE1) => {
                self.state = State::Pause1;
                Step::Pending
            }
            (State::Idle, _) => key(false, byte),
            (State::Extended, _) => {
                self.state = State::Idle;
                key(true, byte)
            }
            (State::Pause1, _) => {
                self.state = State::Pause2(byte);
                Step::Pending
            }
            (State::Pause2(first), _) => {
                self.state = State::Idle;
                match (first, byte) {
                    (0x1D, 0x45) => Step::Tap(usage::PAUSE),
                    // The break half some controllers still send.
                    (0x9D, 0xC5) => Step::Ignored,
                    _ => Step::Unknown,
                }
            }
        }
    }
}

/// Decode one `(extended, byte)` pair once the prefix is known.
fn key(extended: bool, byte: u8) -> Step {
    let pressed = byte & 0x80 == 0;
    let code = byte & 0x7F;
    // The keyboard brackets PrintScreen (and some navigation keys under
    // NumLock/Shift) with fake shift make/break pairs.
    if extended && matches!(code, 0x2A | 0x36) {
        return Step::Ignored;
    }
    match translate(extended, code) {
        Some(usage) => Step::Key(usage, pressed),
        None => Step::Unknown,
    }
}

/// The HID usage of set-1 make code `code` (release bit clear) in the plain
/// (`extended == false`) or `E0` bank.
pub fn translate(extended: bool, code: u8) -> Option<u16> {
    let usage = if extended {
        extended_usage(code)
    } else {
        plain_usage(code)
    };
    (usage != 0).then_some(usage as u16)
}

/// Plain bank: index is the make code; `0` is unmapped (usage 0 is reserved).
const PLAIN: [u8; 0x59] = {
    let mut t = [0u8; 0x59];
    // Escape, digit row 1..9,0, - = Backspace Tab.
    t[0x01] = 0x29;
    let mut i = 0;
    while i < 10 {
        t[0x02 + i] = 0x1E + i as u8;
        i += 1;
    }
    t[0x0C] = 0x2D;
    t[0x0D] = 0x2E;
    t[0x0E] = 0x2A;
    t[0x0F] = 0x2B;
    // Letter rows (QWERTY positions; the layout is inputd's business).
    let top = [0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13];
    let mut i = 0;
    while i < 10 {
        t[0x10 + i] = top[i];
        i += 1;
    }
    t[0x1A] = 0x2F;
    t[0x1B] = 0x30;
    t[0x1C] = 0x28;
    t[0x1D] = 0xE0;
    let home = [0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F];
    let mut i = 0;
    while i < 9 {
        t[0x1E + i] = home[i];
        i += 1;
    }
    t[0x27] = 0x33;
    t[0x28] = 0x34;
    t[0x29] = 0x35;
    t[0x2A] = 0xE1;
    t[0x2B] = 0x31;
    let bottom = [0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10];
    let mut i = 0;
    while i < 7 {
        t[0x2C + i] = bottom[i];
        i += 1;
    }
    t[0x33] = 0x36;
    t[0x34] = 0x37;
    t[0x35] = 0x38;
    t[0x36] = 0xE5;
    t[0x37] = 0x55; // keypad *
    t[0x38] = 0xE2;
    t[0x39] = 0x2C;
    t[0x3A] = 0x39; // Caps Lock
    let mut i = 0;
    while i < 10 {
        t[0x3B + i] = 0x3A + i as u8; // F1..F10
        i += 1;
    }
    t[0x45] = 0x53; // Num Lock
    t[0x46] = 0x47; // Scroll Lock
    let pad = [
        (0x47, 0x5F),
        (0x48, 0x60),
        (0x49, 0x61),
        (0x4A, 0x56),
        (0x4B, 0x5C),
        (0x4C, 0x5D),
        (0x4D, 0x5E),
        (0x4E, 0x57),
        (0x4F, 0x59),
        (0x50, 0x5A),
        (0x51, 0x5B),
        (0x52, 0x62),
        (0x53, 0x63),
    ];
    let mut i = 0;
    while i < pad.len() {
        t[pad[i].0] = pad[i].1;
        i += 1;
    }
    t[0x56] = 0x64; // the extra ISO key left of Z
    t[0x57] = 0x44; // F11
    t[0x58] = 0x45; // F12
    t
};

fn plain_usage(code: u8) -> u8 {
    PLAIN.get(code as usize).copied().unwrap_or(0)
}

/// `E0` bank.
fn extended_usage(code: u8) -> u8 {
    match code {
        0x1C => 0x58, // keypad Enter
        0x1D => 0xE4, // right Ctrl
        0x35 => 0x54, // keypad /
        0x37 => 0x46, // PrintScreen
        0x38 => 0xE6, // right Alt / AltGr
        0x46 => 0x48, // Ctrl+Pause ("Break")
        0x47 => 0x4A, // Home
        0x48 => 0x52, // Up
        0x49 => 0x4B, // Page Up
        0x4B => 0x50, // Left
        0x4D => 0x4F, // Right
        0x4F => 0x4D, // End
        0x50 => 0x51, // Down
        0x51 => 0x4E, // Page Down
        0x52 => 0x49, // Insert
        0x53 => 0x4C, // Delete
        0x5B => 0xE3, // left GUI (Super)
        0x5C => 0xE7, // right GUI
        0x5D => 0x65, // Application (menu)
        _ => 0,
    }
}
