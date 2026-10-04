//! Nanosecond deadlines and the timer queue (docs/performance-plan.md, P2).
//!
//! `queue` checks the indexed min-heap on its own (ordering, cancellation,
//! equal deadlines, past and far deadlines, a seeded soak against a model);
//! `live` checks the wiring: tick deadlines keep their 10 ms meaning, ns
//! deadlines expire exactly, real sleeps from 100 µs to 1 s land on time
//! through the deadline timer, and a soak of real kernel threads arming and
//! cancelling timers leaves nothing behind.

use super::*;

mod live;
mod queue;
mod soak;

pub(super) const CASES: &[(&str, Test)] = &[
    ("deadline_queue_order", queue::order),
    ("deadline_queue_cancel", queue::cancel),
    ("deadline_queue_same_deadline", queue::same_deadline),
    ("deadline_queue_past_and_wrap", queue::past_and_wrap),
    ("deadline_queue_soak", queue::soak),
    ("deadline_tick_conversion", live::tick_conversion),
    ("deadline_ns_expiry_exact", live::ns_expiry_exact),
    ("deadline_wake_cancels_timer", live::wake_cancels_timer),
    ("deadline_past_returns_at_once", live::past_returns_at_once),
    ("deadline_native_syscall", live::native_syscall),
    ("deadline_realtime_conversion", live::realtime_conversion),
    ("deadline_event_timer_boot", live::event_timer_boot),
    ("deadline_tick_abi_10ms", live::tick_abi_10ms),
    ("deadline_sleep_accuracy", live::sleep_accuracy),
    ("deadline_thread_soak", soak::thread_soak),
];

/// A lone runnable kernel task.
fn kernel_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `body` with interrupts on and only the tick's PIC line unmasked,
/// restoring every mask and `IF=0` afterwards.
fn with_tick<T>(body: impl FnOnce() -> T) -> T {
    use crate::arch::pic;
    let saved: [bool; 16] = core::array::from_fn(|l| pic::is_masked(l as u8));
    for line in 0..16u8 {
        pic::set_masked(line, line != 0);
    }
    x86_64::instructions::interrupts::enable();
    let result = body();
    x86_64::instructions::interrupts::disable();
    for (line, masked) in saved.into_iter().enumerate() {
        pic::set_masked(line as u8, masked);
    }
    result
}

/// A small deterministic generator for the soaks (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// The hypervisor's CPUID signature (leaf 0x4000_0000), if one is reported.
fn hypervisor() -> Option<[u8; 12]> {
    use core::arch::x86_64::__cpuid;
    if __cpuid(1).ecx & (1 << 31) == 0 {
        return None;
    }
    let leaf = __cpuid(0x4000_0000);
    let mut sig = [0u8; 12];
    sig[..4].copy_from_slice(&leaf.ebx.to_le_bytes());
    sig[4..8].copy_from_slice(&leaf.ecx.to_le_bytes());
    sig[8..].copy_from_slice(&leaf.edx.to_le_bytes());
    Some(sig)
}

/// Whether QEMU runs this guest with hardware acceleration (WHPX reports
/// Hyper-V's signature, KVM its own); TCG reports none or `TCGTCGTCGTCG`.
/// Timing bounds are only enforced where virtual time is real time.
fn accelerated() -> bool {
    matches!(
        hypervisor().as_ref().map(|s| &s[..]),
        Some(b"Microsoft Hv") | Some(b"KVMKVMKVM\0\0\0")
    )
}
