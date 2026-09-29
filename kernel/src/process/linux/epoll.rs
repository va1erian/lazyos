//! Readiness polling and `eventfd`: `eventfd2`, `epoll_create1`, `epoll_ctl`,
//! and `epoll_wait`. `eventfd` lives here rather than with the fd-table
//! syscalls because its only two callers, `epoll_ctl` watching one and
//! `write`/`read` on it directly, are both readiness-shaped operations.

use alloc::sync::Arc;

use crate::ipc::epoll::{Epoll, NestError};
use crate::ipc::eventfd::EventFd;
use crate::task::{self, Fd, WakeReason};
use crate::user_ptr;

use super::errno::{err, EBADF, EEXIST, EINTR, EINVAL, ELOOP, EMFILE, ENOENT};
use super::flags::{O_CLOEXEC, O_NONBLOCK};
use super::time::millis_to_ticks;

/// `eventfd2(2)` flags.
const EFD_SEMAPHORE: u64 = 1;

/// `epoll_ctl(2)` operations and creation flags.
const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;
const EPOLL_CLOEXEC: u64 = 0o2000000;

/// `eventfd2(initval, flags)`: a counter descriptor.
pub(super) fn sys_eventfd2(init: u64, flags: u64) -> u64 {
    let allowed = EFD_SEMAPHORE | O_NONBLOCK | O_CLOEXEC;
    if flags & !allowed != 0 {
        return err(EINVAL);
    }
    let event = EventFd::new(init & 0xFFFF_FFFF, flags & EFD_SEMAPHORE != 0);
    if flags & O_NONBLOCK != 0 {
        event.set_nonblock(true);
    }
    match task::fd_open(Fd::Event { event }) {
        Some(fd) => {
            if flags & O_CLOEXEC != 0 {
                task::fd_set_cloexec(fd, true);
            }
            fd as u64
        }
        None => err(EMFILE),
    }
}

/// `epoll_create1(flags)`: an empty epoll instance.
pub(super) fn sys_epoll_create1(flags: u64) -> u64 {
    if flags & !EPOLL_CLOEXEC != 0 {
        return err(EINVAL);
    }
    match task::fd_open(Fd::Epoll {
        epoll: Epoll::new(),
    }) {
        Some(fd) => {
            if flags & EPOLL_CLOEXEC != 0 {
                task::fd_set_cloexec(fd, true);
            }
            fd as u64
        }
        None => err(EMFILE),
    }
}

/// Resolve an epoll descriptor, distinguishing a closed slot (`EBADF`) from a
/// descriptor that is not an epoll instance (`EINVAL`).
fn epoll_instance(epfd: u64) -> Result<Arc<Epoll>, u64> {
    match task::fd_clone(epfd as usize).as_ref() {
        Some(Fd::Epoll { epoll }) => Ok(Arc::clone(epoll)),
        Some(_) => Err(EINVAL),
        None => Err(EBADF),
    }
}

/// Read a user `struct epoll_event` (packed on x86_64: `u32 events`, `u64 data`
/// at offset 4). The struct is packed, so both fields may be unaligned.
fn read_epoll_event(ptr: u64) -> (u32, u64) {
    // Safety: user `struct epoll_event` (the syscall ABI's contract).
    let events = unsafe { user_ptr::read_unaligned::<u32>(ptr) };
    // Safety: same struct, packed `data` field at offset 4.
    let data = unsafe { user_ptr::read_unaligned::<u64>(ptr + 4) };
    (events, data)
}

/// Write the ready list as packed `struct epoll_event`s, returning the count.
fn write_epoll_events(ptr: u64, ready: &[(u32, u64)]) -> u64 {
    for (index, (events, data)) in ready.iter().enumerate() {
        let base = ptr + (index as u64) * 12;
        // Safety: user `struct epoll_event` array (the syscall ABI's contract);
        // the packed 12-byte stride leaves both fields unaligned.
        unsafe {
            user_ptr::write_unaligned::<u32>(base, *events);
            user_ptr::write_unaligned::<u64>(base + 4, *data);
        }
    }
    ready.len() as u64
}

/// `epoll_ctl(epfd, op, fd, event)`.
pub(super) fn sys_epoll_ctl(epfd: u64, op: u64, fd: u64, event: u64) -> u64 {
    let epoll = match epoll_instance(epfd) {
        Ok(epoll) => epoll,
        Err(error) => return err(error),
    };
    match op {
        EPOLL_CTL_ADD => {
            let Some(target) = task::fd_clone(fd as usize) else {
                return err(EBADF);
            };
            // An epoll may not watch itself, nor close a cycle of epolls, nor
            // stack them past `MAX_NEST`: readiness recurses through nested
            // instances, so any of those overflows the kernel stack.
            match Epoll::check_nest(&epoll, &target) {
                Ok(()) => {}
                Err(NestError::Loop) if fd == epfd => return err(EINVAL),
                Err(NestError::Loop) => return err(ELOOP),
                Err(NestError::TooDeep) => return err(EINVAL),
            }
            let (events, data) = read_epoll_event(event);
            match Epoll::add(&epoll, fd as usize, target, events, data) {
                Ok(()) => {
                    task::notify_poll();
                    0
                }
                Err(()) => err(EEXIST),
            }
        }
        EPOLL_CTL_MOD => {
            let (events, data) = read_epoll_event(event);
            match epoll.modify(fd as usize, events, data) {
                Ok(()) => {
                    task::notify_poll();
                    0
                }
                Err(()) => err(ENOENT),
            }
        }
        EPOLL_CTL_DEL => match Epoll::delete(&epoll, fd as usize) {
            Ok(()) => 0,
            Err(()) => err(ENOENT),
        },
        _ => err(EINVAL),
    }
}

/// `epoll_wait(epfd, events, maxevents, timeout)`: scan the interests, park on
/// the poll queue while none is ready, and honour millisecond timeouts.
pub(super) fn sys_epoll_wait(epfd: u64, events: u64, maxevents: u64, timeout: u64) -> u64 {
    if (maxevents as i64) <= 0 {
        return err(EINVAL);
    }
    let max = maxevents as usize;
    let epoll = match epoll_instance(epfd) {
        Ok(epoll) => epoll,
        Err(error) => return err(error),
    };
    let timeout = timeout as i64;
    let deadline = if timeout < 0 {
        None
    } else if timeout == 0 {
        return write_epoll_events(events, &epoll.ready(max));
    } else {
        Some(task::ticks() + millis_to_ticks(timeout as u64))
    };
    loop {
        let ready = epoll.ready(max);
        if !ready.is_empty() {
            return write_epoll_events(events, &ready);
        }
        match task::wait_poll(deadline) {
            WakeReason::Woken => {}
            WakeReason::TimedOut => return 0,
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}
