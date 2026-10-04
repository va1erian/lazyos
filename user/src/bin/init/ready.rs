//! Service readiness (docs/performance-plan.md P7.3).
//!
//! A row used to count as up the moment it was spawned, so its dependents
//! raced its registration and every client carried a retry loop, and the
//! desktop's apps opened after fixed delays (500 ms, then 400 ms apart)
//! chosen to be long enough. Now a service that announces readiness sends
//! `init.Ready` (`services::init::notify_ready`) once its name is registered
//! and it answers requests: its dependents start then, and the autostart
//! opens the session once every boot service is ready. Rows not listed in
//! [`ANNOUNCES`] (drivers and services that do not send it) are ready when
//! spawned, as before. A listed row that never says so is taken as ready
//! after [`READY_TIMEOUT_TICKS`] (`INIT:READY:LATE`), so a hung service
//! delays its dependents, never the boot.

use alloc::format;

use user::messenger::Message;
use user::sys;

use super::service::{Phase, Service};

/// Manifest services that send `Ready` once they serve.
const ANNOUNCES: &[&str] = &[
    "messengerd",
    "keyd",
    "confd",
    "timed",
    "accountsd",
    "logind",
    "logd",
    "healthd",
    "clipboardd",
    "mimed",
    "pkgd",
    "sysmond",
    "audiod",
];

/// How long a listed row may take to announce itself (100 Hz): 5 s.
pub(super) const READY_TIMEOUT_TICKS: u64 = 500;

/// Whether `row` sends `Ready` (launched apps never do).
fn announces(row: &Service) -> bool {
    !row.launched && ANNOUNCES.contains(&row.name)
}

/// Whether `row` is up for its dependents: running, and ready if it
/// announces readiness.
pub(super) fn is_ready(row: &Service) -> bool {
    row.phase == Phase::Running && (row.ready || !announces(row))
}

/// A `Ready` from `message.sender`: mark the running manifest row whose task
/// sent it. Returns whether a row became ready (its dependents may start).
pub(super) fn observe(services: &mut [Service], message: &Message) -> bool {
    let Some(row) = services.iter_mut().find(|row| {
        !row.launched && row.phase == Phase::Running && row.pid != 0 && row.pid == message.sender
    }) else {
        return false;
    };
    if row.ready {
        return false;
    }
    row.ready = true;
    sys::write_str(&format!(
        "INIT:READY name={} ticks={}\n",
        row.name,
        sys::clock().saturating_sub(row.started_tick)
    ));
    true
}

/// Take every listed row that is overdue as ready; returns whether any was.
pub(super) fn expire(services: &mut [Service], now: u64) -> bool {
    let mut any = false;
    for row in services.iter_mut() {
        if row.phase == Phase::Running
            && !row.ready
            && announces(row)
            && now >= row.started_tick + READY_TIMEOUT_TICKS
        {
            row.ready = true;
            any = true;
            sys::write_str(&format!("INIT:READY:LATE name={}\n", row.name));
        }
    }
    any
}

/// When the next row becomes overdue, if any is still awaited.
pub(super) fn next_deadline(services: &[Service]) -> Option<u64> {
    services
        .iter()
        .filter(|row| row.phase == Phase::Running && !row.ready && announces(row))
        .map(|row| row.started_tick + READY_TIMEOUT_TICKS)
        .min()
}

/// Whether the boot services are up: every manifest row is ready, or has
/// settled for good (`Stopped`, `Failed`). The desktop's apps wait for this.
pub(super) fn settled(services: &[Service]) -> bool {
    services
        .iter()
        .filter(|row| !row.launched)
        .all(|row| is_ready(row) || matches!(row.phase, Phase::Stopped | Phase::Failed))
}
