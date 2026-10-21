//! The three kernel IPIs (docs/smp-plan.md S8.2: vectors 0x60..0x62) and
//! the sender half of a TLB shootdown. All three handlers obey the
//! `irq_window` contract exactly: no lock but this CPU's own area, no
//! allocation, no task switch — the SMP generalization of the window-safe
//! handlers.

use core::sync::atomic::Ordering;
use x86_64::structures::idt::InterruptStackFrame;
use x86_64::VirtAddr;

use super::{rdtsc, spin_loop, SHOOTDOWN_VECTOR};
use crate::arch::{lapic, percpu};

/// "A wake made a task more deserving of the CPU than the current one": the
/// next preemption point acts on the flag (S8.3 is when an AP has tasks).
pub(crate) extern "x86-interrupt" fn resched_handler(_frame: InterruptStackFrame) {
    percpu::current()
        .preempt
        .need_resched
        .store(true, Ordering::Relaxed);
    lapic::eoi();
}

/// The shootdown doorbell: the mailbox names the pages; invalidate and ack.
pub(crate) extern "x86-interrupt" fn shootdown_handler(_frame: InterruptStackFrame) {
    let cpu = percpu::current();
    let addr = cpu.ipi.sd_addr.load(Ordering::Acquire);
    let pages = cpu.ipi.sd_pages.load(Ordering::Acquire).min(1024);
    if addr == 0 && pages == 0 {
        // The "everything" case (`mem::tlb::shootdown_all`).
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
pub(crate) extern "x86-interrupt" fn stop_handler(_frame: InterruptStackFrame) {
    lapic::eoi();
    serial_println!("HW:SMP:STOP cpu={}", percpu::current().index);
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
        while cpu.ipi.sd_ack.load(Ordering::Acquire) == before {
            if rdtsc().wrapping_sub(started) > 1_000_000_000 {
                panic!(
                    "shootdown: CPU{index} apic={:#x} never acknowledged ({pages} pages at {addr:#x})",
                    cpu.apic_id
                );
            }
            spin_loop();
        }
    }
}
