//! Waiting on several endpoints, and on the raw input bus, at once
//! (docs/performance-plan.md P1.3 and P1.4).
//!
//! `recv` parks on one endpoint, so a service with two sources of work (a
//! compositor with its request endpoint and its input-event endpoint;
//! `inputd` with its service endpoint and the raw input bus) had to sleep on
//! one with a short deadline and poll the other. [`wait_any`] parks once on
//! all of them and returns as soon as any is ready. It does not receive: the
//! caller then takes what it wants with `try_recv` (or drains the bus), so the
//! wait composes with every existing receive path and their rules
//! (transfers, poll grace, quota) stay where they are.
//!
//! The registration is the one `recv` makes: the task is added to each
//! endpoint's waiter list under the same lock that saw its inbox empty, and
//! to the raw bus's doorbell under the bus lock, then parks on the Messenger
//! wait queue. Any delivery, peer close or input publication wakes it through
//! that queue. All of this runs with interrupts off (syscall context) on one
//! CPU, so nothing can slip in between the checks and the park.

use super::*;

/// Most endpoints one wait may name.
pub const MAX_WAIT_ENDPOINTS: usize = 8;
/// Doorbell: the caller's raw input ring (syscall 25; `inputd`).
pub const WAIT_RAW_INPUT: u64 = 1;
/// Doorbell: a key event in the display input queue (the compositor).
pub const WAIT_DISPLAY_KEYS: u64 = 2;
/// Doorbell: an application acted on an `AF_INET` socket (the attached
/// `netd` only; `ipc::inet::bell`, docs/performance-plan.md P4.1).
pub const WAIT_INET: u64 = 4;
/// Every doorbell [`wait_any`] knows.
pub const WAIT_DOORBELLS: u64 = WAIT_RAW_INPUT | WAIT_DISPLAY_KEYS | WAIT_INET;
/// Bit of the ready mask that means "the raw input bus has records".
pub const RAW_INPUT_READY: u64 = 1 << 63;
/// Bit of the ready mask that means "the display input queue has events".
pub const DISPLAY_INPUT_READY: u64 = 1 << 62;
/// Bit of the ready mask that means "the `AF_INET` pump has work".
pub const INET_READY: u64 = 1 << 61;

/// Park until one of `handles` has a message (or its peer closed), or until
/// one of the `doorbells` ([`WAIT_RAW_INPUT`], [`WAIT_DISPLAY_KEYS`],
/// [`WAIT_INET`]) rings,
/// or until `deadline` (absolute ticks) passes. Returns the ready mask: bit
/// `i` for `handles[i]`, [`RAW_INPUT_READY`], [`DISPLAY_INPUT_READY`] and
/// [`INET_READY`] for the doorbells.
///
/// Errors: `BadParcel` for an empty or oversized set or an unknown doorbell,
/// the handle errors of `recv` for a bad handle, `WrongKind` for a doorbell
/// the caller may not ring (no raw ring; not the display owner), `TimedOut`,
/// and `Canceled` when a fatal signal must end the task.
pub fn wait_any(handles: &[u64], doorbells: u64, deadline: Option<u64>) -> Result<u64, Error> {
    let empty = handles.is_empty() && doorbells == 0;
    if handles.len() > MAX_WAIT_ENDPOINTS || empty || doorbells & !WAIT_DOORBELLS != 0 {
        return Err(Error::BadParcel);
    }
    let mut ends = [(0u64, 0usize); MAX_WAIT_ENDPOINTS];
    for (end, &handle) in ends.iter_mut().zip(handles) {
        *end = endpoint_of(handle, rights::CALL)?;
    }
    let ends = &ends[..handles.len()];
    let me = task::current();
    loop {
        let mut ready = ready_or_register(ends, me);
        match arm_doorbells(doorbells, me) {
            Ok(rung) => ready |= rung,
            Err(error) => {
                unregister(ends, me, doorbells);
                return Err(error);
            }
        }
        if ready != 0 {
            unregister(ends, me, doorbells);
            return Ok(ready);
        }
        let reason = MESSENGER.wait(me, deadline);
        unregister(ends, me, doorbells);
        if reason == WakeReason::TimedOut {
            return Err(Error::TimedOut);
        }
        // As in `recv`: a fatal signal must reach the syscall return.
        if reason == WakeReason::Interrupted && task::signal::native_fatal_pending(me).is_some() {
            return Err(Error::Canceled);
        }
    }
}

/// The ready mask of `ends`; `me` is registered on every endpoint that is not
/// ready, under the one lock that saw it empty. A vanished channel counts as
/// ready, so the caller's receive reports what happened to it.
fn ready_or_register(ends: &[(u64, usize)], me: usize) -> u64 {
    let mut channels = CHANNELS.lock();
    let mut ready = 0;
    for (index, &(id, side)) in ends.iter().enumerate() {
        match find_channel(&mut channels, id) {
            Ok(channel) => {
                let inbox_ready = !channel.endpoints[side].inbox.is_empty();
                if inbox_ready || channel.endpoints[1 - side].closed {
                    ready |= 1 << index;
                } else {
                    add_waiter(&mut channel.endpoints[side], me);
                }
            }
            Err(_) => ready |= 1 << index,
        }
    }
    ready
}

/// Arm each requested doorbell; the ready bits of those already ringing
/// (nothing is armed for them).
fn arm_doorbells(doorbells: u64, me: usize) -> Result<u64, Error> {
    let mut ready = 0;
    if doorbells & WAIT_RAW_INPUT != 0 {
        match crate::input::bus::arm_doorbell(me) {
            Ok(true) => ready |= RAW_INPUT_READY,
            Ok(false) => {}
            Err(_) => return Err(Error::WrongKind),
        }
    }
    if doorbells & WAIT_DISPLAY_KEYS != 0 {
        match crate::display::arm_key_doorbell(me) {
            Ok(true) => ready |= DISPLAY_INPUT_READY,
            Ok(false) => {}
            Err(()) => return Err(Error::WrongKind),
        }
    }
    if doorbells & WAIT_INET != 0 {
        match crate::ipc::inet::bell::arm(me) {
            Ok(true) => ready |= INET_READY,
            Ok(false) => {}
            Err(()) => return Err(Error::WrongKind),
        }
    }
    Ok(ready)
}

/// Drop every registration [`ready_or_register`] and the doorbells made.
fn unregister(ends: &[(u64, usize)], me: usize, doorbells: u64) {
    for &(id, side) in ends {
        remove_waiter(id, side, me);
    }
    if doorbells & WAIT_RAW_INPUT != 0 {
        crate::input::bus::disarm_doorbell(me);
    }
    if doorbells & WAIT_DISPLAY_KEYS != 0 {
        crate::display::disarm_key_doorbell(me);
    }
    if doorbells & WAIT_INET != 0 {
        crate::ipc::inet::bell::disarm(me);
    }
}

/// Wake `slot` if it is parked on the Messenger queue (the raw bus's
/// doorbell). Takes the queue and task-table locks: call with no bus lock
/// held.
pub fn wake_parked(slot: usize) {
    MESSENGER.notify_task(slot);
}
