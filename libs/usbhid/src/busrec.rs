//! Gamepad output as raw-bus records (`docs/usb-gamepad-plan.md` G2): the
//! pure part of what `usbd` publishes, kept here so the host tests judge it.
//!
//! The kind numbers repeat `lazyos_sys::raw_kind` (this crate has no
//! dependencies); `usbd` asserts at compile time that they agree, so the two
//! cannot drift apart. The mapping is the plan's decision 2:
//!
//! | [`PadOut`] | kind | code | value |
//! |---|---|---|---|
//! | `Axis` | [`AXIS`] | Generic Desktop usage | normalized value |
//! | `Button` | [`BUTTON`] | button usage `1..=32` | `0` release, `1` press |
//! | (once per device) | [`DEVICE`] | vendor id | `product \| version << 16` |

use crate::report::gamepad::PadOut;

/// `raw_kind::BUTTON`.
pub const BUTTON: u8 = 4;
/// `raw_kind::AXIS`.
pub const AXIS: u8 = 8;
/// `raw_kind::DEVICE`.
pub const DEVICE: u8 = 9;

/// One record for the bus: the fields of `lazyos_sys::SourceRecord`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusRecord {
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

/// The bus record for one decoded gamepad change.
pub fn pad_record(out: PadOut) -> BusRecord {
    match out {
        PadOut::Axis { usage, value } => BusRecord {
            kind: AXIS,
            code: usage,
            value,
        },
        PadOut::Button { usage, pressed } => BusRecord {
            kind: BUTTON,
            code: usage,
            value: i32::from(pressed),
        },
    }
}

/// The identity record a driver publishes once after registering a source:
/// `code` is the vendor id, `value` the product id with the `bcdDevice`
/// release in the high half. Diagnostics and a future mapping table key on
/// it; it never decides what a source is.
pub fn device_record(vendor: u16, product: u16, version: u16) -> BusRecord {
    BusRecord {
        kind: DEVICE,
        code: vendor,
        value: (u32::from(product) | u32::from(version) << 16) as i32,
    }
}
