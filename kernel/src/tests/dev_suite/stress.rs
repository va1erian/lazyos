//! Stress and soak tests for the interrupt path and the device syscall (issue
//! #240): interrupt storms, driver crash/restart churn, and claim/release
//! cycles through the syscall path. Each ends by proving nothing leaked.

use super::fixture::*;
use super::irq::rig;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::dev::intx::{self, ACK_DEADLINE_TICKS};
use crate::dev::irq;
use crate::dev::syscall::*;
use crate::quota;
use crate::quota::Resource;

const DEVICE_MEM: u64 = 0xFED0_0000;

/// 100k interrupts with the driver never acking: the claim's queue depth
/// stays at exactly one and the interrupt is only remembered. Then 100k
/// serviced interrupts: every one is delivered exactly once, in order, and
/// nothing is left queued, charged or armed.
pub fn irq_storm_keeps_queue_depth_one() -> Result<(), String> {
    const STORM: u64 = 100_000;
    let _fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    let queue_before = quota::usage(0, Resource::QueueDepth);
    let (delivered_before, _) = intx::counters();

    // Phase 1: the driver hangs.
    fire(LINE_A, 1_000);
    for burst in 0..STORM {
        irq::dispatch(LINE_A);
        intx::service_at(1_000);
        if burst % 4_096 == 0 {
            check!(
                r.queued()? == 1,
                "burst {burst}: queue depth {}",
                r.queued()?
            );
        }
    }
    check!(r.queued()? == 1, "storm left queue depth {}", r.queued()?);
    check!(
        intx::counters().0 == delivered_before + 1,
        "the storm posted extra messages"
    );
    check!(
        r.flags()? == (true, true, true),
        "storm state {:?}",
        r.flags()?
    );
    check!(
        masked(LINE_A),
        "the line was left unmasked during the storm"
    );
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "ack after the storm")?;
    // The missed interrupt is re-posted once, not 100k times.
    check!(r.queued()? == 1, "recovery posted {} messages", r.queued()?);
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "final ack")?;

    // Phase 2: a healthy driver under a storm.
    let base = intx::counters().0;
    for round in 0..STORM {
        fire(LINE_A, 2_000 + round);
        let (sender, ..) = take_irq(r.endpoint)?;
        check!(
            sender == task::KERNEL_TASK,
            "round {round}: sender {sender}"
        );
        expect_ok(r.ack(), "storm ack")?;
        check!(
            !masked(LINE_A),
            "round {round}: the line stayed masked after the ack"
        );
    }
    check!(
        intx::counters().0 == base + STORM,
        "delivered {} of {STORM}",
        intx::counters().0 - base
    );
    check!(r.queued()? == 0, "messages left queued");
    check!(
        quota::usage(0, Resource::QueueDepth) == queue_before,
        "queue-depth charge leaked: {} -> {}",
        queue_before,
        quota::usage(0, Resource::QueueDepth)
    );
    check!(
        r.flags()? == (true, false, false),
        "not idle after the storm"
    );
    Ok(())
}

/// A storm on a shared line with one hung claimant: the healthy one gets every
/// interrupt, the laggard is timed out once and its queue stays at one.
pub fn irq_shared_storm_with_laggard() -> Result<(), String> {
    const STORM: u64 = 20_000;
    let _fx = Fixture::new()?;
    let (healthy, hung) = (rig(LINE_A, true, true)?, rig(LINE_A, true, true)?);
    let timeouts_before = intx::counters().1;
    let mut clock = 1_000;
    for round in 0..STORM {
        fire(LINE_A, clock);
        enter(healthy.slot)?;
        take_irq(healthy.endpoint)?;
        expect_ok(healthy.ack(), "healthy ack")?;
        // Advance past the deadline once so the hung claimant is dropped, then
        // keep the clock ticking slowly (well inside a fresh deadline).
        clock += if round == 0 { ACK_DEADLINE_TICKS } else { 1 };
        intx::service_at(clock);
        if round > 0 {
            check!(
                !masked(LINE_A),
                "round {round}: a hung claimant holds the line"
            );
        }
    }
    check!(
        intx::counters().1 == timeouts_before + 1,
        "expected exactly one timeout, saw {}",
        intx::counters().1 - timeouts_before
    );
    check!(
        hung.queued()? == 1,
        "the hung claimant's queue is {}",
        hung.queued()?
    );
    check!(
        hung.flags()? == (true, true, true),
        "hung claimant state {:?}",
        hung.flags()?
    );
    Ok(())
}

/// Spawn a driver that claims a device with a mapped BAR, an armed interrupt
/// in flight and a port window, then kill it: 10k times. The device must be
/// claimable again every time, its generation must advance by one per crash,
/// and quotas, handles, frames and the MMIO address range must not leak.
pub fn claim_kill_soak_10k() -> Result<(), String> {
    const ROUNDS: u32 = 10_000;
    let fx = Fixture::new()?;
    let dev = add_device(
        Spec::nic(Some(LINE_A))
            .with_bars(vec![mem_bar(0, DEVICE_MEM, 0x2000), io_bar(1, 0x0700, 8)]),
    )?;
    let (_, generation) = table_state(dev);
    let frames_before = mem::frame_stats();
    let audit_before = audit::total();
    let mut first_va = None;
    for round in 0..ROUNDS {
        let slot = spawn_driver(driver_cred())?;
        enter(slot)?;
        let mut endpoint = 0u64;
        let handle = expect_ok(claim_irq(dev, &mut endpoint, false), "claim")?;
        expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
        let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map_bar")?;
        expect_ok(sys(OP_PIO, handle, 1, 0, pio_word(1, false, 0)), "pio")?;
        if round % 3 == 0 {
            fire(LINE_A, u64::from(round));
        }
        leave(&fx);
        task::harness::finish(slot, 0);
        check!(
            task::reap_child().map(|reaped| reaped.0) == Some(slot),
            "round {round}: the driver was not reaped"
        );
        check!(
            table_state(dev) == (None, generation + round + 1),
            "round {round}: device state {:?}",
            table_state(dev)
        );
        check!(CLAIMS.lock().len() == 0, "round {round}: a claim survived");
        check!(masked(LINE_A), "round {round}: the line stayed unmasked");
        // The first mapping fixes the address; every later one must reuse it.
        let expected = *first_va.get_or_insert(va);
        check!(
            va == expected,
            "round {round}: MMIO va {va:#x}, first was {expected:#x}"
        );
    }
    check!(
        usage(Resource::DeviceClaims) == 0
            && usage(Resource::UserMemory) == 0
            && usage(Resource::Handles) == 0,
        "quota leaked: claims {} memory {} handles {}",
        usage(Resource::DeviceClaims),
        usage(Resource::UserMemory),
        usage(Resource::Handles)
    );
    let frames_after = mem::frame_stats();
    check!(
        frames_after.live() == frames_before.live()
            && frames_after.double_frees == frames_before.double_frees,
        "frames {} -> {}",
        frames_before.live(),
        frames_after.live()
    );
    check!(
        audit::total() >= audit_before + 2 * u64::from(ROUNDS),
        "claims and teardowns were not all audited"
    );
    // Claimable after all that.
    let last = spawn_driver(driver_cred())?;
    enter(last)?;
    expect_ok(claim_plain(dev), "final claim")?;
    Ok(())
}

/// Claim/use/release cycles through the syscall path with the task alive:
/// ownership, generation, quotas, handles, the MMIO range and frames all
/// return to their starting values, and successful cycles add no denials.
pub fn claim_release_syscall_soak() -> Result<(), String> {
    const CYCLES: u32 = 20_000;
    let _fx = Fixture::new()?;
    let dev = add_device(
        Spec::nic(Some(LINE_B))
            .with_bars(vec![mem_bar(0, DEVICE_MEM, 0x1000), io_bar(1, 0x0700, 8)]),
    )?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (_, generation) = table_state(dev);
    let handles_before = handles::count_for_task(slot);
    let denials_before = audit::denials();
    let mut frames = None;
    let mut first_va = None;
    for cycle in 0..CYCLES {
        let handle = expect_ok(claim_plain(dev), "claim")?;
        let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map_bar")?;
        expect_ok(sys(OP_PIO, handle, 1, 4, pio_word(4, false, 0)), "pio")?;
        expect_ok(sys(OP_CFG_READ, handle, 0, 2, 0), "cfg_read")?;
        expect_ok(sys(OP_CFG_WRITE, handle, 4, 2, 0x0002), "cfg_write")?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        let expected = *first_va.get_or_insert(va);
        check!(
            va == expected,
            "cycle {cycle}: MMIO va {va:#x} != {expected:#x}"
        );
        // Page tables for the mapping are allocated once, on the first cycle.
        let live = mem::frame_stats().live();
        let baseline = *frames.get_or_insert(live);
        check!(
            live == baseline,
            "cycle {cycle}: frames {baseline} -> {live}"
        );
    }
    check!(
        table_state(dev) == (None, generation + CYCLES),
        "final device state {:?}",
        table_state(dev)
    );
    check!(
        usage(Resource::DeviceClaims) == 0 && usage(Resource::UserMemory) == 0,
        "quota leaked"
    );
    check!(
        handles::count_for_task(slot) == handles_before,
        "handles leaked"
    );
    check!(CLAIMS.lock().len() == 0, "claims leaked");
    check!(
        audit::denials() == denials_before,
        "a successful cycle recorded a denial"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_stress_irq_storm_queue_depth_one",
        irq_storm_keeps_queue_depth_one,
    ),
    (
        "dev_stress_irq_shared_storm_with_laggard",
        irq_shared_storm_with_laggard,
    ),
    ("dev_stress_claim_kill_soak_10k", claim_kill_soak_10k),
    (
        "dev_stress_claim_release_syscall_soak",
        claim_release_syscall_soak,
    ),
];
