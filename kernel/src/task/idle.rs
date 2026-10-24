//! Per-CPU idle contexts (docs/smp-plan.md S8.3b).
//!
//! When nothing is runnable the scheduler resumes its CPU's idle context,
//! which halts with the entry lock dropped (`sync::entry::halt`) until an
//! interrupt makes something runnable. Before S8.3b the scheduler resumed
//! the task that had just parked (or finished) and let it halt in its own
//! wait loop. With several CPUs that cannot work: the CPU would still be
//! "in" that task, on its kernel stack and its page table, so no other CPU
//! could run it once woken, its slot could not be reclaimed once dead, and
//! its address space could not be freed while the idle CPU kept it in
//! `CR3`.
//!
//! An idle context is a slot number, not a task. Slots
//! [`FIRST_IDLE_SLOT`]`..MAX_TASKS` are reserved, one per CPU, and the task
//! table never holds anything there: `claim_slot` stops below them
//! (`limit.task_max` is capped at [`TASK_SLOTS`]). So `current()` is always
//! a valid index for the per-slot arrays (the FPU areas, `NAPPING`,
//! `YIELDING`, the kernel stack tops), and every path that reads the
//! current task out of the table finds an empty slot, which they already
//! treat as idle: a tick is charged as idle time, a wake preempts, no
//! signal sweep or interval timer sees it. Nothing that walks the table
//! lists it.
//!
//! The context itself is a kernel stack (never freed), the frame saved by
//! the last switch out of it, and the boot page table, which is what it runs
//! on (the dying-table rule, docs/architecture/smp.md): a CPU with nothing
//! to do holds no user address space in `CR3`. It has no floating-point
//! state (the kernel is soft-float): the scheduler neither saves nor
//! restores one for it, and does not count its runs as context switches or
//! in the `PERF:` run counts.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use x86_64::instructions::segmentation::{Segment, CS, SS};

use super::{kstack::KernelStack, MAX_TASKS};
use crate::arch::percpu::{self, MAX_CPUS};

/// Slots user tasks may occupy: the table minus one idle slot per CPU.
pub const TASK_SLOTS: usize = MAX_TASKS - MAX_CPUS;
/// The idle slot of CPU 0; CPU `i`'s is `FIRST_IDLE_SLOT + i`.
pub const FIRST_IDLE_SLOT: usize = TASK_SLOTS;

/// Words of the initial frame: 15 general registers, then RIP, CS, RFLAGS,
/// RSP and SS, the layout `task::switch` restores.
const FRAME_WORDS: u64 = 20;

/// Per CPU: the saved frame of its idle context.
static RSP: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Per CPU: whether [`ensure_here`] created its idle context, i.e. whether
/// the CPU schedules.
static CREATED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// How many CPUs schedule (have an idle context), and the highest index of
/// one plus one (what a scan over them stops at).
static SCHEDULING: AtomicUsize = AtomicUsize::new(0);
static SCHEDULING_END: AtomicUsize = AtomicUsize::new(0);
/// Per CPU: whether its own periodic tick runs (an AP's, S8.3c).
static AP_TICK: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// The suite's pause (test kernels only set it): the APs run no task and
/// stop their tick, so the cases after the scheduling ones find them idle.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Whether `slot` is an idle context.
pub fn is_idle(slot: usize) -> bool {
    (FIRST_IDLE_SLOT..MAX_TASKS).contains(&slot)
}

/// This CPU's idle slot, once [`ensure_here`] created it.
pub fn here() -> Option<usize> {
    let cpu = percpu::current().index() as usize;
    CREATED[cpu]
        .load(Ordering::Relaxed)
        .then_some(FIRST_IDLE_SLOT + cpu)
}

/// Whether CPU `index` schedules (it has an idle context).
pub fn scheduling(index: usize) -> bool {
    CREATED
        .get(index)
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// How many CPUs schedule (at least 1 once the boot CPU registered its
/// kernel task).
pub fn scheduling_cpus() -> usize {
    SCHEDULING.load(Ordering::Relaxed).max(1)
}

/// How many CPUs may run tasks now: the scheduling ones, or the boot CPU
/// alone while the suite holds the APs back. The guard's budgets and the
/// idle count scale by it.
pub fn running_cpus() -> usize {
    if paused() {
        1
    } else {
        scheduling_cpus()
    }
}

/// Whether the suite holds the APs back ([`pause_aps`]).
pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// One past the highest index of a scheduling CPU: scans stop there.
pub fn scheduling_end() -> usize {
    SCHEDULING_END.load(Ordering::Relaxed)
}

/// Whether this CPU must run no task now: an AP while the suite paused them.
pub fn held_back() -> bool {
    paused() && percpu::current().index() != 0
}

/// An AP's scheduler entry: run its tick while it may run tasks, stop it
/// while it is held back (`set_ap_tick` reprograms only on a change).
pub(super) fn ap_tick_policy() {
    let cpu = percpu::current().index() as usize;
    if cpu == 0 || !scheduling(cpu) {
        return;
    }
    let want = !held_back();
    if AP_TICK[cpu].load(Ordering::Relaxed) != want {
        AP_TICK[cpu].store(crate::arch::timer::set_ap_tick(want), Ordering::Relaxed);
    }
}

/// The CPU an idle slot belongs to.
fn cpu_of(slot: usize) -> usize {
    slot - FIRST_IDLE_SLOT
}

/// The frame to resume idle `slot` with.
pub(super) fn rsp(slot: usize) -> u64 {
    RSP[cpu_of(slot)].load(Ordering::Relaxed)
}

/// Record the frame idle `slot` was switched out at.
pub(super) fn save_rsp(slot: usize, rsp: u64) {
    RSP[cpu_of(slot)].store(rsp, Ordering::Relaxed);
}

/// Create this CPU's idle context if it has none: a kernel stack with a
/// frame that enters [`idle_main`] in ring 0 with interrupts off. Called
/// with the task table set up (`register_kernel`); idempotent. Panics if
/// the heap cannot supply the stack: a CPU without an idle context cannot
/// schedule.
pub(super) fn ensure_here() {
    let cpu = percpu::current().index() as usize;
    if CREATED[cpu].load(Ordering::Relaxed) {
        return;
    }
    let slot = FIRST_IDLE_SLOT + cpu;
    let Some(stack) = KernelStack::new(slot) else {
        panic!("idle: no kernel stack for CPU{cpu}");
    };
    let top = stack.top();
    let base = top - FRAME_WORDS * 8;
    // SAFETY: `base..top` lies inside the stack just allocated, which
    // nothing else uses.
    unsafe {
        let frame = base as *mut u64;
        for word in 0..15 {
            core::ptr::write_volatile(frame.add(word), 0);
        }
        core::ptr::write_volatile(frame.add(15), idle_main as *const () as u64);
        core::ptr::write_volatile(frame.add(16), u64::from(CS::get_reg().0));
        // IF clear: the loop runs like kernel code and halts through `nap`.
        core::ptr::write_volatile(frame.add(17), 0x2);
        // As after a `call`: rsp + 8 is 16-aligned at the function's entry.
        core::ptr::write_volatile(frame.add(18), top - 8);
        core::ptr::write_volatile(frame.add(19), u64::from(SS::get_reg().0));
    }
    // The idle context lives as long as its CPU: the stack is never freed.
    core::mem::forget(stack);
    RSP[cpu].store(base, Ordering::Relaxed);
    CREATED[cpu].store(true, Ordering::Relaxed);
    SCHEDULING.fetch_add(1, Ordering::Relaxed);
    SCHEDULING_END.fetch_max(cpu + 1, Ordering::Relaxed);
}

/// An AP's way into scheduling (S8.3c), from `ap_entry` once the scheduler
/// started (`arch::smp::start_aps`), with interrupts off: take the entry
/// lock (kernel code from here on), create the idle context, make it this
/// CPU's current task, start the CPU's tick and resume the context's first
/// frame. From then on the AP is a scheduling CPU like the boot CPU.
pub fn start_ap() -> ! {
    crate::sync::entry::boot_hold();
    ensure_here();
    let cpu = percpu::current().index() as usize;
    super::set_current(FIRST_IDLE_SLOT + cpu);
    super::oncpu::several_cpus();
    AP_TICK[cpu].store(
        crate::arch::timer::set_ap_tick(!held_back()),
        Ordering::Relaxed,
    );
    crate::serial_println!("HW:SMP:CPU{cpu} schedules");
    // SAFETY: the frame `ensure_here` built at the top of the idle stack,
    // never resumed before; it enters `idle_main` in ring 0 with interrupts
    // off, and this CPU holds the entry lock it runs under.
    unsafe { super::switch::task_resume_frame(RSP[cpu].load(Ordering::Relaxed)) }
}

/// Test hook: hold the APs back (no task, no tick) or let them go again.
/// Each scheduling AP is kicked so its next scheduler entry applies it.
#[cfg(lazyos_tests)]
pub fn pause_aps(paused: bool) {
    PAUSED.store(paused, Ordering::Relaxed);
    for cpu in 1..MAX_CPUS {
        if scheduling(cpu) {
            if let Some(area) = percpu::area(cpu) {
                area.preempt.need_resched.store(true, Ordering::Relaxed);
            }
            crate::arch::smp::kick(cpu);
        }
    }
}

/// The idle loop: halt (entry lock dropped) until an interrupt, then give
/// the CPU away if that interrupt made something runnable. A wake raises
/// `need_resched` (the current task is idle, so any runnable task beats it,
/// `preempt::note_wake`). An interrupt's own preemption point does nothing
/// here (`preempt::preempt_point` says why): the switch happens in this
/// loop, after the interrupt returned. A tick still switches directly (its
/// scheduler entry selects).
extern "C" fn idle_main() -> ! {
    loop {
        #[cfg(lazyos_tests)]
        probe::note();
        rest();
        // The suite's stand-in for a peer acting while its task waits.
        #[cfg(lazyos_tests)]
        super::harness::run_nap_hook();
        super::kick::flush_unlocked();
        super::preempt::switch_if_wanted();
    }
}

/// `task::nap` for the idle context: the halt lasts across interrupts that
/// do not take the entry lock (`sync::entry::halt_while`) while nothing is
/// asked of this CPU, so an idle CPU woken by a shootdown or a rig IPI does
/// not queue for the lock behind a long holder. A gate that takes the lock
/// (a tick, a kick, a device) or a raised `need_resched` ends it.
fn rest() {
    crate::arch::irqoff::close();
    crate::perf::irqoff_pause();
    super::preempt::nap_begin();
    crate::sync::entry::halt_while(|| {
        held_back()
            || !percpu::current()
                .preempt
                .need_resched
                .load(Ordering::Relaxed)
    });
    super::preempt::nap_end();
    crate::perf::irqoff_resume();
    crate::arch::irqoff::resume();
}

/// What the suite reads about the idle loop (`tests/smp_idle_suite.rs`):
/// how often it ran, and the page table and kernel-task mark it saw.
#[cfg(lazyos_tests)]
pub mod probe {
    use core::sync::atomic::{AtomicU64, Ordering};

    static LOOPS: AtomicU64 = AtomicU64::new(0);
    static CR3: AtomicU64 = AtomicU64::new(0);
    /// `oncpu::cpu_of(KERNEL_TASK)` as the loop saw it (`u64::MAX`: none).
    static KERNEL_MARK: AtomicU64 = AtomicU64::new(0);

    pub(super) fn note() {
        LOOPS.fetch_add(1, Ordering::Relaxed);
        let (frame, _) = x86_64::registers::control::Cr3::read();
        CR3.store(frame.start_address().as_u64(), Ordering::Relaxed);
        let mark = super::super::oncpu::cpu_of(super::super::KERNEL_TASK);
        KERNEL_MARK.store(mark.map_or(u64::MAX, |cpu| cpu as u64), Ordering::Relaxed);
    }

    /// Passes through the idle loop since boot.
    pub fn loops() -> u64 {
        LOOPS.load(Ordering::Relaxed)
    }

    /// The `CR3` of the latest pass.
    pub fn cr3() -> u64 {
        CR3.load(Ordering::Relaxed)
    }

    /// The kernel task's on-CPU mark at the latest pass.
    pub fn kernel_mark() -> Option<u64> {
        match KERNEL_MARK.load(Ordering::Relaxed) {
            u64::MAX => None,
            cpu => Some(cpu),
        }
    }
}
