//! The growable descriptor table (`task::FdTable`): growth up to
//! `limit.fd_max`, `EMFILE` past it, `dup2` to a high descriptor, the fork and
//! exec copies at scale, and an open/close soak.

use super::*;
use crate::limits::{self, Id};
use crate::task::{Fd, FdTable, SocketKind, FD_CLOEXEC};

const SYS_CLOSE: u64 = 3;
const SYS_DUP2: u64 = 33;
const SYS_EVENTFD2: u64 = 290;
const EMFILE: u64 = (-24i64) as u64;
const EBADF: u64 = (-9i64) as u64;

fn eventfd() -> u64 {
    process::linux::dispatch_for_test(SYS_EVENTFD2, 0, 0, 0)
}

fn close_fd(fd: u64) -> u64 {
    process::linux::dispatch_for_test(SYS_CLOSE, fd, 0, 0)
}

/// A cheap entry with no shared object behind it.
fn unbound() -> Fd {
    Fd::Unbound {
        kind: SocketKind::Stream,
        nonblock: false,
    }
}

/// Run `body` with `limit.fd_max` set to `max`, restoring the default after.
fn with_fd_max(max: u64, body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    limits::set_for_test(Id::FdMax, max);
    let outcome = body();
    limits::reset_for_test();
    outcome
}

/// Opening past the old 16-descriptor table works up to the limit, the next
/// open is `EMFILE`, `dup2` reaches the last descriptor but not one past it,
/// and closing everything leaves the table clean.
pub fn fd_table_grows_to_the_limit() -> Result<(), String> {
    fresh()?;
    with_fd_max(300, || {
        let mut fds = Vec::new();
        loop {
            let fd = eventfd();
            if fd as i64 <= 0 {
                check!(fd == EMFILE, "a full table gave {fd:#x}, not EMFILE");
                break;
            }
            check!(fd < 300, "descriptor {fd} is past the limit");
            fds.push(fd);
            check!(fds.len() <= 300, "the table never filled");
        }
        check!(fds.len() == 297, "{} descriptors before EMFILE", fds.len());
        // A hole is refilled first (lowest free descriptor).
        check!(close_fd(fds[10]) == 0, "close failed");
        check!(eventfd() == fds[10], "the hole was not reused");
        check!(close_fd(299) == 0, "close of the top failed");
        let dup = process::linux::dispatch_for_test(SYS_DUP2, fds[0], 299, 0);
        check!(dup == 299, "dup2 to the last descriptor gave {dup:#x}");
        let past = process::linux::dispatch_for_test(SYS_DUP2, fds[0], 300, 0);
        check!(past == EBADF, "dup2 past the limit gave {past:#x}");
        for fd in 3..300 {
            let _ = close_fd(fd);
        }
        check!(fds_clean(), "descriptors survived closing");
        Ok(())
    })
}

/// The table type itself: lowest-free install, sparse `put`, flags, the fork
/// copy (everything) and the exec copy (no `FD_CLOEXEC`, flags cleared).
pub fn fd_table_copies_at_scale() -> Result<(), String> {
    with_fd_max(4096, || {
        let mut table = FdTable::standard();
        for expected in 3..2000 {
            let fd = table
                .install_lowest(3, unbound())
                .map_err(|_| "install refused")?;
            check!(fd == expected, "installed at {fd}, expected {expected}");
            if fd % 2 == 1 {
                table.set_flags(fd, FD_CLOEXEC);
            }
        }
        check!(table.put(3000, Fd::Terminal).is_ok(), "sparse put refused");
        check!(
            table.put(4096, Fd::Terminal).is_err(),
            "put past the limit accepted"
        );
        let forked = table.fork_copy().ok_or("fork copy failed")?;
        let exec = table.exec_copy().ok_or("exec copy failed")?;
        for fd in 0..3001 {
            check!(
                forked.is_open(fd) == table.is_open(fd) && forked.flags(fd) == table.flags(fd),
                "fork copy differs at {fd}"
            );
            let kept = table.is_open(fd) && table.flags(fd).unwrap_or(0) & FD_CLOEXEC == 0;
            check!(exec.is_open(fd) == kept, "exec copy wrong at {fd}");
            check!(
                !exec.is_open(fd) || exec.flags(fd) == Some(0),
                "exec kept flags at {fd}"
            );
        }
        check!(table.cloexec_fds().len() == 999, "cloexec count");
        let mut drained = table;
        let old = drained.take_all();
        check!(
            old.iter().count() == 2001 && drained.iter().count() == 0,
            "take_all"
        );
        Ok(())
    })
}

/// Soak: open and close 1000 descriptors, 30 times; the table is reused and
/// the heap returns to where it was after the first round grew the table.
pub fn fd_table_open_close_soak() -> Result<(), String> {
    fresh()?;
    let mut baseline = None;
    for round in 0..30 {
        let mut fds = Vec::new();
        for _ in 0..1000 {
            let fd = eventfd();
            check!((fd as i64) > 0, "round {round}: eventfd gave {fd:#x}");
            fds.push(fd);
        }
        for fd in fds {
            check!(close_fd(fd) == 0, "round {round}: close {fd} failed");
        }
        check!(fds_clean(), "round {round}: descriptors leaked");
        let used = mem::heap_stats().used;
        match baseline {
            None => baseline = Some(used),
            Some(base) => check!(
                used <= base + 4096,
                "round {round}: heap use grew from {base} to {used}"
            ),
        }
    }
    Ok(())
}
