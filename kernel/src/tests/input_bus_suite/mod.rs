//! The raw input event bus: HID translation, per-consumer rings, the
//! `input.raw` capability gate, and the syscall front end
//! (`docs/input-plan.md`, phase I0).

use super::*;
use crate::input::bus::{self, kind, RawEvent, RING_CAP};
use crate::input::hid::{Set1Decoder, Step};

mod hid_table;
mod ring;
mod stress;
mod syscall;

/// Reset every piece of global state the bus tests share.
fn fresh() {
    bus::reset();
    crate::input::keyboard::reset();
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    crate::ipc::credentials::reset_for_task(task::KERNEL_TASK);
}

/// A live scratch task slot (each is a distinct consumer owner).
fn scratch() -> Result<usize, String> {
    task::spawn_fork().map_err(to_string)
}

/// Drain every record `owner` has, in batches of `batch`.
fn drain_all(owner: usize, batch: usize) -> Result<Vec<RawEvent>, String> {
    let id = bus::consumer_of(owner).ok_or("no consumer")?;
    let mut out = Vec::new();
    loop {
        let before = out.len();
        bus::drain(id, owner, batch, &mut out).map_err(|e| format!("{e:?}"))?;
        if out.len() == before {
            return Ok(out);
        }
    }
}

/// Check `records` continue a gapless sequence from `expected`: a `Dropped`
/// marker's `seq` is the first lost number and its `value` the span it covers.
/// Returns `(next expected seq, events lost)`.
fn gapless(records: &[RawEvent], mut expected: u64) -> Result<(u64, u64), String> {
    let mut lost = 0u64;
    for record in records {
        check!(
            record.seq == expected,
            "seq {} where {expected} was due",
            record.seq
        );
        if record.kind == kind::DROPPED {
            check!(record.value > 0, "empty Dropped marker at {}", record.seq);
            expected += record.value as u64;
            lost += record.value as u64;
        } else {
            expected += 1;
        }
    }
    Ok((expected, lost))
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("input_hid_table_round_trips", hid_table::table_round_trips),
    ("input_hid_known_keys", hid_table::known_keys),
    (
        "input_hid_printscreen_and_pause",
        hid_table::printscreen_and_pause,
    ),
    (
        "input_hid_unknown_and_recovery",
        hid_table::unknown_and_recovery,
    ),
    ("input_bus_order_and_fields", ring::order_and_fields),
    ("input_bus_ring_wraparound", ring::wraparound),
    ("input_bus_overflow_marker", ring::overflow_marker),
    (
        "input_bus_marker_accumulates_and_resets",
        ring::marker_resets,
    ),
    (
        "input_bus_consumers_independent",
        ring::consumers_independent,
    ),
    ("input_bus_dead_owner_reclaimed", ring::dead_owner_reclaimed),
    ("input_raw_typematic_suppressed", ring::typematic_suppressed),
    ("input_raw_ps2_reaches_bus", syscall::ps2_reaches_bus),
    ("input_raw_capability_gate", syscall::capability_gate),
    (
        "input_raw_drop_caps_only_removes_the_named_bits",
        syscall::drop_caps_only_removes_the_named_bits,
    ),
    (
        "input_raw_poll_bounds_and_faults",
        syscall::poll_bounds_and_faults,
    ),
    (
        "input_raw_display_owner_reports_the_compositor",
        syscall::display_owner_reports_the_compositor,
    ),
    (
        "input_raw_legacy_display_path_unchanged",
        syscall::legacy_path_unchanged,
    ),
    ("input_bus_stress_no_silent_loss", stress::no_silent_loss),
    ("input_bus_stress_many_producers", stress::many_producers),
    ("input_raw_stress_scancode_storm", stress::scancode_storm),
];
