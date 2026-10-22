//! One AP's start (docs/architecture/smp.md, "Bring-up"): the BSP's
//! INIT-SIPI-SIPI and wait ([`bring_up_one`]) and the AP's first kernel code
//! ([`ap_entry`]).

use core::sync::atomic::{fence, Ordering};

use super::{
    rdtsc, spin_for, spin_loop, trampoline, tsc_tolerance, tscsync, APS_ONLINE, AP_PML4, RETIRED,
    SKEW_REFUSALS, TRAMPOLINE_PHYS,
};
use crate::arch::{lapic, msr, percpu};
use crate::mem;

/// The INIT-SIPI-SIPI sequence for one AP, and the wait for its online.
/// The online count is raised **before** the AP may publish `online`, so it
/// never trails an online area (the panic path and the shootdown gate read
/// it), and lowered again if the AP never gets there.
pub(super) fn bring_up_one(index: usize, apic_id: u32, ms_cycles: u64) -> bool {
    let Some(area_addr) = percpu::prepare_ap(index) else {
        return false;
    };
    let Some(cpu) = percpu::area(index) else {
        return false;
    };
    cpu.set_apic_id(apic_id);
    write_mailbox(area_addr, cpu);

    cpu.ipi.stopped.store(false, Ordering::Release);
    // A request posted before this CPU last went down (the suite's `STOP`)
    // is moot: it starts with an empty TLB (`ap_entry`'s full flush).
    let posted = cpu.ipi.sd_req.load(Ordering::Relaxed);
    cpu.ipi.sd_ack.store(posted, Ordering::Release);
    tscsync::reset(cpu);
    let started = rdtsc();
    lapic::send_init(apic_id);
    spin_for(ms_cycles * 10); // the SDM's 10 ms INIT-to-SIPI wait
    if !refuse_sipi() {
        lapic::send_sipi(apic_id, (TRAMPOLINE_PHYS >> 12) as u8);
        storm_for(cpu, apic_id, ms_cycles / 5); // 200 us between the two SIPIs
        lapic::send_sipi(apic_id, (TRAMPOLINE_PHYS >> 12) as u8);
    }

    // The AP either publishes `online` (Release) or never comes: wait a
    // generous second of ticks, then leave it offline and carry on.
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    let mut checked = false;
    let mut counted = false;
    while rdtsc().wrapping_sub(started) < bound {
        if cpu.online.load(Ordering::Acquire) {
            return true;
        }
        // The AP is waiting for its clock to be checked: do it once, and
        // refuse a CPU whose TSC does not read like ours.
        if !checked && tscsync::ap_ready(cpu) {
            checked = true;
            match tscsync::bsp_side(cpu, started, bound) {
                Some(skew) if skew <= tsc_tolerance() => {
                    APS_ONLINE.fetch_add(1, Ordering::AcqRel);
                    counted = true;
                    tscsync::accept(cpu);
                    if deadline_now() {
                        break;
                    }
                }
                Some(skew) => {
                    // Still in its exchange loop (it never published
                    // `online`), so the INIT parks it for certain and the
                    // area stays reusable.
                    lapic::send_init(apic_id);
                    SKEW_REFUSALS.fetch_add(1, Ordering::Relaxed);
                    serial_println!(
                        "HW:SMP:TSC:SKEW cpu{index} apic={apic_id:#x} {skew} cycles, refused"
                    );
                    return false;
                }
                None => break,
            }
        }
        storm(cpu, apic_id);
        super::shootdown::serve_pending();
        spin_loop();
    }
    // An AP that claimed its accept before the deadline publishes `online`
    // a few instructions later: it is up, not late.
    if counted && !tscsync::withdraw(cpu) {
        let claimed = rdtsc();
        while !cpu.online.load(Ordering::Acquire) {
            if rdtsc().wrapping_sub(claimed) > bound / 10 {
                // Wedged between its claim and its store. It may still go
                // online, so it stays counted (the count must never trail
                // an online area) and its area retired; it is not parked
                // (an INIT now could land after it went online).
                RETIRED.fetch_or(1 << index, Ordering::Relaxed);
                serial_println!("HW:SMP:CPU{index} apic={apic_id:#x} FAIL (claimed, not online)");
                return false;
            }
            spin_loop();
        }
        return true;
    }
    // Park it, and never hand its area out again: an AP that started but
    // never published may still be running on it.
    lapic::send_init(apic_id);
    RETIRED.fetch_or(1 << index, Ordering::Relaxed);
    if counted {
        APS_ONLINE.fetch_sub(1, Ordering::AcqRel);
    }
    serial_println!("HW:SMP:CPU{index} apic={apic_id:#x} FAIL (no start)");
    false
}

/// Write the trampoline's per-AP mailbox: written before the sequence (the
/// AP reads it only after its SIPI; there is no concurrency until `online`
/// publishes).
fn write_mailbox(area_addr: u64, cpu: &percpu::Cpu) {
    let mb = |off: u32| -> *mut u64 {
        (mem::phys_to_virt(x86_64::PhysAddr::new(TRAMPOLINE_PHYS + off as u64)).as_u64())
            as *mut u64
    };
    // SAFETY: the reserved trampoline page's mapped copy; the BSP is the
    // only writer until the AP is online.
    unsafe {
        mb(trampoline::MB_CPU_OFF).write_volatile(area_addr);
        mb(trampoline::MB_ENTRY_OFF).write_volatile(ap_entry as *const () as u64);
        mb(trampoline::MB_STACK_OFF).write_volatile(cpu.kernel_stack_top());
        // `CR3` and the magic were written by `prepare` (they are the same
        // for every AP); rewrite them anyway — the mailbox is one structure.
        mb(trampoline::MB_CR3_OFF).write_volatile(AP_PML4.load(Ordering::Relaxed));
        (mb(trampoline::MB_MAGIC_OFF) as *mut u32).write_volatile(trampoline::MB_MAGIC);
        (mb(trampoline::EARLY_STARTED_OFF) as *mut u32).write_volatile(0);
    }
}

/// The AP's first kernel instruction: long mode, its own stack, `%gs` base
/// already pointed at its area by the trampoline, the trampoline's early
/// NMI table in `IDTR`.
extern "C" fn ap_entry() -> ! {
    // `EFER.NXE` is per-CPU MSR state; the trampoline already set it (the
    // kernel half's NX bits fault without it), this keeps it explicit.
    const EFER_NXE: u64 = 1 << 11;
    let efer = msr::read(msr::IA32_EFER);
    msr::write(msr::IA32_EFER, efer | EFER_NXE);
    // This CPU's own GDT, TSS and stacks (its area), then `%gs` again: the
    // selector loads in `init_cpu` reset the hidden GS base to 0.
    let area = percpu::current();
    #[cfg(lazyos_tests)]
    super::testrig::early_pause(area, 1);
    // SAFETY: this AP owns its offline area's TSS and GDT (`prepare_ap`);
    // nothing else touches them until `online` is published below. The
    // kernel GDT's code (0x08) and stack (0x10) selectors match the
    // trampoline's, so an early NMI's frame stays loadable across the switch.
    unsafe {
        crate::arch::gdt::init_cpu(area);
    }
    percpu::set_gs_base(area.self_ptr.load(Ordering::Relaxed));
    #[cfg(lazyos_tests)]
    super::testrig::early_pause(area, 2);
    // The machine-wide IDT (its NMI gate needs the TSS above for its IST
    // stack, and `%gs` for its paranoid test). Until here an NMI took the
    // trampoline's early table, which lives in the identity-mapped page.
    if !crate::arch::idt::load_on_this_cpu() {
        loop {
            x86_64::instructions::hlt();
        }
    }
    // Only now leave the trampoline's page table for the boot table (the
    // machine-wide kernel half): the early table is unmapped after this.
    mem::switch_to(mem::boot_table());
    // Without the serial lock: if the BSP's timeout INIT stopped this CPU
    // inside a locked print, the BSP's next print would deadlock.
    {
        use core::fmt::Write;
        let _ = writeln!(
            crate::arch::raw_serial::RawSerial,
            "HW:SMP:CPU{} entered long mode",
            area.index()
        );
    }
    mem::wc::pat_this_cpu();
    crate::arch::cpu::init();
    crate::arch::linux::init();
    lapic::init_ap_local();

    area.ipi.tsc_entry.store(rdtsc(), Ordering::Relaxed);
    // The BSP measures how far this CPU's TSC reads from its own before it
    // lets the CPU online (a skewed one is parked from there).
    if !tscsync::ap_side(area) {
        // Refused or unvouched for: the BSP parks this CPU with INIT; until
        // it lands (or forever, if nobody is left to) it halts here.
        loop {
            x86_64::instructions::interrupts::disable();
            x86_64::instructions::hlt();
        }
    }
    area.online.store(true, Ordering::Release);
    // Pairs with the shootdown sender's fence before its scan of `online`:
    // a sender that did not see this CPU online changed its tables before
    // this flush, so nothing stale survives it.
    fence(Ordering::SeqCst);
    x86_64::instructions::tlb::flush_all();
    // Idle with interrupts ON, on the boot table: the handlers that can
    // reach this CPU are the window-safe set (no locks, no allocation, no
    // task switch), so a delivered IPI cannot race anything and its mailbox
    // is serviced promptly. No scheduler and no timer runs here until S8.3.
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Test hook: the bring-up reaches its deadline right after it accepted the
/// AP's clock (the race between its withdrawal and the AP's claim).
fn deadline_now() -> bool {
    #[cfg(lazyos_tests)]
    {
        super::testrig::RUSH.load(Ordering::Acquire)
    }
    #[cfg(not(lazyos_tests))]
    {
        false
    }
}

/// Test hook: whether the bring-up withholds the SIPI.
fn refuse_sipi() -> bool {
    #[cfg(lazyos_tests)]
    {
        super::testrig::NO_SIPI.load(Ordering::Acquire)
    }
    #[cfg(not(lazyos_tests))]
    {
        false
    }
}

/// Test hook: while the suite's NMI storm is armed, send the starting AP
/// NMIs (`super::testrig::storm`); nothing in a normal kernel.
#[inline]
fn storm(cpu: &percpu::Cpu, apic_id: u32) {
    #[cfg(lazyos_tests)]
    super::testrig::storm(cpu, apic_id);
    #[cfg(not(lazyos_tests))]
    let _ = (cpu, apic_id);
}

/// [`spin_for`], storming the AP meanwhile when the suite asks.
fn storm_for(cpu: &percpu::Cpu, apic_id: u32, cycles: u64) {
    let started = rdtsc();
    while rdtsc().wrapping_sub(started) < cycles {
        storm(cpu, apic_id);
        spin_loop();
    }
}
