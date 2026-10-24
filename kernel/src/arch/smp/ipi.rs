//! The reschedule and stop IPIs (docs/smp-plan.md S8.2: vectors 0x60 and
//! 0x62; the shootdown, 0x61, is `smp::shootdown`) and the panic path's
//! stop-the-world. The handlers (behind `arch::gsgate`'s IRQ gates) obey the
//! `irq_window` contract exactly: no lock but this CPU's own area, no
//! allocation, no task switch — the SMP generalization of the window-safe
//! handlers.

use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::structures::idt::InterruptStackFrameValue;

use super::{rdtsc, spin_loop};

use crate::arch::gsgate::irq_gate;
use crate::arch::{lapic, percpu};

irq_gate!(resched_gate => resched_handler, window_safe);
irq_gate!(stop_gate => stop_handler, window_safe);

/// "A wake made a task more deserving of the CPU than the current one": the
/// next preemption point acts on the flag (S8.3 is when an AP has tasks).
extern "C" fn resched_handler(_frame: &InterruptStackFrameValue) {
    percpu::current()
        .preempt
        .need_resched
        .store(true, Ordering::Relaxed);
    #[cfg(lazyos_tests)]
    super::testrig::run_test_job();
    lapic::eoi();
}

/// Stop the world: someone is panicking (or the PAT changes). This CPU
/// halts with interrupts off and never returns — the panicking CPU owns the
/// machine from here.
extern "C" fn stop_handler(_frame: &InterruptStackFrameValue) {
    lapic::eoi();
    halt_stopped();
}

/// Report this CPU stopped and halt for good, interrupts off.
fn halt_stopped() -> ! {
    use core::fmt::Write;
    // `RawSerial`: no lock (the window-safe contract; the panicking CPU may
    // hold the serial lock).
    let _ = writeln!(
        crate::arch::raw_serial::RawSerial,
        "HW:SMP:STOP cpu={}",
        percpu::current().index()
    );
    percpu::current().ipi.stopped.store(true, Ordering::Release);
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// The CPU that panicked first (its area index + 1; 0: none). It owns the
/// machine from then on.
static PANIC_CPU: AtomicU32 = AtomicU32::new(0);

/// Halt like a `STOP` would, if another CPU is panicking: the stop IPI
/// cannot land on a CPU spinning with interrupts off, so such spins ask
/// (`shootdown::serve_pending`).
pub(super) fn halt_if_stopping() {
    let owner = PANIC_CPU.load(Ordering::Acquire);
    if owner != 0 && owner != percpu::current().index() + 1 {
        halt_stopped();
    }
}

/// Whether any CPU but the boot CPU's area is online — `stop_others`'s gate
/// (the suite's `stop_all_for_test` calls this same function). Read from the
/// areas themselves, never `%gs` (the panic path may run before it is set
/// up) nor the online count (a derived number: the bring-up keeps it ahead
/// of the areas, but the gate must not depend on that).
pub(super) fn any_ap_online() -> bool {
    (1..percpu::MAX_CPUS)
        .filter_map(percpu::area)
        .any(|cpu| cpu.online.load(Ordering::Acquire))
}

/// Stop the world for a panic (docs/smp-plan.md S8.2): every other online
/// CPU is sent `STOP_VECTOR` and given a bounded time to halt, so nothing
/// else runs while the panic report is written (the serial lock's
/// force-unlock and the on-screen report both assume one CPU). Takes no
/// lock and allocates nothing; does nothing (and reads no per-CPU state)
/// while no AP is online. A second CPU that panics while the first is
/// stopping the world just halts.
pub fn stop_others() {
    if !any_ap_online() {
        return;
    }
    // An AP is up, so every CPU's `%gs` names its area.
    let me = percpu::current().index();
    if PANIC_CPU
        .compare_exchange(0, me + 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        loop {
            x86_64::instructions::interrupts::disable();
            x86_64::instructions::hlt();
        }
    }
    stop_online_others(me);
}

/// Send `STOP` to every online CPU but `me` and wait (200 ms in all) for
/// each to report it halted; how many did. A CPU that cannot be stopped
/// (wedged with interrupts off) is left: the report matters more.
pub(super) fn stop_online_others(me: u32) -> usize {
    let others = || {
        (0..percpu::MAX_CPUS)
            .filter(move |&index| index as u32 != me)
            .filter_map(percpu::area)
            .filter(|cpu| cpu.online.load(Ordering::Acquire))
    };
    for cpu in others() {
        lapic::send_fixed(cpu.apic_id(), super::STOP_VECTOR);
    }
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 20;
    let mut stopped = 0;
    for cpu in others() {
        while !cpu.ipi.stopped.load(Ordering::Acquire) && rdtsc().wrapping_sub(started) < bound {
            spin_loop();
        }
        stopped += usize::from(cpu.ipi.stopped.load(Ordering::Acquire));
    }
    stopped
}
