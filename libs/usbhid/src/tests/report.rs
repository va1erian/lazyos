//! Report descriptors: QEMU's tablet and mouse, report ids, Push/Pop,
//! hostile and truncated streams.

use std::vec::Vec;

use super::golden::{MOUSE_REPORT, TABLET_REPORT, TABLET_REPORT_3};
use crate::boot::MouseOut;
use crate::report::{parse_pointer, Decoder, Field, Out, PointerReport};
use crate::Error;

#[test]
fn qemu_tablet_layout() {
    let tablet = parse_pointer(&TABLET_REPORT).unwrap();
    assert!(tablet.absolute());
    assert_eq!(tablet.report_id, None);
    let x = tablet.x.unwrap();
    assert_eq!(
        (x.bit, x.bits, x.logical_min, x.logical_max, x.relative),
        (8, 16, 0, 0x7fff, false)
    );
    assert_eq!(tablet.y.unwrap().bit, 24);
    let wheel = tablet.wheel.unwrap();
    assert_eq!((wheel.bit, wheel.bits, wheel.relative), (40, 8, true));
    for (n, button) in tablet.buttons.iter().take(5).enumerate() {
        assert_eq!(button.unwrap().bit, n as u32);
    }
    assert!(tablet.buttons[5].is_none());
    // Left button, x = 0x4000 (mid-scale), y = 0x7fff, wheel -1.
    let report = [0x01, 0x00, 0x40, 0xff, 0x7f, 0xff];
    assert_eq!(
        tablet.read(&report),
        Some(PointerReport {
            x: 0x4000,
            y: 0x7fff,
            wheel: -1,
            buttons: 1
        })
    );
    assert_eq!(x.normalize(0x4000), 0x8000);
    assert_eq!(x.normalize(0x7fff), 0xffff);
    assert_eq!(x.normalize(-5), 0, "below the range clamps");
    assert_eq!(x.normalize(0x9000), 0xffff, "above the range clamps");
    assert_eq!(tablet.read(&report[..5]), None, "a short report is refused");
}

#[test]
fn qemu_mouse_is_relative() {
    let mouse = parse_pointer(&MOUSE_REPORT).unwrap();
    assert!(!mouse.absolute());
    let report = mouse.read(&[0x06, 0xfe, 0x05, 0x01]).unwrap();
    assert_eq!(
        report,
        PointerReport {
            x: -2,
            y: 5,
            wheel: 1,
            buttons: 0b110
        }
    );
}

/// A two-report device: report 1 is a keyboard-ish item, report 2 the
/// pointer. Fields are placed per report id, and the id byte is checked.
#[test]
fn report_ids_select_the_pointer() {
    let descriptor = [
        0x85, 0x01, // Report ID 1
        0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
        0x02, // eight modifier bits (not a pointer)
        0x85, 0x02, // Report ID 2
        0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, // 3 buttons
        0x95, 0x01, 0x75, 0x05, 0x81, 0x03, // padding
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x06, // relative X, Y
    ];
    let pointer = parse_pointer(&descriptor).unwrap();
    assert_eq!(pointer.report_id, Some(2));
    assert_eq!(
        pointer.x.unwrap().bit,
        8,
        "offsets count from report 2's own start"
    );
    assert_eq!(
        pointer.read(&[2, 0b001, 3, 0xfd]).unwrap(),
        PointerReport {
            x: 3,
            y: -3,
            wheel: 0,
            buttons: 1
        }
    );
    assert_eq!(
        pointer.read(&[1, 0xff, 3, 3]),
        None,
        "another report id is not the pointer's"
    );
    assert_eq!(pointer.read(&[]), None);
}

/// Push and Pop restore the global state; long items and unknown tags are
/// skipped; four-byte usages carry their own page.
#[test]
fn push_pop_long_items_and_extended_usages() {
    let descriptor = [
        0x05, 0x09, // Usage Page (Button)
        0xa4, // Push
        0x05, 0x01, 0x15, 0x00, 0x26, 0xff, 0x0f, 0x75, 0x0c, 0x95, 0x01, 0x0b, 0x30, 0x00, 0x01,
        0x00, // Usage (Generic Desktop: X), four bytes
        0x81, 0x02, 0xfe, 0x02, 0x10, 0xaa, 0xbb, // a long item, skipped
        0x0b, 0x31, 0x00, 0x01, 0x00, 0x81, 0x02, // Y
        0xb4, // Pop: back to the Button page, no size or count
        0x09, 0x01, 0x75, 0x01, 0x95, 0x01, 0x25, 0x01, 0x81, 0x02, // button 1
    ];
    let pointer = parse_pointer(&descriptor).unwrap();
    assert_eq!(pointer.x.unwrap().bits, 12);
    assert_eq!(pointer.y.unwrap().bit, 12);
    assert_eq!(pointer.buttons[0].unwrap().bit, 24);
    assert!(pointer.absolute());
}

#[test]
fn hostile_descriptors_are_refused_or_bounded() {
    // No X/Y at all, or only one of them.
    assert_eq!(parse_pointer(&[]), Err(Error::WrongType));
    assert_eq!(
        parse_pointer(&[0x05, 0x01, 0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02]),
        Err(Error::WrongType)
    );
    // Truncated items and long items.
    assert_eq!(parse_pointer(&[0x26, 0xff]), Err(Error::Short));
    assert_eq!(parse_pointer(&[0xfe]), Err(Error::Short));
    // A field past the 64-byte report is never placed.
    let far = [
        0x05, 0x01, 0x75, 0x20, 0x95, 0x10, 0x81, 0x01, // 64 bytes of padding
        0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    ];
    assert_eq!(parse_pointer(&far), Err(Error::WrongType));
    // Arrays, constants and absurd sizes carry no pointer value.
    for flags in [0x00, 0x01, 0x03] {
        let item = [
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, flags,
        ];
        assert_eq!(
            parse_pointer(&item),
            Err(Error::WrongType),
            "flags {flags:#x}"
        );
    }
    let huge = [
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x75, 0x40, 0x95, 0x02, 0x81, 0x02,
    ];
    assert_eq!(parse_pointer(&huge), Err(Error::WrongType));
    // Deep Push nesting and unbalanced Pops do not overflow.
    let mut nested: Vec<u8> = std::vec![0xa4; 64];
    nested.extend_from_slice(&[0xb4; 80]);
    nested.extend_from_slice(&TABLET_REPORT);
    assert!(parse_pointer(&nested).unwrap().absolute());
}

#[test]
fn fields_sign_extend_and_read_unaligned() {
    let field = Field {
        bit: 3,
        bits: 7,
        logical_min: -64,
        logical_max: 63,
        relative: true,
    };
    // Bits 3..10 = 0b1111111 = -1.
    assert_eq!(field.read(&[0b1111_1000, 0b0000_0011]), Some(-1));
    assert_eq!(field.read(&[0b0000_1000, 0b0000_0000]), Some(1));
    assert_eq!(field.read(&[0xff]), None, "a field past the data");
    let unsigned = Field {
        bit: 0,
        bits: 32,
        logical_min: 0,
        logical_max: i32::MAX,
        relative: false,
    };
    assert_eq!(unsigned.read(&[0xff, 0xff, 0xff, 0x7f]), Some(i32::MAX));
    let empty = Field {
        bit: 0,
        bits: 8,
        logical_min: 5,
        logical_max: 5,
        relative: false,
    };
    assert_eq!(
        empty.normalize(5),
        0,
        "an empty range does not divide by zero"
    );
}

fn outs(decoder: &mut Decoder, report: &[u8]) -> Vec<Out> {
    let mut out = Vec::new();
    assert!(
        decoder.feed(report, |o| out.push(o)),
        "report refused: {report:x?}"
    );
    out
}

#[test]
fn tablet_decoder_emits_position_wheel_then_edges() {
    let mut tablet = Decoder::new(parse_pointer(&TABLET_REPORT).unwrap());
    assert!(tablet.absolute());
    // Mid-screen, left down, one notch up.
    assert_eq!(
        outs(&mut tablet, &[0x01, 0xff, 0x3f, 0xff, 0x3f, 0x01]),
        [
            Out::Position {
                x: 0x7ffe,
                y: 0x7ffe
            },
            Out::Mouse(MouseOut::Wheel(1)),
            Out::Mouse(MouseOut::Button {
                usage: 1,
                pressed: true
            }),
        ]
    );
    // Same place again: no position record; left up, right down.
    assert_eq!(
        outs(&mut tablet, &[0x02, 0xff, 0x3f, 0xff, 0x3f, 0x00]),
        [
            Out::Mouse(MouseOut::Button {
                usage: 1,
                pressed: false
            }),
            Out::Mouse(MouseOut::Button {
                usage: 2,
                pressed: true
            }),
        ]
    );
    // The corner and the far corner.
    assert_eq!(
        outs(&mut tablet, &[0x02, 0, 0, 0, 0, 0]),
        [Out::Position { x: 0, y: 0 }]
    );
    assert_eq!(
        outs(&mut tablet, &[0x02, 0xff, 0x7f, 0xff, 0x7f, 0]),
        [Out::Position {
            x: 0xffff,
            y: 0xffff
        }]
    );
    let mut released = Vec::new();
    tablet.release_all(|o| released.push(o));
    assert_eq!(
        released,
        [Out::Mouse(MouseOut::Button {
            usage: 2,
            pressed: false
        })]
    );
    assert!(!tablet.feed(&[0x01], |_| panic!("a short report emits nothing")));
}

#[test]
fn report_mouse_decoder_is_relative() {
    let mut mouse = Decoder::new(parse_pointer(&MOUSE_REPORT).unwrap());
    assert!(!mouse.absolute());
    assert_eq!(
        outs(&mut mouse, &[0x00, 0x05, 0xfb, 0x00]),
        [Out::Mouse(MouseOut::Motion { dx: 5, dy: -5 })]
    );
    assert_eq!(outs(&mut mouse, &[0x00, 0, 0, 0]), []);
}

#[test]
fn older_qemu_tablet_has_three_buttons() {
    let tablet = parse_pointer(&TABLET_REPORT_3).unwrap();
    assert!(tablet.absolute());
    assert!(tablet.buttons[2].is_some() && tablet.buttons[3].is_none());
    assert_eq!(
        tablet.x.unwrap().bit,
        8,
        "the padding still ends the first byte"
    );
    let report = tablet.read(&[0x04, 0x00, 0x40, 0x00, 0x20, 0x00]).unwrap();
    assert_eq!(
        report,
        PointerReport {
            x: 0x4000,
            y: 0x2000,
            wheel: 0,
            buttons: 0b100
        }
    );
}

/// A wireless mouse's descriptor (the shape of Logitech's receivers, issue
/// "scroll wheel dead on real hardware"): report id 1, eight buttons, 12-bit
/// relative X and Y, an 8-bit wheel. It sits on a *boot* mouse interface, whose
/// 3-byte boot report has no wheel byte, so `usbd` must run it in report
/// protocol: `has_wheel` is what tells it to.
const WIRELESS_MOUSE_REPORT: [u8; 60] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x08, 0x15, 0x00, 0x25, 0x01, 0x95, 0x08, 0x75, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30,
    0x09, 0x31, 0x16, 0x01, 0xf8, 0x26, 0xff, 0x07, 0x75, 0x0c, 0x95, 0x02, 0x81, 0x06, 0x09, 0x38,
    0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
];

#[test]
fn wireless_mouse_wheel_is_in_the_report_layout() {
    let mouse = parse_pointer(&WIRELESS_MOUSE_REPORT).unwrap();
    assert!(mouse.has_wheel());
    assert!(!mouse.absolute());
    assert_eq!(mouse.report_id, Some(1));
    // id 1, left button, x = 5, y = -1 (12 bits each), wheel +1.
    let report = mouse.read(&[0x01, 0x01, 0x05, 0xf0, 0xff, 0x01]).unwrap();
    assert_eq!(
        report,
        PointerReport {
            x: 5,
            y: -1,
            wheel: 1,
            buttons: 1
        }
    );
    let mut decoder = Decoder::new(mouse);
    let mut outs = Vec::new();
    assert!(decoder.feed(&[0x01, 0x00, 0x00, 0x00, 0x00, 0xff], |o| outs.push(o)));
    assert_eq!(outs, [Out::Mouse(MouseOut::Wheel(-1))]);
}

#[test]
fn has_wheel_is_false_without_a_wheel_or_for_a_tablet() {
    // Boot-style 3-byte layout: buttons, X, Y and no wheel usage.
    let mut plain = MOUSE_REPORT;
    plain[38] = 0x09; // Usage (0x31 again) instead of the wheel's 0x38
    plain[39] = 0x31;
    plain[47] = 0x02; // Report Count (2)
    let layout = parse_pointer(&plain).unwrap();
    assert!(layout.wheel.is_none() && !layout.has_wheel());
    assert!(!parse_pointer(&TABLET_REPORT).unwrap().has_wheel());
    assert!(parse_pointer(&MOUSE_REPORT).unwrap().has_wheel());
}
