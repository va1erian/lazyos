//! The APs schedule (docs/smp-plan.md S8.3c): their start, and the kick
//! that hands a CPU a task to run.
//!
//! An AP comes up idle in `ap_entry` (S8.2), serving the window-safe IPIs.
//! Once the boot CPU starts the scheduler it calls [`start_aps`]; each AP
//! then takes the entry lock, enters its idle context (`task::idle`) with a
//! periodic tick of its own, and from there runs whatever the one global run
//! queue gives it, exactly as the boot CPU does.
//!
//! **The kick** (`KICK_VECTOR`). A wake on one CPU may concern a task that
//! should run on another: an idle CPU, or one running something of a lower
//! class (`task::kick`). The waker sets that CPU's `need_resched` and sends
//! the kick; its gate is an ordinary locked one (the entry lock, taken from
//! ring 3 or the idle halt) whose handler is a preemption point. Unlike
//! `RESCHED_VECTOR` (window-safe: it only sets the flag, and the suite's rig
//! runs its jobs there), the kick switches tasks.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::structures::idt::InterruptStackFrameValue;

use crate::arch::gsgate::irq_gate;
use crate::arch::{lapic, percpu};

irq_gate!(kick_gate => kick_handler);

/// Set once the boot CPU started the scheduler: an AP leaving `ap_entry`'s
/// idle loop enters its idle context.
static STARTED: AtomicBool = AtomicBool::new(false);

/// The kick: a wake decided this CPU should switch (`need_resched` is set).
extern "C" fn kick_handler(_frame: &InterruptStackFrameValue) {
    lapic::eoi();
    crate::task::preempt_point();
}

/// Let the online APs schedule (the boot CPU calls it once tasks run, after
/// `task::start`). Only a `LAZYOS_SMP=1` kernel has the entry lock that
/// makes that sound, and an AP needs an APIC timer for its tick; without
/// either the APs stay idle in `ap_entry`, and the boot log says why.
pub fn start_aps() {
    if super::online_cpus() <= 1 {
        return;
    }
    if !cfg!(lazyos_smp) {
        crate::serial_println!("HW:SMP:SCHED=off (a LAZYOS_SMP=1 kernel schedules the APs)");
        return;
    }
    if crate::arch::timer::ap_tick_count().is_none() {
        crate::serial_println!("HW:SMP:SCHED=off (no APIC timer for the APs' tick)");
        return;
    }
    STARTED.store(true, Ordering::Release);
    wake_aps();
    crate::serial_println!("HW:SMP:SCHED=on cpus={}", super::online_cpus());
}

/// Wake every online AP from its halt (`RESCHED_VECTOR` only sets a flag;
/// `ap_entry`'s loop re-reads [`started`]).
fn wake_aps() {
    let me = percpu::current().index();
    for index in 0..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            continue;
        };
        if index as u32 != me && cpu.online.load(Ordering::Acquire) {
            lapic::send_fixed(cpu.apic_id(), super::RESCHED_VECTOR);
        }
    }
}

/// Whether the APs were let in ([`start_aps`]).
pub fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// Kick CPU `index` (an online, scheduling CPU): its `need_resched` is set
/// by the caller.
pub fn kick(index: usize) {
    if let Some(cpu) = percpu::area(index) {
        lapic::send_fixed(cpu.apic_id(), super::KICK_VECTOR);
    }
}

/// Test hook: let the online APs schedule in a test kernel (whose scheduler
/// never starts); the suite pauses them again (`task::idle::pause_aps`).
#[cfg(lazyos_tests)]
pub fn start_aps_for_test() -> bool {
    if !cfg!(lazyos_smp) || crate::arch::timer::ap_tick_count().is_none() {
        return false;
    }
    STARTED.store(true, Ordering::Release);
    wake_aps();
    true
}
