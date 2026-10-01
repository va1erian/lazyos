//! Input sources (`docs/usb-hid-plan.md`, phase U1): the `input.source`
//! capability, kernel-stamped device ids, class and range checks, ownership,
//! stale ids, releases on close and on task death, the table bound and the
//! rate limit. Calls go through the real syscall 25 front end.

use super::*;
use crate::input::bus::{device, pointer};
use crate::input::rawsys::op;
use crate::input::sources::{self, class, Record, BURST, FIRST_DEVICE, MAX_BATCH, MAX_SOURCES};
use crate::ipc::credentials::{self, Cred, CAP_INPUT_RAW, CAP_INPUT_SOURCE};

const EPERM: i64 = 1;
const EBADF: i64 = 9;
const EFAULT: i64 = 14;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;

pub(super) fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

pub(super) fn call(operation: u64, a1: u64, a2: u64) -> u64 {
    process::dispatch_for_test(25, operation, a1, a2)
}

/// A scratch task that is current and holds exactly `caps`.
pub(super) fn driver_task(caps: u32) -> Result<usize, String> {
    let slot = scratch()?;
    credentials::set(slot, Cred::new(1000, 1000, caps, 0, 1));
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Register a source of `source_class` from the current task.
pub(super) fn register(source_class: u8) -> Result<u64, String> {
    let id = call(op::REGISTER_SOURCE, u64::from(source_class), 0);
    check!((id as i64) > 0, "register({source_class}) -> {:#x}", id);
    Ok(id)
}

pub(super) fn rec(kind_: u8, code: u16, value: i32) -> Record {
    Record {
        kind: kind_,
        code,
        value,
    }
}

/// Publish `records` from source `id`; returns the raw syscall result.
pub(super) fn publish(id: u64, records: &[Record]) -> u64 {
    let mut bytes = Vec::with_capacity(records.len() * 8);
    for record in records {
        bytes.push(record.kind);
        bytes.push(0);
        bytes.extend_from_slice(&record.code.to_le_bytes());
        bytes.extend_from_slice(&record.value.to_le_bytes());
    }
    call(
        op::PUBLISH,
        bytes.as_ptr() as u64,
        id << 16 | records.len() as u64,
    )
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

fn clean() {
    sources::reset();
    bus::reset();
    fresh();
}

/// `input.source` gates ops 4-6, `input.raw` gates the rest, and neither bit
/// implies the other.
pub fn capability_gate() -> Result<(), String> {
    clean();
    driver_task(CAP_INPUT_RAW)?;
    for operation in [op::REGISTER_SOURCE, op::PUBLISH, op::CLOSE_SOURCE] {
        let code = call(operation, u64::from(class::KEYBOARD), 0);
        check!(
            code == failed(EPERM),
            "op {operation} with input.raw -> {code:#x}"
        );
    }
    driver_task(CAP_INPUT_SOURCE)?;
    check!(
        call(op::OPEN, 0, 0) == failed(EPERM),
        "a source opened the consumer ring"
    );
    check!(
        call(op::DISPLAY_OWNER, 0, 0) == failed(EPERM),
        "display owner leaked"
    );
    register(class::KEYBOARD)?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        call(op::REGISTER_SOURCE, u64::from(class::KEYBOARD), 0) == failed(EPERM),
        "the kernel task registered a source"
    );
    // Every kernel-started program but `init` loses the bit too.
    let slot = scratch()?;
    credentials::set(slot, Cred::ROOT);
    credentials::drop_caps(slot, CAP_INPUT_RAW | CAP_INPUT_SOURCE);
    check!(
        !credentials::of(slot).has_cap(CAP_INPUT_SOURCE),
        "drop_caps kept input.source"
    );
    clean();
    Ok(())
}

/// Records reach the bus with the kernel's device id, sequence and order;
/// two sources get distinct ids.
pub fn device_ids_are_stamped() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    let mouse = register(class::POINTER)?;
    check!(keyboard != mouse, "two sources share an id");
    check!(
        publish(
            keyboard,
            &[rec(kind::KEY, 0x04, 1), rec(kind::KEY, 0x04, 0)]
        ) == 2,
        "keys"
    );
    let motion = rec(kind::REL_MOTION, 0, pointer::pack_rel(3, -2));
    let button = rec(kind::BUTTON, pointer::button::LEFT, 1);
    check!(publish(mouse, &[motion, button]) == 2, "pointer");
    let records = drain_all(reader, 16)?;
    gapless(&records, 1)?;
    let (kbd_dev, mouse_dev) = (records[0].device, records[2].device);
    check!(
        kbd_dev >= FIRST_DEVICE && mouse_dev >= FIRST_DEVICE && kbd_dev != mouse_dev,
        "devices {kbd_dev:#x} {mouse_dev:#x}"
    );
    check!(
        ![device::PS2_KEYBOARD, device::PS2_MOUSE].contains(&kbd_dev),
        "a source got a kernel driver's id"
    );
    check!(
        shape(&records)
            == [
                (kbd_dev, kind::KEY, 0x04, 1),
                (kbd_dev, kind::KEY, 0x04, 0),
                (mouse_dev, kind::REL_MOTION, 0, motion.value),
                (mouse_dev, kind::BUTTON, 1, 1),
            ],
        "records {:x?}",
        shape(&records)
    );
    clean();
    Ok(())
}

/// A class publishes only its kinds, and only in range; the rest is dropped
/// and counted, and the valid records in the same batch still go out.
pub fn class_and_ranges_enforced() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    let tablet = register(class::TABLET)?;
    let keys = [
        rec(kind::KEY, 0x05, 1),
        rec(kind::REL_MOTION, 0, 1), // not a keyboard kind
        rec(kind::BUTTON, 1, 1),     // not a keyboard kind
        rec(kind::KEY, 0x03, 1),     // phantom usage
        rec(kind::KEY, 0xE8, 1),     // past the keyboard page
        rec(kind::KEY, 0x06, 2),     // not an edge
        rec(kind::DROPPED, 0, 5),    // consumer-only marker
        rec(kind::KEY, 0x05, 0),
    ];
    check!(
        publish(keyboard, &keys) == 2,
        "keyboard accepted the wrong count"
    );
    let tablet_records = [
        rec(kind::ABS_MOTION, 0, pointer::pack_abs(10, 20)),
        rec(kind::REL_MOTION, 0, 1),             // a tablet is absolute
        rec(kind::ABS_MOTION, 1, 0),             // motion has code 0
        rec(kind::BUTTON, 6, 1),                 // past forward
        rec(kind::SCROLL, pointer::VERTICAL, 0), // empty scroll
        rec(kind::SCROLL, 2, 1),                 // no third axis
        rec(kind::SCROLL, pointer::HORIZONTAL, 128), // over the bound
        rec(kind::SCROLL, pointer::VERTICAL, -3),
    ];
    check!(
        publish(tablet, &tablet_records) == 2,
        "tablet accepted the wrong count"
    );
    let records = drain_all(reader, 32)?;
    let kinds: Vec<(u8, u16, i32)> = records.iter().map(|r| (r.kind, r.code, r.value)).collect();
    check!(
        kinds
            == [
                (kind::KEY, 0x05, 1),
                (kind::KEY, 0x05, 0),
                (kind::ABS_MOTION, 0, pointer::pack_abs(10, 20)),
                (kind::SCROLL, 0, -3),
            ],
        "bus carried {kinds:x?}"
    );
    check!(
        sources::counters().0 == 12,
        "rejected {:?}",
        sources::counters()
    );
    clean();
    Ok(())
}

/// Only the owner may use an id; closed and garbage ids fail closed; batch
/// bounds and bad buffers are refused before anything is published.
pub fn ownership_and_stale_ids() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let owner = driver_task(CAP_INPUT_SOURCE)?;
    let id = register(class::KEYBOARD)?;
    let key = [rec(kind::KEY, 0x04, 1)];
    driver_task(CAP_INPUT_SOURCE)?;
    check!(publish(id, &key) == failed(EBADF), "a stranger published");
    check!(
        call(op::CLOSE_SOURCE, id, 0) == failed(EBADF),
        "a stranger closed"
    );
    task::harness::switch_current(owner);
    check!(
        publish(id ^ 0x100, &key) == failed(EBADF),
        "wrong generation accepted"
    );
    check!(
        publish(0xFF, &key) == failed(EBADF),
        "out-of-table index accepted"
    );
    check!(
        publish(u64::MAX >> 16, &key) == failed(EBADF),
        "garbage id accepted"
    );
    check!(
        call(op::REGISTER_SOURCE, 0, 0) == failed(EINVAL),
        "class 0 accepted"
    );
    check!(
        call(op::REGISTER_SOURCE, 0x101, 0) == failed(EINVAL),
        "class > u8 accepted"
    );
    check!(
        call(op::PUBLISH, key.as_ptr() as u64, id << 16) == failed(EINVAL),
        "empty batch accepted"
    );
    let big = [rec(kind::KEY, 0x04, 0); MAX_BATCH + 1];
    check!(
        publish(id, &big) == failed(EINVAL),
        "oversized batch accepted"
    );
    check!(
        call(op::PUBLISH, 0, id << 16 | 1) == failed(EFAULT),
        "null buffer accepted"
    );
    // With validation on (the harness trusts kernel buffers by default), an
    // unmapped buffer faults cleanly instead of publishing.
    let previous = crate::user_ptr::set_trust_kernel_pointers(false);
    let unmapped = call(op::PUBLISH, 0x10, id << 16 | 1);
    crate::user_ptr::set_trust_kernel_pointers(previous);
    check!(
        unmapped == failed(EFAULT),
        "unmapped buffer -> {unmapped:#x}"
    );
    check!(drain_all(reader, 8)?.is_empty(), "a refused call published");
    check!(call(op::CLOSE_SOURCE, id, 0) == 0, "close failed");
    check!(
        publish(id, &key) == failed(EBADF),
        "closed id still publishes"
    );
    let again = register(class::KEYBOARD)?;
    check!(again != id, "a closed id was handed out again");
    check!(
        publish(id, &key) == failed(EBADF),
        "stale id names its successor"
    );
    clean();
    Ok(())
}

/// Closing a source releases every key and button it held, under its own
/// device id; keys it already released are not released twice.
pub fn close_releases_held() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    let mouse = register(class::POINTER)?;
    publish(
        keyboard,
        &[
            rec(kind::KEY, 0x04, 1),
            rec(kind::KEY, 0xE1, 1),
            rec(kind::KEY, 0x05, 1),
            rec(kind::KEY, 0x05, 0),
        ],
    );
    publish(mouse, &[rec(kind::BUTTON, 1, 1), rec(kind::BUTTON, 3, 1)]);
    let held = drain_all(reader, 16)?;
    let (kbd_dev, mouse_dev) = (held[0].device, held[4].device);
    check!(call(op::CLOSE_SOURCE, keyboard, 0) == 0, "close keyboard");
    check!(call(op::CLOSE_SOURCE, mouse, 0) == 0, "close mouse");
    let released = drain_all(reader, 16)?;
    check!(
        shape(&released)
            == [
                (kbd_dev, kind::KEY, 0x04, 0),
                (kbd_dev, kind::KEY, 0xE1, 0),
                (mouse_dev, kind::BUTTON, 1, 0),
                (mouse_dev, kind::BUTTON, 3, 0),
            ],
        "releases {:x?}",
        shape(&released)
    );
    clean();
    Ok(())
}

/// A driver that dies holding keys has them released by task teardown, and
/// its slots become free.
pub fn task_death_releases_held() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    let driver = driver_task(CAP_INPUT_SOURCE)?;
    let keyboard = register(class::KEYBOARD)?;
    publish(keyboard, &[rec(kind::KEY, 0x2C, 1)]);
    let _ = drain_all(reader, 8)?;
    sources::teardown_task(driver);
    let released = drain_all(reader, 8)?;
    check!(
        released.len() == 1
            && released[0].kind == kind::KEY
            && released[0].code == 0x2C
            && released[0].value == 0,
        "teardown released {:x?}",
        shape(&released)
    );
    check!(
        publish(keyboard, &[rec(kind::KEY, 4, 1)]) == failed(EBADF),
        "dead source published"
    );
    clean();
    Ok(())
}

/// The table holds [`MAX_SOURCES`]; a dead owner's slot is reclaimed.
pub fn table_bound_and_reclaim() -> Result<(), String> {
    clean();
    driver_task(CAP_INPUT_SOURCE)?;
    for _ in 0..MAX_SOURCES {
        register(class::POINTER)?;
    }
    check!(
        call(op::REGISTER_SOURCE, u64::from(class::POINTER), 0) == failed(EBUSY),
        "table not full"
    );
    // A table full of sources whose owner is gone (teardown has not run):
    // a live driver still gets a slot.
    sources::reset();
    let dead = task::MAX_TASKS - 1;
    check!(!task::live(dead), "slot {dead} unexpectedly live");
    for _ in 0..MAX_SOURCES {
        sources::register(dead, class::POINTER).map_err(|e| format!("{e:?}"))?;
    }
    register(class::KEYBOARD)?;
    clean();
    Ok(())
}

/// A source bursting past its bucket has the excess dropped and counted
/// (the PIT does not tick in test mode, so no refill happens).
pub fn rate_limited() -> Result<(), String> {
    clean();
    let reader = consumer()?;
    driver_task(CAP_INPUT_SOURCE)?;
    let mouse = register(class::POINTER)?;
    let motion = [rec(kind::REL_MOTION, 0, pointer::pack_rel(1, 0)); MAX_BATCH];
    let mut accepted = 0u64;
    for _ in 0..(BURST as usize / MAX_BATCH + 2) {
        accepted += publish(mouse, &motion);
    }
    check!(
        accepted == u64::from(BURST),
        "accepted {accepted}, want {BURST}"
    );
    check!(
        sources::counters().1 == 2 * MAX_BATCH as u64,
        "throttled {:?}",
        sources::counters()
    );
    // Merged on the bus, but every accepted delta arrived.
    let records = drain_all(reader, 64)?;
    let dx: i64 = records
        .iter()
        .map(|r| i64::from(pointer::unpack_rel(r.value).0))
        .sum();
    check!(dx == i64::from(BURST), "motion sum {dx}");
    clean();
    Ok(())
}
