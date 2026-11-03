//! What HID over GATT discovery reports: the service, its reports, the
//! events and steps of the driver.

use alloc::vec::Vec;

use crate::Error;

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
        self.reports
            .iter()
            .any(|r| r.kind == kind::BOOT_MOUSE_INPUT)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HogpEvent {
    Discovered(HidService),
    /// Every CCCD write was answered: the device will notify.
    Ready,
    Notification {
        handle: u16,
        value: Vec<u8>,
    },
    /// A notification that arrived before any CCCD write was answered: the
    /// device cannot have been asked for it, so it is not reported as input.
    Unsolicited {
        handle: u16,
    },
    Failed(Error),
}

/// PDUs to send, in order, and what happened.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub send: Vec<Vec<u8>>,
    pub events: Vec<HogpEvent>,
}
