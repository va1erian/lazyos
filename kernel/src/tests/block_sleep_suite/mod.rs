//! Block I/O that sleeps (docs/performance-plan.md P5): the virtio-blk request
//! planner, real kernel threads parked in virtio-blk while the device works
//! (the device's own buffers, mixed with a spinning caller, a waiter that is
//! being killed), and a cached ext2 volume driven by several threads at once
//! through its gate, which is where the kernel lets requests sleep.
//!
//! The thread tests run on the 16 MiB scratch virtio disk `tools/test/run.py`
//! attaches, and report a skip without one.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::arch::clock;
use crate::block::{self, BlockDevice};
use crate::task::wait::WaitQueue;
use crate::task::WaitKind;

mod ata;
mod ext2;
mod plan;
mod spinwait;
mod virtio;

pub(super) const CASES: &[(&str, Test)] = &[
    ("block_sleep_plan_pieces", plan::pieces_follow_the_rules),
    ("block_sleep_plan_errors", plan::unmapped_and_tiny),
    ("block_sleep_soak_plan_random", plan::soak_random_segments),
    ("block_sleep_virtio_threads", virtio::threads_and_a_spinner),
    (
        "block_sleep_virtio_killed_waiter",
        virtio::killed_waiter_finishes,
    ),
    ("block_sleep_ata_threads", ata::threads_and_a_spinner),
    (
        "block_sleep_spin_ends_by_deadline",
        spinwait::spin_ends_by_deadline_with_windows,
    ),
    ("block_sleep_soak_spins_bounded", spinwait::soak_spins_stay_bounded),
    (
        "block_sleep_soak_ext2_threads",
        ext2::soak_threads_on_one_volume,
    ),
];

/// Sectors of the scratch disk the runner attaches (16 MiB).
const SCRATCH_SECTORS: u64 = 32 * 1024;

/// The scratch virtio disk, if the runner attached one.
fn scratch(test: &str) -> Option<&'static dyn BlockDevice> {
    let boot = block::boot_device().map(|device| device.name());
    let found = block::devices().into_iter().find(|device| {
        device.name().starts_with("virtio")
            && Some(device.name()) != boot
            && device.sector_count() == SCRATCH_SECTORS
    });
    if found.is_none() {
        serial_println!("TEST:{test}:INFO:no scratch virtio disk; skipped");
    }
    found
}

/// The kernel task, current and alone.
fn kernel_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `body` with interrupts on and only the tick's PIC line unmasked, and
/// with kernel threads allowed to sleep in I/O; restore everything after.
fn with_sleeping_threads<T>(body: impl FnOnce() -> T) -> T {
    use crate::arch::irqchip;
    let saved: [bool; 16] = core::array::from_fn(|l| irqchip::is_masked(l as u8));
    for line in 0..16u8 {
        irqchip::set_masked(line, line != 0);
    }
    block::iowait::set_test_sleep(true);
    x86_64::instructions::interrupts::enable();
    let result = body();
    x86_64::instructions::interrupts::disable();
    block::iowait::set_test_sleep(false);
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    result
}

/// Where a finished thread parks for good.
static PARKED: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Threads that finished their work, and the first failure among them.
static DONE: AtomicUsize = AtomicUsize::new(0);
static FAILED: spin::Mutex<Option<String>> = spin::Mutex::new(None);
/// Per-thread seeds, handed out in spawn order.
static SEED: AtomicU64 = AtomicU64::new(1);

/// A thread's last act: record `result`, then park forever.
fn finish_thread(result: Result<(), String>) -> ! {
    if let Err(error) = result {
        let mut failed = FAILED.lock();
        if failed.is_none() {
            *failed = Some(error);
        }
    }
    DONE.fetch_add(1, Ordering::Relaxed);
    let me = task::current();
    loop {
        x86_64::instructions::interrupts::disable();
        PARKED.wait_ns(me, None);
    }
}

/// Spawn `threads` kernel threads at `entry`, let them run (the kernel task
/// idling, calling `between` each time it wakes) until all finished or
/// `limit_ns` passed, then end and reap them. Returns the first failure.
fn run_threads(
    threads: usize,
    entry: extern "C" fn() -> !,
    limit_ns: u64,
    mut between: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    kernel_only();
    DONE.store(0, Ordering::Relaxed);
    *FAILED.lock() = None;
    let mut slots = Vec::new();
    for _ in 0..threads {
        let slot =
            task::kthread::spawn_kernel_thread("block-sleep", entry, task::PriorityClass::Normal)
                .map_err(|e| format!("spawn: {e}"))?;
        slots.push(slot);
    }
    let start = clock::monotonic_ns();
    let outcome = with_sleeping_threads(|| {
        while DONE.load(Ordering::Relaxed) < threads {
            if clock::monotonic_ns() - start > limit_ns {
                return Err(format!(
                    "{} of {threads} threads finished in {} s",
                    DONE.load(Ordering::Relaxed),
                    limit_ns / 1_000_000_000
                ));
            }
            x86_64::instructions::interrupts::disable();
            let between = between();
            x86_64::instructions::interrupts::enable();
            between?;
            task::idle_ns(clock::monotonic_ns() + 200_000);
        }
        Ok(())
    });
    for &slot in &slots {
        task::harness::finish(slot, 0);
    }
    PARKED.notify_all();
    while task::reap_child().is_some() {}
    task::harness::reset();
    outcome?;
    match FAILED.lock().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn seeded() -> Rng {
        Rng(SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) | 1)
    }

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

/// The byte at absolute disk offset `at` for pattern `salt`.
fn pattern(at: u64, salt: u64) -> u8 {
    (at.wrapping_add(salt.wrapping_mul(0xD1B5_4A32_D192_ED03))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        >> 56) as u8
}
