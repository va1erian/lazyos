//! Syscalls that monopolise the CPU.
//!
//! The kernel does not preempt a task inside a syscall: interrupts are taken
//! in `irq_window`s, but no other task runs until the call returns. One long
//! call therefore freezes every service and the desktop for its whole length,
//! and `arch::irqoff` cannot see it when the windows keep each interrupts-off
//! span short. This probe times each Linux syscall and logs any that ran 50 ms
//! or more while no other task got the CPU (a call that parked, a `futex` wait
//! or a `poll`, lets others run and is not reported):
//!
//! `SYS:SLOW abi=<linux|native> nr=<n> <name> ms=<n> task=<slot> a1=<hex> a2=<hex> a3=<hex>`
//!
//! The first lines are enough to name the call, and its arguments usually its
//! size; reports stop after [`MAX_REPORTS`] so a pathological loop cannot
//! flood the log.

use core::sync::atomic::{AtomicU32, Ordering};

/// A syscall this long, in nanoseconds, with nobody else scheduled, is slow.
const SLOW_NS: u64 = 50_000_000;
/// Reports per boot.
const MAX_REPORTS: u32 = 64;

static REPORTED: AtomicU32 = AtomicU32::new(0);

/// The clock and switch count at a syscall's start.
pub(crate) struct Probe {
    started_ns: u64,
    switches: u64,
}

pub(crate) fn begin() -> Probe {
    Probe {
        started_ns: crate::arch::clock::monotonic_ns(),
        switches: crate::task::context_switches(),
    }
}

/// Close the probe: log the call when it was slow and monopolised the CPU.
/// `abi` is `"linux"` (`syscall`) or `"native"` (`int 0x80`).
pub(crate) fn end(probe: Probe, abi: &str, nr: u64, args: [u64; 3]) {
    let elapsed = crate::arch::clock::monotonic_ns().saturating_sub(probe.started_ns);
    if elapsed < SLOW_NS || crate::task::context_switches() != probe.switches {
        return;
    }
    if REPORTED.fetch_add(1, Ordering::Relaxed) >= MAX_REPORTS {
        return;
    }
    let name = if abi == "linux" {
        super::names::syscall_name(nr)
    } else {
        "native"
    };
    crate::serial_println!(
        "SYS:SLOW abi={abi} nr={nr} {name} ms={} task={} a1={:#x} a2={:#x} a3={:#x}",
        elapsed / 1_000_000,
        crate::task::current(),
        args[0],
        args[1],
        args[2]
    );
}
