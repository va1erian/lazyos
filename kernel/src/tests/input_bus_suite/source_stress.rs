//! Input sources under sustained load: register/close generations and many
//! producers publishing mixed valid and hostile records. The invariants are
//! the bus contract (gapless), the kernel's stamping (a device id belongs to
//! a live source, a record fits its source's class) and the release promise
//! (once every source is closed, no key or button is still held). The
//! kernel's `SOURCE` records are held to their own contract too: each close is
//! announced exactly once, a registration only for a device not registered,
//! and what the stream says is live is what the source table says.

use alloc::collections::BTreeMap;

use super::source::{call, device_of, driver_task, failed, publish, rec, register};
use super::source_records::Announced;
use super::*;
use crate::input::bus::{axis, pointer};
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

/// Held keys and buttons per device, rebuilt from what the bus carried, and
/// the records the consumer ring dropped on the way (a `Dropped` marker's
/// `value`).
#[derive(Default)]
struct Held {
    held: BTreeMap<(u8, u8, u16), ()>,
    dropped: u64,
}

impl Held {
    fn take(&mut self, record: &RawEvent) {
        if record.kind == kind::DROPPED {
            self.dropped += record.value as u64;
            return;
        }
        if matches!(record.kind, kind::KEY | kind::BUTTON) {
            let key = (record.device, record.kind, record.code);
            if record.value == 1 {
                self.held.insert(key, ());
            } else {
                self.held.remove(&key);
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
    let mut announced = Announced::default();
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
        let table = sources::snapshot();
        let listed = table.entries();
        check!(
            listed.len() == 1
                && listed[0].device == device_of(id)
                && listed[0].class == class::KEYBOARD
                && u64::from(listed[0].generation) == id >> 8,
            "cycle {cycle}: the table lists {listed:?} for {id:#x}",
        );
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
        records.iter().for_each(|r| announced.take(r));
        check!(
            held.held.is_empty(),
            "cycle {cycle}: still held {:?} (dropped {})",
            held.held,
            held.dropped
        );
        // The close is announced after the key's release, once.
        check!(
            records
                .last()
                .is_some_and(|r| (r.kind, r.value) == (kind::SOURCE, 0)),
            "cycle {cycle}: the close was not announced last"
        );
        announced
            .matches_table()
            .map_err(|e| format!("cycle {cycle}: {e}"))?;
        check!(
            announced.opened == cycle + 1 && announced.closed == cycle + 1,
            "cycle {cycle}: {} opened, {} closed",
            announced.opened,
            announced.closed
        );
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
            rng.below(11) as u8,
            rng.below(300) as u16,
            rng.below(300) as i32 - 150,
        ),
        (class::KEYBOARD, _) => rec(kind::KEY, 0x04 + rng.below(0xE4) as u16, edge),
        (class::GAMEPAD, 1..=3) => axis_record(rng),
        (class::GAMEPAD, 4) => rec(kind::BUTTON, 1 + rng.below(32) as u16, edge),
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

/// A gamepad axis record: a known usage, a value in range most of the time.
fn axis_record(rng: &mut Rng) -> Record {
    const USAGES: [u16; 9] = [
        axis::usage::X,
        axis::usage::Y,
        axis::usage::Z,
        axis::usage::RX,
        axis::usage::RY,
        axis::usage::RZ,
        axis::usage::SLIDER,
        axis::usage::DIAL,
        axis::usage::HAT,
    ];
    let usage = USAGES[rng.below(9) as usize];
    let value = if usage == axis::usage::HAT {
        rng.below(10) as i32 - 1
    } else {
        rng.below(0x1_0002) as i32 - 0x8001
    };
    rec(kind::AXIS, usage, value)
}

/// Whether a record on the bus fits the class of the source that sent it.
fn fits(source_class: u8, record: &RawEvent) -> bool {
    match record.kind {
        kind::KEY => source_class == class::KEYBOARD && (0x04..=0xE7).contains(&record.code),
        kind::BUTTON => {
            source_class != class::KEYBOARD && {
                let highest = if source_class == class::GAMEPAD {
                    32
                } else {
                    5
                };
                (1..=highest).contains(&record.code)
            }
        }
        kind::SCROLL => source_class != class::KEYBOARD,
        kind::REL_MOTION => source_class == class::POINTER,
        kind::ABS_MOTION => source_class == class::TABLET,
        kind::AXIS => {
            source_class == class::GAMEPAD
                && (record.code == axis::usage::HAT
                    || (axis::usage::X..=axis::usage::DIAL).contains(&record.code))
                && if record.code == axis::usage::HAT {
                    (0..=axis::HAT_CENTRED).contains(&record.value)
                } else {
                    (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&record.value)
                }
        }
        kind::DEVICE => true,
        // Only the kernel's own announcement of this source.
        kind::SOURCE => record.code == u16::from(source_class) && matches!(record.value, 0 | 1),
        _ => false,
    }
}

/// Eight sources across three tasks publish a million records; each is closed
/// and re-registered before its bucket runs dry, and the buckets are then
/// refilled (re-registering alone no longer does that). Every record on the bus fits
/// its source, the stream is gapless, and closing everything leaves nothing
/// held.
pub fn many_producers() -> Result<(), String> {
    const TOTAL: u64 = 1_000_000;
    const CLASSES: [u8; 8] = [
        class::KEYBOARD,
        class::POINTER,
        class::TABLET,
        class::GAMEPAD,
        class::KEYBOARD,
        class::POINTER,
        class::TABLET,
        class::GAMEPAD,
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
    let mut announced = Announced::default();
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
            records.iter().for_each(|r| announced.take(r));
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
            announced.take(record);
        }
        // The stream and the table agree on what is live, always.
        announced.matches_table()?;
    }
    // Close one source at a time, draining in between: a real consumer reads
    // the bus while it runs, and one close's release burst (up to 228 keys or
    // 32 buttons) is well inside the ring.
    for &(owner, id, _) in &live {
        task::harness::switch_current(owner);
        check!(call(op::CLOSE_SOURCE, id, 0) == 0, "final close");
        let records = drain_all(reader, 128)?;
        expected = gapless(&records, expected)?.0;
        records.iter().for_each(|r| held.take(r));
        records.iter().for_each(|r| announced.take(r));
    }
    announced.matches_table()?;
    check!(
        announced.live.is_empty() && announced.opened == announced.closed,
        "{} opened, {} closed, live {:?}",
        announced.opened,
        announced.closed,
        announced.live
    );
    check!(
        sources::snapshot().entries().is_empty(),
        "the table still lists a closed source"
    );
    let (rejected, throttled) = sources::counters();
    check!(
        held.held.is_empty(),
        "stuck after closing everything: {:?} (dropped {}, rejected {}, throttled {})",
        held.held,
        held.dropped,
        rejected,
        throttled
    );
    check!(
        held.dropped == 0,
        "the consumer ring dropped {} records",
        held.dropped
    );
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
