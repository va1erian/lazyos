//! The source-table query, `SOURCES` (op 10 of syscall 25;
//! `docs/usb-gamepad-plan.md`, G3): a header with the bus's next sequence
//! number, then one entry per source in slot order. The table is exactly
//! consistent with the record stream: a slot is listed from its `SOURCE` 1 to
//! its `SOURCE` 0, whether or not its owner is alive, and the header says
//! where in the stream that is. Calls go through the real syscall 25 front end.

use alloc::collections::BTreeMap;

use super::source::{call, device_of, driver_task, failed, publish, rec, register};
use super::source_records::{clean, consumer, Announced};
use super::*;
use crate::input::rawsys::op;
use crate::input::sources::{self, class, ENTRY_BYTES, HEADER_BYTES, MAX_SOURCES};
use crate::ipc::credentials::{CAP_INPUT_RAW, CAP_INPUT_SOURCE};

const EPERM: i64 = 1;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;

/// A buffer of `len` bytes pre-filled with a sentinel, and the query's result.
fn query(len: usize) -> (u64, Vec<u8>) {
    let mut buf = alloc::vec![0xAAu8; len];
    let result = call(op::SOURCES, buf.as_mut_ptr() as u64, len as u64);
    (result, buf)
}

fn header(buf: &[u8]) -> u64 {
    u64::from_le_bytes(buf[..HEADER_BYTES].try_into().unwrap_or_default())
}

fn entry(id: u64, source_class: u8) -> [u8; ENTRY_BYTES] {
    let generation = ((id >> 8) as u16).to_le_bytes();
    [device_of(id), source_class, generation[0], generation[1]]
}

/// The query needs `CAP_INPUT_RAW` (not `CAP_INPUT_SOURCE`, not the kernel
/// task) and no consumer ring.
pub fn capability_gate() -> Result<(), String> {
    clean();
    driver_task(CAP_INPUT_SOURCE)?;
    register(class::KEYBOARD)?;
    let (result, buf) = query(64);
    check!(
        result == failed(EPERM) && buf.iter().all(|&b| b == 0xAA),
        "a source read the table ({result:#x})"
    );
    driver_task(0)?;
    check!(query(64).0 == failed(EPERM), "an unprivileged task read it");
    task::harness::switch_current(task::KERNEL_TASK);
    check!(query(64).0 == failed(EPERM), "the kernel task read it");
    let reader = driver_task(CAP_INPUT_RAW)?;
    check!(
        bus::consumer_of(reader).is_none(),
        "the reader holds a ring"
    );
    let (result, buf) = query(64);
    check!(result == 1, "query without a ring -> {result:#x}");
    check!(
        buf[HEADER_BYTES + 1] == class::KEYBOARD,
        "entry {:x?}",
        &buf[..HEADER_BYTES + 4]
    );
    clean();
    Ok(())
}

/// The table in slot order with each source's class and generation; a closed
/// source leaves it and a reused slot reports its new class and generation.
pub fn contents() -> Result<(), String> {
    clean();
    let reader = driver_task(CAP_INPUT_RAW)?;
    let (result, buf) = query(64);
    check!(result == 0, "an empty table reported entries");
    check!(header(&buf) == 1, "an empty bus is at seq {}", header(&buf));
    let mouse_driver = driver_task(CAP_INPUT_SOURCE)?;
    let first = register(class::POINTER)?;
    driver_task(CAP_INPUT_SOURCE)?;
    let second = register(class::GAMEPAD)?;
    task::harness::switch_current(reader);
    let (result, buf) = query(HEADER_BYTES + MAX_SOURCES * ENTRY_BYTES);
    let mut want = Vec::new();
    want.extend_from_slice(&entry(first, class::POINTER));
    want.extend_from_slice(&entry(second, class::GAMEPAD));
    check!(
        result == 2 && buf[HEADER_BYTES..HEADER_BYTES + 8] == want[..],
        "table {result:#x} {:x?}",
        &buf[..HEADER_BYTES + 12]
    );
    check!(
        buf[HEADER_BYTES + 8..].iter().all(|&b| b == 0xAA),
        "the query wrote past its entries"
    );
    // Close the first, register a keyboard: the slot is reused, the class and
    // generation are the new ones, and the old id is not in the table.
    task::harness::switch_current(mouse_driver);
    check!(call(op::CLOSE_SOURCE, first, 0) == 0, "close");
    task::harness::switch_current(reader);
    let (result, buf) = query(64);
    check!(
        result == 1 && buf[HEADER_BYTES..HEADER_BYTES + 4] == entry(second, class::GAMEPAD),
        "after close {result:#x} {:x?}",
        &buf[..HEADER_BYTES + 8]
    );
    task::harness::switch_current(mouse_driver);
    let third = register(class::KEYBOARD)?;
    check!(
        device_of(third) == device_of(first) && third != first,
        "slot not reused"
    );
    task::harness::switch_current(reader);
    let (result, buf) = query(64);
    let mut want = Vec::new();
    want.extend_from_slice(&entry(third, class::KEYBOARD));
    want.extend_from_slice(&entry(second, class::GAMEPAD));
    check!(
        result == 2
            && buf[HEADER_BYTES..HEADER_BYTES + 8] == want[..]
            && buf[HEADER_BYTES + 2..HEADER_BYTES + 4] != entry(first, 0)[2..4],
        "reused slot {result:#x} {:x?}",
        &buf[..HEADER_BYTES + 8]
    );
    clean();
    Ok(())
}

/// A source whose owner died without teardown stays in the table until the
/// slot is released (its releases and `SOURCE` 0 published), so the table
/// never says a source is gone while its records are still to come.
pub fn dead_owner_stays_listed_until_released() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let dead = task::MAX_TASKS - 1;
    check!(!task::live(dead), "slot {dead} unexpectedly live");
    let id = sources::register(dead, class::GAMEPAD).map_err(|e| format!("{e:?}"))?;
    let held = [sources::Record {
        kind: kind::BUTTON,
        code: 4,
        value: 1,
    }];
    sources::publish(id, dead, &held).map_err(|e| format!("{e:?}"))?;
    let table = sources::snapshot();
    check!(
        table.entries().len() == 1 && table.entries()[0].class == class::GAMEPAD,
        "a dead owner's source is not listed: {:?}",
        table.entries()
    );
    // The stream has not announced its close yet.
    let before = drain_all(reader, 16)?;
    check!(
        before
            .iter()
            .all(|r| !(r.kind == kind::SOURCE && r.value == 0)),
        "close announced early"
    );
    // The slot is released by the next registration: releases, close, then
    // the new source, and the table follows in step.
    driver_task(CAP_INPUT_RAW | CAP_INPUT_SOURCE)?;
    let fresh_id = register(class::POINTER)?;
    let after = sources::snapshot();
    check!(
        after.entries().len() == 1 && after.entries()[0].class == class::POINTER,
        "reclaimed table {:?}",
        after.entries()
    );
    let (result, buf) = query(64);
    check!(
        result == 1 && buf[HEADER_BYTES..HEADER_BYTES + 4] == entry(fresh_id, class::POINTER),
        "query after reclaim {result:#x}"
    );
    let records = drain_all(reader, 16)?;
    check!(
        shape_of(&records)
            == [
                (device_of(id), kind::BUTTON, 4, 0),
                (device_of(id), kind::SOURCE, u16::from(class::GAMEPAD), 0),
                (device_of(id), kind::SOURCE, u16::from(class::POINTER), 1),
            ],
        "reclaim stream {:x?}",
        shape_of(&records)
    );
    clean();
    Ok(())
}

fn shape_of(records: &[RawEvent]) -> Vec<(u8, u8, u16, i32)> {
    super::source_records::shape(records)
}

/// The header splits the stream exactly: replaying the `SOURCE` records
/// below `next_seq` gives the table, replaying all of them gives the table
/// read later, and a record published at `next_seq` is the first the cut
/// does not cover.
pub fn header_cuts_the_stream() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let reader_raw = driver_task(CAP_INPUT_RAW)?;
    let keyboard_driver = driver_task(CAP_INPUT_SOURCE)?;
    let pad = register(class::GAMEPAD)?;
    publish(pad, &[rec(kind::BUTTON, 1, 1)]);
    // Records drained before the read are below the cut.
    let early = drain_all(reader, 16)?;
    let last_early = early.last().map(|r| r.seq).unwrap_or(0);
    task::harness::switch_current(reader_raw);
    let (result, buf) = query(64);
    check!(result == 1, "first table has {result} entries");
    let first_cut = header(&buf);
    check!(
        first_cut == last_early + 1,
        "cut {first_cut}, drained through {last_early}"
    );
    // Then the pad goes and a mouse takes its slot; those records are at or
    // beyond the cut.
    task::harness::switch_current(keyboard_driver);
    check!(call(op::CLOSE_SOURCE, pad, 0) == 0, "close");
    let mouse = register(class::POINTER)?;
    let late = drain_all(reader, 16)?;
    check!(
        late.first().is_some_and(|r| r.seq == first_cut),
        "the first record after the read is {:?}, the cut is {first_cut}",
        late.first()
    );
    // The table seen at the cut is the replay of everything below it.
    let mut replay = Announced::default();
    early
        .iter()
        .chain(late.iter())
        .filter(|r| r.seq < first_cut)
        .for_each(|r| replay.take(r));
    check!(
        replay.live == BTreeMap::from([(device_of(pad), class::GAMEPAD)]),
        "replay below the cut {:?}",
        replay.live
    );
    // And the later table is the replay of everything.
    task::harness::switch_current(reader_raw);
    let (result, buf) = query(64);
    check!(
        result == 1 && buf[HEADER_BYTES..HEADER_BYTES + 4] == entry(mouse, class::POINTER),
        "second table {result:#x}"
    );
    let mut all = Announced::default();
    early.iter().chain(late.iter()).for_each(|r| all.take(r));
    check!(
        header(&buf) == all.next && all.matches_table().is_ok(),
        "second cut {} vs stream {}: {:?}",
        header(&buf),
        all.next,
        all.matches_table()
    );
    clean();
    Ok(())
}

/// Too small a buffer is `EINVAL`, a header-only buffer gets the header and
/// no entries, a short buffer gets whole entries only, no buffer is a fault,
/// and a full table reports all `MAX_SOURCES`.
pub fn bounds_and_faults() -> Result<(), String> {
    clean();
    let reader = driver_task(CAP_INPUT_RAW)?;
    driver_task(CAP_INPUT_SOURCE)?;
    for _ in 0..MAX_SOURCES {
        register(class::POINTER)?;
    }
    task::harness::switch_current(reader);
    for capacity in [0u64, 1, 7] {
        let mut bytes = alloc::vec![0xAAu8; 64];
        let result = call(op::SOURCES, bytes.as_mut_ptr() as u64, capacity);
        check!(
            result == failed(EINVAL) && bytes.iter().all(|&b| b == 0xAA),
            "capacity {capacity}: {result:#x} (a buffer under the header must be refused)"
        );
    }
    let next = bus::next_seq();
    for (capacity, want) in [
        (8u64, 0u64),
        (11, 0),
        (12, 1),
        (15, 1),
        (16, 2),
        (8 + 37, 9),
    ] {
        let mut bytes = alloc::vec![0xAAu8; 128];
        let result = call(op::SOURCES, bytes.as_mut_ptr() as u64, capacity);
        check!(
            result == want,
            "capacity {capacity}: {result:#x}, wanted {want}"
        );
        check!(
            header(&bytes) == next,
            "capacity {capacity}: header {}",
            header(&bytes)
        );
        let written = HEADER_BYTES + want as usize * ENTRY_BYTES;
        check!(
            bytes[written..].iter().all(|&b| b == 0xAA),
            "capacity {capacity}: wrote past {written} bytes"
        );
    }
    let (result, buf) = query(HEADER_BYTES + MAX_SOURCES * ENTRY_BYTES + 32);
    check!(result == MAX_SOURCES as u64, "full table -> {result:#x}");
    check!(
        buf[HEADER_BYTES + MAX_SOURCES * ENTRY_BYTES..]
            .iter()
            .all(|&b| b == 0xAA),
        "wrote past the table"
    );
    let devices: Vec<u8> = buf[HEADER_BYTES..]
        .chunks_exact(ENTRY_BYTES)
        .take(MAX_SOURCES)
        .map(|e| e[0])
        .collect();
    let want: Vec<u8> = (0..MAX_SOURCES).map(sources::device_of).collect();
    check!(devices == want, "slot order {devices:x?}");
    check!(
        call(op::SOURCES, 0, 64) == failed(EFAULT),
        "null buffer accepted"
    );
    let previous = crate::user_ptr::set_trust_kernel_pointers(false);
    let unmapped = call(op::SOURCES, 0x10, 64);
    crate::user_ptr::set_trust_kernel_pointers(previous);
    check!(
        unmapped == failed(EFAULT),
        "unmapped buffer -> {unmapped:#x}"
    );
    clean();
    Ok(())
}
