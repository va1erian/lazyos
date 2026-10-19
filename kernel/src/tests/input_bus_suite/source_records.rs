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
use crate::input::sources::{self, class};
use crate::ipc::credentials::CAP_INPUT_SOURCE;

pub(super) fn clean() {
    sources::reset();
    bus::reset();
    fresh();
}

/// A consumer on the bus, owned by its own scratch task.
pub(super) fn consumer() -> Result<usize, String> {
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    Ok(owner)
}

pub(super) fn shape(records: &[RawEvent]) -> Vec<(u8, u8, u16, i32)> {
    records
        .iter()
        .map(|r| (r.device, r.kind, r.code, r.value))
        .collect()
}

/// `(device, SOURCE, class, registered)`.
pub(super) fn announced(device: u8, source_class: u8, registered: bool) -> (u8, u8, u16, i32) {
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
    /// One past the newest sequence number seen (a lost span counts).
    pub next: u64,
}

impl Announced {
    pub fn take(&mut self, record: &RawEvent) {
        let span = if record.kind == kind::DROPPED {
            record.value as u64
        } else {
            1
        };
        self.next = self.next.max(record.seq + span);
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

    /// No contradiction so far, and the stream's view is the table's: the
    /// same sources with the same classes, and the table's cut of the stream
    /// is just past the newest record seen (the caller has drained it all).
    pub fn matches_table(&self) -> Result<(), String> {
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        let table = sources::snapshot();
        let actual: BTreeMap<u8, u8> = table
            .entries()
            .iter()
            .map(|s| (s.device, s.class))
            .collect();
        check!(
            actual == self.live,
            "stream says {:?}, table says {actual:?}",
            self.live
        );
        check!(
            self.opened - self.closed == table.entries().len() as u64,
            "{} opened, {} closed, {} live",
            self.opened,
            self.closed,
            table.entries().len()
        );
        check!(
            table.next_seq == self.next.max(1),
            "the table is cut at {}, the stream is at {}",
            table.next_seq,
            self.next
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
    let table = sources::snapshot();
    check!(
        table.entries().len() == 1 && table.entries()[0].device == device_of(other),
        "the survivor's table {:?}",
        table.entries()
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
    // Until its release has been published the dead owner's source is still
    // in the table: the table never runs ahead of the stream.
    check!(
        sources::snapshot().entries().len() == 1,
        "a dead owner's source left the table before its close was announced"
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
    check!(
        sources::snapshot().entries().len() == 1,
        "the reclaimed slot is listed once, as the new source"
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
