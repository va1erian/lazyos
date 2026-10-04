//! The native monotonic-time syscall (34, docs/performance-plan.md P2.4):
//! a nanosecond clock and a nanosecond sleep (see
//! `kernel/src/process/timesys.rs`).

use core::arch::asm;

/// `mono_time(op, a1)` — the monotonic clock and sleep.
pub const SYS_MONO_TIME: u64 = 34;

const OP_NOW: u64 = 0;
const OP_SLEEP_UNTIL: u64 = 1;
/// `sleep_until`'s result when a signal ended the sleep.
const INTERRUPTED: u64 = 1;

fn mono_time(op: u64, arg: u64) -> u64 {
    let result: u64;
    // Safety: `int 0x80` with syscall 34; no pointers cross the gate.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_MONO_TIME,
            in("rdi") op,
            in("rsi") arg,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    result
}

/// Nanoseconds since boot on the monotonic clock (the Linux
/// `CLOCK_MONOTONIC`), with sub-tick resolution.
pub fn monotonic_ns() -> u64 {
    mono_time(OP_NOW, 0)
}

/// Milliseconds since boot on the monotonic clock.
pub fn monotonic_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

/// Sleep until [`monotonic_ns`] reaches `deadline`. Returns `false` when a
/// signal ended the sleep early.
pub fn sleep_until_ns(deadline: u64) -> bool {
    mono_time(OP_SLEEP_UNTIL, deadline) != INTERRUPTED
}

/// Sleep for `ns` nanoseconds. Returns `false` when a signal ended the
/// sleep early.
pub fn sleep_ns(ns: u64) -> bool {
    sleep_until_ns(monotonic_ns().saturating_add(ns))
}
