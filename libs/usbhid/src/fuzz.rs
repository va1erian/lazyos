//! Fuzz entry points, shared by the seeded tests below and the cargo-fuzz
//! targets (`fuzz/fuzz_targets/usbdesc.rs`, `hidreport.rs`,
//! `hidreportdesc.rs`), so a crash found
//! by one replays under the other.
//!
//! * [`run_desc`]: any bytes as a device descriptor and as a configuration
//!   chain. Nothing may panic, and an accepted configuration must be
//!   self-consistent.
//! * [`run_report`]: a script of keyboard and mouse reports. The decoders
//!   are checked against a naive model after every report: the held set is
//!   exactly the last accepted report's, every edge is a real change, and no
//!   edge carries a usage the bus would refuse.
//! * [`run_pointer_desc`]: any bytes as a HID report descriptor, then a
//!   report read through the layout found. Every placed field must fit a
//!   64-byte report, and normalized positions stay in `0..=0xFFFF`.

use std::collections::BTreeSet;
use std::vec::Vec;

use crate::boot::{parse_mouse, BootKeyboard, BootMouse, MouseOut, MOUSE_BUTTONS};
use crate::desc::{parse_config, parse_device, Protocol};
use crate::report::gamepad::{parse_gamepad, Decoder as PadDecoder, PadOut, AXES, AXIS_COUNT};
use crate::report::{parse_pointer, Field, MAX_REPORT};
use crate::xinput;

/// Parse `data` both ways; panics on an inconsistent result.
pub fn run_desc(data: &[u8]) {
    let _ = parse_device(data);
    for superspeed in [false, true] {
        if let Ok(hub) = crate::hub::parse_hub(data, superspeed) {
            assert!(
                hub.ports != 0 && hub.think_time <= 3,
                "hub descriptor fields"
            );
        }
        if let Ok(status) = crate::hub::PortStatus::decode(data, superspeed) {
            assert!(status.change_features().count() <= 6, "change selectors");
        }
    }
    let ports: Vec<u8> =
        crate::hub::changed_ports(data, data.first().copied().unwrap_or(0)).collect();
    assert!(
        ports
            .iter()
            .all(|&p| (1..=crate::hub::MAX_PORTS).contains(&p)),
        "bitmap ports"
    );
    let Ok(config) = parse_config(data) else {
        return;
    };
    assert!(
        usize::from(config.total_len) <= data.len(),
        "chain past the input"
    );
    for hid in config.hid_interfaces() {
        if let Some(endpoint) = hid.endpoint {
            assert!(endpoint.address & 0x80 != 0, "endpoint is not IN");
            assert!(endpoint.number() != 0, "endpoint 0 is control");
            assert!(endpoint.max_packet <= 0x07FF, "packet size bits");
        }
    }
    for boot in config.boot_interfaces() {
        assert!(boot.protocol != Protocol::None && boot.endpoint.is_some());
    }
    for interface in config.interfaces() {
        assert!(interface.endpoints().count() <= crate::desc::MAX_ENDPOINTS);
        assert!(interface
            .endpoints()
            .all(|e| e.number() != 0 && e.extra <= 2 && e.max_burst <= 15));
    }
}

/// Split `data` into a report descriptor and a report (its first byte is the
/// report's length, the report is the tail), parse and read; panics on an
/// inconsistent layout.
pub fn run_pointer_desc(data: &[u8]) {
    let Some((&len, rest)) = data.split_first() else {
        return;
    };
    let split = rest.len().saturating_sub(usize::from(len % 70));
    let (descriptor, report) = rest.split_at(split);
    let Ok(pointer) = parse_pointer(descriptor) else {
        return;
    };
    let fields = [pointer.x, pointer.y, pointer.wheel]
        .into_iter()
        .chain(pointer.buttons)
        .flatten();
    for field in fields {
        check_field(&field);
    }
    let (x, y) = (pointer.x.expect("x"), pointer.y.expect("y"));
    if let Some(read) = pointer.read(report) {
        // Normalizing can never leave the bus's range (it is a u16), but it
        // must not panic either, whatever the logical range.
        let _ = (x.normalize(read.x), y.normalize(read.y));
    }
}

fn check_field(field: &Field) {
    assert!((1..=32).contains(&field.bits), "field width {}", field.bits);
    let end = field.bit + u32::from(field.bits);
    assert!(
        end as usize <= (MAX_REPORT - 1) * 8,
        "field past the report"
    );
}

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
        check_pad_report(&mut decoder, report);
        let mutated: Vec<u8> = report.iter().map(|byte| byte ^ 0x55).collect();
        check_pad_report(&mut decoder, &mutated);
        check_pad_release(&mut decoder);
    }
    check_xinput(data);
}

/// Feed one report to a pad decoder and check every output against what the
/// layout reads from it: one changed axis, or a button edge that is a change.
fn check_pad_report(decoder: &mut PadDecoder, report: &[u8]) {
    let expected = decoder.layout().read(report);
    let mut seen_axes = [false; AXIS_COUNT];
    let mut seen_buttons: u32 = 0;
    decoder.feed(report, |out| match out {
        PadOut::Axis { usage, value } => {
            let slot = AXES
                .iter()
                .position(|&axis| axis == usage)
                .expect("axis usage outside the canonical set");
            assert!(!seen_axes[slot], "an axis emitted twice in one report");
            seen_axes[slot] = true;
            if usage == crate::report::usage::HAT {
                assert!((0..=8).contains(&value), "hat value {value}");
            } else {
                assert!((-0x8000..=0x7FFF).contains(&value), "axis value {value}");
            }
        }
        PadOut::Button { usage, pressed } => {
            assert!((1..=32).contains(&usage), "button usage {usage}");
            let bit = 1u32 << (usage - 1);
            assert_eq!(seen_buttons & bit, 0, "a button emitted twice");
            seen_buttons |= bit;
            assert_eq!(
                pressed,
                expected
                    .map(|read| read.buttons & bit != 0)
                    .unwrap_or(false),
                "button edge without a change"
            );
        }
    });
}

/// Unplug a pad decoder: only releases and rest values may come out, and a
/// second pass emits nothing.
fn check_pad_release(decoder: &mut PadDecoder) {
    let mut released = Vec::new();
    decoder.release_all(|out| released.push(out));
    for out in &released {
        match *out {
            PadOut::Axis { usage, value } => {
                assert!(AXES.contains(&usage), "release axis usage {usage}");
                let rest = if usage == crate::report::usage::HAT {
                    8
                } else {
                    0
                };
                assert_eq!(value, rest, "release did not return an axis to rest");
            }
            PadOut::Button { pressed, .. } => assert!(!pressed, "release_all pressed"),
        }
    }
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
    let mut check = |decoder: &mut xinput::Decoder, report: &[u8]| {
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

/// Run a report script: each record is a selector byte, a length byte and
/// that many report bytes (keyboard when the selector is even, mouse when odd).
pub fn run_report(data: &[u8]) {
    let mut keyboard = BootKeyboard::new();
    let mut key_model: BTreeSet<u16> = BTreeSet::new();
    let mut mouse = BootMouse::new();
    let mut at = 0;
    while at + 2 <= data.len() {
        let (selector, len) = (data[at], usize::from(data[at + 1] % 12));
        let report = &data[(at + 2).min(data.len())..(at + 2 + len).min(data.len())];
        at += 2 + len;
        if selector % 2 == 0 {
            check_keyboard(&mut keyboard, &mut key_model, report);
        } else {
            check_mouse(&mut mouse, report);
        }
    }
    let mut released = Vec::new();
    keyboard.release_all(|edge| released.push(edge));
    assert!(released.iter().all(|edge| !edge.pressed));
    assert_eq!(released.len(), key_model.len(), "release_all missed keys");
}

fn check_keyboard(keyboard: &mut BootKeyboard, model: &mut BTreeSet<u16>, report: &[u8]) {
    let mut edges = Vec::new();
    if keyboard.feed(report, |edge| edges.push(edge)).is_err() {
        assert!(report.len() < 2 && edges.is_empty());
        return;
    }
    let keys = &report[2..report.len().min(8)];
    if keys.iter().any(|&key| (1..=3).contains(&key)) {
        assert!(edges.is_empty(), "a rollover report produced edges");
        return;
    }
    let mut next = BTreeSet::new();
    for bit in 0..8 {
        if report[0] & (1 << bit) != 0 {
            next.insert(0xE0 + bit);
        }
    }
    next.extend(
        keys.iter()
            .map(|&key| u16::from(key))
            .filter(|key| (4..=0xE7).contains(key)),
    );
    for edge in &edges {
        assert!(
            (4..=0xE7).contains(&edge.usage),
            "edge outside the keyboard page"
        );
        assert_eq!(edge.pressed, next.contains(&edge.usage), "edge direction");
        assert_ne!(
            model.contains(&edge.usage),
            next.contains(&edge.usage),
            "edge without a change"
        );
    }
    let changes = model.symmetric_difference(&next).count();
    assert_eq!(edges.len(), changes, "missing edges");
    *model = next;
}

fn check_mouse(mouse: &mut BootMouse, report: &[u8]) {
    let Ok(parsed) = parse_mouse(report) else {
        assert!(report.len() < 3);
        return;
    };
    let before = mouse.held();
    let mut out = Vec::new();
    mouse.feed(&parsed, |o| out.push(o));
    let mask = (1u8 << MOUSE_BUTTONS) - 1;
    assert_eq!(mouse.held(), parsed.buttons & mask);
    let mut seen_button = false;
    for o in &out {
        match *o {
            MouseOut::Motion { .. } | MouseOut::Wheel(_) => {
                assert!(!seen_button, "motion or wheel after a button edge");
            }
            MouseOut::Button { usage, pressed } => {
                seen_button = true;
                assert!((1..=5).contains(&usage));
                let bit = 1 << (usage - 1);
                assert_ne!(before & bit != 0, pressed, "edge without a change");
            }
        }
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("usbdesc", run_desc);
        replay("hidreport", run_report);
        replay("hidreportdesc", run_pointer_desc);
        replay("hidreportdesc", run_gamepad_desc);
    }

    /// Mutations of QEMU's tablet and mouse report descriptors, and of the
    /// gamepad descriptors: mostly valid structure with a few hostile bytes.
    #[test]
    fn mutated_report_descriptors() {
        let golden: [&[u8]; 5] = [
            &crate::tests::golden::TABLET_REPORT,
            &crate::tests::golden::MOUSE_REPORT,
            &crate::tests::golden::PAD_REPORT,
            &crate::tests::golden::PAD_REPORT_4WAY,
            &crate::tests::golden::PAD_REPORT_IDS,
        ];
        for_seeds(
            "usbhid::fuzz::mutated_report_descriptors",
            |_, rng: &mut Rng| {
                let mut data = std::vec![rng.byte()];
                data.extend_from_slice(golden[rng.below(5) as usize]);
                for _ in 0..1 + rng.below(4) {
                    let at = 1 + rng.below(data.len() as u64 - 1) as usize;
                    data[at] = rng.byte();
                }
                for _ in 0..rng.below(12) {
                    data.push(rng.byte());
                }
                run_pointer_desc(&data);
                run_gamepad_desc(&data);
            },
        );
    }

    /// Mutations of the golden configurations: mostly valid structure with
    /// one or two hostile bytes, which is where parsers break.
    #[test]
    fn mutated_descriptors() {
        let golden = [
            &crate::tests::golden::KBD_CONFIG,
            &crate::tests::golden::MOUSE_CONFIG,
            &crate::tests::golden::TABLET_CONFIG,
        ];
        for_seeds("usbhid::fuzz::mutated_descriptors", |_, rng: &mut Rng| {
            let mut data = golden[rng.below(3) as usize].to_vec();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(data.len() as u64) as usize;
                data[at] = rng.byte();
            }
            let cut = rng.below(data.len() as u64 + 8) as usize;
            data.resize(cut, rng.byte());
            run_desc(&data);
        });
    }

    #[test]
    fn random_report_scripts() {
        for_seeds("usbhid::fuzz::random_report_scripts", |_, rng: &mut Rng| {
            let mut data = Vec::new();
            for _ in 0..rng.below(200) {
                data.push(rng.byte());
                let len = rng.below(12) as u8;
                data.push(len);
                for _ in 0..len {
                    // Bias key bytes toward real usages and the phantom codes.
                    data.push(match rng.below(4) {
                        0 => rng.below(4) as u8,
                        1 => 4 + rng.below(8) as u8,
                        _ => rng.byte(),
                    });
                }
            }
            run_report(&data);
        });
    }
}
