//! Gamepad report layouts and the HID and X-input decoders
//! (`docs/usb-gamepad-plan.md` G0).

use std::vec::Vec;

use super::golden::{PAD_REPORT, PAD_REPORT_4WAY, PAD_REPORT_IDS, TABLET_REPORT};
use crate::report::gamepad::{parse_gamepad, Decoder, PadOut, HAT_CENTRED};
use crate::report::usage;
use crate::xinput;
use crate::Error;

#[test]
fn generic_pad_layout_places_every_axis_and_button() {
    let pad = parse_gamepad(&PAD_REPORT).unwrap();
    assert_eq!(pad.report_id, None);
    // Buttons 1..=12 at bits 0..=11; 13 is not in the descriptor.
    assert_eq!(pad.buttons[0].unwrap().bit, 0);
    assert_eq!(pad.buttons[11].unwrap().bit, 11);
    assert!(pad.buttons[12].is_none());
    // The hat is bit 16, the sticks 8 bits each from bit 24, the triggers
    // last: Z at bit 56, Rz at bit 64.
    let x = pad.axes[0].unwrap();
    assert_eq!(
        (x.bit, x.bits, x.logical_min, x.logical_max, x.relative),
        (24, 8, -127, 127, false)
    );
    assert_eq!(pad.axes[1].unwrap().bit, 32); // Y
    assert_eq!(pad.axes[2].unwrap().bit, 56); // Z
    assert_eq!(pad.axes[2].unwrap().logical_min, 0); // a trigger is unsigned
    assert_eq!(pad.axes[3].unwrap().bit, 40); // Rx
    assert_eq!(pad.axes[4].unwrap().bit, 48); // Ry
    assert_eq!(pad.axes[5].unwrap().bit, 64); // Rz
    assert!(pad.axes[6].is_none() && pad.axes[7].is_none()); // slider, dial
    assert_eq!(pad.axes[8].unwrap().bit, 16); // hat
    assert!(pad.hat());
    // Reading a report: buttons 1 and 2, hat east, X far left, Y far right,
    // Rx centred, Ry far left, left trigger full, right trigger at rest.
    let report = [0b0000_0011, 0x00, 0x02, 0x81, 0x7f, 0x00, 0x81, 0xff, 0x00];
    let read = pad.read(&report).unwrap();
    assert_eq!(read.buttons, 0b11);
    assert_eq!(
        (
            read.axes[0],
            read.axes[1],
            read.axes[3],
            read.axes[4],
            read.axes[8]
        ),
        (-127, 127, 0, -127, 2)
    );
    // A report too short for the last field is refused whole.
    assert_eq!(pad.read(&report[..8]), None);
}

#[test]
fn pad_decoder_emits_axes_then_edges_and_releases_to_rest() {
    let mut decoder = Decoder::new(parse_gamepad(&PAD_REPORT).unwrap());
    let mut out = Vec::new();
    let report = [0b0000_0011, 0x00, 0x02, 0x81, 0x7f, 0x00, 0x81, 0xff, 0x00];
    assert!(decoder.feed(&report, |o| out.push(o)));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: -32768
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 32767
            },
            PadOut::Axis {
                usage: usage::Z,
                value: 32767
            },
            // The centre of a `-127..=127` range lands one step off zero;
            // `inputd`'s deadzone is what absorbs it.
            PadOut::Axis {
                usage: usage::RX,
                value: -1
            },
            PadOut::Axis {
                usage: usage::RY,
                value: -32768
            },
            PadOut::Axis {
                usage: usage::RZ,
                value: 0
            },
            PadOut::Axis {
                usage: usage::HAT,
                value: 2
            },
            PadOut::Button {
                usage: 1,
                pressed: true
            },
            PadOut::Button {
                usage: 2,
                pressed: true
            },
        ]
    );
    // The same state again emits nothing (a report is state, not events).
    out.clear();
    assert!(decoder.feed(&report, |o| out.push(o)));
    assert!(out.is_empty());
    // Only X moved; buttons 1 and 2 up, 3 down.
    out.clear();
    let moved = [0b0000_0100, 0x00, 0x02, 0x00, 0x7f, 0x00, 0x81, 0xff, 0x00];
    decoder.feed(&moved, |o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: -1
            },
            PadOut::Button {
                usage: 1,
                pressed: false
            },
            PadOut::Button {
                usage: 2,
                pressed: false
            },
            PadOut::Button {
                usage: 3,
                pressed: true
            },
        ]
    );
    // Unplugging returns every axis that was deflected to its rest value and
    // releases the held button; a second release emits nothing.
    out.clear();
    decoder.release_all(|o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: 0
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 0
            },
            PadOut::Axis {
                usage: usage::Z,
                value: 0
            },
            PadOut::Axis {
                usage: usage::RX,
                value: 0
            },
            PadOut::Axis {
                usage: usage::RY,
                value: 0
            },
            PadOut::Axis {
                usage: usage::HAT,
                value: HAT_CENTRED
            },
            PadOut::Button {
                usage: 3,
                pressed: false
            },
        ]
    );
    out.clear();
    decoder.release_all(|o| out.push(o));
    assert!(out.is_empty());
}

#[test]
fn a_four_way_hat_spreads_over_eight_directions_and_nulls_centred() {
    let pad = parse_gamepad(&PAD_REPORT_4WAY).unwrap();
    assert!(pad.hat());
    let mut decoder = Decoder::new(pad);
    let mut hats = Vec::new();
    let mut feed = |report: &[u8]| {
        hats.clear();
        assert!(decoder.feed(report, |out| {
            if let PadOut::Axis {
                usage: usage::HAT,
                value,
            } = out
            {
                hats.push(value)
            }
        }));
        hats.clone()
    };
    // The four directions spread over 0, 2, 4, 6; the null state (4) and any
    // value outside the logical range centre the hat.
    assert_eq!(feed(&[0, 0, 0x00]), [0]);
    assert_eq!(feed(&[0, 0, 0x01]), [2]);
    assert_eq!(feed(&[0, 0, 0x02]), [4]);
    assert_eq!(feed(&[0, 0, 0x03]), [6]);
    assert_eq!(feed(&[0, 0, 0x04]), [HAT_CENTRED]);
    assert_eq!(feed(&[0, 0, 0x00]), [0], "moving away emits again");
    assert_eq!(feed(&[0, 0, 0x0f]), [HAT_CENTRED], "past the range is null");
}

#[test]
fn report_ids_select_the_pad() {
    let pad = parse_gamepad(&PAD_REPORT_IDS).unwrap();
    assert_eq!(pad.report_id, Some(2));
    assert_eq!(pad.buttons[3].unwrap().bit, 3);
    assert_eq!(pad.axes[0].unwrap().bit, 8, "offsets count from report 2");
    let report = [2, 0b0011, 0x00, 0x80, 0xff, 0x7f];
    let read = pad.read(&report).unwrap();
    assert_eq!(
        (read.axes[0], read.axes[1], read.buttons),
        (-32768, 32767, 3)
    );
    assert_eq!(pad.read(&[1, 0, 0]), None, "report 1 is the vendor blob");
    assert_eq!(pad.read(&[2, 0, 0, 0]), None, "short");
    // The decoder turns the id-checked report into edges.
    let mut decoder = Decoder::new(pad);
    let mut out = Vec::new();
    assert!(decoder.feed(&report, |o| out.push(o)));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: -32768
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 32767
            },
            PadOut::Button {
                usage: 1,
                pressed: true
            },
            PadOut::Button {
                usage: 2,
                pressed: true
            },
        ]
    );
    assert!(!decoder.feed(&[1, 0, 0, 0], |_| panic!("not the pad's report")));
}

#[test]
fn hostile_pad_descriptors_are_refused_or_bounded() {
    // A tablet is not a gamepad: its X/Y sit in a Mouse collection.
    assert_eq!(parse_gamepad(&TABLET_REPORT), Err(Error::WrongType));
    // No collection at all.
    let bare = [
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    ];
    assert_eq!(parse_gamepad(&bare), Err(Error::WrongType));
    // A pad collection with X but no Y.
    let mut half = PAD_REPORT.to_vec();
    half[51] = 0x30; // Usage (X) again, where Y was
    assert_eq!(parse_gamepad(&half), Err(Error::WrongType));
    // Relative axes are not placed: the bus's AXIS is absolute.
    let mut relative = PAD_REPORT.to_vec();
    relative[65] = 0x06; // Input (Data, Variable, Relative) for the sticks
    assert_eq!(parse_gamepad(&relative), Err(Error::WrongType));
    // Truncated items and a field past the 64-byte report.
    assert_eq!(parse_gamepad(&[0x26, 0xff]), Err(Error::Short));
    // A well-formed prefix that never reaches the axes is not a pad; a cut
    // inside an item is refused as short.
    assert_eq!(parse_gamepad(&PAD_REPORT[..40]), Err(Error::WrongType));
    assert_eq!(parse_gamepad(&PAD_REPORT[..41]), Err(Error::Short));
    // A descriptor that never closes its collection is still parsed.
    let open = &PAD_REPORT[..PAD_REPORT.len() - 1];
    assert!(parse_gamepad(open).is_ok());
}

#[test]
fn deeply_nested_collections_keep_the_application() {
    // Five nested collections around the pad's axes: the outermost is the
    // application collection, and nesting past the tracked depth does not
    // unbalance it.
    let mut descriptor = std::vec![
        0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, // Game Pad application
        0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, // Pointer
        0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, // Pointer
        0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, // Pointer
        0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, // Pointer, past the table
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x02, // X, Y
    ];
    descriptor.extend_from_slice(&[0xc0; 5]);
    let pad = parse_gamepad(&descriptor).unwrap();
    assert_eq!(pad.axes[0].unwrap().bit, 0);
    assert_eq!(pad.axes[1].unwrap().bit, 8);
}

#[test]
fn xinput_reports_become_canonical_axes_and_buttons() {
    let report = |buttons: u16, lt: u8, rt: u8, lx: i16, ly: i16, rx: i16, ry: i16| {
        let mut out = [0u8; 20];
        out[0..2].copy_from_slice(&buttons.to_le_bytes());
        out[2] = lt;
        out[3] = rt;
        out[4..6].copy_from_slice(&lx.to_le_bytes());
        out[6..8].copy_from_slice(&ly.to_le_bytes());
        out[8..10].copy_from_slice(&rx.to_le_bytes());
        out[10..12].copy_from_slice(&ry.to_le_bytes());
        out
    };
    let mut decoder = xinput::Decoder::new();
    let mut out = Vec::new();
    let first = report(
        xinput::button::A | xinput::button::UP,
        255,
        0,
        -32768,
        32767,
        0,
        0,
    );
    assert!(decoder.feed(&first, |o| out.push(o)));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: -32768
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 32767
            },
            PadOut::Axis {
                usage: usage::Z,
                value: 32767
            },
            PadOut::Axis {
                usage: usage::RX,
                value: 0
            },
            PadOut::Axis {
                usage: usage::RY,
                value: 0
            },
            PadOut::Axis {
                usage: usage::RZ,
                value: 0
            },
            PadOut::Axis {
                usage: usage::HAT,
                value: 0
            },
            PadOut::Button {
                usage: 1,
                pressed: true
            },
        ]
    );
    out.clear();
    assert!(decoder.feed(&first, |o| out.push(o)));
    assert!(out.is_empty(), "the same state emits nothing");
    // D-pad up+right, A up, B down, the left trigger released.
    out.clear();
    let second = report(
        xinput::button::UP | xinput::button::RIGHT | xinput::button::B,
        0,
        0,
        -32768,
        32767,
        0,
        0,
    );
    decoder.feed(&second, |o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::Z,
                value: 0
            },
            PadOut::Axis {
                usage: usage::HAT,
                value: 1
            },
            PadOut::Button {
                usage: 1,
                pressed: false
            },
            PadOut::Button {
                usage: 2,
                pressed: true
            },
        ]
    );
    // An opposed d-pad pair centres the hat and releases B; a short report
    // changes nothing.
    out.clear();
    let opposed = report(
        xinput::button::UP | xinput::button::DOWN,
        0,
        0,
        -32768,
        32767,
        0,
        0,
    );
    decoder.feed(&opposed, |o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::HAT,
                value: HAT_CENTRED
            },
            PadOut::Button {
                usage: 2,
                pressed: false
            },
        ]
    );
    assert!(!decoder.feed(&opposed[..11], |_| panic!("short reports emit nothing")));
    // Unplugging centres the sticks; nothing else is left held.
    out.clear();
    decoder.release_all(|o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::X,
                value: 0
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 0
            },
        ]
    );
}
