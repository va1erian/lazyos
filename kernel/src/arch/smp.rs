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
//! user code there yet. TLB shootdowns already reach them (`shootdown`):
//! every page-table change goes through `mem::tlb` from S8.2 on.
//!
//! Configuration (`lazyos.cfg`, [`PREFIX`]): the CPU count is the MADT's
//! enabled processors, capped by `smp.cpus=N` and by `limit.cpu_max`, whose
//! default is **1**: until S8.3 gives the APs work, a normal boot keeps them
//! parked, and an image opts in with `LAZYOS_LIMIT_CPU_MAX=N`
//! (`run_demo.py --limit cpu_max=N`). `smp.tsc=force` skips the invariance
//! gate for TCG rigs (which refuse `invtsc`), built into test kernels only.

mod bringup;
mod ipi;
mod prepare;
mod shootdown;
#[cfg(lazyos_tests)]
mod testrig;
mod trampoline;
mod tscsync;

pub use ipi::stop_others;
pub(crate) use ipi::{resched_gate, stop_gate};
// `serve_pending` is the rule every interrupts-off spin follows (its first
// caller outside this module is S8.3's entry lock).
#[allow(unused_imports)]
pub use shootdown::serve_pending;
pub(crate) use shootdown::shootdown_gate;
pub use shootdown::shootdown_others;
#[cfg(lazyos_tests)]
pub use testrig::{
    early_hits, early_nmis, early_pause_at, finish_on, nmi_storm, refuse_sipi, retired, run_on,
    rush_deadline, start_on, stop_all_for_test, stop_cpu_for_test, storm_sent, unretire,
};

use alloc::vec::Vec;
use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::lapic;
use super::percpu;
use bringup::bring_up_one;
use prepare::prepare;

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
/// APs refused for a TSC offset since boot (the suite tells a skew refusal
/// from any other failed start by it).
static SKEW_REFUSALS: AtomicUsize = AtomicUsize::new(0);
/// How many APs came up (`HW:SMP:CPUS` reports it).
static APS_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// The `smp.*` lines of `lazyos.cfg` (`fs::bootcfg` routes the whole text
/// here; unknown keys are logged and ignored, as everywhere).
pub fn apply_config(text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
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

/// APs refused for a TSC offset so far.
#[cfg(lazyos_tests)]
pub fn skew_refusals() -> usize {
    SKEW_REFUSALS.load(Ordering::Relaxed)
}

/// CPUs online (the BSP included; 1 until the APs came up).
pub fn online_cpus() -> usize {
    1 + APS_ONLINE.load(Ordering::Relaxed)
}

/// The online CPUs other than this one whose `CR3` holds `table` (one bit
/// per area index): what `mem::free_user_table` refuses to free under. 0
/// whenever no AP is online, without reading per-CPU state.
pub fn others_on_table(table: u64) -> u64 {
    if !ipi::any_ap_online() {
        return 0;
    }
    let me = percpu::current().index() as usize;
    (0..percpu::MAX_CPUS)
        .filter(|&index| index != me)
        .filter(|&index| {
            percpu::area(index).is_some_and(|cpu| {
                cpu.online.load(Ordering::Acquire) && cpu.cr3.load(Ordering::Acquire) == table
            })
        })
        .fold(0, |mask, index| mask | 1 << index)
}

/// The decided CPU count: what the kernel may run on (1 until [`init`]).
pub fn cpus() -> usize {
    CPUS.load(Ordering::Relaxed)
}

/// Whether the TSC is invariant (`CPUID.8000_0007H.EDX[8]`).
fn invariant_tsc() -> bool {
    let leaf = core::arch::x86_64::__cpuid(0x8000_0007);
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
    // The TSC invariance gate (the plan's coherence half, a skew
    // measurement per AP, is still to come: today only the entry latency is
    // logged): the catch-up clock needs an invariant TSC for wall-clock
    // sanity; TCG
    // refuses `invtsc`, so the gate yields to the test-rig override rather
    // than disarming every TCG-based SMP test.
    if !invariant_tsc() && !TSC_FORCE.load(Ordering::Relaxed) {
        serial_println!("HW:SMP:TSC:NONINV staying single-CPU");
        CPUS.store(1, Ordering::Relaxed);
        serial_println!("HW:SMP:CPUS=1");
        return;
    }
    if super::clock::cycles_per_tick() == 0 {
        // Every bring-up wait (INIT-SIPI, the online poll) is timed on it.
        serial_println!("HW:SMP:CPUS=1 (the TSC is not calibrated: no time base)");
        CPUS.store(1, Ordering::Relaxed);
        return;
    }
    if prepare().is_none() {
        serial_println!("HW:SMP:CPUS=1 (trampoline refused)");
        CPUS.store(1, Ordering::Relaxed);
        return;
    }
    bring_up(cpus());
    serial_println!("HW:SMP:CPUS={}", online_cpus());
}

/// Decide the CPU count: the config's cap, the MADT's ids (the BSP's
/// excluded), `limit.cpu_max` and the per-CPU table's capacity, minimum 1.
fn decide() {
    let madt_cpus = platform_apic_ids().map(|ids| ids.len()).unwrap_or(1);
    let config = CONFIG_CPUS.load(Ordering::Relaxed) as usize;
    let want = if config == 0 { madt_cpus } else { config };
    // `cpu_max` is at least 1 (its key's minimum), so the bounds are ordered.
    let capped = want.clamp(1, crate::limits::cpu_max().min(percpu::MAX_CPUS));
    CPUS.store(capped, Ordering::Relaxed);
}

/// The MADT's processor APIC ids (empty when there is no usable MADT).
fn platform_apic_ids() -> Option<Vec<u32>> {
    let platform = super::acpi_tables::platform()?;
    let madt = platform.madt.as_ref().ok()?;
    let ids: Vec<u32> = madt.startable.as_slice().to_vec();
    (!ids.is_empty()).then_some(ids)
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
    if cycles_per_tick == 0 {
        serial_println!(
            "HW:SMP:CPUS={} (no time base for the bring-up)",
            online_cpus()
        );
        return;
    }
    let ms = cycles_per_tick / 10; // one 10 ms tick -> 1 ms
    let xapic = lapic::mode() == Some(lapic::Mode::XApic);
    for &id in ids.iter() {
        if online_cpus() >= count {
            break;
        }
        // An xAPIC ICR names 8-bit destinations: a larger id would be
        // truncated, possibly onto a running CPU (the BSP's 0).
        if xapic && id > 0xFF {
            serial_println!("HW:SMP:apic={id:#x} skipped (beyond xAPIC addressing)");
            continue;
        }
        // Never INIT a CPU that runs: the BSP, or an AP already online (an
        // INIT resets it mid-flight, the idempotence the doc promises).
        if id == bsp || apic_online(id) {
            continue;
        }
        let Some(index) = free_area_index() else {
            break;
        };
        if bring_up_one(index, id, ms) {
            serial_println!(
                "HW:SMP:CPU{index} apic={id:#x} online (latency={})",
                entry_latency(index)
            );
        }
    }
}

/// Whether an online area already belongs to APIC `id`.
fn apic_online(id: u32) -> bool {
    (1..percpu::MAX_CPUS)
        .filter_map(percpu::area)
        .any(|cpu| cpu.online.load(Ordering::Acquire) && cpu.apic_id() == id)
}

/// Areas given to an AP that timed out: it may still be running on one (a
/// late start the parking INIT did not catch), so none is handed out again.
/// One bit per area (`MAX_CPUS` is 64).
static RETIRED: AtomicU64 = AtomicU64::new(0);

/// A per-CPU area no CPU owns yet (offline and never retired).
fn free_area_index() -> Option<usize> {
    let retired = RETIRED.load(Ordering::Relaxed);
    (0..percpu::MAX_CPUS).find(|&index| {
        retired & (1 << index) == 0
            && percpu::area(index).is_some_and(|cpu| !cpu.online.load(Ordering::Acquire))
    })
}

/// How long ago, in cycles, an online AP stamped its entry TSC: the
/// bring-up's latency as the BSP sees it. Not a skew measure (that needs a
/// two-way exchange, and the plan's coherence gate is still to come); a TSC
/// that started elsewhere shows as an absurd value. Wraps if the AP's TSC is
/// ahead.
fn entry_latency(index: usize) -> u64 {
    let Some(cpu) = percpu::area(index) else {
        return 0;
    };
    rdtsc().wrapping_sub(cpu.ipi.tsc_entry.load(Ordering::Acquire))
}

/// The TSC offset an AP may show against the BSP and still be trusted: 50 us
/// (a 10 ms tick is `cycles_per_tick`). Above it, time would visibly jump
/// when a task changes CPU.
fn tsc_tolerance() -> u64 {
    super::clock::cycles_per_tick() / 200
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
