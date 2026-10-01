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
use crate::report::{parse_pointer, Field, MAX_REPORT};

/// Parse `data` both ways; panics on an inconsistent result.
pub fn run_desc(data: &[u8]) {
    let _ = parse_device(data);
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
    if let Some(boot) = config.first_boot() {
        assert!(boot.protocol != Protocol::None && boot.endpoint.is_some());
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
    }

    /// Mutations of QEMU's tablet and mouse report descriptors.
    #[test]
    fn mutated_report_descriptors() {
        let golden: [&[u8]; 2] = [
            &crate::tests::golden::TABLET_REPORT,
            &crate::tests::golden::MOUSE_REPORT,
        ];
        for_seeds(
            "usbhid::fuzz::mutated_report_descriptors",
            |_, rng: &mut Rng| {
                let mut data = std::vec![rng.byte()];
                data.extend_from_slice(golden[rng.below(2) as usize]);
                for _ in 0..1 + rng.below(4) {
                    let at = 1 + rng.below(data.len() as u64 - 1) as usize;
                    data[at] = rng.byte();
                }
                for _ in 0..rng.below(12) {
                    data.push(rng.byte());
                }
                run_pointer_desc(&data);
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
