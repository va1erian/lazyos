//! HIDP messages (HID profile 1.1.1, section 7): one header octet, the
//! transaction type in the high nibble and a parameter in the low one, then
//! the payload.

use alloc::vec::Vec;

use crate::Error;

/// Transaction types.
mod ty {
    pub const HANDSHAKE: u8 = 0x0;
    pub const HID_CONTROL: u8 = 0x1;
    pub const SET_PROTOCOL: u8 = 0x7;
    pub const DATA: u8 = 0xA;
}

/// `DATA` report types (the parameter).
pub mod report_type {
    pub const INPUT: u8 = 1;
    pub const OUTPUT: u8 = 2;
}

/// `HID_CONTROL` operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Nop,
    HardReset,
    SoftReset,
    Suspend,
    ExitSuspend,
    /// The device is unpairing itself: forget it.
    VirtualCableUnplug,
}

/// A handshake result: a device answers a control request with one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handshake {
    Success,
    NotReady,
    InvalidReportId,
    Unsupported,
    InvalidParameter,
    Unknown,
    Fatal,
}

/// What a device sent on the control channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromControl<'a> {
    Handshake(Handshake),
    Control(Control),
    /// A `DATA` message answering a request (a report); the type is the
    /// parameter.
    Data {
        report_type: u8,
        data: &'a [u8],
    },
}

/// Read a message from the control channel. A device only answers on it, so
/// requests (`GET_*`, `SET_*`) are [`Error::Unexpected`].
pub fn parse_control(payload: &[u8]) -> Result<FromControl<'_>, Error> {
    let (&head, rest) = payload.split_first().ok_or(Error::Truncated)?;
    let param = head & 0x0F;
    match head >> 4 {
        ty::HANDSHAKE => Ok(FromControl::Handshake(match param {
            0 => Handshake::Success,
            1 => Handshake::NotReady,
            2 => Handshake::InvalidReportId,
            3 => Handshake::Unsupported,
            4 => Handshake::InvalidParameter,
            0xF => Handshake::Fatal,
            _ => Handshake::Unknown,
        })),
        ty::HID_CONTROL => Ok(FromControl::Control(match param {
            0 => Control::Nop,
            1 => Control::HardReset,
            2 => Control::SoftReset,
            3 => Control::Suspend,
            4 => Control::ExitSuspend,
            5 => Control::VirtualCableUnplug,
            _ => return Err(Error::Unexpected),
        })),
        ty::DATA => Ok(FromControl::Data {
            report_type: param,
            data: rest,
        }),
        // Requests (`GET_*`, `SET_*`) are the host's to send, and the
        // remaining types are not defined for a device here.
        _ => Err(Error::Unexpected),
    }
}

/// Read a message from the interrupt channel: a device sends input reports
/// (`DATA` with type input) and nothing else here. Returns the report.
pub fn parse_interrupt(payload: &[u8]) -> Result<&[u8], Error> {
    let (&head, rest) = payload.split_first().ok_or(Error::Truncated)?;
    if head >> 4 == ty::DATA && head & 0x0F == report_type::INPUT {
        Ok(rest)
    } else {
        Err(Error::Unexpected)
    }
}

/// `SET_PROTOCOL`: the boot protocol, or the report protocol.
pub fn set_protocol(boot: bool) -> [u8; 1] {
    [ty::SET_PROTOCOL << 4 | u8::from(!boot)]
}

/// `HID_CONTROL` with `op`.
pub fn hid_control(op: Control) -> [u8; 1] {
    let param = match op {
        Control::Nop => 0,
        Control::HardReset => 1,
        Control::SoftReset => 2,
        Control::Suspend => 3,
        Control::ExitSuspend => 4,
        Control::VirtualCableUnplug => 5,
    };
    [ty::HID_CONTROL << 4 | param]
}

/// An output report (keyboard LEDs) for the interrupt channel.
pub fn output_report(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + data.len());
    out.push(ty::DATA << 4 | report_type::OUTPUT);
    out.extend_from_slice(data);
    out
}
