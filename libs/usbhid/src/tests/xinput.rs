//! The fixed X-input report (`xinput::Decoder`): the canonical usages the
//! 20-byte report maps to, change-only output and the release pass.

use std::vec::Vec;

use crate::report::gamepad::{PadOut, HAT_CENTRED};
use crate::report::usage;
use crate::xinput;

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
