//! The gamepad fuzz entry points (`docs/usb-gamepad-plan.md` G0): any bytes
//! as a HID report descriptor and a report, through the gamepad layout and
//! decoder and the fixed X-input decoder, both checked against what the
//! report they read says.

use std::vec::Vec;

use super::check_field;
use crate::report::gamepad::{parse_gamepad, Decoder as PadDecoder, PadOut, AXES, AXIS_COUNT};
use crate::xinput;

/// Split `data` into a report descriptor and a report (as [`run_pointer_desc`]
/// does), parse the descriptor as a gamepad, and check what the decoder makes
/// of the report and of a mutated one: known axis usages, values in range, one
/// edge per real change, and a release pass that only releases and centres.
/// The same bytes are also fed to the X-input decoder, whose edges are checked
/// against the report word.
pub fn run_gamepad_desc(data: &[u8]) {
    let Some((&len, rest)) = data.split_first() else {
        return;
    };
    let split = rest.len().saturating_sub(usize::from(len % 70));
    let (descriptor, report) = rest.split_at(split);
    if let Ok(layout) = parse_gamepad(descriptor) {
        for field in layout.axes.into_iter().flatten() {
            check_field(&field);
        }
        for field in layout.buttons.into_iter().flatten() {
            check_field(&field);
        }
        let mut decoder = PadDecoder::new(layout);
        let mut model = PadModel::default();
        check_pad_report(&mut decoder, &mut model, report);
        let mutated: Vec<u8> = report.iter().map(|byte| byte ^ 0x55).collect();
        check_pad_report(&mut decoder, &mut model, &mutated);
        check_pad_release(&mut decoder, &mut model);
    }
    check_xinput(data);
}

/// The model of what a pad decoder should hold: the normalized value of every
/// axis it has seen, and the buttons it holds.
#[derive(Default)]
struct PadModel {
    axes: [Option<i32>; AXIS_COUNT],
    buttons: u32,
}

/// Feed one report to a pad decoder and check every output against what the
/// layout reads from it: exactly the axes whose normalized value changed and
/// exactly the buttons whose bit changed.
fn check_pad_report(decoder: &mut PadDecoder, model: &mut PadModel, report: &[u8]) {
    let Some(read) = decoder.layout().read(report) else {
        // A report the layout cannot read changes nothing and emits nothing.
        let mut emitted = false;
        decoder.feed(report, |_| emitted = true);
        assert!(!emitted, "a refused report emitted an output");
        return;
    };
    let mut want_axes = [None; AXIS_COUNT];
    for (slot, field) in decoder.layout().axes.iter().enumerate() {
        if let Some(field) = field {
            want_axes[slot] = Some(if AXES[slot] == crate::report::usage::HAT {
                field.normalize_hat(read.axes[slot])
            } else {
                field.normalize_axis(read.axes[slot])
            });
        }
    }
    let want_buttons = read.buttons;
    let mut emitted_axes = [None; AXIS_COUNT];
    let mut emitted_buttons = 0u32;
    assert!(decoder.feed(report, |out| match out {
        PadOut::Axis { usage, value } => {
            let slot = AXES
                .iter()
                .position(|&axis| axis == usage)
                .expect("axis usage outside the canonical set");
            assert!(emitted_axes[slot].is_none(), "an axis emitted twice");
            if usage == crate::report::usage::HAT {
                assert!((0..=8).contains(&value), "hat value {value}");
            } else {
                assert!((-0x8000..=0x7FFF).contains(&value), "axis value {value}");
            }
            emitted_axes[slot] = Some(value);
        }
        PadOut::Button { usage, pressed } => {
            assert!((1..=32).contains(&usage), "button usage {usage}");
            let bit = 1u32 << (usage - 1);
            assert_eq!(emitted_buttons & bit, 0, "a button emitted twice");
            emitted_buttons |= bit;
            assert_eq!(
                pressed,
                want_buttons & bit != 0,
                "button edge without a change"
            );
        }
    }));
    for slot in 0..AXIS_COUNT {
        let changed = model.axes[slot] != want_axes[slot];
        assert_eq!(
            emitted_axes[slot].is_some(),
            changed,
            "axis {slot}: emitted {:?}, changed {changed}",
            emitted_axes[slot]
        );
        if let Some(value) = emitted_axes[slot] {
            assert_eq!(Some(value), want_axes[slot], "axis {slot} value");
        }
    }
    assert_eq!(
        emitted_buttons,
        model.buttons ^ want_buttons,
        "the emitted button set is not the changed one"
    );
    model.axes = want_axes;
    model.buttons = want_buttons;
}

/// Unplug a pad decoder: exactly the axes away from rest and the held buttons
/// come back, and a second pass emits nothing.
fn check_pad_release(decoder: &mut PadDecoder, model: &mut PadModel) {
    let mut released = Vec::new();
    decoder.release_all(|out| released.push(out));
    for out in &released {
        match *out {
            PadOut::Axis { usage, value } => {
                let slot = AXES
                    .iter()
                    .position(|&axis| axis == usage)
                    .expect("release axis usage outside the canonical set");
                let rest = if usage == crate::report::usage::HAT {
                    8
                } else {
                    0
                };
                assert_eq!(value, rest, "release did not return an axis to rest");
                assert_ne!(model.axes[slot], Some(rest), "an axis at rest released");
                model.axes[slot] = Some(rest);
            }
            PadOut::Button { pressed, .. } => assert!(!pressed, "release_all pressed"),
        }
    }
    for (slot, field) in decoder.layout().axes.iter().enumerate() {
        if field.is_none() {
            assert_eq!(model.axes[slot], None, "an absent axis holds a value");
            continue;
        }
        let rest = if AXES[slot] == crate::report::usage::HAT {
            8
        } else {
            0
        };
        assert!(
            model.axes[slot].is_none() || model.axes[slot] == Some(rest),
            "release_all left axis {slot} deflected"
        );
    }
    model.buttons = 0;
    let mut again = Vec::new();
    decoder.release_all(|out| again.push(out));
    assert!(again.is_empty(), "release_all emitted twice");
}

/// The X-input decoder's model: the six axes, the hat and the held buttons.
#[derive(Default)]
struct XinputModel {
    axes: [Option<i32>; 6],
    hat: Option<i32>,
    buttons: u32,
}

/// The X-input decoder reads any bytes as its fixed report: check that exactly
/// the axes and buttons a report changes come out, over the report, a mutated
/// one and a release pass.
fn check_xinput(data: &[u8]) {
    let mut decoder = xinput::Decoder::new();
    let mut model = XinputModel::default();
    check_xinput_report(&mut decoder, &mut model, data);
    let mutated: Vec<u8> = data.iter().map(|byte| byte ^ 0x55).collect();
    check_xinput_report(&mut decoder, &mut model, &mutated);
    check_xinput_release(&mut decoder, &mut model);
}

/// The report's button bits, the canonical usages they become and the d-pad
/// bits: restated independently of `xinput::BUTTONS` and `xinput::hat_of`.
const EDGES: [(u16, u16); 11] = [
    (0x1000, 1),  // A
    (0x2000, 2),  // B
    (0x4000, 3),  // X
    (0x8000, 4),  // Y
    (0x0100, 5),  // LB
    (0x0200, 6),  // RB
    (0x0020, 7),  // Back
    (0x0010, 8),  // Start
    (0x0040, 9),  // L3
    (0x0080, 10), // R3
    (0x0400, 11), // Guide
];

fn xinput_hat(word: u16) -> i32 {
    let (up, down, left, right) = (
        word & 0x0001 != 0,
        word & 0x0002 != 0,
        word & 0x0004 != 0,
        word & 0x0008 != 0,
    );
    match (up, down, left, right) {
        (true, false, true, false) => 7,
        (true, false, false, false) => 0,
        (true, false, false, true) => 1,
        (false, false, false, true) => 2,
        (false, true, false, true) => 3,
        (false, true, false, false) => 4,
        (false, true, true, false) => 5,
        (false, false, true, false) => 6,
        _ => 8,
    }
}

fn check_xinput_report(decoder: &mut xinput::Decoder, model: &mut XinputModel, report: &[u8]) {
    if report.len() < 12 {
        let mut emitted = false;
        assert!(!decoder.feed(report, |_| emitted = true));
        assert!(!emitted, "a short X-input report emitted an output");
        return;
    }
    let word = u16::from_le_bytes([report[0], report[1]]);
    let stick = |at: usize| i32::from(i16::from_le_bytes([report[at], report[at + 1]]));
    let want_axes = [
        stick(4),
        stick(6),
        i32::from(report[2]) * 0x7FFF / 0xFF,
        stick(8),
        stick(10),
        i32::from(report[3]) * 0x7FFF / 0xFF,
    ];
    let want_hat = xinput_hat(word);
    let want_buttons = EDGES.iter().fold(0u32, |bits, &(mask, usage)| {
        if word & mask != 0 {
            bits | 1 << (usage - 1)
        } else {
            bits
        }
    });
    let mut emitted_axes = [None; 6];
    let mut emitted_hat = None;
    let mut emitted_buttons = 0u32;
    assert!(decoder.feed(report, |out| match out {
        PadOut::Axis { usage, value } => {
            assert!(AXES.contains(&usage), "xinput axis usage {usage}");
            if usage == crate::report::usage::HAT {
                assert!((0..=8).contains(&value), "xinput hat {value}");
                assert!(emitted_hat.is_none(), "the hat emitted twice");
                emitted_hat = Some(value);
            } else if matches!(usage, crate::report::usage::Z | crate::report::usage::RZ) {
                assert!((0..=0x7FFF).contains(&value), "xinput trigger {value}");
            } else {
                assert!((-0x8000..=0x7FFF).contains(&value), "xinput axis {value}");
            }
            let slot = [
                crate::report::usage::X,
                crate::report::usage::Y,
                crate::report::usage::Z,
                crate::report::usage::RX,
                crate::report::usage::RY,
                crate::report::usage::RZ,
            ]
            .iter()
            .position(|&axis| axis == usage);
            if let Some(slot) = slot {
                assert!(emitted_axes[slot].is_none(), "an xinput axis emitted twice");
                emitted_axes[slot] = Some(value);
            }
        }
        PadOut::Button { usage, pressed } => {
            let bit = 1u32 << (usage - 1);
            assert_eq!(emitted_buttons & bit, 0, "an xinput button emitted twice");
            emitted_buttons |= bit;
            assert_eq!(
                pressed,
                want_buttons & bit != 0,
                "xinput edge without a change"
            );
        }
    }));
    for slot in 0..6 {
        assert_eq!(
            emitted_axes[slot].is_some(),
            model.axes[slot] != Some(want_axes[slot]),
            "xinput axis {slot} emission does not match the change"
        );
        if let Some(value) = emitted_axes[slot] {
            assert_eq!(Some(value), Some(want_axes[slot]), "xinput axis {slot}");
        }
    }
    assert_eq!(
        emitted_hat.is_some(),
        model.hat != Some(want_hat),
        "the hat emission does not match the change"
    );
    assert_eq!(
        emitted_buttons,
        model.buttons ^ want_buttons,
        "the emitted xinput button set is not the changed one"
    );
    model.axes = want_axes.map(Some);
    model.hat = Some(want_hat);
    model.buttons = want_buttons;
}

/// Unplug the X-input decoder: every deflected axis and held button comes
/// back, and a second pass emits nothing.
fn check_xinput_release(decoder: &mut xinput::Decoder, model: &mut XinputModel) {
    let mut released = Vec::new();
    decoder.release_all(|out| released.push(out));
    for out in &released {
        match *out {
            PadOut::Axis { usage, value } => {
                if usage == crate::report::usage::HAT {
                    assert_eq!(value, 8, "the hat did not centre");
                    assert_ne!(model.hat, Some(8), "a centred hat was released");
                    model.hat = Some(8);
                } else {
                    assert_eq!(value, 0, "an axis did not return to rest");
                }
            }
            PadOut::Button { pressed, .. } => assert!(!pressed, "release_all pressed"),
        }
    }
    for axis in model.axes.iter_mut() {
        *axis = Some(0);
    }
    model.buttons = 0;
    let mut again = Vec::new();
    decoder.release_all(|out| again.push(out));
    assert!(again.is_empty(), "release_all emitted twice");
}
