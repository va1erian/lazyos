//! `RLIMIT_NOFILE` is a real, per-process limit (`task::fdlimit`): the soft
//! limit stops new descriptors with `EMFILE`, `setrlimit` may lower it and
//! raise it back to the hard limit but never past the kernel's `fd_max`, and
//! copies of a table (`fork`, `execve`, threads) carry it.

use super::*;
use crate::task::{Fd, FdTable, SocketKind};

const SYS_DUP2: u64 = 33;
const SYS_FCNTL: u64 = 72;
const SYS_GETRLIMIT: u64 = 97;
const SYS_SETRLIMIT: u64 = 160;
const SYS_EVENTFD2: u64 = 290;
const RLIMIT_NOFILE: u64 = 7;
const F_DUPFD: u64 = 0;
const EPERM: u64 = (-1i64) as u64;
const EBADF: u64 = (-9i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const EMFILE: u64 = (-24i64) as u64;

fn eventfd() -> u64 {
    process::linux::dispatch_for_test(SYS_EVENTFD2, 0, 0, 0)
}

fn set_nofile(soft: u64, hard: u64) -> u64 {
    let pair = [soft, hard];
    process::linux::dispatch_for_test(SYS_SETRLIMIT, RLIMIT_NOFILE, pair.as_ptr() as u64, 0)
}

fn get_nofile() -> Result<[u64; 2], String> {
    let mut pair = [0u64; 2];
    let got = process::linux::dispatch_for_test(
        SYS_GETRLIMIT,
        RLIMIT_NOFILE,
        pair.as_mut_ptr() as u64,
        0,
    );
    check!(got == 0, "getrlimit(NOFILE) = {got:#x}");
    Ok(pair)
}

fn close_from(first: usize) {
    for fd in first..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
}

/// The soft limit stops descriptors at exactly its value, `dup2` and
/// `F_DUPFD` past it are refused, raising it to the hard limit reopens the
/// table, and the hard limit can only go down.
pub fn nofile_soft_limit_is_enforced() -> Result<(), String> {
    fresh()?;
    let kernel_max = task::fd_max() as u64;
    check!(
        get_nofile()? == [kernel_max; 2],
        "default {:?}",
        get_nofile()
    );

    // Grow the table past the coming limit and close it all again: the slots
    // stay allocated, and must not be handed out past the lowered limit.
    for _ in 0..60 {
        check!((eventfd() as i64) > 0, "the table did not grow");
    }
    close_from(3);
    check!(
        set_nofile(40, kernel_max) == 0,
        "lowering the soft limit failed"
    );
    check!(get_nofile()? == [40, kernel_max], "got {:?}", get_nofile());
    let mut opened = Vec::new();
    loop {
        let fd = eventfd();
        if fd as i64 <= 0 {
            check!(fd == EMFILE, "a full table gave {fd:#x}, not EMFILE");
            break;
        }
        check!(fd < 40, "descriptor {fd} is past the soft limit");
        opened.push(fd);
        check!(opened.len() < 64, "the soft limit never bit");
    }
    check!(
        opened.len() == 37,
        "{} descriptors before EMFILE",
        opened.len()
    );
    let dup = process::linux::dispatch_for_test(SYS_DUP2, opened[0], 40, 0);
    check!(dup == EBADF, "dup2 past the soft limit gave {dup:#x}");
    let dupfd = process::linux::dispatch_for_test(SYS_FCNTL, opened[0], F_DUPFD, 40);
    check!(
        dupfd == EINVAL,
        "F_DUPFD past the soft limit gave {dupfd:#x}"
    );

    // Raising the soft limit up to the hard one is what programs do at start.
    check!(
        set_nofile(kernel_max, kernel_max) == 0,
        "raising to the hard limit"
    );
    check!(
        (eventfd() as i64) > 0,
        "no room after raising the soft limit"
    );

    // The hard limit only goes down.
    check!(set_nofile(20, 30) == 0, "lowering both limits failed");
    check!(get_nofile()? == [20, 30], "got {:?}", get_nofile());
    check!(set_nofile(20, 31) == EPERM, "the hard limit was raised");
    check!(set_nofile(31, 30) == EINVAL, "soft above hard accepted");
    check!(
        set_nofile(30, 30) == 0,
        "raising the soft limit to the hard one"
    );
    check!(
        set_nofile(30, kernel_max + 1) == EPERM,
        "hard above fd_max accepted"
    );
    check!(
        set_nofile(u64::MAX, u64::MAX) == EPERM,
        "an unbounded hard limit accepted"
    );

    close_from(3);
    check!(fds_clean(), "descriptors survived closing");
    Ok(())
}

/// The limit travels with the table: `fork`, `execve` and thread copies keep
/// it, and a copy stops growing at it.
pub fn nofile_limit_travels_with_the_table() -> Result<(), String> {
    fresh()?;
    let mut table = FdTable::standard();
    table.set_limits(6, 9);
    for copy in [
        table.fork_copy().ok_or("fork copy")?,
        table.exec_copy().ok_or("exec copy")?,
        table.mirror_copy().ok_or("mirror copy")?,
    ] {
        check!(
            (copy.soft_limit(), copy.hard_limit()) == (6, 9),
            "copy has {:?}",
            (copy.soft_limit(), copy.hard_limit())
        );
    }
    let mut copy = table.fork_copy().ok_or("fork copy")?;
    let unbound = || Fd::Unbound {
        kind: SocketKind::Stream,
        nonblock: false,
    };
    for expected in 3..6 {
        let fd = copy
            .install_lowest(3, unbound())
            .map_err(|_| "refused early")?;
        check!(fd == expected, "installed at {fd}");
    }
    check!(
        copy.install_lowest(3, unbound()).is_err(),
        "the copy grew past its soft limit"
    );
    check!(
        copy.put(6, Fd::Terminal).is_err(),
        "put past the soft limit"
    );
    Ok(())
}

/// A copy of a table (a thread's, a child's) is as long as the descriptors it
/// holds, not as long as the table ever grew: closed slots at the end are not
/// copied, and every copy keeps the open ones where they were.
pub fn fd_table_copies_skip_the_closed_tail() -> Result<(), String> {
    fresh()?;
    let mut table = FdTable::standard();
    let unbound = || Fd::Unbound {
        kind: SocketKind::Stream,
        nonblock: false,
    };
    let before = mem::heap_stats().used;
    for _ in 3..5000 {
        table
            .install_lowest(3, unbound())
            .map_err(|_| "install refused")?;
    }
    // The price of an open descriptor in a table (the slot, its doubling
    // headroom included), for docs/architecture/limits.md.
    crate::trace::test!(
        "linux_fd_table_copies_skip_the_closed_tail:INFO:bytes_per_descriptor={}",
        (mem::heap_stats().used - before) / 4997
    );
    for fd in 7..5000 {
        check!(table.take(fd).is_some(), "close {fd}");
    }
    check!(table.len() >= 5000, "the table shrank on its own");
    for (name, copy) in [
        ("fork", table.fork_copy()),
        ("exec", table.exec_copy()),
        ("mirror", table.mirror_copy()),
    ] {
        let copy = copy.ok_or("copy failed")?;
        check!(copy.len() == 7, "{name} copy is {} slots long", copy.len());
        check!(
            (0..7).all(|fd| copy.is_open(fd)),
            "{name} copy lost a descriptor"
        );
    }
    Ok(())
}

/// `setrlimit` in one thread changes the whole process: every thread of the
/// group sees it, and so does a thread started later.
pub fn nofile_limit_covers_the_thread_group() -> Result<(), String> {
    fresh()?;
    let process_slot = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    task::harness::switch_current(process_slot);
    let first = task::spawn_thread("limit-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    check!(set_nofile(33, 44) == 0, "setrlimit failed");
    let second = task::spawn_thread("limit-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("second thread: {error}"))?;
    let outcome = [process_slot, first, second].iter().try_for_each(|&slot| {
        let limits = task::nofile_limits_of(slot);
        check!(limits == Some((33, 44)), "slot {slot}: {limits:?}");
        Ok(())
    });
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    outcome
}
