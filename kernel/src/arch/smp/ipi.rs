//! The three kernel IPIs (docs/smp-plan.md S8.2: vectors 0x60..0x62) and
//! the sender half of a TLB shootdown. All three handlers (behind `arch::gsgate`'s IRQ gates) obey the
//! `irq_window` contract exactly: no lock but this CPU's own area, no
//! allocation, no task switch — the SMP generalization of the window-safe
//! handlers.

use core::sync::atomic::Ordering;
use x86_64::structures::idt::InterruptStackFrameValue;
use x86_64::VirtAddr;

use super::{rdtsc, spin_loop, SHOOTDOWN_VECTOR};

/// Above this many pages a shootdown target flushes everything instead.
const FLUSH_ALL_ABOVE: u64 = 64;
use crate::arch::gsgate::irq_gate;
use crate::arch::{lapic, percpu};

irq_gate!(resched_gate => resched_handler);
irq_gate!(shootdown_gate => shootdown_handler);
irq_gate!(stop_gate => stop_handler);

/// "A wake made a task more deserving of the CPU than the current one": the
/// next preemption point acts on the flag (S8.3 is when an AP has tasks).
extern "C" fn resched_handler(_frame: &InterruptStackFrameValue) {
    percpu::current()
        .preempt
        .need_resched
        .store(true, Ordering::Relaxed);
    lapic::eoi();
}

/// The shootdown doorbell: the mailbox names the pages; invalidate and ack.
extern "C" fn shootdown_handler(_frame: &InterruptStackFrameValue) {
    let cpu = percpu::current();
    let addr = cpu.ipi.sd_addr.load(Ordering::Acquire);
    let pages = cpu.ipi.sd_pages.load(Ordering::Acquire);
    // The "everything" case (`mem::tlb::shootdown_all`, 0/0), and any range
    // too long to walk page by page in a handler: a full flush covers it
    // (never truncate: an entry left stale here is a use-after-free).
    if (addr == 0 && pages == 0) || pages > FLUSH_ALL_ABOVE {
        x86_64::instructions::tlb::flush_all();
    } else {
        for page in 0..pages {
            x86_64::instructions::tlb::flush(VirtAddr::new(addr + page * 4096));
        }
    }
    cpu.ipi.sd_ack.fetch_add(1, Ordering::AcqRel);
    lapic::eoi();
}

/// Stop the world: someone is panicking (or the PAT changes). This CPU
/// halts with interrupts off and never returns — the panicking CPU owns the
/// machine from here.
extern "C" fn stop_handler(_frame: &InterruptStackFrameValue) {
    use core::fmt::Write;
    lapic::eoi();
    // `RawSerial`: no lock (the window-safe contract; the panicking CPU may
    // hold the serial lock).
    let _ = writeln!(
        crate::arch::raw_serial::RawSerial,
        "HW:SMP:STOP cpu={}",
        percpu::current().index
    );
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// `mem::tlb::shootdown`'s cross-CPU half: write every other online CPU's
/// mailbox and ring its doorbell, waiting for each ack. Callers: only one
/// CPU is in the kernel at a time today, so a mailbox is never contended;
/// S8.3's entry lock keeps that true and records it.
// The kernel's callers arrive with S8.2's shootdown conversions; the smp
// suite drives it today.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn shootdown_others(addr: u64, pages: u64) {
    let me = percpu::current().index;
    for index in 0..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if index as u32 == me || !cpu.online.load(Ordering::Acquire) {
            continue;
        }
        let before = cpu.ipi.sd_ack.load(Ordering::Acquire);
        cpu.ipi.sd_addr.store(addr, Ordering::Release);
        cpu.ipi.sd_pages.store(pages, Ordering::Release);
        lapic::send_fixed(cpu.apic_id, SHOOTDOWN_VECTOR);
        let started = rdtsc();
        // A second of the calibrated clock (the bring-up refuses to start
        // APs without one, so it is never 0 here).
        let bound = crate::arch::clock::cycles_per_tick() * 100;
        while cpu.ipi.sd_ack.load(Ordering::Acquire) == before {
            if rdtsc().wrapping_sub(started) > bound {
                panic!(
                    "shootdown: CPU{index} apic={:#x} never acknowledged ({pages} pages at {addr:#x})",
                    cpu.apic_id
                );
            }
            spin_loop();
        }
    }
}
