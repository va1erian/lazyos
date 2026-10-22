//! The suite's rigs over the IPIs (test kernels only): run a function on an
//! AP inside its reschedule IPI, stop one AP as a panic would, and the hooks
//! that stage the bring-up's failure paths (docs/architecture/smp.md,
//! "Tests").

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{rdtsc, spin_loop};
use crate::arch::{lapic, percpu};

/// Run the function the BSP left in this CPU's mailbox (from the
/// reschedule IPI handler).
pub(super) fn run_test_job() {
    let cpu = percpu::current();
    let job = cpu.ipi.job.swap(0, Ordering::AcqRel);
    if job == 0 {
        return;
    }
    // SAFETY: `start_on` stored a `fn(u64) -> u64` as this address.
    let job: fn(u64) -> u64 = unsafe { core::mem::transmute(job) };
    let result = job(cpu.ipi.job_arg.load(Ordering::Acquire));
    cpu.ipi.job_result.store(result, Ordering::Release);
    cpu.ipi.job_done.fetch_add(1, Ordering::AcqRel);
}

/// Make AP `index` run `job(arg)` in its next reschedule IPI, without
/// waiting: the ticket [`finish_on`] waits on, or `None` when the AP is not
/// online. The job runs in the IPI handler with interrupts off, so it must
/// not lock (but the shootdown sender's turn), allocate or fault.
pub fn start_on(index: usize, job: fn(u64) -> u64, arg: u64) -> Option<u64> {
    let cpu = percpu::area(index)?;
    if !cpu.online.load(Ordering::Acquire) {
        return None;
    }
    let ticket = cpu.ipi.job_done.load(Ordering::Acquire);
    cpu.ipi.job_arg.store(arg, Ordering::Release);
    cpu.ipi.job.store(job as usize as u64, Ordering::Release);
    lapic::send_fixed(cpu.apic_id(), super::RESCHED_VECTOR);
    Some(ticket)
}

/// Wait (a calibrated `seconds`) for the job [`start_on`] posted to finish;
/// its result, or `None` on a timeout. Serves this CPU's shootdown mailbox
/// while it waits: the job may be sending to it.
pub fn finish_on(index: usize, ticket: u64, seconds: u64) -> Option<u64> {
    let cpu = percpu::area(index)?;
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 100 * seconds;
    while cpu.ipi.job_done.load(Ordering::Acquire) == ticket {
        if rdtsc().wrapping_sub(started) > bound {
            return None;
        }
        super::shootdown::serve_pending();
        spin_loop();
    }
    Some(cpu.ipi.job_result.load(Ordering::Acquire))
}

/// [`start_on`] then [`finish_on`] (a second): run `job(arg)` on AP `index`
/// and return its result; `None` if it never answered.
pub fn run_on(index: usize, job: fn(u64) -> u64, arg: u64) -> Option<u64> {
    let ticket = start_on(index, job, arg)?;
    finish_on(index, ticket, 1)
}

/// Stop one AP and take it out of the online set, as a panic would stop it:
/// the suite's way of exercising `STOP_VECTOR` without ending the run. The
/// AP can be started again with `bring_up`. Returns whether it halted.
pub fn stop_cpu_for_test(index: usize) -> bool {
    let Some(cpu) = percpu::area(index) else {
        return false;
    };
    if index == 0 || !cpu.online.load(Ordering::Acquire) {
        return false;
    }
    cpu.ipi.stopped.store(false, Ordering::Release);
    lapic::send_fixed(cpu.apic_id(), super::STOP_VECTOR);
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    while !cpu.ipi.stopped.load(Ordering::Acquire) {
        if rdtsc().wrapping_sub(started) > bound {
            return false;
        }
        super::shootdown::serve_pending();
        spin_loop();
    }
    take_offline(index);
    true
}

/// Mark a halted AP offline and drop it from the count.
fn take_offline(index: usize) {
    if let Some(cpu) = percpu::area(index) {
        if cpu.online.swap(false, Ordering::AcqRel) {
            super::APS_ONLINE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The stop-the-world sender itself (`stop_others` without claiming the
/// panic), with the online count hidden first when `hide_count`: the window
/// between an AP's own `online` store and the BSP's count, which the panic
/// path must not trust. Every AP it stopped is taken offline afterwards (so
/// `bring_up` can restart it); returns how many reported stopped.
pub fn stop_all_for_test(hide_count: bool) -> usize {
    let aps = super::APS_ONLINE.load(Ordering::Relaxed);
    if hide_count {
        super::APS_ONLINE.store(0, Ordering::Relaxed);
    }
    for index in 1..percpu::MAX_CPUS {
        if let Some(cpu) = percpu::area(index) {
            cpu.ipi.stopped.store(false, Ordering::Release);
        }
    }
    // The gate `stop_others` uses: it must see the APs with the count hidden.
    let stopped = if super::ipi::any_ap_online() {
        super::ipi::stop_online_others(percpu::current().index())
    } else {
        0
    };

    super::APS_ONLINE.store(aps, Ordering::Relaxed);
    for index in 1..percpu::MAX_CPUS {
        if percpu::area(index).is_some_and(|cpu| cpu.ipi.stopped.load(Ordering::Acquire)) {
            take_offline(index);
        }
    }
    stopped
}

/// Test hook: the bring-up sends INIT but no SIPI, so the AP never starts
/// (a CPU that refuses to start, for the timeout path).
pub(super) static NO_SIPI: AtomicBool = AtomicBool::new(false);

/// Test hook: the bring-up gives up the moment it accepted an AP's clock.
pub(super) static RUSH: AtomicBool = AtomicBool::new(false);
/// Cycles the rushed bring-up waits between its accept and its give-up (0:
/// none), so the AP's claim can win the race too.
static RUSH_DELAY: AtomicU64 = AtomicU64::new(0);

/// Make the next bring-ups give up `delay` cycles after the accept (or
/// stop doing so).
pub fn rush_deadline(on: bool, delay: u64) {
    RUSH_DELAY.store(delay, Ordering::Relaxed);
    RUSH.store(on, Ordering::Release);
}

/// The bring-up's hook: whether to give up now (after the delay).
pub(super) fn rushed() -> bool {
    if !RUSH.load(Ordering::Acquire) {
        return false;
    }
    let started = rdtsc();
    while rdtsc().wrapping_sub(started) < RUSH_DELAY.load(Ordering::Relaxed) {
        spin_loop();
    }
    true
}

/// Make the next bring-ups refuse to start their APs (or stop refusing).
pub fn refuse_sipi(on: bool) {
    NO_SIPI.store(on, Ordering::Release);
}

/// Hand a retired area out again: the suite's undo after its refused-SIPI
/// case, sound only because that AP never received a SIPI (so never ran on
/// the area).
pub fn unretire(index: usize) {
    super::RETIRED.fetch_and(!(1 << index), Ordering::Relaxed);
}

/// Whether area `index` is retired.
pub fn retired(index: usize) -> bool {
    super::RETIRED.load(Ordering::Relaxed) & (1 << index) != 0
}

/// Test hook: the bring-up sends the starting AP NMIs, from its first SIPI
/// until it is online (the early NMI table's case), every [`STORM_PERIOD`].
static STORM: AtomicBool = AtomicBool::new(false);
static STORM_LAST: AtomicU64 = AtomicU64::new(0);
static STORM_SENT: AtomicU64 = AtomicU64::new(0);
/// The storm's pace, in TSC cycles (set with the switch).
static STORM_PERIOD: AtomicU64 = AtomicU64::new(0);

/// Arm (or disarm) the bring-up's NMI storm, one NMI every `period` cycles
/// (0: back to back). Arming restarts [`storm_sent`].
pub fn nmi_storm(on: bool, period: u64) {
    if on {
        STORM_PERIOD.store(period, Ordering::Relaxed);
        STORM_SENT.store(0, Ordering::Relaxed);
    }
    STORM.store(on, Ordering::Release);
}

/// NMIs the storm sent since it was last armed.
pub fn storm_sent() -> u64 {
    STORM_SENT.load(Ordering::Relaxed)
}

/// NMIs the early table took while an AP stood at an `ap_entry` pause point
/// (one per point that was served).
static EARLY_HITS: AtomicU64 = AtomicU64::new(0);

/// Make the next AP to start stop at `ap_entry`'s pause points (`mask`: 1
/// before its own GDT, 2 after it, both before the IDT load) on `cpu`'s
/// area, for the BSP's bring-up loop to send it an NMI there.
pub fn early_pause_at(cpu: &percpu::Cpu, mask: u8) {
    EARLY_HITS.store(0, Ordering::Relaxed);
    cpu.ipi.early_paused.store(0, Ordering::Release);
    cpu.ipi.early_pause.store(mask, Ordering::Release);
}

/// Pause points served with an NMI taken by the early table.
pub fn early_hits() -> u64 {
    EARLY_HITS.load(Ordering::Relaxed)
}

/// The AP's side: stop at `point` if asked, until the BSP has sent its NMI
/// (bounded: a BSP that never answers costs 100 ms, not the start).
pub(super) fn early_pause(cpu: &percpu::Cpu, point: u8) {
    if cpu.ipi.early_pause.load(Ordering::Acquire) & point == 0 {
        return;
    }
    cpu.ipi.early_paused.store(point, Ordering::Release);
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 10;
    while cpu.ipi.early_paused.load(Ordering::Acquire) != 0 && rdtsc().wrapping_sub(started) < bound
    {
        spin_loop();
    }
}

/// The BSP's side of a pause point: one NMI, wait for the early stub to
/// count it, release the AP.
fn serve_pause(cpu: &percpu::Cpu, apic_id: u32) {
    let point = cpu.ipi.early_paused.load(Ordering::Acquire);
    if point == 0 {
        return;
    }
    let before = early_nmis();
    lapic::send_nmi(apic_id);
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 5;
    while early_nmis() == before && rdtsc().wrapping_sub(started) < bound {
        super::shootdown::serve_pending();
        spin_loop();
    }
    if early_nmis() != before {
        EARLY_HITS.fetch_add(1, Ordering::Relaxed);
    }
    let mask = cpu.ipi.early_pause.load(Ordering::Relaxed) & !point;
    cpu.ipi.early_pause.store(mask, Ordering::Relaxed);
    cpu.ipi.early_paused.store(0, Ordering::Release);
}

/// One storm step (bring-up only): serve a pause point, then an NMI to
/// `apic_id` if the storm is on, the AP is past its trampoline's `lidt`
/// (before it, an NMI takes INIT's `IDTR`: the accepted window) and the pace
/// allows.
pub(super) fn storm(cpu: &percpu::Cpu, apic_id: u32) {
    serve_pause(cpu, apic_id);
    if !STORM.load(Ordering::Acquire) || page_word(super::trampoline::EARLY_STARTED_OFF) == 0 {
        return;
    }
    let now = rdtsc();
    if now.wrapping_sub(STORM_LAST.load(Ordering::Relaxed)) >= STORM_PERIOD.load(Ordering::Relaxed)
    {
        STORM_LAST.store(now, Ordering::Relaxed);
        lapic::send_nmi(apic_id);
        STORM_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

/// NMIs the trampoline's early stubs took since `prepare` (all APs).
pub fn early_nmis() -> u32 {
    page_word(super::trampoline::EARLY_NMIS_OFF)
}

/// A `u32` of the trampoline page.
fn page_word(offset: u32) -> u32 {
    let at = crate::mem::phys_to_virt(x86_64::PhysAddr::new(
        super::TRAMPOLINE_PHYS + u64::from(offset),
    ));
    // SAFETY: the reserved trampoline page's mapped copy; an aligned `u32`
    // the AP's blob writes (atomically, where it counts).
    unsafe { at.as_ptr::<u32>().read_volatile() }
}
