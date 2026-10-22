//! The SMP stage's robustness cases (docs/architecture/smp.md): several
//! shootdown senders at once, nested NMIs, NMIs during an AP's start, a CPU
//! that refuses to start, and the panic path's view of a CPU the online
//! count has not caught up with. Each leaves the machine as it found it
//! (every AP online again, every hook off). The AP cases are vacuous with
//! one CPU online; the nested-NMI case runs on the boot CPU either way.

use super::smp_tlb_suite::online_aps;
use super::*;
use crate::arch::nmi::test as nmi_test;
use crate::arch::{lapic, percpu, smp};
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

/// Rounds per sender of the concurrent-shootdown case (`--soak` builds
/// with `LAZYOS_SMP_SOAK=1`: the long one).
const SENDER_ROUNDS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    200_000
} else {
    3_000
};

/// Nested-NMI rounds.
const NMI_ROUNDS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    20_000
} else {
    300
};

/// An address mapped in no table the suite runs on (the boot table maps
/// nothing in the user half): the fault the nested-NMI case takes.
const UNMAPPED: u64 = 0x0000_0020_0000_0000;

/// Every CPU's shootdown ack count, by area index.
fn all_acks() -> Vec<u64> {
    (0..percpu::MAX_CPUS)
        .map(|i| percpu::area(i).map_or(0, |cpu| cpu.ipi.sd_ack.load(Ordering::Acquire)))
        .collect()
}

/// The AP job of the concurrent case: `rounds` shootdowns, alternating a
/// page and a range long enough to be a full flush, sent from inside the
/// AP's reschedule IPI (interrupts off: the worst case for a sender).
fn job_send_shootdowns(rounds: u64) -> u64 {
    for round in 0..rounds {
        let pages = if round % 2 == 0 { 1 } else { 100 };
        crate::mem::tlb::shootdown(0xE000_0000 + (round % 64) * 4096, pages);
    }
    rounds
}

/// Shootdowns from the BSP and up to two APs at once, in a loop: no
/// deadlock (every sender finishes, each with interrupts off while it may
/// itself be a target), and every request is acknowledged exactly once by
/// every CPU it was sent to.
fn concurrent_senders_are_all_acked() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let senders: Vec<usize> = aps.iter().copied().take(2).collect();
    let before = all_acks();
    let mut tickets = Vec::new();
    for &ap in &senders {
        tickets.push(smp::start_on(ap, job_send_shootdowns, SENDER_ROUNDS).ok_or("AP offline")?);
    }
    job_send_shootdowns(SENDER_ROUNDS);
    for (&ap, &ticket) in senders.iter().zip(&tickets) {
        let done = smp::finish_on(ap, ticket, 600);
        check!(
            done == Some(SENDER_ROUNDS),
            "AP {ap} did not finish its {SENDER_ROUNDS} shootdowns: {done:?}"
        );
    }
    let after = all_acks();
    let mut online = aps.clone();
    online.push(0);
    for cpu in online {
        let others = 1 + senders.len() as u64 - u64::from(cpu == 0 || senders.contains(&cpu));
        let got = after[cpu].wrapping_sub(before[cpu]);
        check!(
            got == others * SENDER_ROUNDS,
            "CPU{cpu} acknowledged {got} shootdowns, expected {}",
            others * SENDER_ROUNDS
        );
    }
    Ok(())
}

/// A nested NMI: the handler takes a fault (whose `iretq` unblocks NMIs),
/// a second NMI arrives while it still runs, and the gate drops it instead
/// of reusing the IST top under the first. Repeated, then the first NMI
/// must still return normally every time.
fn nested_nmi_is_dropped() -> Result<(), String> {
    // The NMIs are sent through the local APIC (a PIC-only boot has none).
    if lapic::current_mode().is_none() {
        return Ok(());
    }
    check!(
        crate::arch::probe::read_faults(UNMAPPED),
        "{UNMAPPED:#x} is mapped; the case needs a faulting address"
    );
    let cpu = percpu::current();
    let nested = crate::arch::nmi::nested_dropped();
    let taken = cpu.ipi.nmis.load(Ordering::Relaxed);
    let result = (|| {
        for round in 0..NMI_ROUNDS {
            nmi_test::arm(cpu, nmi_test::NESTED, UNMAPPED);
            lapic::send_nmi(lapic::id());
            let outcome = wait_for(|| nmi_test::outcome() != nmi_test::Outcome::Pending);
            check!(
                outcome && nmi_test::outcome() == nmi_test::Outcome::Dropped,
                "round {round}: {:?}",
                nmi_test::outcome()
            );
        }
        Ok(())
    })();
    nmi_test::arm(cpu, nmi_test::REPORT, 0);
    result?;
    let dropped = crate::arch::nmi::nested_dropped() - nested;
    let ran = cpu.ipi.nmis.load(Ordering::Relaxed) - taken;
    check!(
        dropped == NMI_ROUNDS && ran == NMI_ROUNDS,
        "{NMI_ROUNDS} rounds: {dropped} nested NMIs dropped, the handler ran {ran} times"
    );
    Ok(())
}

/// Spin up to a second of the calibrated clock for `done`.
fn wait_for(done: impl Fn() -> bool) -> bool {
    wait_ms(1000, done)
}

/// Spin up to `ms` milliseconds of the calibrated TSC for `done` (the
/// suite runs with interrupts off: no tick advances the clock).
fn wait_ms(ms: u64, done: impl Fn() -> bool) -> bool {
    // SAFETY: reads the TSC; no memory or privilege effects.
    let tsc = || unsafe { core::arch::x86_64::_rdtsc() };
    let started = tsc();
    let bound = crate::arch::clock::cycles_per_tick() / 10 * ms;
    while !done() {
        if tsc().wrapping_sub(started) > bound {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// An NMI reaches every online AP's real handler (its own IST stack, its
/// own TSS) and returns: 100 to each, all counted.
fn nmi_reaches_every_online_ap() -> Result<(), String> {
    let aps = online_aps();
    let areas: Vec<&percpu::Cpu> = aps.iter().filter_map(|&i| percpu::area(i)).collect();
    for cpu in &areas {
        nmi_test::arm(cpu, nmi_test::QUIET, 0);
    }
    let before: Vec<u64> = areas
        .iter()
        .map(|c| c.ipi.nmis.load(Ordering::Relaxed))
        .collect();
    let mut ok = true;
    for (cpu, &was) in areas.iter().zip(&before) {
        for round in 1..=100 {
            lapic::send_nmi(cpu.apic_id());
            ok &= wait_for(|| cpu.ipi.nmis.load(Ordering::Relaxed) >= was + round);
        }
    }
    for cpu in &areas {
        nmi_test::arm(cpu, nmi_test::REPORT, 0);
    }
    check!(ok, "an AP did not take all of its 100 NMIs");
    Ok(())
}

/// The last online AP, stopped (offline, its area free again).
fn stop_last_ap() -> Result<usize, String> {
    let victim = *online_aps().last().ok_or("no online AP")?;
    check!(
        smp::stop_cpu_for_test(victim),
        "AP {victim} did not halt on STOP"
    );
    Ok(victim)
}

/// NMIs from the AP's first SIPI until it is online — through the
/// trampoline's real, protected and long modes and `ap_entry` before its
/// IDT load — and the AP still comes up. Without the early NMI table, an
/// NMI there triple-faults the machine. Where the storm lands is up to
/// timing, so the AP also stops at two points of `ap_entry` before its IDT
/// load (on the trampoline's GDT, then on its own) and takes one NMI at
/// each, through the early table, for certain.
fn nmi_storm_during_bring_up_is_survived() -> Result<(), String> {
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let victim = stop_last_ap()?;
    let cpu = percpu::area(victim).ok_or("no area")?;
    nmi_test::arm(cpu, nmi_test::QUIET, 0);
    smp::early_pause_at(cpu, 3);
    let early = smp::early_nmis();
    let late = cpu.ipi.nmis.load(Ordering::Relaxed);
    // An NMI every 10 us of the BSP's clock (a tick is 10 ms): dense, and
    // never so dense the AP could do nothing but take them.
    smp::nmi_storm(true, crate::arch::clock::cycles_per_tick() / 1000);
    smp::bring_up_for_test(percpu::MAX_CPUS);
    smp::nmi_storm(false, 0);
    // Let NMIs still in flight land in the quiet handler.
    wait_ms(20, || false);
    let (early, late) = (
        smp::early_nmis().wrapping_sub(early),
        cpu.ipi.nmis.load(Ordering::Relaxed) - late,
    );
    let hits = smp::early_hits();
    nmi_test::arm(cpu, nmi_test::REPORT, 0);
    smp::early_pause_at(cpu, 0);
    serial_println!(
        "TEST:smp_nmi storm: {} sent, {early} taken by the early stubs ({hits} at the pause points), {late} by the handler",
        smp::storm_sent()
    );
    check!(
        smp::online_cpus() == online,
        "the stormed AP did not come up ({} CPUs online, expected {online})",
        smp::online_cpus()
    );
    check!(
        hits == 2,
        "the early NMI table served {hits} of the 2 pause points"
    );
    check!(
        smp::storm_sent() > 0 && early as u64 + late > 0,
        "the storm never reached the AP"
    );
    Ok(())
}

/// A CPU that never starts (no SIPI): the bring-up waits out its second,
/// parks it with INIT, retires its area and carries on; the suite then
/// hands the area back (sound: that CPU never ran) and starts it for real.
fn refused_sipi_times_out_and_boot_continues() -> Result<(), String> {
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let victim = stop_last_ap()?;
    smp::refuse_sipi(true);
    smp::bring_up_for_test(percpu::MAX_CPUS);
    smp::refuse_sipi(false);
    let (count, retired) = (smp::online_cpus(), smp::retired(victim));
    smp::unretire(victim);
    smp::bring_up_for_test(percpu::MAX_CPUS);
    check!(
        count == online - 1 && retired,
        "a CPU that never started: {count} CPUs online (expected {}), area retired: {retired}",
        online - 1
    );
    check!(
        smp::online_cpus() == online
            && percpu::area(victim).is_some_and(|c| c.online.load(Ordering::Acquire)),
        "the area was not reusable after the undo ({} CPUs online)",
        smp::online_cpus()
    );
    Ok(())
}

/// The panic path's gate reads the areas, not the online count: with the
/// count hidden (an AP that stored its own `online` before the BSP counted
/// it), every AP is still sent `STOP` and halts.
fn stop_sees_aps_the_count_missed() -> Result<(), String> {
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let stopped = smp::stop_all_for_test(true);
    let left = smp::online_cpus();
    smp::bring_up_for_test(percpu::MAX_CPUS);
    check!(
        stopped == online - 1 && left == 1,
        "with the count hidden {stopped} of {} APs stopped ({left} CPUs left online)",
        online - 1
    );
    check!(
        smp::online_cpus() == online,
        "the restart left {} CPUs online, expected {online}",
        smp::online_cpus()
    );
    Ok(())
}

/// The bring-up gives up the instant it accepted an AP's clock, racing the
/// AP's claim: either the AP claimed first (online, counted, and it answers
/// a shootdown) or the withdrawal won (offline, not counted, its area
/// retired). Never a parked CPU marked online, nor an online one uncounted.
fn deadline_race_never_strands_a_cpu() -> Result<(), String> {
    const ROUNDS: u32 = 10;
    let online = smp::online_cpus();
    if online <= 1 {
        return Ok(());
    }
    let (mut claimed, mut withdrawn) = (0, 0);
    for round in 0..ROUNDS {
        let victim = stop_last_ap()?;
        smp::rush_deadline(true);
        smp::bring_up_for_test(percpu::MAX_CPUS);
        smp::rush_deadline(false);
        let areas = (0..percpu::MAX_CPUS)
            .filter_map(percpu::area)
            .filter(|cpu| cpu.online.load(Ordering::Acquire))
            .count();
        check!(
            smp::online_cpus() == areas,
            "round {round}: {} CPUs counted, {areas} areas online",
            smp::online_cpus()
        );
        if percpu::area(victim).is_some_and(|c| c.online.load(Ordering::Acquire)) {
            claimed += 1;
            // It must really run: a stranded CPU would never acknowledge.
            crate::mem::tlb::shootdown(0xE000_0000, 1);
        } else {
            withdrawn += 1;
            check!(
                smp::retired(victim),
                "round {round}: a withdrawn area was not retired"
            );
            // The INIT that parked it has landed long before this.
            wait_ms(10, || false);
            smp::unretire(victim);
            smp::bring_up_for_test(percpu::MAX_CPUS);
        }
        check!(
            smp::online_cpus() == online,
            "round {round}: {} CPUs online after the restart, expected {online}",
            smp::online_cpus()
        );
    }
    serial_println!("TEST:smp_deadline race: {claimed} claimed, {withdrawn} withdrawn");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "smp_shootdown_concurrent_senders_are_all_acked",
        concurrent_senders_are_all_acked,
    ),
    ("smp_nmi_nested_is_dropped", nested_nmi_is_dropped),
    (
        "smp_nmi_reaches_every_online_ap",
        nmi_reaches_every_online_ap,
    ),
    (
        "smp_nmi_storm_during_bring_up_is_survived",
        nmi_storm_during_bring_up_is_survived,
    ),
    (
        "smp_refused_sipi_times_out_and_boot_continues",
        refused_sipi_times_out_and_boot_continues,
    ),
    (
        "smp_stop_sees_aps_the_count_missed",
        stop_sees_aps_the_count_missed,
    ),
    (
        "smp_deadline_race_never_strands_a_cpu",
        deadline_race_never_strands_a_cpu,
    ),
];
