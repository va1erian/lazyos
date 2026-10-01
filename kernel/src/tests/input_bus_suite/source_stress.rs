//! Input sources under sustained load: register/close generations and many
//! producers publishing mixed valid and hostile records. The invariants are
//! the bus contract (gapless), the kernel's stamping (a device id belongs to
//! a live source, a record fits its source's class) and the release promise
//! (once every source is closed, no key or button is still held).

use alloc::collections::BTreeMap;

use super::source::{call, driver_task, failed, publish, rec, register};
use super::*;
use crate::input::bus::pointer;
use crate::input::rawsys::op;
use crate::input::sources::{self, class, Record, BURST, FIRST_DEVICE, MAX_BATCH, MAX_SOURCES};
use crate::ipc::credentials::CAP_INPUT_SOURCE;

const EBADF: i64 = 9;

/// Deterministic xorshift64*, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound
    }
}

/// Held keys and buttons per device, rebuilt from what the bus carried.
#[derive(Default)]
struct Held(BTreeMap<(u8, u8, u16), ()>);

impl Held {
    fn take(&mut self, record: &RawEvent) {
        if matches!(record.kind, kind::KEY | kind::BUTTON) {
            let key = (record.device, record.kind, record.code);
            if record.value == 1 {
                self.0.insert(key, ());
            } else {
                self.0.remove(&key);
            }
        }
    }
}

/// Twenty thousand register/close cycles from three tasks, each leaving a
/// key down: every close releases it, no id is ever valid twice, and the
/// table ends empty.
pub fn generations() -> Result<(), String> {
    const CYCLES: u64 = 20_000;
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    let drivers = [
        driver_task(CAP_INPUT_SOURCE)?,
        driver_task(CAP_INPUT_SOURCE)?,
        driver_task(CAP_INPUT_SOURCE)?,
    ];
    let mut held = Held::default();
    let mut expected = 1u64;
    let mut previous: Option<(usize, u64)> = None;
    for cycle in 0..CYCLES {
        let driver = drivers[(cycle % 3) as usize];
        task::harness::switch_current(driver);
        if cycle % 256 == 0 {
            // Buckets outlive their sources; stand in for time passing.
            sources::refill_all();
        }
        let id = register(class::KEYBOARD)?;
        let usage = 0x04 + (cycle % 0x60) as u16;
        check!(
            publish(id, &[rec(kind::KEY, usage, 1)]) == 1,
            "cycle {cycle}: press"
        );
        if let Some((owner, stale)) = previous {
            task::harness::switch_current(owner);
            check!(
                publish(stale, &[rec(kind::KEY, 4, 1)]) == failed(EBADF),
                "cycle {cycle}: a closed id published"
            );
            task::harness::switch_current(driver);
        }
        check!(call(op::CLOSE_SOURCE, id, 0) == 0, "cycle {cycle}: close");
        previous = Some((driver, id));
        let records = drain_all(reader, 64)?;
        expected = gapless(&records, expected)?.0;
        records.iter().for_each(|r| held.take(r));
        check!(held.0.is_empty(), "cycle {cycle}: still held {:?}", held.0);
    }
    // Every slot is free again.
    task::harness::switch_current(drivers[0]);
    for _ in 0..MAX_SOURCES {
        register(class::POINTER)?;
    }
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}

/// A random record for `source_class`: mostly valid, sometimes hostile.
fn record_for(rng: &mut Rng, source_class: u8) -> Record {
    let edge = rng.below(2) as i32;
    match (source_class, rng.below(8)) {
        // Hostile: any kind, code and value at all.
        (_, 0) => rec(
            rng.below(9) as u8,
            rng.below(300) as u16,
            rng.below(300) as i32 - 150,
        ),
        (class::KEYBOARD, _) => rec(kind::KEY, 0x04 + rng.below(0xE4) as u16, edge),
        (_, 1 | 2) => rec(kind::BUTTON, 1 + rng.below(5) as u16, edge),
        (_, 3) => rec(kind::SCROLL, rng.below(2) as u16, rng.below(7) as i32 - 3),
        (class::POINTER, _) => rec(
            kind::REL_MOTION,
            0,
            pointer::pack_rel(rng.below(41) as i16 - 20, rng.below(41) as i16 - 20),
        ),
        _ => rec(
            kind::ABS_MOTION,
            0,
            pointer::pack_abs(rng.below(0x1_0000) as u16, rng.below(0x1_0000) as u16),
        ),
    }
}

/// Whether a record on the bus fits the class of the source that sent it.
fn fits(source_class: u8, record: &RawEvent) -> bool {
    match record.kind {
        kind::KEY => source_class == class::KEYBOARD && (0x04..=0xE7).contains(&record.code),
        kind::BUTTON | kind::SCROLL => source_class != class::KEYBOARD,
        kind::REL_MOTION => source_class == class::POINTER,
        kind::ABS_MOTION => source_class == class::TABLET,
        _ => false,
    }
}

/// Six sources across three tasks publish a million records; each is closed
/// and re-registered before its bucket runs dry, and the buckets are then
/// refilled (re-registering alone no longer does that). Every record on the bus fits
/// its source, the stream is gapless, and closing everything leaves nothing
/// held.
pub fn many_producers() -> Result<(), String> {
    const TOTAL: u64 = 1_000_000;
    const CLASSES: [u8; 6] = [
        class::KEYBOARD,
        class::POINTER,
        class::TABLET,
        class::KEYBOARD,
        class::POINTER,
        class::TABLET,
    ];
    sources::reset();
    bus::reset();
    fresh();
    let reader = scratch()?;
    bus::open(reader).map_err(|e| format!("{e:?}"))?;
    let drivers = [
        driver_task(CAP_INPUT_SOURCE)?,
        driver_task(CAP_INPUT_SOURCE)?,
        driver_task(CAP_INPUT_SOURCE)?,
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // (owner, id, accepted since registration) per source.
    let mut live: Vec<(usize, u64, u64)> = Vec::new();
    // The class behind each device id, from the ids the kernel handed out.
    let mut device_class: BTreeMap<u8, u8> = BTreeMap::new();
    for (n, &source_class) in CLASSES.iter().enumerate() {
        task::harness::switch_current(drivers[n % 3]);
        let id = register(source_class)?;
        device_class.insert(sources::device_of((id & 0xFF) as usize), source_class);
        live.push((drivers[n % 3], id, 0));
    }
    let (mut held, mut expected, mut published) = (Held::default(), 1u64, 0u64);
    let mut batch = Vec::with_capacity(MAX_BATCH);
    while published < TOTAL {
        let n = rng.below(CLASSES.len() as u64) as usize;
        let (owner, id, used) = live[n];
        task::harness::switch_current(owner);
        if used + MAX_BATCH as u64 > u64::from(BURST) {
            // Close (releasing what it held), re-register, then let time
            // refill the buckets.
            check!(call(op::CLOSE_SOURCE, id, 0) == 0, "close");
            let records = drain_all(reader, 128)?;
            expected = gapless(&records, expected)?.0;
            records.iter().for_each(|r| held.take(r));
            let id = register(CLASSES[n])?;
            device_class.insert(sources::device_of((id & 0xFF) as usize), CLASSES[n]);
            sources::refill_all();
            for entry in live.iter_mut() {
                entry.2 = 0;
            }
            live[n] = (owner, id, 0);
            continue;
        }
        batch.clear();
        for _ in 0..1 + rng.below(MAX_BATCH as u64) {
            batch.push(record_for(&mut rng, CLASSES[n]));
        }
        let accepted = publish(id, &batch);
        check!(accepted as usize <= batch.len(), "accepted {accepted:#x}");
        live[n].2 += batch.len() as u64;
        published += batch.len() as u64;
        let records = drain_all(reader, 128)?;
        expected = gapless(&records, expected)?.0;
        for record in &records {
            check!(record.device >= FIRST_DEVICE, "device {:#x}", record.device);
            let source_class = *device_class
                .get(&record.device)
                .ok_or("a record from a device no source holds")?;
            check!(
                fits(source_class, record),
                "{record:?} from a class-{source_class} source"
            );
            held.take(record);
        }
    }
    for &(owner, id, _) in &live {
        task::harness::switch_current(owner);
        check!(call(op::CLOSE_SOURCE, id, 0) == 0, "final close");
    }
    let records = drain_all(reader, 128)?;
    gapless(&records, expected)?;
    records.iter().for_each(|r| held.take(r));
    check!(
        held.0.is_empty(),
        "stuck after closing everything: {:?}",
        held.0
    );
    let (rejected, throttled) = sources::counters();
    check!(
        rejected > 10_000,
        "hostile records were not exercised ({rejected})"
    );
    check!(throttled == 0, "a bucket ran dry ({throttled})");
    sources::reset();
    bus::reset();
    fresh();
    Ok(())
}
