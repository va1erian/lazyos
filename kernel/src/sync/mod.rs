//! The kernel's SMP-era locks (docs/smp-plan.md §4.1, S8.3).
//!
//! [`entry`] is the kernel entry lock: the one global lock a CPU holds while
//! it executes kernel code, so that "at most one CPU is in the kernel" —
//! the invariant every "interrupts off, so nothing else mutates this"
//! argument in the kernel rests on — stays true once several CPUs run user
//! tasks. `sync::Lock<T>` with lockdep follows with its first real user, the
//! scheduler lock of S8.4 (docs/architecture/smp.md, "The entry lock").
//! [`ticket`] is the interrupts-off spin lock both the entry lock and the
//! shootdown sender lock are built on.

pub mod entry;
pub mod ticket;
