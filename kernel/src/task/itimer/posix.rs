//! The POSIX timer calls (`timer_create` and friends) over the shared table.

use super::{changed, clamp_interval, with_table, Caller, Error, Kind, Notify, Reading, Timer};
use super::{MAX_POSIX_PER_PROCESS, MAX_POSIX_PER_UID};

/// `timer_create`: a disarmed POSIX timer of process `pml4`; its id.
pub fn create(
    pml4: u64,
    owner: usize,
    caller: Caller,
    notify: Notify,
    signo: u8,
    value: Option<u64>,
    clock: u8,
) -> Result<i32, Error> {
    with_table(|table| {
        let mine = table
            .timers
            .iter()
            .filter(|t| t.used && t.pml4 == pml4 && t.kind == Kind::Posix)
            .count();
        if mine >= MAX_POSIX_PER_PROCESS {
            return Err(Error::NoSpace);
        }
        if !caller.privileged {
            let theirs = table
                .timers
                .iter()
                .filter(|t| t.used && t.uid == caller.uid && t.kind == Kind::Posix)
                .count();
            if theirs >= MAX_POSIX_PER_UID {
                return Err(Error::NoSpace);
            }
        }
        // The lowest id not in use, like Linux's per-process idr.
        let id = (0..)
            .find(|&id| table.find(pml4, Kind::Posix, id).is_none())
            .ok_or(Error::NoSpace)?;
        table.alloc(Timer {
            kind: Kind::Posix,
            pml4,
            id,
            owner,
            notify,
            signo,
            // Without a sigevent the value is the timer's id (Linux).
            value: value.unwrap_or(id as u64),
            clock,
            uid: caller.uid,
            min_interval_ns: caller.min_interval_ns(),
            ..Timer::EMPTY
        })?;
        Ok(id)
    })
}

/// `timer_settime`: disarm (`start` is `None`) or arm timer `id` to expire at
/// the monotonic instant `start`, repeating every `interval_ns`. Returns the
/// previous reading.
pub fn settime(
    pml4: u64,
    id: i32,
    now_ns: u64,
    start: Option<u64>,
    interval_ns: u64,
) -> Result<Reading, Error> {
    let old = with_table(|table| {
        let index = table
            .find(pml4, Kind::Posix, id)
            .ok_or(Error::NoSuchTimer)?;
        let old = table.timers[index].reading(now_ns);
        match start {
            None => {
                table.disarm(index);
                table.timers[index].interval_ns = 0;
            }
            Some(deadline) => {
                let interval = clamp_interval(interval_ns, table.timers[index].min_interval_ns);
                table.arm(index, deadline, 0, interval);
            }
        }
        Ok(old)
    })?;
    changed();
    Ok(old)
}

/// The clock timer `id` was created on.
pub fn clock_of(pml4: u64, id: i32) -> Result<u8, Error> {
    with_table(|table| {
        let index = table
            .find(pml4, Kind::Posix, id)
            .ok_or(Error::NoSuchTimer)?;
        Ok(table.timers[index].clock)
    })
}

/// `timer_gettime`.
pub fn gettime(pml4: u64, id: i32, now_ns: u64) -> Result<Reading, Error> {
    with_table(|table| {
        let index = table
            .find(pml4, Kind::Posix, id)
            .ok_or(Error::NoSuchTimer)?;
        Ok(table.timers[index].reading(now_ns))
    })
}

/// `timer_getoverrun`.
pub fn overrun(pml4: u64, id: i32) -> Result<u32, Error> {
    with_table(|table| {
        let index = table
            .find(pml4, Kind::Posix, id)
            .ok_or(Error::NoSuchTimer)?;
        Ok(table.timers[index]
            .overrun_last
            .saturating_add(table.timers[index].overrun))
    })
}

/// `timer_delete`.
pub fn delete(pml4: u64, id: i32) -> Result<(), Error> {
    with_table(|table| {
        let index = table
            .find(pml4, Kind::Posix, id)
            .ok_or(Error::NoSuchTimer)?;
        table.free(index);
        Ok(())
    })?;
    changed();
    Ok(())
}
