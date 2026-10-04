//! `select`, `pselect6` and `ppoll`: the other readiness waits, on the same
//! descriptor readiness and wait queue as `poll` ([`super::io`]).
//!
//! `pselect6` and `ppoll` install their signal mask for the duration of the
//! wait the way `rt_sigsuspend` does ([`signal::suspend_begin`]): the original
//! mask comes back on the way out of the syscall, after any handler the
//! temporary mask let through has run, so a signal cannot be lost between
//! unblocking it and waiting.

use crate::ipc::pipe::{POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::task::{self, signal, WakeReason};
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINTR, EINVAL};
use super::io::scan_poll;
use super::time::{deadline_after_ns, now_ns, timespec_ns, write_duration};

/// `FD_SETSIZE`: the largest descriptor set `select` takes.
const FD_SETSIZE: u64 = 1024;
/// `POLLPRI`: exceptional conditions (out-of-band data); nothing here has any.
const POLLPRI: u16 = 0x0002;

/// Read a `struct timespec`/`struct timeval` (`unit` = 1 for nanoseconds,
/// 1000 for microseconds) as nanoseconds; `EINVAL` for a negative or
/// non-canonical value.
fn read_timeout(ptr: u64, unit: u64) -> Result<u64, u64> {
    let (Ok(sec), Ok(frac)) = (
        user_ptr::try_read::<i64>(ptr),
        user_ptr::try_read::<i64>(ptr + 8),
    ) else {
        return Err(err(EFAULT));
    };
    if sec < 0 || frac < 0 || frac as u64 >= 1_000_000_000 / unit {
        return Err(err(EINVAL));
    }
    Ok(timespec_ns(sec as u64, frac as u64 * unit))
}

/// Write the time left before `deadline` back as a timespec/timeval.
fn write_remaining(ptr: u64, deadline: u64, unit: u64) {
    write_duration(ptr, deadline.saturating_sub(now_ns()), unit);
}

/// Wait until `scan` reports a non-zero count (or an error), or `deadline`
/// (monotonic ns) passes (`Ok(0)`), or a signal arrives (`EINTR`). `scan`
/// runs with interrupts off, so no readiness change can slip between it and
/// the park.
fn wait_ready(deadline: Option<u64>, mut scan: impl FnMut() -> u64) -> u64 {
    loop {
        // Record what this scan looks at: only those objects' events wake it.
        task::poll_scan_begin();
        let ready = scan();
        if ready != 0 || deadline.is_some_and(|due| due <= now_ns()) {
            return ready;
        }
        match task::wait_poll_keyed_ns(deadline) {
            WakeReason::Woken => {}
            WakeReason::TimedOut => return scan(),
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// Install a temporary signal mask from a user `sigset_t` (`size` must be 8).
pub(super) fn begin_sigmask(set: u64, size: u64) -> Result<(), u64> {
    if set == 0 {
        return Ok(());
    }
    if size != 8 {
        return Err(err(EINVAL));
    }
    let mask = user_ptr::try_read::<u64>(set).map_err(|_| err(EFAULT))?;
    signal::suspend_begin(task::current(), signal::linux_sigset_to_kernel(mask));
    Ok(())
}

/// `ppoll(fds, nfds, timeout, sigmask, sigsetsize)`.
pub(super) fn sys_ppoll(fds: u64, nfds: u64, timeout: u64, sigmask: u64, size: u64) -> u64 {
    if nfds > FD_SETSIZE {
        return err(EINVAL);
    }
    let deadline = if timeout == 0 {
        None
    } else {
        match read_timeout(timeout, 1) {
            Ok(ns) => Some(deadline_after_ns(ns)),
            Err(code) => return code,
        }
    };
    if let Err(code) = begin_sigmask(sigmask, size) {
        return code;
    }
    let result = wait_ready(deadline, || scan_poll(fds, nfds));
    if let Some(due) = deadline {
        write_remaining(timeout, due, 1);
    }
    result
}

/// The three descriptor sets of a `select`, as bitmaps of `nfds` bits.
struct Sets {
    read: [u64; 16],
    write: [u64; 16],
    except: [u64; 16],
}

fn read_set(ptr: u64, words: usize) -> Result<[u64; 16], u64> {
    let mut set = [0u64; 16];
    if ptr != 0 {
        for (index, word) in set.iter_mut().take(words).enumerate() {
            *word = user_ptr::try_read_at::<u64>(ptr, index).map_err(|_| err(EFAULT))?;
        }
    }
    Ok(set)
}

fn write_set(ptr: u64, set: &[u64; 16], words: usize) -> Result<(), u64> {
    if ptr == 0 {
        return Ok(());
    }
    user_ptr::try_copy_words(ptr, &set[..words]).map_err(|_| err(EFAULT))
}

fn bit(set: &[u64; 16], fd: usize) -> bool {
    set[fd / 64] & (1 << (fd % 64)) != 0
}

/// One pass: the ready sets and their total bit count, or `EBADF` for a
/// requested descriptor that is not open.
fn scan_select(nfds: usize, wanted: &Sets) -> Result<(Sets, u64), u64> {
    let mut ready = Sets {
        read: [0; 16],
        write: [0; 16],
        except: [0; 16],
    };
    let mut count = 0;
    for fd in 0..nfds {
        let (r, w, x) = (
            bit(&wanted.read, fd),
            bit(&wanted.write, fd),
            bit(&wanted.except, fd),
        );
        if !(r || w || x) {
            continue;
        }
        let revents = task::fd_poll(fd, POLLIN | POLLOUT | POLLPRI).ok_or(err(EBADF))?;
        let mark = |set: &mut [u64; 16], count: &mut u64| {
            set[fd / 64] |= 1 << (fd % 64);
            *count += 1;
        };
        if r && revents & (POLLIN | POLLHUP | POLLERR) != 0 {
            mark(&mut ready.read, &mut count);
        }
        if w && revents & (POLLOUT | POLLERR) != 0 {
            mark(&mut ready.write, &mut count);
        }
        if x && revents & POLLPRI != 0 {
            mark(&mut ready.except, &mut count);
        }
    }
    Ok((ready, count))
}

/// The body of `select` and `pselect6` once the timeout is decoded.
fn do_select(nfds: u64, ptrs: [u64; 3], deadline: Option<u64>) -> u64 {
    if nfds > FD_SETSIZE {
        return err(EINVAL);
    }
    let words = (nfds as usize).div_ceil(64);
    let wanted = match (
        read_set(ptrs[0], words),
        read_set(ptrs[1], words),
        read_set(ptrs[2], words),
    ) {
        (Ok(read), Ok(write), Ok(except)) => Sets {
            read,
            write,
            except,
        },
        _ => return err(EFAULT),
    };
    let mut outcome = None;
    let result = wait_ready(deadline, || match scan_select(nfds as usize, &wanted) {
        Ok((sets, count)) => {
            outcome = Some(sets);
            count
        }
        Err(code) => code,
    });
    if (result as i64) < 0 {
        return result;
    }
    let empty = Sets {
        read: [0; 16],
        write: [0; 16],
        except: [0; 16],
    };
    let sets = if result == 0 {
        empty
    } else {
        outcome.unwrap_or(empty)
    };
    for (ptr, set) in ptrs.iter().zip([&sets.read, &sets.write, &sets.except]) {
        if let Err(code) = write_set(*ptr, set, words) {
            return code;
        }
    }
    result
}

/// `select(nfds, readfds, writefds, exceptfds, timeout)`: like Linux, the
/// `struct timeval` is updated with the time left.
pub(super) fn sys_select(nfds: u64, read: u64, write: u64, except: u64, timeout: u64) -> u64 {
    let deadline = if timeout == 0 {
        None
    } else {
        match read_timeout(timeout, 1000) {
            Ok(ns) => Some(deadline_after_ns(ns)),
            Err(code) => return code,
        }
    };
    let result = do_select(nfds, [read, write, except], deadline);
    if let Some(due) = deadline {
        write_remaining(timeout, due, 1000);
    }
    result
}

/// `pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask)`, where
/// `sigmask` points at `{ const sigset_t *set; size_t size; }`.
pub(super) fn sys_pselect6(nfds: u64, sets: [u64; 3], timeout: u64, sigmask: u64) -> u64 {
    let deadline = if timeout == 0 {
        None
    } else {
        match read_timeout(timeout, 1) {
            Ok(ns) => Some(deadline_after_ns(ns)),
            Err(code) => return code,
        }
    };
    if sigmask != 0 {
        let (Ok(set), Ok(size)) = (
            user_ptr::try_read::<u64>(sigmask),
            user_ptr::try_read::<u64>(sigmask + 8),
        ) else {
            return err(EFAULT);
        };
        if let Err(code) = begin_sigmask(set, size) {
            return code;
        }
    }
    let result = do_select(nfds, sets, deadline);
    if let Some(due) = deadline {
        write_remaining(timeout, due, 1);
    }
    result
}
