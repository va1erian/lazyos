//! A closed standard stream is the lowest free descriptor: `open`, `dup` and
//! `F_DUPFD` refill it, as POSIX requires. BusyBox ash gives a background job
//! of a non-interactive shell `/dev/null` as stdin with `close(0);
//! open("/dev/null", O_RDONLY)` and refuses the job unless that open returns
//! 0. Descriptors used to be allocated from 3, so the open succeeded at 3 and
//! ash reported `can't open '/dev/null'` with whatever stale `errno` it had
//! (`sh script.sh` whose script runs `prog &` never started `prog`).

use super::inherit::{as_child, live_bytes};
use super::*;

const SYS_DUP2: u64 = 33;
const F_DUPFD: u64 = 0;
const S_IFMT: u32 = 0o170000;
const S_IFCHR: u32 = 0o020000;
/// `>` as ash opens it.
const REDIRECT_OUT: u64 = O_WRONLY | O_CREAT | O_TRUNC;

/// The `st_mode` `fstat` reports for `fd`.
fn fstat_mode(fd: u64) -> Result<u32, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "fstat({fd}) returned {ret:#x}");
    Ok(u32::from_le_bytes(stat[24..28].try_into().unwrap()))
}

/// Close standard stream `fd` and reopen it as `/dev/null` with `flags`: the
/// open must land on `fd` itself and be the null device.
fn reopen_as_null(fd: u64, flags: u64) -> Result<(), String> {
    check!(close(fd) == 0, "close({fd})");
    let got = open("/dev/null", flags);
    check!(
        got == fd,
        "open(/dev/null) after close({fd}) returned {got:#x}"
    );
    let mode = fstat_mode(fd)?;
    check!(mode & S_IFMT == S_IFCHR, "fd {fd} has mode {mode:#o}");
    Ok(())
}

/// What a forked background job does before it runs: stdin from `/dev/null`
/// (`close(0); open(O_RDONLY) == 0`), and the script's `>/dev/null` and
/// `2>&1` on the other streams. Reads end at once, writes vanish. `next` is
/// the lowest descriptor above the standard streams that is still free.
fn background_job_stdio(next: u64) -> Result<(), String> {
    reopen_as_null(0, O_RDONLY)?;
    check!(
        read(0, 64) == Ok(Vec::new()),
        "stdin from /dev/null is not empty"
    );
    reopen_as_null(1, REDIRECT_OUT)?;
    check!(write(1, b"vanishes") == 8, "write to /dev/null on stdout");
    // `dup` and `F_DUPFD 0` refill the lowest hole too.
    check!(close(2) == 0, "close(2)");
    let dup = syscall(SYS_DUP, 1, 0, 0, 0);
    check!(dup == 2, "dup(1) after close(2) returned {dup:#x}");
    check!(close(2) == 0, "close(2) again");
    let fcntl_dup = syscall(SYS_FCNTL, 1, F_DUPFD, 0, 0);
    check!(fcntl_dup == 2, "F_DUPFD(1, 0) returned {fcntl_dup:#x}");
    // Every hole is filled, so the next open lands above the streams.
    let got = open("/dev/null", O_RDONLY);
    check!(got == next, "the next open returned {got:#x}, not {next}");
    check!(close(got) == 0, "close({got})");
    Ok(())
}

/// The parent's own standard streams are still the terminal.
fn parent_stdio_untouched() -> Result<(), String> {
    for fd in 0..3 {
        check!(
            task::fd_kind(fd) == task::FdKind::Terminal,
            "the parent's fd {fd} changed with the child's"
        );
    }
    Ok(())
}

/// A forked child reopens its standard streams as `/dev/null` at the numbers
/// it closed, without disturbing the parent's; a descriptor above the
/// standard streams (`>/dev/null` held open by the shell) stays where it was.
pub fn forked_child_reopens_stdio_as_null() -> Result<(), String> {
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();

    let held = open("/dev/null", REDIRECT_OUT);
    check!(
        held == 3,
        "open(/dev/null) in the parent returned {held:#x}"
    );
    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    as_child(parent, child, || {
        background_job_stdio(held + 1)?;
        check!(
            write(held, b"x") == 1 && fstat_mode(held)? & S_IFMT == S_IFCHR,
            "the inherited /dev/null descriptor moved"
        );
        // `dup2` onto a standard stream still lands exactly there.
        let onto = syscall(SYS_DUP2, held, 0, 0, 0);
        check!(onto == 0, "dup2(held, 0) returned {onto:#x}");
        Ok(())
    })?;
    task::harness::finish(child, 0);
    check!(task::reap_child().map(|r| r.0) == Some(child), "reap");

    parent_stdio_untouched()?;
    check!(close(held) == 0, "close the held descriptor");
    check!(task::free_slots() == slots, "task slots leaked");
    data.check_clean()
}

/// Soak: many background jobs, each a forked child that reopens all three
/// standard streams as `/dev/null` and writes through them. Task slots,
/// frames and heap bytes return to where they started.
pub fn soak_forked_null_stdio() -> Result<(), String> {
    const ROUNDS: usize = 300;
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();
    let frames = crate::mem::frame_stats().live();
    let bytes = live_bytes();

    for round in 0..ROUNDS {
        let child = task::spawn_fork().map_err(|e| format!("round {round}: fork {e}"))?;
        as_child(parent, child, || {
            background_job_stdio(3).map_err(|e| format!("round {round}: {e}"))?;
            reopen_as_null(2, O_WRONLY).map_err(|e| format!("round {round}: {e}"))?;
            check!(write(2, b"err") == 3, "round {round}: write to stderr");
            Ok(())
        })?;
        task::harness::finish(child, 0);
        check!(
            task::reap_child().map(|r| r.0) == Some(child),
            "round {round}: reap"
        );
    }

    parent_stdio_untouched()?;
    check!(task::free_slots() == slots, "task slots leaked");
    check!(crate::mem::frame_stats().live() == frames, "frames leaked");
    let now = live_bytes();
    check!(
        now < bytes + 16 * 1024,
        "heap grew by {} bytes over {ROUNDS} rounds",
        now.saturating_sub(bytes)
    );
    data.check_clean()
}
