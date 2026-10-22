//! The three kernel IPIs (docs/smp-plan.md S8.2: vectors 0x60..0x62) and
//! the sender half of a TLB shootdown. All three handlers (behind `arch::gsgate`'s IRQ gates) obey the
//! `irq_window` contract exactly: no lock but this CPU's own area, no
//! allocation, no task switch — the SMP generalization of the window-safe
//! handlers.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::structures::idt::InterruptStackFrameValue;
use x86_64::VirtAddr;

use super::{rdtsc, spin_loop, SHOOTDOWN_VECTOR};

use crate::arch::gsgate::irq_gate;
use crate::arch::{lapic, percpu};
use crate::mem::tlb::FLUSH_ALL_ABOVE;

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
    #[cfg(lazyos_tests)]
    run_test_job();
    lapic::eoi();
}

/// The suite's rig: run the function the BSP left in this CPU's mailbox.
#[cfg(lazyos_tests)]
fn run_test_job() {
    let cpu = percpu::current();
    let job = cpu.ipi.job.swap(0, Ordering::AcqRel);
    if job == 0 {
        return;
    }
    // SAFETY: `run_on` stored a `fn(u64) -> u64` as this address.
    let job: fn(u64) -> u64 = unsafe { core::mem::transmute(job) };
    let result = job(cpu.ipi.job_arg.load(Ordering::Acquire));
    cpu.ipi.job_result.store(result, Ordering::Release);
    cpu.ipi.job_done.fetch_add(1, Ordering::AcqRel);
}

/// Make AP `index` run `job(arg)` in its next reschedule IPI and return its
/// result; `None` if it never answered. Test rig only: the job runs in the
/// IPI handler, so it must not lock, allocate or fault.
#[cfg(lazyos_tests)]
pub fn run_on(index: usize, job: fn(u64) -> u64, arg: u64) -> Option<u64> {
    let cpu = percpu::area(index)?;
    if !cpu.online.load(Ordering::Acquire) {
        return None;
    }
    let before = cpu.ipi.job_done.load(Ordering::Acquire);
    cpu.ipi.job_arg.store(arg, Ordering::Release);
    cpu.ipi.job.store(job as usize as u64, Ordering::Release);
    lapic::send_fixed(cpu.apic_id, super::RESCHED_VECTOR);
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    while cpu.ipi.job_done.load(Ordering::Acquire) == before {
        if rdtsc().wrapping_sub(started) > bound {
            return None;
        }
        spin_loop();
    }
    Some(cpu.ipi.job_result.load(Ordering::Acquire))
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
    percpu::current().ipi.stopped.store(true, Ordering::Release);
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

/// Set by the first CPU to panic: it owns the machine from then on.
static PANIC_OWNER: AtomicBool = AtomicBool::new(false);

/// Stop the world for a panic (docs/smp-plan.md S8.2): every other online
/// CPU is sent `STOP_VECTOR` and given a bounded time to halt, so nothing
/// else runs while the panic report is written (the serial lock's
/// force-unlock and the on-screen report both assume one CPU). Takes no
/// lock and allocates nothing; a no-op with one CPU online. A second CPU
/// that panics while the first is stopping the world just halts.
pub fn stop_others() {
    if super::online_cpus() <= 1 {
        return;
    }
    if PANIC_OWNER.swap(true, Ordering::AcqRel) {
        loop {
            x86_64::instructions::interrupts::disable();
            x86_64::instructions::hlt();
        }
    }
    let me = percpu::current().index;
    for index in 0..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if index as u32 != me && cpu.online.load(Ordering::Acquire) {
            lapic::send_fixed(cpu.apic_id, super::STOP_VECTOR);
        }
    }
    // A CPU that cannot be stopped (wedged with interrupts off) is left:
    // the report matters more than a full stop.
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 20;
    for index in 0..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if index as u32 == me || !cpu.online.load(Ordering::Acquire) {
            continue;
        }
        while !cpu.ipi.stopped.load(Ordering::Acquire) && rdtsc().wrapping_sub(started) < bound {
            spin_loop();
        }
    }
}

/// Stop one AP and take it out of the online set, as a panic would stop it:
/// the suite's way of exercising `STOP_VECTOR` without ending the run. The
/// AP can be started again with `bring_up`. Returns whether it halted.
#[cfg(lazyos_tests)]
pub fn stop_cpu_for_test(index: usize) -> bool {
    let Some(cpu) = percpu::area(index) else {
        return false;
    };
    if index == 0 || !cpu.online.load(Ordering::Acquire) {
        return false;
    }
    cpu.ipi.stopped.store(false, Ordering::Release);
    lapic::send_fixed(cpu.apic_id, super::STOP_VECTOR);
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    while !cpu.ipi.stopped.load(Ordering::Acquire) {
        if rdtsc().wrapping_sub(started) > bound {
            return false;
        }
        spin_loop();
    }
    cpu.online.store(false, Ordering::Release);
    super::APS_ONLINE.fetch_sub(1, Ordering::Relaxed);
    true
}

/// `mem::tlb::shootdown`'s cross-CPU half: write every other online CPU's
/// mailbox and ring its doorbell, waiting for each ack. Callers: only one
/// CPU is in the kernel at a time today, so a mailbox is never contended;
/// S8.3's entry lock keeps that true and records it.
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
