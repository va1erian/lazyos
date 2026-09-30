//! Ring behaviour: ordering, wraparound, the `Dropped` marker, consumer
//! independence and slot reclamation.

use super::*;
use crate::input::raw_tap::Tap;

fn key(code: u16, value: i32) {
    bus::publish(bus::device::PS2_KEYBOARD, kind::KEY, code, value);
}

pub fn order_and_fields() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    for index in 0..10u16 {
        key(0x04 + index, (index % 2) as i32);
    }
    let records = drain_all(owner, 4)?;
    check!(records.len() == 10, "{} records, want 10", records.len());
    gapless(&records, 1)?;
    for (index, record) in records.iter().enumerate() {
        check!(
            record.kind == kind::KEY
                && record.device == bus::device::PS2_KEYBOARD
                && record.code == 0x04 + index as u16
                && record.value == (index % 2) as i32,
            "record {index} is {record:?}"
        );
        check!(
            index == 0 || record.ts_ns >= records[index - 1].ts_ns,
            "timestamp went backwards at {index}"
        );
    }
    // The wire encoding is the documented 24 bytes and round-trips the fields.
    let bytes = records[3].to_bytes();
    check!(
        u64::from_le_bytes(bytes[0..8].try_into().unwrap()) == records[3].seq
            && bytes[16] == bus::device::PS2_KEYBOARD
            && bytes[17] == kind::KEY
            && u16::from_le_bytes(bytes[18..20].try_into().unwrap()) == records[3].code,
        "wire layout"
    );
    bus::reset();
    Ok(())
}

/// The ring index wraps many times with the consumer keeping up: no marker,
/// no loss, order preserved.
pub fn wraparound() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let id = bus::consumer_of(owner).ok_or("no consumer")?;
    let mut expected = 1;
    for round in 0..50u16 {
        for _ in 0..(RING_CAP - 7) {
            key(0x04 + round % 8, 1);
        }
        let mut out = Vec::new();
        bus::drain(id, owner, RING_CAP, &mut out).map_err(|e| format!("{e:?}"))?;
        check!(
            out.len() == RING_CAP - 7,
            "round {round}: {} records",
            out.len()
        );
        let (next, lost) = gapless(&out, expected)?;
        check!(lost == 0, "round {round}: unexpected loss {lost}");
        expected = next;
    }
    bus::reset();
    Ok(())
}

/// Overrun by 37: one marker at the head names the lost span, then the newest
/// `RING_CAP` events follow with no gap.
pub fn overflow_marker() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    for _ in 0..(RING_CAP + 37) {
        key(0x04, 1);
    }
    let records = drain_all(owner, 64)?;
    check!(
        records.len() == RING_CAP + 1,
        "{} records, want {}",
        records.len(),
        RING_CAP + 1
    );
    let marker = records[0];
    check!(
        marker.kind == kind::DROPPED && marker.seq == 1 && marker.value == 37,
        "marker is {marker:?}"
    );
    check!(
        records[1].seq == 38,
        "first survivor is seq {}",
        records[1].seq
    );
    let (next, lost) = gapless(&records, 1)?;
    check!(
        lost == 37 && next == RING_CAP as u64 + 38,
        "covered {next}, lost {lost}"
    );
    // A drained ring reports nothing more.
    check!(
        drain_all(owner, 8)?.is_empty(),
        "records after a full drain"
    );
    bus::reset();
    Ok(())
}

/// A marker consumed before the next overrun does not swallow the new one, and
/// a partial drain that only fits the marker still delivers it once.
pub fn marker_resets() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let id = bus::consumer_of(owner).ok_or("no consumer")?;
    for _ in 0..(RING_CAP + 5) {
        key(0x04, 1);
    }
    let mut first = Vec::new();
    bus::drain(id, owner, 1, &mut first).map_err(|e| format!("{e:?}"))?;
    check!(
        first.len() == 1 && first[0].kind == kind::DROPPED && first[0].value == 5,
        "first drain {first:?}"
    );
    // The ring is still full: ten more events lose ten more.
    for _ in 0..10 {
        key(0x05, 1);
    }
    let rest = drain_all(owner, 100)?;
    check!(
        rest[0].kind == kind::DROPPED && rest[0].seq == 6 && rest[0].value == 10,
        "second marker {:?}",
        rest[0]
    );
    let mut all = first;
    all.extend(rest);
    let (next, lost) = gapless(&all, 1)?;
    check!(
        lost == 15 && next == RING_CAP as u64 + 16,
        "covered {next}, lost {lost}"
    );
    bus::reset();
    Ok(())
}

/// Two consumers see the same stream; one falling behind does not disturb the
/// other, and a consumer that opens late sees only what follows.
pub fn consumers_independent() -> Result<(), String> {
    fresh();
    let (a, b, c) = (scratch()?, scratch()?, scratch()?);
    bus::open(a).map_err(|e| format!("{e:?}"))?;
    bus::open(b).map_err(|e| format!("{e:?}"))?;
    for _ in 0..(RING_CAP + 10) {
        key(0x04, 1);
    }
    bus::open(c).map_err(|e| format!("{e:?}"))?;
    for _ in 0..3 {
        key(0x05, 0);
    }
    let a_stream = drain_all(a, 128)?;
    // `b` reads in tiny batches: same records regardless of cadence.
    let b_stream = drain_all(b, 3)?;
    check!(a_stream == b_stream, "consumers diverged");
    let c_stream = drain_all(c, 64)?;
    check!(
        c_stream.len() == 3,
        "late consumer got {} records",
        c_stream.len()
    );
    check!(
        c_stream[0].seq == RING_CAP as u64 + 11 && c_stream[0].kind == kind::KEY,
        "late consumer starts at {:?}",
        c_stream[0]
    );
    // Ownership is enforced: `b` cannot drain `a`'s ring.
    let ida = bus::consumer_of(a).ok_or("no ring")?;
    let mut out = Vec::new();
    check!(
        bus::drain(ida, b, 8, &mut out) == Err(bus::Error::BadId),
        "cross-owner drain allowed"
    );
    bus::reset();
    Ok(())
}

/// Slots held by dead tasks are reclaimed on demand; only live owners can
/// exhaust the table.
pub fn dead_owner_reclaimed() -> Result<(), String> {
    fresh();
    let dead = crate::task::MAX_TASKS - 1;
    check!(!task::live(dead), "slot {dead} unexpectedly live");
    for offset in 0..bus::MAX_CONSUMERS {
        bus::open(dead - offset).map_err(|e| format!("{e:?}"))?;
    }
    let live_a = scratch()?;
    bus::open(live_a).map_err(|e| format!("open over dead owners: {e:?}"))?;
    let (live_b, live_c) = (scratch()?, scratch()?);
    bus::open(live_b).map_err(|e| format!("{e:?}"))?;
    bus::open(live_c).map_err(|e| format!("{e:?}"))?;
    bus::open(task::KERNEL_TASK).map_err(|e| format!("{e:?}"))?;
    let extra = scratch()?;
    check!(bus::open(extra) == Err(bus::Error::Full), "table not full");
    // Re-opening is idempotent, closing frees the slot.
    check!(bus::open(live_a).is_ok(), "reopen refused");
    let id = bus::consumer_of(live_a).ok_or("no ring")?;
    bus::close(id, live_a).map_err(|e| format!("{e:?}"))?;
    check!(bus::open(extra).is_ok(), "freed slot not reusable");
    bus::reset();
    Ok(())
}

/// Hardware typematic re-sends the make code; the bus carries only edges.
pub fn typematic_suppressed() -> Result<(), String> {
    fresh();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let mut tap = Tap::new();
    let before = crate::input::raw_tap::suppressed_repeats();
    for byte in [0x1E, 0x1E, 0x1E, 0x9E, 0x1E, 0x9E] {
        tap.feed(byte);
    }
    // Same for a prefixed key.
    for byte in [0xE0, 0x4B, 0xE0, 0x4B, 0xE0, 0xCB] {
        tap.feed(byte);
    }
    let records = drain_all(owner, 32)?;
    let edges: Vec<(u16, i32)> = records.iter().map(|r| (r.code, r.value)).collect();
    check!(
        edges
            == [
                (0x04, 1),
                (0x04, 0),
                (0x04, 1),
                (0x04, 0),
                (0x50, 1),
                (0x50, 0)
            ],
        "edges {edges:x?}"
    );
    check!(
        crate::input::raw_tap::suppressed_repeats() - before == 3,
        "repeat counter"
    );
    bus::reset();
    Ok(())
}
