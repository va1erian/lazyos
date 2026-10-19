//! The kernel-originated `SOURCE` record and the source-table query
//! (`docs/usb-gamepad-plan.md`, G3): registering a source announces it, every
//! path that frees a slot announces the close after the releases, a driver can
//! never publish one, and `SOURCES` (op 10) reports the live table. Calls go
//! through the real syscall 25 front end.

use alloc::collections::BTreeMap;

use super::source::{call, device_of, driver_task, failed, publish, rec, register};
use super::*;
use crate::input::bus::axis;
use crate::input::rawsys::op;
use crate::input::sources::{self, class, ENTRY_BYTES, MAX_SOURCES};
use crate::ipc::credentials::{CAP_INPUT_RAW, CAP_INPUT_SOURCE};

const EPERM: i64 = 1;
const EFAULT: i64 = 14;

fn clean() {
    sources::reset();
    bus::reset();
    fresh();
}

/// A consumer on the bus, owned by its own scratch task.
fn consumer() -> Result<usize, String> {
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    Ok(owner)
}

fn shape(records: &[RawEvent]) -> Vec<(u8, u8, u16, i32)> {
    records
        .iter()
        .map(|r| (r.device, r.kind, r.code, r.value))
        .collect()
}

/// `(device, SOURCE, class, registered)`.
fn announced(device: u8, source_class: u8, registered: bool) -> (u8, u8, u16, i32) {
    (
        device,
        kind::SOURCE,
        u16::from(source_class),
        i32::from(registered),
    )
}

/// What a consumer of the stream knows about the sources: rebuilt from the
/// `SOURCE` records alone, so a stress test can compare it with the table.
#[derive(Default)]
pub(super) struct Announced {
    pub live: BTreeMap<u8, u8>,
    pub opened: u64,
    pub closed: u64,
    /// The first announcement that contradicted the previous ones: a
    /// registration of a device already registered, a close of one that was
    /// not, a close naming another class, a value that is neither.
    pub fault: Option<String>,
}

impl Announced {
    pub fn take(&mut self, record: &RawEvent) {
        if record.kind != kind::SOURCE {
            return;
        }
        let announced = u8::try_from(record.code).ok();
        let problem = match (record.value, announced) {
            (1, Some(source_class)) => {
                self.opened += 1;
                self.live
                    .insert(record.device, source_class)
                    .map(|_| "registered twice")
            }
            (0, Some(source_class)) => {
                self.closed += 1;
                match self.live.remove(&record.device) {
                    None => Some("closed while not registered"),
                    Some(held) if held != source_class => Some("closed under another class"),
                    Some(_) => None,
                }
            }
            _ => Some("malformed"),
        };
        if let (Some(problem), None) = (problem, &self.fault) {
            self.fault = Some(format!("{record:?}: {problem}"));
        }
    }

    /// No contradiction so far, and the stream's view is the table's.
    pub fn matches_table(&self) -> Result<(), String> {
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        let (table, count) = sources::snapshot();
        let actual: BTreeMap<u8, u8> = table[..count].iter().map(|s| (s.device, s.class)).collect();
        check!(
            actual == self.live,
            "stream says {:?}, table says {actual:?}",
            self.live
        );
        check!(
            self.opened - self.closed == count as u64,
            "{} opened, {} closed, {count} live",
            self.opened,
            self.closed
        );
        Ok(())
    }
}

/// Registering announces the source with the kernel's device id; closing
/// announces it after the keys, buttons and axes it releases, once.
pub fn register_and_close_announce() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    let pad = register(class::GAMEPAD)?;
    let (kbd_dev, pad_dev) = (device_of(keyboard), device_of(pad));
    let records = drain_all(reader, 16)?;
    gapless(&records, 1)?;
    check!(
        shape(&records)
            == [
                announced(kbd_dev, class::KEYBOARD, true),
                announced(pad_dev, class::GAMEPAD, true)
            ],
        "registered: {:x?}",
        shape(&records)
    );
    publish(keyboard, &[rec(kind::KEY, 0x04, 1)]);
    publish(
        pad,
        &[
            rec(kind::BUTTON, 3, 1),
            rec(kind::AXIS, axis::usage::X, 1000),
        ],
    );
    let _ = drain_all(reader, 16)?;
    check!(call(op::CLOSE_SOURCE, pad, 0) == 0, "close pad");
    let records = drain_all(reader, 16)?;
    check!(
        shape(&records)
            == [
                (pad_dev, kind::BUTTON, 3, 0),
                (pad_dev, kind::AXIS, axis::usage::X, 0),
                announced(pad_dev, class::GAMEPAD, false),
            ],
        "pad closed: {:x?}",
        shape(&records)
    );
    check!(call(op::CLOSE_SOURCE, keyboard, 0) == 0, "close keyboard");
    let records = drain_all(reader, 16)?;
    check!(
        shape(&records)
            == [
                (kbd_dev, kind::KEY, 0x04, 0),
                announced(kbd_dev, class::KEYBOARD, false)
            ],
        "keyboard closed: {:x?}",
        shape(&records)
    );
    // A second close is refused and announces nothing.
    check!(
        call(op::CLOSE_SOURCE, pad, 0) == failed(9),
        "double close accepted"
    );
    check!(
        drain_all(reader, 16)?.is_empty(),
        "a refused close published"
    );
    clean();
    Ok(())
}

/// A driver that dies has every source it owned announced closed, after each
/// one's releases and in slot order; another driver's source is untouched.
pub fn task_teardown_announces() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let driver = driver_task(CAP_INPUT_SOURCE)?;
    let mouse = register(class::POINTER)?;
    let tablet = register(class::TABLET)?;
    driver_task(CAP_INPUT_SOURCE)?;
    let other = register(class::KEYBOARD)?;
    task::harness::switch_current(driver);
    publish(mouse, &[rec(kind::BUTTON, 1, 1)]);
    let _ = drain_all(reader, 16)?;
    sources::teardown_task(driver);
    let records = drain_all(reader, 16)?;
    check!(
        shape(&records)
            == [
                (device_of(mouse), kind::BUTTON, 1, 0),
                announced(device_of(mouse), class::POINTER, false),
                announced(device_of(tablet), class::TABLET, false),
            ],
        "teardown: {:x?}",
        shape(&records)
    );
    let (table, count) = sources::snapshot();
    check!(
        count == 1 && table[0].device == device_of(other),
        "the survivor's table {:?}",
        &table[..count]
    );
    clean();
    Ok(())
}

/// `register` reusing the slot of a dead owner (its teardown never ran)
/// announces the old source's close, after its releases, and then the new
/// source under the same device id with its own class.
pub fn dead_owner_reclaim_announces() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let dead = task::MAX_TASKS - 1;
    check!(!task::live(dead), "slot {dead} unexpectedly live");
    let old = sources::register(dead, class::POINTER).map_err(|e| format!("{e:?}"))?;
    let device = device_of(old);
    let held = [sources::Record {
        kind: kind::BUTTON,
        code: 2,
        value: 1,
    }];
    sources::publish(old, dead, &held).map_err(|e| format!("{e:?}"))?;
    let _ = drain_all(reader, 16)?;
    check!(
        sources::snapshot().1 == 0,
        "a dead owner's source is in the table"
    );
    driver_task(CAP_INPUT_SOURCE)?;
    let fresh_id = register(class::GAMEPAD)?;
    check!(
        device_of(fresh_id) == device && fresh_id != old,
        "the dead slot was not the one reused ({old:#x} -> {fresh_id:#x})"
    );
    let records = drain_all(reader, 16)?;
    // The old registration and its button took sequence numbers 1 and 2.
    gapless(&records, 3)?;
    check!(
        shape(&records)
            == [
                (device, kind::BUTTON, 2, 0),
                announced(device, class::POINTER, false),
                announced(device, class::GAMEPAD, true),
            ],
        "reclaim: {:x?}",
        shape(&records)
    );
    clean();
    Ok(())
}

/// A driver cannot forge `SOURCE`, whatever its class, code or value; the
/// valid records around it still go out, and the forgery is counted.
pub fn driver_cannot_forge_source() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let ids = [
        register(class::KEYBOARD)?,
        register(class::POINTER)?,
        register(class::TABLET)?,
        register(class::GAMEPAD)?,
    ];
    let registered = drain_all(reader, 16)?;
    check!(
        registered.len() == 4,
        "registered {:x?}",
        shape(&registered)
    );
    let forged = [
        rec(kind::SOURCE, u16::from(class::GAMEPAD), 1),
        rec(kind::SOURCE, u16::from(class::KEYBOARD), 0),
        rec(kind::SOURCE, 0, 0),
        rec(kind::SOURCE, 0xFFFF, i32::MIN),
    ];
    let before = sources::counters().0;
    for (n, &id) in ids.iter().enumerate() {
        let mut batch = forged.to_vec();
        batch.push(match n {
            0 => rec(kind::KEY, 0x04, 1),
            1 | 2 => rec(kind::BUTTON, 1, 1),
            _ => rec(kind::AXIS, axis::usage::X, 5),
        });
        check!(
            publish(id, &batch) == 1,
            "class {} accepted a forged SOURCE",
            n + 1
        );
    }
    check!(
        sources::counters().0 - before == 16,
        "rejected {} records",
        sources::counters().0 - before
    );
    let records = drain_all(reader, 16)?;
    check!(
        records.len() == 4 && records.iter().all(|r| r.kind != kind::SOURCE),
        "a forged SOURCE reached the bus: {:x?}",
        shape(&records)
    );
    clean();
    Ok(())
}

/// `SOURCE` is an edge: two identical announcements stay two records.
pub fn source_records_never_merge() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    check!(
        !bus::pointer::mergeable(kind::SOURCE),
        "SOURCE is marked mergeable"
    );
    let device = sources::device_of(0);
    for _ in 0..3 {
        bus::publish(device, kind::SOURCE, u16::from(class::POINTER), 1);
    }
    let records = drain_all(reader, 8)?;
    gapless(&records, 1)?;
    check!(records.len() == 3, "merged to {:x?}", shape(&records));
    clean();
    Ok(())
}

/// A buffer of `len` bytes pre-filled with a sentinel, and the query's result.
fn query(len: usize) -> (u64, Vec<u8>) {
    let mut buf = alloc::vec![0xAAu8; len];
    let result = call(op::SOURCES, buf.as_mut_ptr() as u64, len as u64);
    (result, buf)
}

/// The query needs `CAP_INPUT_RAW` (not `CAP_INPUT_SOURCE`, not the kernel
/// task) and no consumer ring.
pub fn source_table_capability_gate() -> Result<(), String> {
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
    check!(buf[1] == class::KEYBOARD, "entry {:x?}", &buf[..4]);
    clean();
    Ok(())
}

/// The table in slot order with each source's class and generation; a closed
/// source leaves it, a reused slot reports its new class and generation, and
/// a dead owner's source is not live.
pub fn source_table_contents() -> Result<(), String> {
    clean();
    let reader = driver_task(CAP_INPUT_RAW)?;
    check!(query(64).0 == 0, "an empty table reported entries");
    let mouse_driver = driver_task(CAP_INPUT_SOURCE)?;
    let first = register(class::POINTER)?;
    driver_task(CAP_INPUT_SOURCE)?;
    let second = register(class::GAMEPAD)?;
    task::harness::switch_current(reader);
    let entry = |id: u64, source_class: u8| {
        let generation = ((id >> 8) as u16).to_le_bytes();
        [device_of(id), source_class, generation[0], generation[1]]
    };
    let (result, buf) = query(MAX_SOURCES * ENTRY_BYTES);
    let mut want = Vec::new();
    want.extend_from_slice(&entry(first, class::POINTER));
    want.extend_from_slice(&entry(second, class::GAMEPAD));
    check!(
        result == 2 && buf[..8] == want[..],
        "table {result:#x} {:x?}",
        &buf[..12]
    );
    check!(
        buf[8..].iter().all(|&b| b == 0xAA),
        "the query wrote past its entries"
    );
    // Close the first, register a keyboard: the slot is reused, the class and
    // generation are the new ones, and the old id is not in the table.
    task::harness::switch_current(mouse_driver);
    check!(call(op::CLOSE_SOURCE, first, 0) == 0, "close");
    task::harness::switch_current(reader);
    let (result, buf) = query(64);
    check!(
        result == 1 && buf[..4] == entry(second, class::GAMEPAD),
        "after close {result:#x} {:x?}",
        &buf[..8]
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
        result == 2 && buf[..8] == want[..] && buf[2..4] != entry(first, 0)[2..4],
        "reused slot {result:#x} {:x?}",
        &buf[..8]
    );
    // A source whose owner died without teardown is not live.
    sources::reset();
    let dead = task::MAX_TASKS - 1;
    sources::register(dead, class::POINTER).map_err(|e| format!("{e:?}"))?;
    check!(query(64).0 == 0, "a dead owner's source is in the table");
    clean();
    Ok(())
}

/// A short buffer gets whole entries only; no buffer is a fault; a full
/// table reports all `MAX_SOURCES`.
pub fn source_table_bounds_and_faults() -> Result<(), String> {
    clean();
    let reader = driver_task(CAP_INPUT_RAW)?;
    driver_task(CAP_INPUT_SOURCE)?;
    for _ in 0..MAX_SOURCES {
        register(class::POINTER)?;
    }
    task::harness::switch_current(reader);
    for (capacity, want) in [(0, 0), (3, 0), (4, 1), (7, 1), (8, 2), (37, 9)] {
        let mut bytes = alloc::vec![0xAAu8; 64];
        let result = call(op::SOURCES, bytes.as_mut_ptr() as u64, capacity);
        check!(
            result == want,
            "capacity {capacity}: {result:#x}, wanted {want}"
        );
        let written = want as usize * ENTRY_BYTES;
        check!(
            bytes[written..].iter().all(|&b| b == 0xAA),
            "capacity {capacity}: wrote past {written} bytes"
        );
    }
    let (result, buf) = query(MAX_SOURCES * ENTRY_BYTES + 32);
    check!(result == MAX_SOURCES as u64, "full table -> {result:#x}");
    check!(
        buf[MAX_SOURCES * ENTRY_BYTES..].iter().all(|&b| b == 0xAA),
        "wrote past the table"
    );
    let devices: Vec<u8> = buf
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
