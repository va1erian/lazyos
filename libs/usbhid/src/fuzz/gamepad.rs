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

/// The X-input decoder reads any bytes as its fixed report: check the axis
/// ranges, that each button edge matches the report word, and that a mutated
/// report and a release pass stay consistent.
fn check_xinput(data: &[u8]) {
    // The mapping, restated independently of `xinput::BUTTONS`.
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
    let mut decoder = xinput::Decoder::new();
    let check = |decoder: &mut xinput::Decoder, report: &[u8]| {
        let word = if report.len() >= 2 {
            u16::from_le_bytes([report[0], report[1]])
        } else {
            0
        };
        let mut seen = 0u32;
        decoder.feed(report, |out| match out {
            PadOut::Axis { usage, value } => {
                assert!(AXES.contains(&usage), "xinput axis usage {usage}");
                if usage == crate::report::usage::HAT {
                    assert!((0..=8).contains(&value), "xinput hat {value}");
                } else if matches!(usage, crate::report::usage::Z | crate::report::usage::RZ) {
                    assert!((0..=0x7FFF).contains(&value), "xinput trigger {value}");
                } else {
                    assert!((-0x8000..=0x7FFF).contains(&value), "xinput axis {value}");
                }
            }
            PadOut::Button { usage, pressed } => {
                let bit = 1u32 << (usage - 1);
                assert_eq!(seen & bit, 0, "an xinput button emitted twice");
                seen |= bit;
                let expected = EDGES
                    .iter()
                    .any(|&(mask, mapped)| mapped == usage && word & mask != 0);
                assert_eq!(pressed, expected, "xinput edge without a change");
            }
        });
    };
    check(&mut decoder, data);
    let mutated: Vec<u8> = data.iter().map(|byte| byte ^ 0x55).collect();
    check(&mut decoder, &mutated);
    let mut released = Vec::new();
    decoder.release_all(|out| released.push(out));
    for out in &released {
        if let PadOut::Button { pressed, .. } = *out {
            assert!(!pressed, "xinput release_all pressed");
        }
    }
}
