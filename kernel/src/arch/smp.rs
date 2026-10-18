//! Application-processor bring-up (docs/smp-plan.md S8.2): the MADT's APIC
//! IDs, the trampoline copy, the AP page table, INIT-SIPI, the TSC gates and
//! the three kernel IPIs (reschedule, shootdown, stop).
//!
//! Stability-first shape, per the plan: the APs come up, initialize their
//! own per-CPU state (control regs, FPU, `EFER`, PAT, syscall MSRs, local
//! APIC) on their own kernel stacks, stamp their entry TSC, mark themselves
//! online and **idle with interrupts enabled** — the handlers that can reach
//! them are exactly the window-safe set (no locks, no allocation, no task
//! switch), so the idle loop services its mailboxes without being able to
//! race anything. No scheduler runs on an AP yet (S8.3 is the enabled
//! milestone); no timer is armed (the tick stays the BSP's); nothing runs
//! user code there yet (TLB shootdowns land with the entry lock in S8.3,
//! when a second CPU first holds user translations).
//!
//! Configuration (`lazyos.cfg`, [`PREFIX`]): `smp.cpus=N` caps the CPU
//! count (default: the MADT's, clamped by `limit.cpu_max`); `smp.tsc=force`
//! skips the invariance gate for TCG rigs (which refuse `invtsc`), built
//! into test kernels only.

mod trampoline;

use alloc::vec::Vec;
use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use x86_64::structures::idt::InterruptStackFrame;
use x86_64::VirtAddr;

use super::lapic;
use super::msr;
use super::percpu;
use crate::mem;

/// The `lazyos.cfg` prefix routed here by `fs::bootcfg`.
pub const PREFIX: &str = "smp.";
/// The physical page (below 1 MiB, conventional memory) the trampoline is
/// copied to; the SIPI vector is its page number, `0x08`.
pub const TRAMPOLINE_PHYS: u64 = 0x8000;

/// The three kernel IPIs, in the audit-pinned free range 0x60..0x7F (below
/// them: MSI at 0x40..0x5F; above: the software gates 0x80/0x81).
pub const RESCHED_VECTOR: u8 = 0x60;
pub const SHOOTDOWN_VECTOR: u8 = 0x61;
pub const STOP_VECTOR: u8 = 0x62;

/// `smp.cpus=N` from `lazyos.cfg` (0 = unset: derive from the MADT).
static CONFIG_CPUS: AtomicU64 = AtomicU64::new(0);
/// `smp.tsc=force`: skip the invariance gate (TCG rigs).
static TSC_FORCE: AtomicBool = AtomicBool::new(false);
/// The decided CPU count (>= 1 once [`init`] ran).
static CPUS: AtomicUsize = AtomicUsize::new(1);
/// The AP page table's physical address (its PML4), 0 = not built.
static AP_PML4: AtomicU64 = AtomicU64::new(0);
/// How many APs came up (`HW:SMP:CPUS` reports it).
static APS_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// The `smp.*` lines of `lazyos.cfg` (`fs::bootcfg` routes the whole text
/// here; unknown keys are logged and ignored, as everywhere).
pub fn apply_config(text: &str) {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = rest.split_once('=') else {
            serial_println!("fs: lazyos.cfg: smp.{rest} is not key=value, ignored");
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "cpus" => match value.parse::<u64>() {
                Ok(count) => CONFIG_CPUS.store(count, Ordering::Relaxed),
                Err(_) => serial_println!("fs: lazyos.cfg: bad value for smp.cpus, ignored"),
            },
            "tsc" if cfg!(lazyos_tests) && value == "force" => {
                TSC_FORCE.store(true, Ordering::Relaxed)
            }
            "tsc" => serial_println!("fs: lazyos.cfg: smp.tsc is a test rig key, ignored"),
            _ => serial_println!("fs: lazyos.cfg: unknown key smp.{key} ignored"),
        }
    }
}

/// CPUs online (the BSP included; 1 until the APs came up).
pub fn online_cpus() -> usize {
    1 + APS_ONLINE.load(Ordering::Relaxed)
}

/// The decided CPU count: what the kernel may run on (1 until [`init`]).
pub fn cpus() -> usize {
    CPUS.load(Ordering::Relaxed)
}

/// Whether the TSC is invariant (`CPUID.8000_0007H.EDX[8]`).
fn invariant_tsc() -> bool {
    let leaf = unsafe { core::arch::x86_64::__cpuid(0x8000_0007) };
    leaf.edx & (1 << 8) != 0
}

/// The suite's hook: the real sequence with an explicit count (`-smp N`
/// must have given QEMU the CPUs; the boot path uses [`init`]). Test-mode
/// kernels also force the TSC invariance gate (TCG refuses `invtsc`).
#[cfg(lazyos_tests)]
pub fn bring_up_for_test(n: usize) {
    TSC_FORCE.store(true, Ordering::Relaxed);
    CPUS.store(n.min(percpu::MAX_CPUS).max(1), Ordering::Relaxed);
    if cpus() > 1 && AP_PML4.load(Ordering::Relaxed) == 0 && prepare().is_none() {
        CPUS.store(1, Ordering::Relaxed);
    }
    bring_up(cpus());
}

/// The boot gate: decide the CPU count and bring the APs up. Idempotent and
/// self-refusing: without a second APIC id, without a coherent TSC, or when
/// the count comes out 1, this is a single-CPU boot and says so on serial.
pub fn init() {
    decide();
    if cpus() <= 1 {
        serial_println!("HW:SMP:CPUS=1 (no APs to bring up)");
        return;
    }
    // The TSC invariance gate (the coherence half runs per AP below): the
    // catch-up clock needs an invariant TSC for wall-clock sanity; TCG
    // refuses `invtsc`, so the gate yields to the test-rig override rather
    // than disarming every TCG-based SMP test.
    if !invariant_tsc() && !TSC_FORCE.load(Ordering::Relaxed) {
        serial_println!("HW:SMP:TSC:NONINV staying single-CPU");
        CPUS.store(1, Ordering::Relaxed);
        serial_println!("HW:SMP:CPUS=1");
        return;
    }
    let Some(page) = prepare() else {
        serial_println!("HW:SMP:CPUS=1 (trampoline refused)");
        CPUS.store(1, Ordering::Relaxed);
        return;
    };
    let _ = page;
    bring_up(cpus());
    serial_println!("HW:SMP:CPUS={}", online_cpus());
}

/// Decide the CPU count: the config's cap, the MADT's ids (the BSP's
/// excluded), `limit.cpu_max` and the per-CPU table's capacity, minimum 1.
fn decide() {
    let madt_cpus = platform_apic_ids().map(|ids| ids.len()).unwrap_or(1);
    let config = CONFIG_CPUS.load(Ordering::Relaxed) as usize;
    let want = if config == 0 { madt_cpus } else { config };
    let capped = want
        .min(crate::limits::cpu_max())
        .min(percpu::MAX_CPUS)
        .max(1);
    CPUS.store(capped, Ordering::Relaxed);
}

/// The MADT's processor APIC ids (empty when there is no usable MADT).
fn platform_apic_ids() -> Option<Vec<u32>> {
    let platform = super::acpi_tables::platform()?;
    let madt = platform.madt.as_ref().ok()?;
    let ids: Vec<u32> = madt.apic_ids.as_slice().iter().copied().collect();
    (!ids.is_empty()).then_some(ids)
}

/// Build the AP page table, copy and patch the trampoline. `None` (with the
/// reason on serial) leaves the machine single-CPU.
fn prepare() -> Option<u64> {
    // The trampoline page below 1 MiB was reserved by `mem::init` (out of
    // the DMA pool when the pool owns low memory, else from the general
    // allocator) — an invariant from the first boot on, so the copy below
    // cannot land on a frame anything else ever holds.
    let pml4 = build_ap_page_table()?;
    AP_PML4.store(pml4, Ordering::Relaxed);

    // Copy the blob and patch the one absolute value in it: the GDTR's base.
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(TRAMPOLINE_PHYS)).as_u64() as *mut u8;
    let start = &raw const trampoline::smp_trampoline_start;
    let end = &raw const trampoline::smp_trampoline_end;
    let len = (end as usize - start as usize) as u64;
    if len as u64 > 4096 {
        serial_println!("HW:SMP:CPUS=1 (trampoline overflows its page)");
        CPUS.store(1, Ordering::Relaxed);
        return None;
    }
    // SAFETY: `at` is the reserved page's mapped copy, `len` its own size;
    // no other CPU is running and the source is the kernel's own text.
    unsafe {
        core::ptr::copy_nonoverlapping(start, at, len as usize);
        // The patch values: the GDTR's base, and both retf transitions'
        // (segment, offset) pairs — each target at the copy's own address.
        let at_u = at as u64;
        let blob_off = |label: &u8| (label as *const u8 as usize) - start as usize;
        let prot_off = (TRAMPOLINE_PHYS as usize + blob_off(&trampoline::smp_prot)) as u16;
        let long_off = (TRAMPOLINE_PHYS as usize + blob_off(&trampoline::smp_long_stub)) as u32;
        let put16 = |off: u32| -> *mut u16 { (at_u + off as u64) as *mut u16 };
        let put32 = |off: u32| -> *mut u32 { (at_u + off as u64) as *mut u32 };
        // The GDTR's base sits at +2 (after the `u16` limit) — unaligned by
        // construction, so the unaligned write.
        ((at_u + trampoline::GDTR_BASE_OFF as u64) as *mut u32)
            .write_unaligned(TRAMPOLINE_PHYS as u32 + trampoline::GDTR_OFF);
        put16(trampoline::RETF1_CS_OFF).write_volatile(0x0008);
        put16(trampoline::RETF1_OFF_OFF).write_volatile(prot_off);
        put32(trampoline::RETF2_CS_OFF).write_volatile(0x0020);
        put32(trampoline::RETF2_OFF_OFF).write_volatile(long_off);
        // And the mailbox fields every AP shares (`CR3`, the magic); the
        // per-AP fields (`entry`, `cpu`, `stack`) are written per AP.
        ((at_u + trampoline::MB_CR3_OFF as u64) as *mut u64)
            .write_volatile(AP_PML4.load(Ordering::Relaxed));
        put32(trampoline::MB_MAGIC_OFF).write_volatile(trampoline::MB_MAGIC);
    }
    Some(TRAMPOLINE_PHYS)
}

/// Build the AP page table: identity-mapped low 2 MiB (the trampoline's
/// transitions run there) over the kernel half copied from the boot table.
/// Returns the PML4's physical address.
fn build_ap_page_table() -> Option<u64> {
    let pml4_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    let pdpt_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    let pd_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    const PRESENT: u64 = 1;
    const WRITE: u64 = 1 << 1;
    const HUGE: u64 = 1 << 7;
    // PML4[0] -> PDPT[0] -> PD[0] -> the 2 MiB identity huge page 0..2 MiB.
    entry(pml4_phys, 0, pdpt_phys | PRESENT | WRITE);
    entry(pdpt_phys, 0, pd_phys | PRESENT | WRITE);
    entry(pd_phys, 0, 0x0000_0000 | PRESENT | WRITE | HUGE);
    // The kernel half: entries 256..512 verbatim from the boot table (the
    // kernel half is machine-wide; sharing the entries shares the mappings).
    let boot = mem::boot_table().as_u64();
    for index in 256..512_usize {
        let value = read_entry(boot, index)?;
        entry(pml4_phys, index, value);
    }
    Some(pml4_phys)
}

/// Write one PML4/PDPT/PD entry: `table_phys` is below 4 GiB (a freshly
/// allocated frame), so the entry write goes through the physical map.
fn entry(table_phys: u64, index: usize, value: u64) {
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(table_phys + (index as u64) * 8)).as_u64()
        as *mut u64;
    // SAFETY: the frame is zeroed, exclusively ours (freshly allocated),
    // mapped through the physical map; `index` is in range.
    unsafe { at.write_volatile(value) };
}

/// Read one entry of the boot PML4 (its physical address is what
/// `mem::boot_table` reports; the entry sits in the physical map).
fn read_entry(table_phys: u64, index: usize) -> Option<u64> {
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(table_phys + (index as u64) * 8)).as_u64()
        as *const u64;
    // SAFETY: the boot PML4 is machine-wide and mapped; the entry is in range.
    unsafe { Some(at.read_volatile()) }
}

/// Bring the APs up, up to `count` CPUs total (the BSP included): for each
/// MADT id that is not the BSP's, write its mailbox and send the
/// INIT-SIPI-SIPI sequence. Idempotent: an online AP is skipped.
pub fn bring_up(count: usize) {
    let Some(ids) = platform_apic_ids() else {
        serial_println!(
            "HW:SMP:CPUS={} (no MADT: no APIC ids to bring up)",
            online_cpus()
        );
        return;
    };
    let bsp = lapic::id();
    let cycles_per_tick = super::clock::cycles_per_tick();
    let ms = cycles_per_tick / 100;
    for &id in ids.iter() {
        if online_cpus() >= count {
            break;
        }
        if id == bsp {
            continue;
        }
        let Some(index) = free_area_index() else {
            break;
        };
        if bring_up_one(index, id, ms) {
            APS_ONLINE.fetch_add(1, Ordering::Relaxed);
            serial_println!(
                "HW:SMP:CPU{index} apic={id:#x} online (skew={})",
                skew_of(index)
            );
        }
    }
}

/// A per-CPU area no CPU owns yet (an offline one).
fn free_area_index() -> Option<usize> {
    (0..percpu::MAX_CPUS).find(|&index| {
        !percpu::area(index)
            .expect("index below MAX_CPUS")
            .online
            .load(Ordering::Acquire)
    })
}

/// The |AP entry TSC - BSP observation TSC| of an online AP, in cycles (the
/// coherence measure: a TSC that starts at 0 or ticks on another base stands
/// out immediately; two cores of one clock differ by the start-up delay).
fn skew_of(index: usize) -> u64 {
    let cpu = percpu::area(index).expect("online index");
    rdtsc().wrapping_sub(cpu.ipi.tsc_entry.load(Ordering::Acquire))
}

/// The INIT-SIPI-SIPI sequence for one AP, and the wait for its online.
fn bring_up_one(index: usize, apic_id: u32, ms_cycles: u64) -> bool {
    let cpu = percpu::area(index).expect("index below MAX_CPUS");
    cpu.apic_id = apic_id;
    // The mailbox: per AP, written before the sequence (the AP reads it only
    // after its SIPI; there is no concurrency until `online` publishes).
    let mb = |off: u32| -> *mut u64 {
        (mem::phys_to_virt(x86_64::PhysAddr::new(TRAMPOLINE_PHYS + off as u64)).as_u64())
            as *mut u64
    };
    // SAFETY: the reserved trampoline page's mapped copy; the BSP is the
    // only writer until the AP is online.
    unsafe {
        mb(trampoline::MB_CPU_OFF).write_volatile((cpu as *mut percpu::Cpu) as u64);
        mb(trampoline::MB_ENTRY_OFF).write_volatile(ap_entry as *const () as u64);
        mb(trampoline::MB_STACK_OFF)
            .write_volatile(cpu.kernel_stack.as_ptr() as u64 + percpu::KERNEL_STACK_SIZE as u64);
        // `CR3` and the magic were written by `prepare` (they are the same
        // for every AP); rewrite them anyway — the mailbox is one structure.
        mb(trampoline::MB_CR3_OFF).write_volatile(AP_PML4.load(Ordering::Relaxed));
        (mb(trampoline::MB_MAGIC_OFF) as *mut u32).write_volatile(trampoline::MB_MAGIC);
    }

    let started = rdtsc();
    lapic::send_init(apic_id);
    spin_for(ms_cycles); // the SDM's INIT-to-SIPI wait
    lapic::send_sipi(apic_id, (TRAMPOLINE_PHYS >> 12) as u8);
    spin_for(ms_cycles / 10);
    lapic::send_sipi(apic_id, (TRAMPOLINE_PHYS >> 12) as u8);

    // The AP either publishes `online` (Release) or never comes: wait a
    // generous second of ticks, then leave it offline and carry on.
    let bound = started + super::clock::cycles_per_tick() * 100;
    while rdtsc().wrapping_sub(started) < bound {
        if cpu.online.load(Ordering::Acquire) {
            return true;
        }
        spin_loop();
    }
    serial_println!("HW:SMP:CPU{index} apic={apic_id:#x} FAIL (no start)");
    false
}

/// Busy-wait `cycles` (boot code with interrupts off: no tick, no window).
fn spin_for(cycles: u64) {
    let started = rdtsc();
    while rdtsc().wrapping_sub(started) < cycles {
        spin_loop();
    }
}

#[inline]
fn spin_loop() {
    core::hint::spin_loop();
}

/// The TSC read (`_rdtsc` is `unsafe`: a TSC-affecting SDE session could
/// skew it; the kernel accepts that for its serial diagnostics and its
/// clocks, as `arch::clock` does).
#[inline]
fn rdtsc() -> u64 {
    // SAFETY: reads the CPU's TSC; no memory or privilege effects.
    unsafe { _rdtsc() }
}

/// The AP's first kernel instruction: long mode, its own stack, `%gs` base
/// already pointed at its area by the trampoline.
extern "C" fn ap_entry() -> ! {
    let cpu = percpu::current();
    serial_println!("HW:SMP:CPU{} entered long mode", cpu.index);
    // From here on the kernel half is all this CPU needs: drop the trampoline's
    // page table for the boot table (the machine-wide kernel half).
    mem::switch_to(mem::boot_table());
    // The per-CPU MSRs and control state: exactly what the BSP's boot did.
    // `EFER.NXE` must be set per CPU (no-exec is per-CPU MSR state).
    const EFER_NXE: u64 = 1 << 63;
    let efer = msr::read(msr::IA32_EFER);
    msr::write(msr::IA32_EFER, efer | EFER_NXE);
    mem::wc::pat_this_cpu();
    super::cpu::init();
    super::linux::init();
    lapic::init_ap_local();

    cpu.ipi.tsc_entry.store(rdtsc(), Ordering::Relaxed);
    cpu.online.store(true, Ordering::Release);
    // Idle with interrupts ON: the handlers that can reach this CPU are the
    // window-safe set (no locks, no allocation, no task switch), so a
    // delivered IPI cannot race anything and its mailbox is serviced
    // promptly. No scheduler and no timer runs here until S8.3.
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

// --- the IPI handlers -----------------------------------------------------
// All three obey the `irq_window` contract exactly: no lock but this CPU's
// own area, no allocation, no task switch — the SMP generalization of the
// window-safe handlers.

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
