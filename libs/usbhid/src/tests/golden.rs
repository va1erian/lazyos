//! Descriptors of QEMU's `usb-kbd`, `usb-mouse` and `usb-tablet` as they
//! appear behind `qemu-xhci` (the high-speed variants: `desc_device_*2` in
//! QEMU's `hw/usb/dev-hid.c`), laid out byte for byte as `hw/usb/desc.c`
//! serialises them. U2 cross-checks them against what the device returns.

/// `usb-kbd` device descriptor (`idVendor` 0x0627, strings 1/4/11).
pub const KBD_DEVICE: [u8; 18] = [
    18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0x00, 0x00, 1, 4, 11, 1,
];

/// `usb-kbd` configuration: config, boot keyboard interface, HID (report
/// descriptor 63 bytes), interrupt IN endpoint 1 (8 bytes, interval 7).
pub const KBD_CONFIG: [u8; 34] = [
    9, 2, 34, 0, 1, 1, 8, 0xA0, 50, //
    9, 4, 0, 0, 1, 3, 1, 1, 0, //
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x3F, 0, //
    7, 5, 0x81, 3, 8, 0, 7,
];

/// `usb-mouse` configuration: boot mouse, report descriptor 52 bytes,
/// 4-byte reports.
pub const MOUSE_CONFIG: [u8; 34] = [
    9, 2, 34, 0, 1, 1, 6, 0xA0, 50, //
    9, 4, 0, 0, 1, 3, 1, 2, 0, //
    9, 0x21, 0x01, 0x00, 0, 1, 0x22, 52, 0, //
    7, 5, 0x81, 3, 4, 0, 7,
];

/// `usb-tablet` configuration: no boot protocol, report descriptor 74 bytes,
/// 8-byte reports, interval 4.
pub const TABLET_CONFIG: [u8; 34] = [
    9, 2, 34, 0, 1, 1, 7, 0xA0, 50, //
    9, 4, 0, 0, 1, 3, 0, 0, 0, //
    9, 0x21, 0x01, 0x00, 0, 1, 0x22, 74, 0, //
    7, 5, 0x81, 3, 8, 0, 4,
];

/// QEMU `usb-tablet` report descriptor (`hw/usb/dev-hid.c`): five buttons,
/// three bits of padding, 16-bit absolute X and Y over `0..=0x7fff`, a signed
/// relative wheel.
pub(crate) const TABLET_REPORT: [u8; 74] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00, 0x46, 0xff, 0x7f,
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x35, 0x00,
    0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
];

/// QEMU `usb-mouse` report descriptor: the same buttons, then relative
/// 8-bit X, Y and wheel in one item.
pub(crate) const MOUSE_REPORT: [u8; 52] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0xc0, 0xc0,
];

/// The same tablet as older QEMU releases (and distribution builds) describe
/// it: three buttons and five bits of padding.
pub(crate) const TABLET_REPORT_3: [u8; 74] = {
    let mut bytes = TABLET_REPORT;
    bytes[15] = 0x03; // Usage Maximum (3)
    bytes[21] = 0x03; // Report Count (3)
    bytes[29] = 0x05; // Report Size (5) of the padding
    bytes
};

/// A generic two-stick gamepad (`docs/usb-gamepad-plan.md` G0): a Game Pad
/// application collection, 12 buttons, an 8-way hat, X/Y/Rx/Ry over
/// `-127..=127` and Z/Rz triggers over `0..=255`. Nine bytes per report:
/// buttons (12 bits) and padding, hat and padding, the four sticks, the two
/// triggers.
pub(crate) const PAD_REPORT: [u8; 82] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x09, //   Usage Page (Button)
    0x19, 0x01, //   Usage Minimum (1)
    0x29, 0x0c, //   Usage Maximum (12)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x0c, //   Report Count (12)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x95, 0x01, //   Report Count (1)
    0x75, 0x04, //   Report Size (4)
    0x81, 0x03, //   Input (Constant): padding
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x39, //   Usage (Hat switch)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x07, //   Logical Maximum (7)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x42, //   Input (Data, Variable, Absolute, Null State)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x03, //   Input (Constant): padding
    0x09, 0x30, //   Usage (X)
    0x09, 0x31, //   Usage (Y)
    0x09, 0x33, //   Usage (Rx)
    0x09, 0x34, //   Usage (Ry)
    0x15, 0x81, //   Logical Minimum (-127)
    0x25, 0x7f, //   Logical Maximum (127)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x04, //   Report Count (4)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x32, //   Usage (Z)
    0x09, 0x35, //   Usage (Rz)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x02, //   Report Count (2)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0xc0, // End Collection
];

/// The same pad with report ids: report 2 carries the pad, report 1 a vendor
/// blob. Four buttons (4 bits, padded to a byte), then 16-bit X and Y over
/// the full signed range: five bytes per report 2.
pub(crate) const PAD_REPORT_IDS: [u8; 62] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
    0x85, 0x02, //   Report ID (2)
    0x05, 0x09, //   Usage Page (Button)
    0x19, 0x01, //   Usage Minimum (1)
    0x29, 0x04, //   Usage Maximum (4)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x04, //   Report Count (4)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x03, //   Input (Constant): padding
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x30, //   Usage (X)
    0x09, 0x31, //   Usage (Y)
    0x16, 0x00, 0x80, //   Logical Minimum (-32768)
    0x26, 0xff, 0x7f, //   Logical Maximum (32767)
    0x75, 0x10, //   Report Size (16)
    0x95, 0x02, //   Report Count (2)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x85, 0x01, //   Report ID (1)
    0x06, 0x00, 0xff, //   Usage Page (Vendor-Defined)
    0x09, 0x01, //   Usage (Vendor 1)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x02, //   Report Count (2)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0xc0, // End Collection
];

/// A four-way hat pad (0..=3, null 4) with 8-bit X and Y: the hat spreads
/// over the same eight directions, and an out-of-range value is centred.
pub(crate) const PAD_REPORT_4WAY: [u8; 42] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x04, // Usage (Joystick)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x30, //   Usage (X)
    0x09, 0x31, //   Usage (Y)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x02, //   Report Count (2)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x09, 0x39, //   Usage (Hat switch)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x03, //   Logical Maximum (3)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x42, //   Input (Data, Variable, Absolute, Null State)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x03, //   Input (Constant): padding
    0xc0, // End Collection
];
