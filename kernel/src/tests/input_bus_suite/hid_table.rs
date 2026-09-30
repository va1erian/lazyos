//! Set-1 to HID translation: table properties and the multi-byte quirks.

use super::*;

fn check_pending(step: Step) -> Result<(), String> {
    check!(step == Step::Pending, "prefix produced {step:?}");
    Ok(())
}

fn keys(decoder: &mut Set1Decoder, bytes: &[u8]) -> Step {
    let mut last = Step::Pending;
    for &byte in bytes {
        last = decoder.feed(byte);
    }
    last
}

/// Every make/break pair in both banks decodes to the same usage, the usage is
/// a real page-0x07 key, and no two distinct keys share a usage (bar the
/// documented Ctrl+Break alias of Pause).
pub fn table_round_trips() -> Result<(), String> {
    let mut seen: Vec<(u16, bool, u8)> = Vec::new();
    for extended in [false, true] {
        // 0x60/0x61 breaks collide with the E0/E1 prefixes; no key uses them.
        for code in 0x01..0x60u8 {
            let mut decoder = Set1Decoder::new();
            if extended {
                check_pending(decoder.feed(0xE0))?;
            }
            let make = decoder.feed(code);
            if extended {
                check_pending(decoder.feed(0xE0))?;
            }
            let brk = decoder.feed(code | 0x80);
            match (make, brk) {
                (Step::Key(a, true), Step::Key(b, false)) => {
                    check!(a == b, "{extended}/{code:#x}: make {a:#x} break {b:#x}");
                    check!(
                        (0x04..=0xE7).contains(&a),
                        "{extended}/{code:#x}: usage {a:#x} outside the keyboard page"
                    );
                    seen.push((a, extended, code));
                }
                (Step::Ignored, Step::Ignored) | (Step::Unknown, Step::Unknown) => {}
                other => return Err(format!("{extended}/{code:#x}: asymmetric {other:?}")),
            }
        }
    }
    for (index, &(usage, extended, code)) in seen.iter().enumerate() {
        for &(other, other_ext, other_code) in &seen[index + 1..] {
            let alias = usage == crate::input::hid::usage::PAUSE;
            check!(
                usage != other || alias,
                "usage {usage:#x} from ({extended},{code:#x}) and ({other_ext},{other_code:#x})"
            );
        }
    }
    // Every key of a 105-key board plus keypad is present.
    check!(seen.len() >= 100, "only {} keys mapped", seen.len());
    Ok(())
}

/// Spot checks against the USB HID usage tables.
pub fn known_keys() -> Result<(), String> {
    let cases: &[(&[u8], u16)] = &[
        (&[0x1E], 0x04),       // A
        (&[0x2C], 0x1D),       // Z
        (&[0x02], 0x1E),       // 1
        (&[0x0B], 0x27),       // 0
        (&[0x1C], 0x28),       // Enter
        (&[0x01], 0x29),       // Escape
        (&[0x39], 0x2C),       // Space
        (&[0x3B], 0x3A),       // F1
        (&[0x44], 0x43),       // F10
        (&[0x57], 0x44),       // F11
        (&[0x58], 0x45),       // F12
        (&[0x56], 0x64),       // ISO extra key
        (&[0x1D], 0xE0),       // left Ctrl
        (&[0x2A], 0xE1),       // left Shift
        (&[0x36], 0xE5),       // right Shift
        (&[0x38], 0xE2),       // left Alt
        (&[0xE0, 0x38], 0xE6), // right Alt / AltGr
        (&[0xE0, 0x1D], 0xE4), // right Ctrl
        (&[0xE0, 0x5B], 0xE3), // left GUI
        (&[0xE0, 0x1C], 0x58), // keypad Enter
        (&[0xE0, 0x48], 0x52), // Up
        (&[0xE0, 0x4B], 0x50), // Left
        (&[0xE0, 0x4D], 0x4F), // Right
        (&[0xE0, 0x50], 0x51), // Down
        (&[0xE0, 0x53], 0x4C), // Delete
        (&[0x47], 0x5F),       // keypad 7 (Home is E0-prefixed)
        (&[0xE0, 0x47], 0x4A), // Home
    ];
    let mut decoder = Set1Decoder::new();
    for (bytes, usage) in cases {
        let step = keys(&mut decoder, bytes);
        check!(
            step == Step::Key(*usage, true),
            "{bytes:02x?} decoded {step:?}, want usage {usage:#x}"
        );
    }
    Ok(())
}

/// PrintScreen's fake shifts are swallowed and Pause (no break code) taps.
pub fn printscreen_and_pause() -> Result<(), String> {
    let mut decoder = Set1Decoder::new();
    // Make: E0 2A E0 37.
    let steps = [0xE0, 0x2A, 0xE0, 0x37].map(|byte| decoder.feed(byte));
    check!(
        steps
            == [
                Step::Pending,
                Step::Ignored,
                Step::Pending,
                Step::Key(0x46, true)
            ],
        "PrintScreen make: {steps:?}"
    );
    // Break: E0 B7 E0 AA.
    let steps = [0xE0, 0xB7, 0xE0, 0xAA].map(|byte| decoder.feed(byte));
    check!(
        steps
            == [
                Step::Pending,
                Step::Key(0x46, false),
                Step::Pending,
                Step::Ignored
            ],
        "PrintScreen break: {steps:?}"
    );
    // The NumLock-on variant uses E0 36 fake shifts.
    check!(
        keys(&mut decoder, &[0xE0, 0x36]) == Step::Ignored,
        "fake right shift"
    );
    // Pause: E1 1D 45 E1 9D C5, one tap and an ignored break half.
    let steps = [0xE1, 0x1D, 0x45].map(|byte| decoder.feed(byte));
    check!(
        steps == [Step::Pending, Step::Pending, Step::Tap(0x48)],
        "Pause: {steps:?}"
    );
    let steps = [0xE1, 0x9D, 0xC5].map(|byte| decoder.feed(byte));
    check!(
        steps == [Step::Pending, Step::Pending, Step::Ignored],
        "Pause break half: {steps:?}"
    );
    // Ctrl+Pause is the E0 46 "Break" pair.
    check!(
        keys(&mut decoder, &[0xE0, 0x46]) == Step::Key(0x48, true),
        "Ctrl+Break"
    );
    Ok(())
}

/// Unmapped and corrupt input is reported, never forwarded, and never wedges
/// the decoder mid-sequence.
pub fn unknown_and_recovery() -> Result<(), String> {
    let mut decoder = Set1Decoder::new();
    check!(
        keys(&mut decoder, &[0xE0, 0x5E]) == Step::Unknown,
        "power key"
    );
    check!(
        keys(&mut decoder, &[0xE0, 0x10]) == Step::Unknown,
        "media key"
    );
    check!(
        keys(&mut decoder, &[0x7F]) == Step::Unknown,
        "code past the table"
    );
    check!(
        keys(&mut decoder, &[0xE1, 0x12, 0x34]) == Step::Unknown,
        "bad Pause"
    );
    // After every failure a plain key still decodes.
    check!(
        keys(&mut decoder, &[0x1E]) == Step::Key(0x04, true),
        "recovery"
    );
    // A reset in the middle of a sequence drops the prefix.
    decoder.feed(0xE0);
    decoder.reset();
    check!(
        keys(&mut decoder, &[0x1E]) == Step::Key(0x04, true),
        "reset"
    );
    // Sustained garbage: nothing panics, and a reset leaves a clean decoder.
    for byte in 0..=255u8 {
        for _ in 0..8 {
            decoder.feed(byte);
        }
    }
    decoder.reset();
    check!(
        keys(&mut decoder, &[0x1E]) == Step::Key(0x04, true),
        "after garbage"
    );
    Ok(())
}
