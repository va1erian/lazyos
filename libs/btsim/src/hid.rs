//! What a simulated HID peripheral sends: descriptors and reports.
//!
//! The bytes are the USB HID boot-compatible layouts, because Bluetooth HID
//! reports are USB HID reports in a different envelope. Both devices use a
//! report id (keyboard 1, mouse 2) in report protocol, and none in boot
//! protocol, as the HID profile specifies.

use std::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidKind {
    Keyboard,
    Mouse,
}

pub const KEYBOARD_REPORT_ID: u8 = 1;
pub const MOUSE_REPORT_ID: u8 = 2;

/// Keyboard: modifiers, reserved, six keys (input); five LEDs (output).
pub const KEYBOARD_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05,
    0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0,
];

/// Mouse: three buttons, relative X, Y and wheel.
pub const MOUSE_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08,
    0x95, 0x03, 0x81, 0x06, 0xC0, 0xC0,
];

impl HidKind {
    pub fn descriptor(self) -> &'static [u8] {
        match self {
            HidKind::Keyboard => KEYBOARD_DESCRIPTOR,
            HidKind::Mouse => MOUSE_DESCRIPTOR,
        }
    }

    /// The HIDDeviceSubclass SDP attribute (boot keyboard / boot mouse bits).
    pub fn subclass(self) -> u8 {
        match self {
            HidKind::Keyboard => 0x40,
            HidKind::Mouse => 0x80,
        }
    }

    /// Class of device: Peripheral major class, keyboard or pointing minor.
    pub fn class_of_device(self) -> u32 {
        0x0500 | u32::from(self.subclass())
    }
}

/// HIDP message headers (transaction type in the high nibble).
pub mod hidp {
    pub const HANDSHAKE_SUCCESS: u8 = 0x00;
    pub const HANDSHAKE_UNSUPPORTED: u8 = 0x03;
    pub const HID_CONTROL: u8 = 0x10;
    pub const VIRTUAL_CABLE_UNPLUG: u8 = 0x05;
    pub const SET_REPORT: u8 = 0x50;
    pub const SET_PROTOCOL: u8 = 0x70;
    /// DATA, input report: the header of every report a peer sends.
    pub const DATA_INPUT: u8 = 0xA1;
    /// DATA, output report (host to device, on the interrupt channel).
    pub const DATA_OUTPUT: u8 = 0xA2;
}

/// A keyboard input report with one key down (or none: usage 0) and the given
/// modifiers. `boot` omits the report id.
pub fn keyboard_report(modifiers: u8, usage: u8, boot: bool) -> Vec<u8> {
    let mut r = Vec::new();
    if !boot {
        r.push(KEYBOARD_REPORT_ID);
    }
    r.extend_from_slice(&[modifiers, 0, usage, 0, 0, 0, 0, 0]);
    r
}

pub fn mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8, boot: bool) -> Vec<u8> {
    let mut r = Vec::new();
    if !boot {
        r.push(MOUSE_REPORT_ID);
    }
    r.extend_from_slice(&[buttons, dx as u8, dy as u8]);
    if !boot {
        r.push(wheel as u8);
    }
    r
}
