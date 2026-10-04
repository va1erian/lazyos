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
//!
//! Two flags widen the wait (P3 follow-ups). [`WAIT_DEADLINE_NS`] makes the
//! deadline absolute `arch::clock::monotonic_ns` instead of 100 Hz ticks, so
//! a 60 Hz frame or a client timer is not rounded to the tick. [`WAIT_FD`]
//! adds one of the caller's Linux descriptors (its number in the high half
//! of the flags) that counts as ready when `poll` would report `POLLIN` or a
//! hang-up: the desktop Terminal parks on its pty master beside its
//! compositor endpoints. Descriptor readiness has no per-object waiter list,
//! so a watching task is flagged in [`FD_WATCHERS`] and every `notify_poll`
//! (the advisory wake that pipes, ptys and sockets already ring for `poll`)
//! also wakes the flagged tasks parked here ([`wake_fd_watchers`]); the woken
//! wait rescans, exactly as `poll` does.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
/// Doorbell: a child of the caller finished and waits to be reaped
/// (`task::childbell`, docs/performance-plan.md P7.1; any task).
pub const WAIT_CHILD: u64 = 8;
/// Doorbell: the Linux descriptor in bits 32..63 of the flags is readable
/// (or hung up).
pub const WAIT_FD: u64 = 16;
/// Every doorbell [`wait_any`] knows.
pub const WAIT_DOORBELLS: u64 =
    WAIT_RAW_INPUT | WAIT_DISPLAY_KEYS | WAIT_INET | WAIT_CHILD | WAIT_FD;
/// Flag: the deadline is absolute monotonic nanoseconds, not PIT ticks.
pub const WAIT_DEADLINE_NS: u64 = 1 << 24;
/// Where [`WAIT_FD`]'s descriptor sits in the flags.
pub const WAIT_FD_SHIFT: u32 = 32;
/// Bit of the ready mask that means "the raw input bus has records".
pub const RAW_INPUT_READY: u64 = 1 << 63;
/// Bit of the ready mask that means "the display input queue has events".
pub const DISPLAY_INPUT_READY: u64 = 1 << 62;
/// Bit of the ready mask that means "the `AF_INET` pump has work".
pub const INET_READY: u64 = 1 << 61;
/// Bit of the ready mask that means "a child waits to be reaped".
pub const CHILD_READY: u64 = 1 << 60;
/// Bit of the ready mask that means "the [`WAIT_FD`] descriptor is readable".
pub const FD_READY: u64 = 1 << 59;

/// Tasks parked in [`wait_any`] on a descriptor ([`WAIT_FD`]).
static FD_WATCHERS: [AtomicBool; task::MAX_TASKS] =
    [const { AtomicBool::new(false) }; task::MAX_TASKS];
/// How many [`FD_WATCHERS`] are set, so a `notify_poll` with nobody watching
/// (the common case) costs one load.
static FD_WATCHING: AtomicUsize = AtomicUsize::new(0);

/// Flag or unflag `slot` as watching a descriptor, keeping the count.
fn watch_fd(slot: usize, on: bool) {
    if let Some(flag) = FD_WATCHERS.get(slot) {
        if flag.swap(on, Ordering::Relaxed) != on {
            if on {
                FD_WATCHING.fetch_add(1, Ordering::Relaxed);
            } else {
                FD_WATCHING.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// How many tasks are parked on a descriptor (test hook: a leak shows up as
/// a count that never returns to zero).
#[allow(dead_code)]
pub fn fd_watchers() -> usize {
    FD_WATCHING.load(Ordering::Relaxed)
}

/// Whether `flags` uses only what [`wait_any`] knows: doorbells, the
/// nanosecond flag, and a descriptor number with [`WAIT_FD`] (the syscall
/// gate refuses anything else before the op runs).
pub fn wait_flags_known(flags: u64) -> bool {
    let low = flags & ((1 << WAIT_FD_SHIFT) - 1);
    let fd = flags >> WAIT_FD_SHIFT;
    low & !(WAIT_DOORBELLS | WAIT_DEADLINE_NS) == 0 && (fd == 0 || low & WAIT_FD != 0)
}

/// Park until one of `handles` has a message (or its peer closed), or until
/// one of the doorbells in `flags` ([`WAIT_RAW_INPUT`], [`WAIT_DISPLAY_KEYS`],
/// [`WAIT_INET`], [`WAIT_FD`]) rings, or until `deadline` passes (absolute
/// ticks, or monotonic nanoseconds with [`WAIT_DEADLINE_NS`]). Returns the
/// ready mask: bit `i` for `handles[i]`, [`RAW_INPUT_READY`],
/// [`DISPLAY_INPUT_READY`], [`INET_READY`] and [`FD_READY`] for the doorbells.
///
/// Errors: `BadParcel` for an empty or oversized set, an unknown flag, or a
/// descriptor number without [`WAIT_FD`]; the handle errors of `recv` for a
/// bad handle; `WrongKind` for a doorbell the caller may not ring (no raw
/// ring; not the display owner; no such descriptor), `TimedOut`, and
/// `Canceled` when a fatal signal must end the task.
pub fn wait_any(handles: &[u64], flags: u64, deadline: Option<u64>) -> Result<u64, Error> {
    let fd = flags >> WAIT_FD_SHIFT;
    let low = flags & ((1 << WAIT_FD_SHIFT) - 1);
    let in_ns = low & WAIT_DEADLINE_NS != 0;
    let doorbells = low & !WAIT_DEADLINE_NS;
    let empty = handles.is_empty() && doorbells == 0;
    let stray_fd = fd != 0 && doorbells & WAIT_FD == 0;
    if handles.len() > MAX_WAIT_ENDPOINTS || empty || doorbells & !WAIT_DOORBELLS != 0 || stray_fd {
        return Err(Error::BadParcel);
    }
    // `fd` came from 32 bits, so it fits a `usize` on this 64-bit kernel.
    let fd = (doorbells & WAIT_FD != 0).then_some(fd as usize);
    let mut ends = [(0u64, 0usize); MAX_WAIT_ENDPOINTS];
    for (end, &handle) in ends.iter_mut().zip(handles) {
        *end = endpoint_of(handle, rights::CALL)?;
    }
    let ends = &ends[..handles.len()];
    let me = task::current();
    loop {
        let mut ready = ready_or_register(ends, me);
        match arm_doorbells(doorbells, fd, me) {
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
        let reason = if in_ns {
            MESSENGER.wait_ns(me, deadline)
        } else {
            MESSENGER.wait(me, deadline)
        };
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
fn arm_doorbells(doorbells: u64, fd: Option<usize>, me: usize) -> Result<u64, Error> {
    let mut ready = 0;
    if let Some(fd) = fd {
        // Flag first, then look: a `notify_poll` between the two finds the
        // flag; one before it is covered by the look itself.
        watch_fd(me, true);
        let interesting = crate::ipc::pipe::POLLIN;
        match task::fd_poll(fd, interesting) {
            Some(0) => {}
            Some(_) => ready |= FD_READY,
            None => return Err(Error::WrongKind),
        }
    }
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
    if doorbells & WAIT_CHILD != 0 && crate::task::childbell::arm(me) {
        ready |= CHILD_READY;
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
    if doorbells & WAIT_CHILD != 0 {
        crate::task::childbell::disarm(me);
    }
    if doorbells & WAIT_FD != 0 {
        watch_fd(me, false);
    }
}

/// Wake every task parked in [`wait_any`] on a descriptor: called by
/// `task::notify_poll`, so anything that can make a descriptor readable
/// (pipe and pty writes, hang-ups, socket data) reaches it as it reaches
/// `poll`. Advisory, like that queue: the woken wait rescans and parks again
/// if its descriptor is still not ready.
pub fn wake_fd_watchers() {
    if FD_WATCHING.load(Ordering::Relaxed) == 0 {
        return;
    }
    for (slot, flag) in FD_WATCHERS.iter().enumerate() {
        if flag.load(Ordering::Relaxed) {
            MESSENGER.notify_task(slot);
        }
    }
}

/// Wake `slot` if it is parked on the Messenger queue (the raw bus's
/// doorbell). Takes the queue and task-table locks: call with no bus lock
/// held.
pub fn wake_parked(slot: usize) {
    MESSENGER.notify_task(slot);
}
