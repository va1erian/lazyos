//! `futex(uaddr, op, val, timeout_or_val2, uaddr2, val3)`: the wait/wake
//! primitive musl's mutexes, condition variables and thread join build on.
//!
//! Implemented operations: `FUTEX_WAIT` (relative timeout), `FUTEX_WAKE`,
//! `FUTEX_REQUEUE`, `FUTEX_CMP_REQUEUE`, `FUTEX_WAKE_OP`, `FUTEX_WAIT_BITSET`
//! (absolute timeout on `CLOCK_MONOTONIC`, or `CLOCK_REALTIME` with
//! `FUTEX_CLOCK_REALTIME`) and `FUTEX_WAKE_BITSET`. The priority-inheritance
//! family and `FUTEX_FD` answer `ENOSYS`, so a caller learns it must fall back
//! instead of believing a lock was taken. The waiter table and its keying (per
//! address space) are in [`super::futex_queue`].

use crate::task::WakeReason;
use crate::user_ptr;

use super::errno::{err, EAGAIN, EFAULT, EINTR, EINVAL, ENOSYS, ETIMEDOUT};
use super::futex_queue::{self as queue, Key};
use super::time::{
    clock_deadline_ns, deadline_after_ns, now_ns, timespec_ns, CLOCK_MONOTONIC, CLOCK_REALTIME,
};

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAKE_OP: u64 = 5;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
/// Modifier bits: the key is private to the process (always true here), and
/// a `WAIT_BITSET` deadline is on the wall clock.
const FUTEX_PRIVATE_FLAG: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;
/// Most waiters one call may wake or move; `INT_MAX` is the "all" musl passes.
const MAX_COUNT: u64 = i32::MAX as u64;

/// The six syscall arguments, named for the futex ABI.
pub(super) struct Args {
    pub(super) uaddr: u64,
    pub(super) op: u64,
    pub(super) val: u64,
    /// `timeout` for the waits, `val2` (a count) for the requeues and `WAKE_OP`.
    pub(super) timeout: u64,
    pub(super) uaddr2: u64,
    pub(super) val3: u64,
}

/// `futex(2)`. An unaligned or unreadable word is `EFAULT`/`EINVAL` before any
/// operation runs, as on Linux.
pub(super) fn sys_futex(args: Args) -> u64 {
    let cmd = args.op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    if args.op & FUTEX_CLOCK_REALTIME != 0 && cmd != FUTEX_WAIT_BITSET && cmd != FUTEX_WAIT {
        return err(ENOSYS);
    }
    if !args.uaddr.is_multiple_of(4) {
        return err(EINVAL);
    }
    match cmd {
        FUTEX_WAIT => {
            let deadline = match relative_deadline(args.timeout) {
                Ok(deadline) => deadline,
                Err(code) => return code,
            };
            futex_wait(args.uaddr, args.val as u32, u32::MAX, deadline)
        }
        FUTEX_WAIT_BITSET => {
            let bitset = args.val3 as u32;
            if bitset == 0 {
                return err(EINVAL);
            }
            let clock = if args.op & FUTEX_CLOCK_REALTIME != 0 {
                CLOCK_REALTIME
            } else {
                CLOCK_MONOTONIC
            };
            let deadline = match absolute_deadline(clock, args.timeout) {
                Ok(deadline) => deadline,
                Err(code) => return code,
            };
            futex_wait(args.uaddr, args.val as u32, bitset, deadline)
        }
        FUTEX_WAKE => wake_count(args.uaddr, u32::MAX, args.val),
        FUTEX_WAKE_BITSET => match args.val3 as u32 {
            0 => err(EINVAL),
            bitset => wake_count(args.uaddr, bitset, args.val),
        },
        FUTEX_REQUEUE => requeue(&args, None),
        FUTEX_CMP_REQUEUE => requeue(&args, Some(args.val3 as u32)),
        FUTEX_WAKE_OP => wake_op(&args),
        // FUTEX_FD (2), the PI family (6-8, 11-13) and anything newer.
        _ => {
            crate::serial_println!("ENOSYS 202 futex op {}", cmd);
            err(ENOSYS)
        }
    }
}

/// Read the futex word, or `EFAULT`.
fn read_word(uaddr: u64) -> Result<u32, u64> {
    user_ptr::try_read::<u32>(uaddr).map_err(|_| err(EFAULT))
}

/// A `struct timespec` at `ptr` as `(sec, nsec)`, validated like Linux
/// (`EINVAL` for a negative or non-canonical value).
fn read_timespec(ptr: u64) -> Result<(u64, u64), u64> {
    let (Ok(sec), Ok(nsec)) = (
        user_ptr::try_read::<i64>(ptr),
        user_ptr::try_read::<i64>(ptr + 8),
    ) else {
        return Err(err(EFAULT));
    };
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err(err(EINVAL));
    }
    Ok((sec as u64, nsec as u64))
}

/// `FUTEX_WAIT`'s timeout: a duration from now; null waits forever.
fn relative_deadline(ptr: u64) -> Result<Option<u64>, u64> {
    if ptr == 0 {
        return Ok(None);
    }
    let (sec, nsec) = read_timespec(ptr)?;
    // A zero timeout is a poll: the deadline is already due.
    Ok(Some(deadline_after_ns(timespec_ns(sec, nsec))))
}

/// `FUTEX_WAIT_BITSET`'s timeout: an absolute instant on `clock`.
fn absolute_deadline(clock: u64, ptr: u64) -> Result<Option<u64>, u64> {
    if ptr == 0 {
        return Ok(None);
    }
    let (sec, nsec) = read_timespec(ptr)?;
    Ok(Some(clock_deadline_ns(clock, sec, nsec)))
}

/// Park while `*uaddr == expected`. The comparison and the park are atomic
/// against every waker: interrupts are off in the syscall gate and there is
/// one CPU, so no `FUTEX_WAKE` can run between them.
fn futex_wait(uaddr: u64, expected: u32, bitset: u32, deadline: Option<u64>) -> u64 {
    let current = match read_word(uaddr) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if current != expected {
        return err(EAGAIN);
    }
    if deadline.is_some_and(|due| due <= now_ns()) {
        return err(ETIMEDOUT);
    }
    match queue::wait(Key::current(uaddr), bitset, deadline) {
        WakeReason::Woken => 0,
        WakeReason::TimedOut => err(ETIMEDOUT),
        WakeReason::Interrupted => err(EINTR),
    }
}

/// A count argument: negative (as an `int`) is `EINVAL`, larger is clamped.
fn count_arg(value: u64) -> Result<usize, u64> {
    if (value as u32 as i32) < 0 {
        return Err(err(EINVAL));
    }
    Ok((value as u32 as u64).min(MAX_COUNT) as usize)
}

fn wake_count(uaddr: u64, bitset: u32, count: u64) -> u64 {
    match count_arg(count) {
        Ok(count) => queue::wake(Key::current(uaddr), bitset, count) as u64,
        Err(code) => code,
    }
}

/// `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE`: wake `val` waiters on `uaddr` and
/// move up to `val2` more to `uaddr2`. The compare form first checks that
/// `*uaddr` still holds `val3` (`EAGAIN` otherwise). Returns the woken count
/// plus, for the compare form, the moved count (as Linux does).
fn requeue(args: &Args, compare: Option<u32>) -> u64 {
    let (wake, moved) = match (count_arg(args.val), count_arg(args.timeout)) {
        (Ok(wake), Ok(moved)) => (wake, moved),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    if !args.uaddr2.is_multiple_of(4) {
        return err(EINVAL);
    }
    if let Some(expected) = compare {
        match read_word(args.uaddr) {
            Ok(value) if value == expected => {}
            Ok(_) => return err(EAGAIN),
            Err(code) => return code,
        }
    }
    let (woken, requeued) = queue::requeue(
        Key::current(args.uaddr),
        Key::current(args.uaddr2),
        wake,
        moved,
    );
    if compare.is_some() {
        (woken + requeued) as u64
    } else {
        woken as u64
    }
}

/// `FUTEX_WAKE_OP`: apply the operation encoded in `val3` to `*uaddr2`, wake
/// `val` waiters on `uaddr`, and if the comparison of the *old* `*uaddr2`
/// holds, also wake `val2` waiters on `uaddr2`. The read-modify-write is
/// atomic for the same reason a wait's check-then-park is.
fn wake_op(args: &Args) -> u64 {
    let (wake1, wake2) = match (count_arg(args.val), count_arg(args.timeout)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    if !args.uaddr2.is_multiple_of(4) {
        return err(EINVAL);
    }
    let Some(encoded) = WakeOp::decode(args.val3 as u32) else {
        return err(ENOSYS);
    };
    let old = match read_word(args.uaddr2) {
        Ok(old) => old,
        Err(code) => return code,
    };
    if user_ptr::try_write::<u32>(args.uaddr2, encoded.apply(old)).is_err() {
        return err(EFAULT);
    }
    let mut woken = queue::wake(Key::current(args.uaddr), u32::MAX, wake1);
    if encoded.compare(old) {
        woken += queue::wake(Key::current(args.uaddr2), u32::MAX, wake2);
    }
    woken as u64
}

/// A decoded `FUTEX_WAKE_OP` word: `op:4 cmp:4 oparg:12 cmparg:12`, where the
/// top bit of `op` (`FUTEX_OP_OPARG_SHIFT`) means "use `1 << oparg`".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WakeOp {
    op: u32,
    cmp: u32,
    oparg: u32,
    cmparg: u32,
}

impl WakeOp {
    /// Decode `val3`; `None` for an operation or comparison Linux rejects.
    pub(super) fn decode(word: u32) -> Option<WakeOp> {
        let raw_op = word >> 28;
        let cmp = (word >> 24) & 0xf;
        let mut oparg = (word >> 12) & 0xfff;
        let cmparg = word & 0xfff;
        let op = raw_op & 0x7;
        if raw_op & 0x8 != 0 {
            if oparg > 31 {
                return None;
            }
            oparg = 1 << oparg;
        }
        (op <= 4 && cmp <= 5).then_some(WakeOp {
            op,
            cmp,
            oparg,
            cmparg,
        })
    }

    /// The new word: `SET`, `ADD`, `OR`, `ANDN`, `XOR`.
    pub(super) fn apply(self, old: u32) -> u32 {
        match self.op {
            0 => self.oparg,
            1 => old.wrapping_add(self.oparg),
            2 => old | self.oparg,
            3 => old & !self.oparg,
            _ => old ^ self.oparg,
        }
    }

    /// The comparison of the old word with `cmparg`: `EQ`, `NE`, `LT`, `LE`,
    /// `GT`, `GE` (signed, as the kernel compares an `int`).
    pub(super) fn compare(self, old: u32) -> bool {
        let (left, right) = (old as i32, self.cmparg as i32);
        match self.cmp {
            0 => left == right,
            1 => left != right,
            2 => left < right,
            3 => left <= right,
            4 => left > right,
            _ => left >= right,
        }
    }
}

/// Wake up to `count` waiters on the futex at `uaddr` in the caller's address
/// space. [`super::procctl`]'s thread-exit path uses it to wake a joiner after
/// clearing `clear_child_tid`.
pub(super) fn futex_wake(uaddr: u64, count: u64) -> u64 {
    queue::wake(Key::current(uaddr), u32::MAX, count.min(MAX_COUNT) as usize) as u64
}

/// Test hooks over the waiter table (`linux_compat_suite`).
#[cfg(lazyos_tests)]
pub mod test_hooks {
    use super::*;

    /// Park `slot` on `uaddr` in the current address space.
    pub fn park(slot: usize, uaddr: u64, bitset: u32) {
        queue::park_for_test(slot, Key::current(uaddr), bitset);
    }

    /// Waiters on `uaddr` in the current address space.
    pub fn waiting(uaddr: u64) -> usize {
        queue::waiting_on(Key::current(uaddr))
    }

    /// Waiters on `uaddr` in the address space `space`.
    pub fn waiting_in(space: u64, uaddr: u64) -> usize {
        queue::waiting_on(Key { space, addr: uaddr })
    }

    /// Park `slot` on `uaddr` in the address space `space`.
    pub fn park_in(slot: usize, space: u64, uaddr: u64) {
        queue::park_for_test(slot, Key { space, addr: uaddr }, u32::MAX);
    }

    pub fn forget(slot: usize) {
        queue::forget_for_test(slot);
    }

    pub fn total() -> usize {
        queue::total_for_test()
    }

    /// `(apply(old), compare(old))` for an encoded `FUTEX_WAKE_OP` word.
    pub fn wake_op(word: u32, old: u32) -> Option<(u32, bool)> {
        WakeOp::decode(word).map(|op| (op.apply(old), op.compare(old)))
    }
}
