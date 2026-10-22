//! The SMP bring-up suite (docs/smp-plan.md S8.2): drive the real
//! INIT-SIPI-SIPI sequence through `smp::bring_up` on whatever `-smp N`
//! gave QEMU, then exercise the IPI paths the APs' idle loops service.

use super::*;
use crate::arch::percpu;
use crate::arch::smp;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

/// Bring every AP the machine has up (bounded by `-smp`): the trampoline is
/// copied and patched, the AP page table built, INIT-SIPI-SIPI sent, and
/// each AP runs `ap_entry` on its own stack — the per-CPU MSRs, the local
/// APIC and the TSC stamp — then goes idle with interrupts on. The test
/// boots with `-smp 4` (`tools/test/run.py --smp 4`), so this is the real
/// sequence, not a mock.
pub fn bring_up_bring_every_ap_online() -> Result<(), String> {
    let before = smp::online_cpus();
    smp::bring_up_for_test(percpu::MAX_CPUS);
    let after = smp::online_cpus();
    check!(
        after >= before,
        "online_cpus went backwards: {before} -> {after}"
    );
    // Whatever `-smp N` QEMU got: either 1 CPU (no APs; the run was not a
    // multicore one — the test passes vacuously and says so) or all of the
    // MADT's ids came up.
    let madt = crate::arch::acpi_tables::platform()
        .map(|p| p.madt.as_ref().ok().map(|m| m.apic_ids.len()))
        .flatten()
        .unwrap_or(1);
    if madt <= 1 {
        check!(after == 1, "no MADT APIC ids but {after} CPUs online");
        return Ok(());
    }
    check!(
        after == madt.min(percpu::MAX_CPUS),
        "expected {} CPUs online, got {after}",
        madt.min(percpu::MAX_CPUS)
    );
    // Every online AP stamped an entry TSC just before going online (the
    // bring-up logs its latency; a real skew gate is still to come).
    for index in 1..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if !cpu.online.load(Ordering::Acquire) {
            continue;
        }
        check!(
            cpu.ipi.tsc_entry.load(Ordering::Acquire) != 0,
            "CPU{index} is online with no TSC stamp"
        );
        check!(
            cpu.apic_id != 0,
            "CPU{index} is online with the BSP's APIC id 0"
        );
    }
    Ok(())
}

/// The reschedule IPI, end to end on this CPU: send it at ourselves through
/// the real ICR, let the window admit it (the handler is window-safe), and
/// observe the flag the scheduler's next entry acts on.
pub fn resched_ipi_sets_the_flag_through_the_icr() -> Result<(), String> {
    use super::irq_window_suite::{in_syscall, NR_A};
    let flag = || {
        percpu::current()
            .preempt
            .need_resched
            .load(Ordering::Relaxed)
    };
    let was = flag();
    percpu::current()
        .preempt
        .need_resched
        .store(false, Ordering::Relaxed);
    crate::arch::lapic::send_fixed(crate::arch::lapic::id(), smp::RESCHED_VECTOR);
    // The suite runs with interrupts off and windows only open inside a
    // syscall span: stage one, and give the self-IPI a few windows to land
    // (its delivery is asynchronous to the ICR write). Re-read the flag
    // through `current()` every time: the handler took its own `&mut`.
    let (ran, _) = in_syscall(NR_A, || {
        (0..1000).any(|_| {
            crate::arch::irq_window::open();
            flag()
        })
    });
    percpu::current()
        .preempt
        .need_resched
        .store(was, Ordering::Relaxed);
    check!(ran, "the resched IPI handler did not run in 1000 windows");
    Ok(())
}

/// The shootdown IPI: the mailbox, the doorbell, the ack — from this CPU to
/// every online AP, which invalidates (nothing it caches, but the ack says
/// the handler ran) and answers. Requires the APs online (`-smp N`, N > 1);
/// passes vacuously otherwise.
pub fn shootdown_ipi_is_acked_by_every_online_ap() -> Result<(), String> {
    if smp::online_cpus() <= 1 {
        return Ok(());
    }
    // A scratch mapping nobody else caches: the kernel half's own text is
    // mapped everywhere, which makes it the honest worst case (the handler
    // must not disturb it for the BSP).
    let before: Vec<u64> = (1..percpu::MAX_CPUS)
        .filter_map(|i| percpu::area(i))
        .filter(|cpu| cpu.online.load(Ordering::Acquire))
        .map(|cpu| cpu.ipi.sd_ack.load(Ordering::Acquire))
        .collect();
    crate::mem::tlb::shootdown(0xE000_0000, 1);
    let mut now = Vec::new();
    for index in 1..percpu::MAX_CPUS {
        let Some(cpu) = percpu::area(index) else {
            break;
        };
        if !cpu.online.load(Ordering::Acquire) {
            continue;
        }
        now.push(cpu.ipi.sd_ack.load(Ordering::Acquire));
    }
    check!(
        now.len() == before.len() && now.iter().zip(before.iter()).all(|(n, b)| n > b),
        "not every online AP acknowledged the shootdown: {before:?} -> {now:?}"
    );
    Ok(())
}

/// Shootdown and reschedule IPIs under sustained load: 20000 round trips to
/// every online AP, alternating a one-page range, a range long enough that
/// the target flushes everything instead, and a reschedule IPI to each AP in
/// between. Every shootdown must be acknowledged by every AP exactly once
/// (the generation counts match), and no AP may drop offline. Vacuous at one
/// CPU.
pub fn ipi_soak_shootdowns_and_rescheds() -> Result<(), String> {
    const ROUNDS: u64 = 20_000;
    if smp::online_cpus() <= 1 {
        return Ok(());
    }
    let online = || {
        (1..percpu::MAX_CPUS)
            .filter_map(percpu::area)
            .filter(|cpu| cpu.online.load(Ordering::Acquire))
    };
    let before: Vec<u64> = online()
        .map(|cpu| cpu.ipi.sd_ack.load(Ordering::Acquire))
        .collect();
    let apics: Vec<u32> = online().map(|cpu| cpu.apic_id).collect();
    for round in 0..ROUNDS {
        let pages = if round % 2 == 0 { 1 } else { 4096 };
        crate::mem::tlb::shootdown(0xE000_0000 + (round % 64) * 4096, pages);
        for &apic in &apics {
            crate::arch::lapic::send_fixed(apic, smp::RESCHED_VECTOR);
        }
    }
    let after: Vec<u64> = online()
        .map(|cpu| cpu.ipi.sd_ack.load(Ordering::Acquire))
        .collect();
    check!(
        after.len() == before.len(),
        "an AP went offline during the soak: {} -> {}",
        before.len(),
        after.len()
    );
    for (b, a) in before.iter().zip(after.iter()) {
        check!(
            a - b == ROUNDS,
            "an AP acknowledged {} of {ROUNDS} shootdowns",
            a - b
        );
    }
    Ok(())
}

/// A second bring-up is a no-op: online APs are skipped, the count holds.
pub fn bring_up_is_idempotent() -> Result<(), String> {
    let before = smp::online_cpus();
    smp::bring_up_for_test(percpu::MAX_CPUS);
    check!(
        smp::online_cpus() == before,
        "a second bring-up moved the count: {before} -> {}",
        smp::online_cpus()
    );
    Ok(())
}

/// `STOP_VECTOR`, the panic path's tool: a stopped AP halts with interrupts
/// off and answers nothing; the bring-up can start it again (the suite
/// leaves the machine as it found it). Vacuous at one CPU.
pub fn stop_vector_halts_an_ap_and_it_restarts() -> Result<(), String> {
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let victim = (1..percpu::MAX_CPUS)
        .rev()
        .find(|&i| percpu::area(i).is_some_and(|cpu| cpu.online.load(Ordering::Acquire)))
        .ok_or("no online AP")?;
    check!(
        smp::stop_cpu_for_test(victim),
        "the AP did not halt on STOP"
    );
    check!(
        smp::online_cpus() == online - 1,
        "online count after the stop: {}",
        smp::online_cpus()
    );
    // The others carry on: a shootdown skips the stopped CPU (offline).
    crate::mem::tlb::shootdown(0xE000_0000, 1);
    smp::bring_up_for_test(percpu::MAX_CPUS);
    check!(
        smp::online_cpus() == online,
        "the restart left {} CPUs online, expected {online}",
        smp::online_cpus()
    );
    let ack = |i: usize| percpu::area(i).map(|cpu| cpu.ipi.sd_ack.load(Ordering::Acquire));
    let before: Vec<_> = (1..percpu::MAX_CPUS).map(ack).collect();
    crate::mem::tlb::shootdown(0xE000_0000, 1);
    let after: Vec<_> = (1..percpu::MAX_CPUS).map(ack).collect();
    let online_ids: Vec<usize> = (1..percpu::MAX_CPUS)
        .filter(|&i| percpu::area(i).is_some_and(|cpu| cpu.online.load(Ordering::Acquire)))
        .collect();
    for &i in &online_ids {
        check!(
            after[i - 1] > before[i - 1],
            "CPU{i} did not acknowledge a shootdown after the restart"
        );
    }
    Ok(())
}

/// The TSC coherence gate: an AP whose counter reads a millisecond away from
/// the BSP's is refused and left parked (`HW:SMP:TSC:SKEW`), and the same CPU
/// started again with a clean counter is accepted. Vacuous at one CPU.
pub fn skewed_tsc_ap_is_refused() -> Result<(), String> {
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let victim = (1..percpu::MAX_CPUS)
        .rev()
        .find(|&i| percpu::area(i).is_some_and(|cpu| cpu.online.load(Ordering::Acquire)))
        .ok_or("no online AP")?;
    check!(
        smp::stop_cpu_for_test(victim),
        "the AP did not halt on STOP"
    );
    // One millisecond of the calibrated clock (a tick is 10 ms), on every
    // area the bring-up might pick; running APs only read it when they start.
    let skew = crate::arch::clock::cycles_per_tick() / 10;
    let set_bias = |bias: u64| {
        for index in 1..percpu::MAX_CPUS {
            if let Some(cpu) = percpu::area(index) {
                cpu.ipi.tsc_bias.store(bias, Ordering::Release);
            }
        }
    };
    let refusals = smp::skew_refusals();
    set_bias(skew);
    smp::bring_up_for_test(percpu::MAX_CPUS);
    let refused = smp::online_cpus() == online - 1 && smp::skew_refusals() == refusals + 1;
    set_bias(0);
    check!(
        refused,
        "a CPU with a 1 ms TSC offset was not refused for it ({} CPUs, {} refusals)",
        smp::online_cpus(),
        smp::skew_refusals() - refusals
    );
    smp::bring_up_for_test(percpu::MAX_CPUS);
    check!(
        smp::online_cpus() == online,
        "the clean restart left {} CPUs online, expected {online}",
        smp::online_cpus()
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "smp_bring_up_bring_every_ap_online",
        bring_up_bring_every_ap_online,
    ),
    (
        "smp_resched_ipi_sets_the_flag_through_the_icr",
        resched_ipi_sets_the_flag_through_the_icr,
    ),
    (
        "smp_shootdown_ipi_is_acked_by_every_online_ap",
        shootdown_ipi_is_acked_by_every_online_ap,
    ),
    (
        "smp_ipi_soak_shootdowns_and_rescheds",
        ipi_soak_shootdowns_and_rescheds,
    ),
    ("smp_bring_up_is_idempotent", bring_up_is_idempotent),
    (
        "smp_stop_vector_halts_an_ap_and_it_restarts",
        stop_vector_halts_an_ap_and_it_restarts,
    ),
    ("smp_skewed_tsc_ap_is_refused", skewed_tsc_ap_is_refused),
];
