//! Gamepad sources (`docs/usb-gamepad-plan.md` G1): the `Gamepad` class and
//! its record kinds, the axis encoding, the device-identity record, the
//! release of up to 32 held buttons, and the axis merge rule.

use super::source::{call, device_of, driver_task, failed, publish, rec, register};
use super::*;
use crate::input::bus::axis;
use crate::input::rawsys::op;
use crate::input::sources::{self, class, Record};
use crate::ipc::credentials::CAP_INPUT_SOURCE;

const EBADF: i64 = 9;

fn shape(records: &[RawEvent]) -> Vec<(u8, u8, u16, i32)> {
    records
        .iter()
        .map(|r| (r.device, r.kind, r.code, r.value))
        .collect()
}

/// A source of `class` publishes only its kinds and only in range: a gamepad
/// may send axes and 32 buttons, nothing else, and a valid record in the same
/// batch still goes out.
pub fn class_and_ranges() -> Result<(), String> {
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    driver_task(CAP_INPUT_SOURCE)?;
    let pad = register(class::GAMEPAD)?;
    let records = [
        rec(kind::AXIS, axis::usage::X, -32768),
        rec(kind::AXIS, axis::usage::HAT, 8),
        rec(kind::AXIS, axis::usage::RZ, 32767),
        rec(kind::BUTTON, 1, 1),
        rec(kind::BUTTON, 32, 1),
        // Not a gamepad record, or out of range.
        rec(kind::KEY, 0x04, 1),
        rec(kind::REL_MOTION, 0, 1),
        rec(kind::ABS_MOTION, 0, 1),
        rec(kind::SCROLL, 0, 1),
        rec(kind::AXIS, 0x40, 0),
        rec(kind::AXIS, 0x38, 0), // a wheel is SCROLL, never an axis
        rec(kind::AXIS, axis::usage::X, 32768),
        rec(kind::AXIS, axis::usage::X, -32769),
        rec(kind::AXIS, axis::usage::HAT, 9),
        rec(kind::BUTTON, 33, 1),
        rec(kind::BUTTON, 0, 1),
        rec(kind::BUTTON, 5, 2),
    ];
    check!(publish(pad, &records) == 5, "accepted the wrong count");
    let all = drain_all(reader, 32)?;
    check!(all.len() == 6, "bus carried {}", all.len());
    let device = device_of(pad);
    check!(
        shape(&all[..1]) == [(device, kind::SOURCE, u16::from(class::GAMEPAD), 1)],
        "announcement {:x?}",
        shape(&all[..1])
    );
    let carried = &all[1..];
    check!(
        shape(carried)
            == [
                (device, kind::AXIS, axis::usage::X, -32768),
                (device, kind::AXIS, axis::usage::HAT, 8),
                (device, kind::AXIS, axis::usage::RZ, 32767),
                (device, kind::BUTTON, 1, 1),
                (device, kind::BUTTON, 32, 1),
            ],
        "bus carried {:x?}",
        shape(carried)
    );
    check!(
        sources::counters().0 == 12,
        "rejected {:?}",
        sources::counters()
    );
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// The identity record names the device and is accepted from every class: it
/// carries no input, only what a per-device mapping table and a diagnostic
/// line want to know.
pub fn device_record_is_accepted_from_every_class() -> Result<(), String> {
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    let pad = register(class::GAMEPAD)?;
    let identity = rec(kind::DEVICE, 0x2dc8, 0x310b);
    check!(publish(keyboard, &[identity]) == 1, "keyboard identity");
    check!(publish(pad, &[identity]) == 1, "gamepad identity");
    let all = drain_all(reader, 8)?;
    check!(
        all.len() == 4
            && all[..2].iter().all(|r| r.kind == kind::SOURCE)
            && all[2..]
                .iter()
                .all(|r| r.kind == kind::DEVICE && r.code == 0x2dc8 && r.value == 0x310b),
        "identity records {:x?}",
        shape(&all)
    );
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// Closing a gamepad releases every one of its 32 buttons, in usage order,
/// under its own device id.
pub fn close_releases_all_32_buttons() -> Result<(), String> {
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    driver_task(CAP_INPUT_SOURCE)?;
    let pad = register(class::GAMEPAD)?;
    let presses: Vec<Record> = (1..=32).map(|n| rec(kind::BUTTON, n, 1)).collect();
    check!(publish(pad, &presses) == 32, "presses");
    let pressed = drain_all(reader, 64)?;
    check!(pressed.len() == 33, "bus carried {}", pressed.len());
    let device = device_of(pad);
    check!(call(op::CLOSE_SOURCE, pad, 0) == 0, "close");
    let released = drain_all(reader, 64)?;
    check!(released.len() == 33, "released {}", released.len());
    check!(
        shape(&released[32..]) == [(device, kind::SOURCE, u16::from(class::GAMEPAD), 0)],
        "the close was not announced last: {:x?}",
        shape(&released[32..])
    );
    for (index, record) in released[..32].iter().enumerate() {
        check!(
            record.device == device
                && record.kind == kind::BUTTON
                && record.code == index as u16 + 1
                && record.value == 0,
            "release {index}: {record:?}"
        );
    }
    check!(
        publish(pad, &[rec(kind::BUTTON, 1, 1)]) == failed(EBADF),
        "a closed pad published"
    );
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// Closing a gamepad returns every axis that was away from its rest value to
/// rest — a crashed or unplugged driver cannot leave a stick deflected — and
/// an axis already back at rest is not re-published. Task death takes the
/// same path.
pub fn close_centres_held_axes() -> Result<(), String> {
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    driver_task(CAP_INPUT_SOURCE)?;
    let pad = register(class::GAMEPAD)?;
    check!(
        publish(
            pad,
            &[
                rec(kind::AXIS, axis::usage::X, -32768),
                rec(kind::AXIS, axis::usage::HAT, 2),
                rec(kind::AXIS, axis::usage::RZ, 0), // already at rest
                rec(kind::BUTTON, 1, 1),
            ]
        ) == 4,
        "publish"
    );
    let pressed = drain_all(reader, 16)?;
    check!(pressed.len() == 5, "bus carried {}", pressed.len());
    let device = device_of(pad);
    // The hat goes back to rest on its own: closing must not re-publish it.
    check!(
        publish(pad, &[rec(kind::AXIS, axis::usage::HAT, 8)]) == 1,
        "centre the hat"
    );
    let _ = drain_all(reader, 16)?;
    check!(call(op::CLOSE_SOURCE, pad, 0) == 0, "close");
    let released = drain_all(reader, 16)?;
    check!(
        shape(&released)
            == [
                (device, kind::BUTTON, 1, 0),
                (device, kind::AXIS, axis::usage::X, 0),
                (device, kind::SOURCE, u16::from(class::GAMEPAD), 0),
            ],
        "released {:x?}",
        shape(&released)
    );
    // A driver that dies holding a stick has it centred too.
    let driver = driver_task(CAP_INPUT_SOURCE)?;
    let pad = register(class::GAMEPAD)?;
    check!(
        publish(pad, &[rec(kind::AXIS, axis::usage::Y, 1234)]) == 1,
        "deflect"
    );
    let deflected = drain_all(reader, 16)?;
    let device = device_of(pad);
    check!(
        shape(&deflected)
            == [
                (device, kind::SOURCE, u16::from(class::GAMEPAD), 1),
                (device, kind::AXIS, axis::usage::Y, 1234),
            ],
        "deflected {:x?}",
        shape(&deflected)
    );
    sources::teardown_task(driver);
    let released = drain_all(reader, 16)?;
    check!(
        shape(&released)
            == [
                (device, kind::AXIS, axis::usage::Y, 0),
                (device, kind::SOURCE, u16::from(class::GAMEPAD), 0),
            ],
        "teardown released {:x?}",
        shape(&released)
    );
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// An axis is absolute state: a newer value *replaces* the queued one, so a
/// starved consumer costs one slot per axis instead of one per report. The
/// pointer's turn rule does not apply, and neither a button in between nor
/// another device merges.
pub fn axis_tail_merges_by_replacing() -> Result<(), String> {
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    let pad = 0x20u8;
    bus::publish(pad, kind::AXIS, axis::usage::X, -100);
    bus::publish(pad, kind::AXIS, axis::usage::X, 200);
    bus::publish(pad, kind::AXIS, axis::usage::X, -300);
    let records = drain_all(reader, 8)?;
    check!(
        shape(&records) == [(pad, kind::AXIS, axis::usage::X, -300)],
        "merged {:x?}",
        shape(&records)
    );
    let (expected, _) = gapless(&records, 1)?;
    // A button between two axis records stops the merge; so does another
    // device's axis.
    bus::publish(pad, kind::AXIS, axis::usage::X, 1);
    bus::publish(pad, kind::BUTTON, 1, 1);
    bus::publish(pad, kind::AXIS, axis::usage::X, 2);
    bus::publish(pad + 1, kind::AXIS, axis::usage::X, 7);
    let records = drain_all(reader, 8)?;
    check!(
        shape(&records)
            == [
                (pad, kind::AXIS, axis::usage::X, 1),
                (pad, kind::BUTTON, 1, 1),
                (pad, kind::AXIS, axis::usage::X, 2),
                (pad + 1, kind::AXIS, axis::usage::X, 7),
            ],
        "interleaved {:x?}",
        shape(&records)
    );
    let (expected, _) = gapless(&records, expected)?;
    // A drained tail is never merged into.
    bus::publish(pad, kind::AXIS, axis::usage::X, 3);
    let records = drain_all(reader, 8)?;
    let (expected, _) = gapless(&records, expected)?;
    bus::publish(pad, kind::AXIS, axis::usage::X, 4);
    let records = drain_all(reader, 8)?;
    check!(
        records.len() == 1 && records[0].value == 4,
        "post-drain merge"
    );
    let (expected, _) = gapless(&records, expected)?;
    // A rejected record never merges and never consumes a sequence number.
    driver_task(CAP_INPUT_SOURCE)?;
    let id = register(class::GAMEPAD)?;
    check!(
        publish(id, &[rec(kind::AXIS, axis::usage::X, 100)]) == 1,
        "publish"
    );
    check!(
        publish(id, &[rec(kind::AXIS, axis::usage::X, 99999)]) == 0,
        "an out-of-range axis was accepted"
    );
    let records = drain_all(reader, 8)?;
    check!(
        records.len() == 2 && records[0].kind == kind::SOURCE && records[1].value == 100,
        "rejected record merged: {:x?}",
        shape(&records)
    );
    gapless(&records, expected)?;
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}
