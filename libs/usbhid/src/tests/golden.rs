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
