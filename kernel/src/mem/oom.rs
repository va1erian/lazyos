//! The out-of-memory kill.
//!
//! Pages are demand-zero: `mmap` and `brk` promise address space and the
//! frame is found at the first touch. When the touch finds the allocator
//! empty the program has done nothing wrong, so `SIGSEGV` would be a lie (no
//! handler written for a bad pointer can recover, and a JavaScript engine's
//! "allocation failed" path never runs). Linux answers a shortage it cannot
//! refuse earlier by killing a process with `SIGKILL` and logging why; this is
//! the same, for the process that asked (the page fault handler calls
//! [`kill_current`] when `mem::demand_resolve` or `cow_resolve` says
//! `OutOfFrames`). `wait4` then reports the signal, the frames come back when
//! the address space is torn down, and nothing else is hurt.
//!
//! Refusing earlier is cheaper where the request is plainly hopeless; see
//! `process::linux::overcommit`.

use crate::task::{self, signal};

/// The status a killed process leaves: `128 + SIGKILL`, which `wait4` decodes
/// as "killed by signal 9".
pub const EXIT_STATUS: u64 = 128 + signal::SIGKILL as u64;

/// Print the kill report for the process of `slot` and end it (every thread
/// sharing its address space). Returns the number of tasks ended; the kernel
/// task is never killed.
///
/// The report line is `oom: killed <comm> (<uid>): <n> MiB mapped, <n> frames`:
/// what the process had reserved and how many frames were free at the
/// moment it asked. Nothing here allocates a frame.
pub fn kill(slot: usize) -> usize {
    if slot == task::KERNEL_TASK {
        return 0;
    }
    let Some(pml4) = task::pml4_of(slot) else {
        return 0;
    };
    let (comm, len) = task::linuxstate::comm(slot);
    crate::serial_println!(
        "oom: killed {} ({}): {} MiB mapped, {} frames",
        core::str::from_utf8(&comm[..len]).unwrap_or("?"),
        crate::ipc::credentials::of(slot).uid,
        super::vma::mapped_bytes(x86_64::PhysAddr::new(pml4)) >> 20,
        super::frame_stats().free
    );
    signal::terminate_process(pml4, EXIT_STATUS)
}

/// End the process that owns the current task as an OOM victim and hand the
/// CPU on; the faulting instruction never runs again.
pub fn kill_current() -> ! {
    kill(task::current());
    // Hand the CPU on now rather than at the next tick (P1.5).
    task::exit_cpu()
}
