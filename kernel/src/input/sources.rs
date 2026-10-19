//! Input sources: unprivileged drivers publishing onto the raw bus
//! (`docs/usb-hid-plan.md`, phase U1, decision 6).
//!
//! A driver holding `CAP_INPUT_SOURCE` registers one source per device
//! interface, naming its [`class`]. The kernel assigns the source a device id
//! and stamps it on every record, so a driver can never pose as the PS/2
//! keyboard, the PS/2 mouse or another driver's device. Each record is
//! checked against the source's class (a keyboard cannot publish motion) and
//! the bus's ranges before it is published; anything else is dropped and
//! counted. A token bucket bounds how fast one source can publish.
//!
//! The kernel remembers which keys and buttons each source holds. Closing a
//! source, or its task dying ([`teardown_task`]), publishes a release for
//! each, so unplugging a device or crashing a driver cannot leave a key or a
//! button stuck. A gamepad's axes are state rather than edges, but the kernel
//! remembers which of them were last away from their rest value and returns
//! them there on close and on task death too, so a crashed driver cannot leave
//! a stick deflected either (`docs/usb-gamepad-plan.md`).
//!
//! Every registration and every freeing of a slot is announced on the bus as a
//! kernel-originated [`kind::SOURCE`] record (`code` the class, `value` 1 or
//! 0, `device` the source's id), so a consumer can route a device's records by
//! class. The close announcement comes after the release burst, from
//! [`release`] itself, so `close`, task teardown and `register` reclaiming a
//! dead owner's slot all publish it. A driver can never publish one
//! ([`permitted`]). A record can be evicted from a full ring, so [`snapshot`]
//! (the `SOURCES` op of syscall 25) is the truth a consumer re-reads.
//!
//! Ids: a source id is `generation << 8 | index`; the generation changes on
//! every registration of the slot, so a stale id from a closed source fails
//! closed with [`Error::BadId`] instead of naming its successor.

use spin::Mutex;

use super::bus::{self, axis, kind, pointer, value};
use crate::task;

/// Sources the table holds (one per device interface).
pub const MAX_SOURCES: usize = 16;
/// The device id of source slot `n` is `FIRST_DEVICE + n`; ids below it are
/// the kernel's own drivers.
pub const FIRST_DEVICE: u8 = 0x10;
/// Records one publish call may carry.
pub const MAX_BATCH: usize = 64;
/// Bytes per published record: kind, reserved, code (u16), value (i32).
pub const RECORD_BYTES: usize = 8;
/// Token bucket: records a source may burst, and refill per PIT tick (10 ms),
/// so a sustained 6400 records/s; a 1 kHz mouse needs a fraction of it.
pub const BURST: u32 = 512;
pub const REFILL_PER_TICK: u32 = 64;
/// Largest wheel notches in one record.
pub const MAX_SCROLL: i32 = 127;

/// Source classes and the record kinds each may publish.
pub mod class {
    pub const KEYBOARD: u8 = 1;
    /// A relative pointer (mouse).
    pub const POINTER: u8 = 2;
    /// An absolute pointer (tablet, touchscreen in single-touch mode).
    pub const TABLET: u8 = 3;
    /// A gamepad: `AXIS` records (up to 32 buttons are still `BUTTON`s).
    pub const GAMEPAD: u8 = 4;
}

/// Why a source operation failed; `rawsys` maps these to errnos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Unknown class.
    BadClass,
    /// Every slot is held by a live task.
    Full,
    /// The id names no source of the caller's (closed, stale or foreign).
    BadId,
}

/// One record a driver hands in (the kernel adds seq, time and device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: u8,
    pub code: u16,
    pub value: i32,
}

impl Record {
    /// Decode record `index` of a publish buffer.
    pub fn decode(bytes: &[u8], index: usize) -> Option<Record> {
        let at = index.checked_mul(RECORD_BYTES)?;
        let raw = bytes.get(at..at.checked_add(RECORD_BYTES)?)?;
        Some(Record {
            kind: raw[0],
            code: u16::from_le_bytes([raw[2], raw[3]]),
            value: i32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
        })
    }
}

/// What one publish call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Published {
    pub accepted: usize,
    /// Wrong kind for the class, or out of range.
    pub rejected: usize,
    /// Over the rate limit.
    pub throttled: usize,
}

#[derive(Clone, Copy)]
struct Source {
    owner: usize,
    class: u8,
    generation: u16,
    /// One bit per key usage 0..=255.
    keys: [u64; 4],
    /// Bit `n` is button usage `n + 1` (a gamepad reports up to 32).
    buttons: u32,
    /// Axes whose last published value was not their rest value, one bit per
    /// [`axis::slot`]: a driver that dies holding a stick must not leave it
    /// deflected.
    axes: u16,
    tokens: u32,
    refilled_at: u64,
}

/// A slot's token bucket while no source holds it. It outlives the source,
/// so closing and re-registering cannot buy a fresh burst.
#[derive(Clone, Copy)]
struct Bucket {
    tokens: u32,
    refilled_at: u64,
}

const FULL: Bucket = Bucket {
    tokens: BURST,
    refilled_at: 0,
};

struct Table {
    slots: [Option<Source>; MAX_SOURCES],
    generations: [u16; MAX_SOURCES],
    buckets: [Bucket; MAX_SOURCES],
    /// Totals over the table's life, for tests and diagnostics.
    rejected: u64,
    throttled: u64,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    slots: [None; MAX_SOURCES],
    generations: [0; MAX_SOURCES],
    buckets: [FULL; MAX_SOURCES],
    rejected: 0,
    throttled: 0,
});

/// Run with interrupts off: the bus lock taken inside is shared with the
/// PS/2 IRQ producers.
fn locked<R>(body: impl FnOnce(&mut Table) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| body(&mut TABLE.lock()))
}

fn id_of(index: usize, generation: u16) -> u64 {
    u64::from(generation) << 8 | index as u64
}

/// The device id stamped on source `index`'s records.
pub fn device_of(index: usize) -> u8 {
    FIRST_DEVICE + index as u8
}

/// Register a source of `class` for task `owner`; returns its id.
pub fn register(owner: usize, source_class: u8) -> Result<u64, Error> {
    if !matches!(
        source_class,
        class::KEYBOARD | class::POINTER | class::TABLET | class::GAMEPAD
    ) {
        return Err(Error::BadClass);
    }
    locked(|table| {
        let index = table
            .slots
            .iter()
            .position(|slot| slot.is_none_or(|s| !task::live(s.owner)))
            .ok_or(Error::Full)?;
        // A dead owner's source is released before the slot is reused (its
        // task teardown normally did that already).
        release(table, index);
        let generation = table.generations[index].wrapping_add(1).max(1);
        table.generations[index] = generation;
        let bucket = table.buckets[index];
        let mut source = Source {
            owner,
            class: source_class,
            generation,
            keys: [0; 4],
            buttons: 0,
            axes: 0,
            tokens: bucket.tokens,
            refilled_at: bucket.refilled_at,
        };
        refill(&mut source, task::ticks());
        table.slots[index] = Some(source);
        announce(index, source_class, true);
        Ok(id_of(index, generation))
    })
}

/// The slot `id` names, if it is a live source of `owner`.
fn slot_of(table: &Table, id: u64, owner: usize) -> Result<usize, Error> {
    let index = (id & 0xFF) as usize;
    let generation = u16::try_from(id >> 8).map_err(|_| Error::BadId)?;
    match table.slots.get(index).copied().flatten() {
        Some(source) if source.owner == owner && source.generation == generation => Ok(index),
        _ => Err(Error::BadId),
    }
}

/// Publish `records` from source `id` (owned by `owner`).
pub fn publish(id: u64, owner: usize, records: &[Record]) -> Result<Published, Error> {
    locked(|table| {
        let index = slot_of(table, id, owner)?;
        let mut outcome = Published::default();
        let Some(source) = table.slots[index].as_mut() else {
            return Err(Error::BadId);
        };
        refill(source, task::ticks());
        crate::perf::source_publishing(owner);
        for record in records {
            if !permitted(source.class, record) {
                outcome.rejected += 1;
                continue;
            }
            if source.tokens == 0 {
                outcome.throttled += 1;
                continue;
            }
            source.tokens -= 1;
            track(source, record);
            bus::publish(device_of(index), record.kind, record.code, record.value);
            outcome.accepted += 1;
        }
        crate::perf::source_published();
        table.rejected += outcome.rejected as u64;
        table.throttled += outcome.throttled as u64;
        Ok(outcome)
    })
}

/// Close source `id`, releasing what it held.
pub fn close(id: u64, owner: usize) -> Result<(), Error> {
    locked(|table| {
        let index = slot_of(table, id, owner)?;
        release(table, index);
        Ok(())
    })
}

/// Whether task `owner` still owns any source.
pub fn owns_any(owner: usize) -> bool {
    locked(|table| {
        table
            .slots
            .iter()
            .flatten()
            .any(|source| source.owner == owner)
    })
}

/// Task `slot` is gone: release and free every source it owned.
pub fn teardown_task(slot: usize) {
    locked(|table| {
        for index in 0..MAX_SOURCES {
            if table.slots[index].is_some_and(|s| s.owner == slot) {
                release(table, index);
            }
        }
    });
}

/// Publish a release for every key, button and deflected axis source `index`
/// holds, then free the slot.
fn release(table: &mut Table, index: usize) {
    let Some(source) = table.slots[index].take() else {
        return;
    };
    table.buckets[index] = Bucket {
        tokens: source.tokens,
        refilled_at: source.refilled_at,
    };
    let device = device_of(index);
    for (word, &held) in source.keys.iter().enumerate() {
        let mut bits = held;
        while bits != 0 {
            let bit = bits.trailing_zeros() as u16;
            bits &= bits - 1;
            bus::publish(device, kind::KEY, word as u16 * 64 + bit, value::RELEASE);
        }
    }
    for bit in 0..32u16 {
        if source.buttons & (1 << bit) != 0 {
            bus::publish(device, kind::BUTTON, bit + 1, value::RELEASE);
        }
    }
    for slot in 0..axis::SLOTS {
        if source.axes & (1 << slot) != 0 {
            let code = axis::usage_of(slot);
            bus::publish(device, kind::AXIS, code, axis::rest(code));
        }
    }
    announce(index, source.class, false);
}

/// Publish the kernel-originated [`kind::SOURCE`] record for slot `index`.
fn announce(index: usize, source_class: u8, registered: bool) {
    bus::publish(
        device_of(index),
        kind::SOURCE,
        u16::from(source_class),
        i32::from(registered),
    );
}

/// Bytes per [`snapshot`] entry as `rawsys` writes it: device, class,
/// generation (little-endian `u16`).
pub const ENTRY_BYTES: usize = 4;

/// One live source, as the table query reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub device: u8,
    pub class: u8,
    pub generation: u16,
}

impl Info {
    /// The wire encoding.
    pub fn to_bytes(self) -> [u8; ENTRY_BYTES] {
        let generation = self.generation.to_le_bytes();
        [self.device, self.class, generation[0], generation[1]]
    }
}

/// The live sources in slot order, and how many there are. A slot whose owner
/// died but was not reclaimed yet is not live (as [`register`] sees it).
pub fn snapshot() -> ([Info; MAX_SOURCES], usize) {
    locked(|table| {
        let mut out = [Info::default(); MAX_SOURCES];
        let mut count = 0;
        for (index, slot) in table.slots.iter().enumerate() {
            if let Some(source) = slot.filter(|s| task::live(s.owner)) {
                out[count] = Info {
                    device: device_of(index),
                    class: source.class,
                    generation: source.generation,
                };
                count += 1;
            }
        }
        (out, count)
    })
}

/// Whether `record` is a kind `class` may publish, with in-range fields.
fn permitted(source_class: u8, record: &Record) -> bool {
    let edge = matches!(record.value, value::RELEASE | value::PRESS);
    match (source_class, record.kind) {
        (class::KEYBOARD, kind::KEY) => (0x04..=0xE7).contains(&record.code) && edge,
        (class::POINTER | class::TABLET, kind::BUTTON) => {
            (pointer::button::LEFT..=pointer::button::FORWARD).contains(&record.code) && edge
        }
        (class::GAMEPAD, kind::BUTTON) => (1..=32).contains(&record.code) && edge,
        (class::GAMEPAD, kind::AXIS) => axis::valid(record.code, record.value),
        (class::POINTER, kind::REL_MOTION) | (class::TABLET, kind::ABS_MOTION) => record.code == 0,
        (class::POINTER | class::TABLET, kind::SCROLL) => {
            record.code <= pointer::HORIZONTAL
                && record.value != 0
                && record.value.abs() <= MAX_SCROLL
        }
        // A device's identity: any class may state it, it carries no input.
        (_, kind::DEVICE) => true,
        // The kernel's own announcement of a registration: a driver forging
        // one would re-route another device's records.
        (_, kind::SOURCE) => false,
        _ => false,
    }
}

/// Remember the edge so a close can release it.
fn track(source: &mut Source, record: &Record) {
    let pressed = record.value == value::PRESS;
    match record.kind {
        kind::KEY => {
            let (word, bit) = (usize::from(record.code >> 6), 1u64 << (record.code & 63));
            if pressed {
                source.keys[word] |= bit;
            } else {
                source.keys[word] &= !bit;
            }
        }
        kind::BUTTON => {
            let bit = 1u32 << (record.code - 1);
            if pressed {
                source.buttons |= bit;
            } else {
                source.buttons &= !bit;
            }
        }
        kind::AXIS => {
            let Some(slot) = axis::slot(record.code) else {
                return;
            };
            if record.value == axis::rest(record.code) {
                source.axes &= !(1 << slot);
            } else {
                source.axes |= 1 << slot;
            }
        }
        _ => {}
    }
}

fn refill(source: &mut Source, now: u64) {
    let elapsed = now.saturating_sub(source.refilled_at);
    if elapsed > 0 {
        let added = elapsed.saturating_mul(u64::from(REFILL_PER_TICK));
        source.tokens = u64::from(source.tokens)
            .saturating_add(added)
            .min(u64::from(BURST)) as u32;
        source.refilled_at = now;
    }
}

/// Lifetime totals of rejected and throttled records (tests, diagnostics).
#[cfg(lazyos_tests)]
pub fn counters() -> (u64, u64) {
    locked(|table| (table.rejected, table.throttled))
}

/// Test hook: free every source without publishing releases, reset counters.
#[cfg(lazyos_tests)]
pub fn reset() {
    locked(|table| {
        table.slots = [None; MAX_SOURCES];
        table.buckets = [FULL; MAX_SOURCES];
        table.rejected = 0;
        table.throttled = 0;
    });
}

/// Test hook: refill every bucket, standing in for time passing.
#[cfg(lazyos_tests)]
pub fn refill_all() {
    locked(|table| {
        table.buckets = [FULL; MAX_SOURCES];
        for source in table.slots.iter_mut().flatten() {
            source.tokens = BURST;
        }
    });
}
