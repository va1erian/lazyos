//! Report-protocol keyboards: the boot layout behind an optional report id.

use std::vec;
use std::vec::Vec;

use crate::boot::KeyEdge;
use crate::report::keyboard::{parse_keyboard, Decoder, Keyboard};
use crate::Error;

/// The USB spec's boot keyboard descriptor (HID 1.11 appendix B.1): no id.
const BOOT_KEYBOARD: [u8; 63] = [
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// The same keyboard with `Report ID (1)` after the collection opens.
fn with_id(id: u8) -> Vec<u8> {
    let mut d = BOOT_KEYBOARD[..6].to_vec();
    d.extend_from_slice(&[0x85, id]);
    d.extend_from_slice(&BOOT_KEYBOARD[6..]);
    d
}

fn edges(decoder: &mut Decoder, report: &[u8]) -> (bool, Vec<(u16, bool)>) {
    let mut out = Vec::new();
    let took = decoder.feed(report, |KeyEdge { usage, pressed }| {
        out.push((usage, pressed))
    });
    (took, out)
}

#[test]
fn a_boot_layout_keyboard_has_no_id() {
    assert_eq!(
        parse_keyboard(&BOOT_KEYBOARD),
        Ok(Keyboard { report_id: None })
    );
}

#[test]
fn an_id_is_stripped_and_another_id_is_not_ours() {
    let layout = parse_keyboard(&with_id(1)).unwrap();
    assert_eq!(layout.report_id, Some(1));
    let mut d = Decoder::new(layout);
    let (took, e) = edges(&mut d, &[1, 0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    assert!(took);
    assert_eq!(e, [(0x04, true), (0xE1, true)]);
    assert_eq!(edges(&mut d, &[2, 0, 0, 0, 0, 0, 0, 0, 0]), (false, vec![]));
    assert_eq!(edges(&mut d, &[]), (false, vec![]));
    let (_, e) = edges(&mut d, &[1, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(e, [(0x04, false), (0xE1, false)]);
}

#[test]
fn release_all_lets_go_of_held_keys() {
    let mut d = Decoder::new(parse_keyboard(&BOOT_KEYBOARD).unwrap());
    edges(&mut d, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]);
    let mut out = Vec::new();
    d.release_all(|e| out.push((e.usage, e.pressed)));
    assert_eq!(out, [(0x04, false), (0x05, false)]);
    assert!(!d.is_held(4));
}

#[test]
fn a_descriptor_that_is_not_a_boot_layout_is_refused() {
    // A mouse is not a keyboard.
    let mouse = [
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x30, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95,
        0x01, 0x81, 0x06, 0xC0,
    ];
    assert_eq!(parse_keyboard(&mouse), Err(Error::WrongType));
    // Modifiers but no key array.
    assert_eq!(parse_keyboard(&BOOT_KEYBOARD[..24]), Err(Error::WrongType));
    // Truncated mid-item, and empty.
    assert!(parse_keyboard(&BOOT_KEYBOARD[..10]).is_err());
    assert_eq!(parse_keyboard(&[]), Err(Error::WrongType));
    // Four key slots are not a boot layout.
    let mut four = BOOT_KEYBOARD;
    let count = four.windows(2).rposition(|w| w == [0x95, 0x06]).unwrap();
    four[count + 1] = 0x04; // Report Count (6) -> (4)
    assert_eq!(parse_keyboard(&four), Err(Error::WrongType));
}

#[test]
fn rollover_reports_change_nothing() {
    let mut d = Decoder::new(parse_keyboard(&BOOT_KEYBOARD).unwrap());
    edges(&mut d, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    let (took, e) = edges(&mut d, &[0, 0, 1, 1, 1, 1, 1, 1]);
    assert!(took && e.is_empty());
    assert!(d.is_held(4));
}
