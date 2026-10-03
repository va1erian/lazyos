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
/// Bit of the ready mask that means "the raw input bus has records".
pub const RAW_INPUT_READY: u64 = 1 << 63;

/// Park until one of `handles` has a message (or its peer closed), or, with
/// `raw_input`, until the caller's raw input ring holds records, or until
/// `deadline` (absolute ticks) passes. Returns the ready mask: bit `i` for
/// `handles[i]`, [`RAW_INPUT_READY`] for the bus.
///
/// Errors: `BadParcel` for an empty or oversized set, the handle errors of
/// `recv` for a bad handle, `WrongKind` for `raw_input` without a consumer
/// ring, `TimedOut`, and `Canceled` when a fatal signal must end the task.
pub fn wait_any(handles: &[u64], raw_input: bool, deadline: Option<u64>) -> Result<u64, Error> {
    if handles.len() > MAX_WAIT_ENDPOINTS || (handles.is_empty() && !raw_input) {
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
        if raw_input {
            match crate::input::bus::arm_doorbell(me) {
                Ok(true) => ready |= RAW_INPUT_READY,
                Ok(false) => {}
                Err(_) => {
                    unregister(ends, me, false);
                    return Err(Error::WrongKind);
                }
            }
        }
        if ready != 0 {
            unregister(ends, me, raw_input);
            return Ok(ready);
        }
        let reason = MESSENGER.wait(me, deadline);
        unregister(ends, me, raw_input);
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

/// Drop every registration [`ready_or_register`] and the doorbell made.
fn unregister(ends: &[(u64, usize)], me: usize, raw_input: bool) {
    for &(id, side) in ends {
        remove_waiter(id, side, me);
    }
    if raw_input {
        crate::input::bus::disarm_doorbell(me);
    }
}

/// Wake `slot` if it is parked on the Messenger queue (the raw bus's
/// doorbell). Takes the queue and task-table locks: call with no bus lock
/// held.
pub fn wake_parked(slot: usize) {
    MESSENGER.notify_task(slot);
}
