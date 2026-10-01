//! Pointer records on the raw bus (`docs/usb-hid-plan.md`, phase P0): the
//! encoding, tail merging, and the PS/2 mouse tap.

use super::*;
use crate::input::bus::{device, pointer};
use crate::input::mouse;
use crate::input::mouse_tap::{MouseTap, TAP};

const MOUSE: u8 = device::PS2_MOUSE;

fn rel(dx: i16, dy: i16) {
    bus::publish(MOUSE, kind::REL_MOTION, 0, pointer::pack_rel(dx, dy));
}

fn key(code: u16, value: i32) {
    bus::publish(device::PS2_KEYBOARD, kind::KEY, code, value);
}

/// Open a consumer for a fresh scratch task.
fn consumer() -> Result<usize, String> {
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    Ok(owner)
}

/// `(kind, code, value)` of each record, for compact comparisons.
fn shape(records: &[RawEvent]) -> Vec<(u8, u16, i32)> {
    records.iter().map(|r| (r.kind, r.code, r.value)).collect()
}

/// Packing round-trips every corner, including negative and extreme deltas.
pub fn encoding_round_trips() -> Result<(), String> {
    for (dx, dy) in [
        (0, 0),
        (1, -1),
        (-1, 1),
        (i16::MIN, i16::MAX),
        (i16::MAX, i16::MIN),
        (-300, 255),
    ] {
        let packed = pointer::pack_rel(dx, dy);
        check!(
            pointer::unpack_rel(packed) == (dx, dy),
            "rel ({dx}, {dy}) -> {packed:#x}"
        );
    }
    for (x, y) in [
        (0, 0),
        (0xFFFF, 0),
        (0, 0xFFFF),
        (0xFFFF, 0xFFFF),
        (0x1234, 0xABCD),
    ] {
        let packed = pointer::pack_abs(x, y);
        check!(
            pointer::unpack_abs(packed) == (x, y),
            "abs ({x}, {y}) -> {packed:#x}"
        );
    }
    // Merging saturates rather than wrapping.
    let merged = pointer::merge(
        kind::REL_MOTION,
        pointer::pack_rel(30_000, -30_000),
        pointer::pack_rel(30_000, -30_000),
    );
    check!(
        pointer::unpack_rel(merged) == (i16::MAX, i16::MIN),
        "saturation gave {:?}",
        pointer::unpack_rel(merged)
    );
    check!(
        pointer::merge(kind::SCROLL, i32::MAX, 5) == i32::MAX,
        "scroll saturation"
    );
    check!(
        pointer::merge(kind::ABS_MOTION, 7, 9) == 9,
        "absolute positions replace"
    );
    Ok(())
}

/// Consecutive motion folds into one record that keeps the first `seq`; the
/// next unrelated record takes the next number, so the stream stays gapless.
pub fn tail_merging() -> Result<(), String> {
    fresh();
    let owner = consumer()?;
    rel(1, 2);
    rel(3, -4);
    rel(-10, 0);
    bus::publish(MOUSE, kind::SCROLL, pointer::VERTICAL, 1);
    bus::publish(MOUSE, kind::SCROLL, pointer::VERTICAL, 2);
    bus::publish(MOUSE, kind::ABS_MOTION, 0, pointer::pack_abs(5, 6));
    bus::publish(MOUSE, kind::ABS_MOTION, 0, pointer::pack_abs(700, 800));
    key(0x04, 1);
    let records = drain_all(owner, 16)?;
    check!(
        shape(&records)
            == [
                (kind::REL_MOTION, 0, pointer::pack_rel(-6, -2)),
                (kind::SCROLL, pointer::VERTICAL, 3),
                (kind::ABS_MOTION, 0, pointer::pack_abs(700, 800)),
                (kind::KEY, 0x04, 1),
            ],
        "records {:x?}",
        shape(&records)
    );
    let (next, lost) = gapless(&records, 1)?;
    check!(next == 5 && lost == 0, "sequence ended at {next}");
    bus::reset();
    Ok(())
}

/// Merging is strictly at the tail: never across a key or button, never
/// across devices, codes or kinds, and never for edges.
pub fn merge_boundaries() -> Result<(), String> {
    fresh();
    let owner = consumer()?;
    rel(1, 0);
    key(0x04, 1);
    rel(1, 0);
    bus::publish(MOUSE, kind::BUTTON, pointer::button::LEFT, 1);
    rel(1, 0);
    bus::publish(3, kind::REL_MOTION, 0, pointer::pack_rel(1, 0));
    bus::publish(MOUSE, kind::SCROLL, pointer::VERTICAL, 1);
    bus::publish(MOUSE, kind::SCROLL, pointer::HORIZONTAL, 1);
    bus::publish(MOUSE, kind::BUTTON, pointer::button::LEFT, 0);
    bus::publish(MOUSE, kind::BUTTON, pointer::button::LEFT, 0);
    key(0x04, 0);
    key(0x04, 0);
    let records = drain_all(owner, 32)?;
    check!(records.len() == 12, "{} records, want 12", records.len());
    gapless(&records, 1)?;
    check!(
        records[5].device == 3 && records[4].device == MOUSE,
        "devices merged"
    );
    bus::reset();
    Ok(())
}

/// A record already drained by its consumer is not modified behind its back:
/// the next motion is a new record.
pub fn no_merge_after_drain() -> Result<(), String> {
    fresh();
    let owner = consumer()?;
    rel(1, 1);
    let first = drain_all(owner, 8)?;
    rel(2, 2);
    let second = drain_all(owner, 8)?;
    check!(
        first.len() == 1 && second.len() == 1,
        "{} then {}",
        first.len(),
        second.len()
    );
    check!(
        first[0].seq == 1 && second[0].seq == 2,
        "seqs {} {}",
        first[0].seq,
        second[0].seq
    );
    check!(
        pointer::unpack_rel(second[0].value) == (2, 2),
        "second motion {:?}",
        pointer::unpack_rel(second[0].value)
    );
    bus::reset();
    Ok(())
}

/// Two consumers, one of which drained the tail: neither merges, so both see
/// the same records with the same sequence numbers and every delta arrives.
pub fn merge_is_all_or_nothing() -> Result<(), String> {
    fresh();
    let (a, b) = (consumer()?, consumer()?);
    rel(1, 0);
    let a_first = drain_all(a, 8)?;
    rel(2, 0);
    rel(4, 0);
    let a_rest = drain_all(a, 8)?;
    let b_all = drain_all(b, 8)?;
    let mut a_all = a_first;
    a_all.extend(a_rest);
    check!(a_all == b_all, "consumers diverged: {a_all:?} vs {b_all:?}");
    check!(
        shape(&b_all)
            == [
                (kind::REL_MOTION, 0, pointer::pack_rel(1, 0)),
                (kind::REL_MOTION, 0, pointer::pack_rel(6, 0)),
            ],
        "records {:x?}",
        shape(&b_all)
    );
    gapless(&b_all, 1)?;
    bus::reset();
    Ok(())
}

/// A motion flood between a key press and its release costs one slot: no
/// `Dropped` marker, so `inputd` never releases keys spuriously.
pub fn motion_flood_keeps_keys() -> Result<(), String> {
    fresh();
    let owner = consumer()?;
    key(0x04, 1);
    for index in 0..100_000 {
        rel(if index % 2 == 0 { 3 } else { -2 }, 1);
        if index % 1000 == 0 {
            bus::publish(MOUSE, kind::SCROLL, pointer::VERTICAL, -1);
            rel(0, 0);
        }
    }
    key(0x04, 0);
    let records = drain_all(owner, 512)?;
    check!(
        records.iter().all(|r| r.kind != kind::DROPPED),
        "flood overflowed the ring ({} records)",
        records.len()
    );
    check!(
        records.first().map(|r| (r.kind, r.value)) == Some((kind::KEY, 1))
            && records.last().map(|r| (r.kind, r.value)) == Some((kind::KEY, 0)),
        "key edges lost"
    );
    check!(records.len() < RING_CAP, "{} records", records.len());
    gapless(&records, 1)?;
    bus::reset();
    Ok(())
}

/// Bytes through the real IRQ12 entry point reach the bus as pointer records:
/// screen-oriented motion, an up-positive wheel, then button edges (so the
/// wheel lands before the press it came with).
pub fn ps2_mouse_reaches_bus() -> Result<(), String> {
    fresh();
    TAP.lock().reset();
    mouse::set_wheel_mode_for_test(true);
    let owner = consumer()?;
    // Move right 5 and *up* 3 (PS/2 Y is up-positive), press left, wheel up.
    for byte in [0x09, 5, 3, 0xFF] {
        mouse::push_byte(byte);
    }
    // No motion, swap left for right, no wheel.
    for byte in [0x0A, 0, 0, 0] {
        mouse::push_byte(byte);
    }
    // Negative X (sign bit 0x10), release everything.
    for byte in [0x18, 0xF6, 0, 0] {
        mouse::push_byte(byte);
    }
    mouse::set_wheel_mode_for_test(false);
    let records = drain_all(owner, 16)?;
    use pointer::button::{LEFT, RIGHT};
    check!(
        shape(&records)
            == [
                (kind::REL_MOTION, 0, pointer::pack_rel(5, -3)),
                (kind::SCROLL, pointer::VERTICAL, 1),
                (kind::BUTTON, LEFT, 1),
                (kind::BUTTON, LEFT, 0),
                (kind::BUTTON, RIGHT, 1),
                (kind::REL_MOTION, 0, pointer::pack_rel(-10, 0)),
                (kind::BUTTON, RIGHT, 0),
            ],
        "records {:x?}",
        shape(&records)
    );
    check!(records.iter().all(|r| r.device == MOUSE), "wrong device id");
    TAP.lock().reset();
    bus::reset();
    Ok(())
}

/// The tap reports only changes: a packet repeating the held buttons with no
/// motion or wheel publishes nothing.
pub fn mouse_tap_edges_only() -> Result<(), String> {
    fresh();
    let owner = consumer()?;
    let mut tap = MouseTap::new();
    let held = mouse::decode_packet(&[0x0D, 0, 0, 0], false);
    for _ in 0..5 {
        tap.feed(&held);
    }
    let records = drain_all(owner, 16)?;
    check!(
        shape(&records)
            == [
                (kind::BUTTON, pointer::button::LEFT, 1),
                (kind::BUTTON, pointer::button::MIDDLE, 1),
            ],
        "records {:x?}",
        shape(&records)
    );
    bus::reset();
    Ok(())
}
