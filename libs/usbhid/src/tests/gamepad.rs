//! Gamepad report layouts and the HID and X-input decoders
//! (`docs/usb-gamepad-plan.md` G0).

use std::vec::Vec;

use super::golden::{PAD_REPORT, PAD_REPORT_4WAY, PAD_REPORT_IDS, TABLET_REPORT};
use crate::report::gamepad::{parse_gamepad, Decoder, PadOut, HAT_CENTRED};
use crate::report::usage;
use crate::report::Field;
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
                value: -32767
            },
            PadOut::Axis {
                usage: usage::Y,
                value: 32767
            },
            PadOut::Axis {
                usage: usage::Z,
                value: 32767
            },
            // The centre of a `-127..=127` range is exactly 0.
            PadOut::Axis {
                usage: usage::RX,
                value: 0
            },
            PadOut::Axis {
                usage: usage::RY,
                value: -32767
            },
            // An unsigned axis is bipolar too: HID cannot say this one is a
            // trigger, so its logical minimum is the far end, not 0.
            PadOut::Axis {
                usage: usage::RZ,
                value: -32767
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
                value: 0
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
    // Unplugging returns every axis that was deflected to its rest value (X
    // and Rx are already there) and releases the held button; a second
    // release emits nothing.
    out.clear();
    decoder.release_all(|o| out.push(o));
    assert_eq!(
        out,
        [
            PadOut::Axis {
                usage: usage::Y,
                value: 0
            },
            PadOut::Axis {
                usage: usage::Z,
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
                value: -32767
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
fn a_full_i32_hat_range_normalizes_without_panicking() {
    // A hostile descriptor can declare a hat over the whole `i32` range:
    // `max - min` must not overflow (debug) or wrap to a zero span (release).
    let descriptor = [
        0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, // Game Pad application
        0x09, 0x30, 0x09, 0x31, // X, Y
        0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, // 0..255
        0x09, 0x39, // Hat switch
        0x17, 0x00, 0x00, 0x00, 0x80, // Logical Minimum (i32::MIN)
        0x27, 0xff, 0xff, 0xff, 0x7f, // Logical Maximum (i32::MAX)
        0x75, 0x20, 0x95, 0x01, 0x81, 0x02, // a 32-bit hat
        0xc0,
    ];
    let pad = parse_gamepad(&descriptor).unwrap();
    assert!(pad.hat());
    let hat = pad.axes[8].unwrap();
    assert_eq!(
        (hat.bits, hat.logical_min, hat.logical_max),
        (32, i32::MIN, i32::MAX)
    );
    assert_eq!(hat.normalize_hat(i32::MIN), 0);
    assert_eq!(hat.normalize_hat(0), 4, "the range's midpoint is south");
    assert_eq!(hat.normalize_hat(i32::MAX), 7);
    // The decoder path takes the same value.
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
    assert_eq!(feed(&[0, 0, 0x00, 0x00, 0x00, 0x00]), [4]);
    assert_eq!(feed(&[0, 0, 0xff, 0xff, 0xff, 0x7f]), [7]);
    assert_eq!(feed(&[0, 0, 0x00, 0x00, 0x00, 0x80]), [0]);
    // A logical maximum below the minimum centres rather than dividing.
    let inverted = Field {
        bit: 0,
        bits: 8,
        logical_min: 10,
        logical_max: 5,
        relative: false,
    };
    assert_eq!(inverted.normalize_hat(7), HAT_CENTRED);
}

#[test]
fn a_later_report_id_does_not_give_the_first_report_one() {
    // A malformed descriptor: the pad's report has no id, a later report
    // does. The pad's reports must not be read as if they carried an id byte.
    let descriptor = [
        0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, // Game Pad application
        0x09, 0x30, 0x09, 0x31, // X, Y (no report id in force)
        0x15, 0x00, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, // 0..127
        0x85, 0x02, // Report ID (2): a later report
        0x09, 0x32, 0x15, 0x00, 0x25, 0xff, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, // Z
        0xc0,
    ];
    let pad = parse_gamepad(&descriptor).unwrap();
    assert_eq!(pad.report_id, None);
    assert_eq!(pad.axes[0].unwrap().bit, 0);
    assert!(pad.axes[2].is_none(), "report 2's Z is not this report's");
    assert!(pad.read(&[0x00, 0x80]).is_some());
}

#[test]
fn every_axis_is_bipolar_around_the_centre_of_its_range() {
    let field = |logical_min, logical_max| Field {
        bit: 0,
        bits: 32,
        logical_min,
        logical_max,
        relative: false,
    };
    // An unsigned stick (most pads) rests mid-range, near 0, not at -32767:
    // unplugging must not pin it to one side.
    let byte = field(0, 255);
    assert_eq!(
        [0, 127, 128, 255].map(|v| byte.normalize_axis(v)),
        [-32767, -128, 128, 32767]
    );
    let word = field(0, 65535);
    assert_eq!(word.normalize_axis(32768), 0);
    assert_eq!(word.normalize_axis(65535), 32767);
    // A symmetric signed range rests at exactly 0.
    let signed = field(-127, 127);
    assert_eq!(
        [-127, 0, 127].map(|v| signed.normalize_axis(v)),
        [-32767, 0, 32767]
    );
    // The full `i16` range, the full `i32` range, out-of-range values and an
    // empty range stay inside `-32767..=32767` without overflowing.
    let short = field(-32768, 32767);
    assert_eq!(short.normalize_axis(-32768), -32767);
    assert_eq!(short.normalize_axis(i32::MAX), 32767);
    let wide = field(i32::MIN, i32::MAX);
    assert_eq!(wide.normalize_axis(i32::MIN), -32767);
    assert_eq!(wide.normalize_axis(i32::MAX), 32767);
    assert_eq!(field(5, 5).normalize_axis(5), 0);
}
