//! Message-signalled interrupts (issue #616): the same contract as INTx
//! (`irq.rs`) on a vector of the claim's own.
//!
//! The synthetic devices carry an MSI capability at [`GHOST`], so the
//! kernel's capability programming goes nowhere; the vector handler is
//! `msi::dispatch(index)` called directly (interrupts are off) and the bottom
//! half `intx::service_at(now)`, as for a line. Without message interrupts
//! (`LAZYOS_MSI=0`, or no local APIC) the cases check the INTx fallback.

use super::fixture::*;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::dev::errno::*;
use crate::dev::msi;
use crate::dev::syscall::{OP_IRQ_ACK, OP_IRQ_ENABLE, OP_RELEASE};
use crate::dev::Msi;

/// `irq_enable`'s answer for a claim on a vector.
const MODE_MSI: u64 = 1;

/// A capability with per-vector masking.
const CAP: Msi = Msi {
    cap: 0x50,
    is_64: true,
    maskable: true,
};

/// A synthetic NIC with an MSI capability and INTx on `line` (or none).
pub fn msi_device(line: Option<u8>) -> Result<DeviceId, String> {
    let mut spec = Spec::nic(line);
    spec.msi = Some(CAP);
    add_device(spec)
}

/// One driver with one armed MSI claim.
pub struct MsiRig {
    pub slot: usize,
    pub dev: DeviceId,
    pub handle: u64,
    pub endpoint: u64,
    pub peer: u64,
    pub index: u8,
}

impl MsiRig {
    /// Claim a fresh MSI device in a new driver task and arm it.
    pub fn new(line: Option<u8>) -> Result<MsiRig, String> {
        MsiRig::in_task(spawn_driver(driver_cred())?, line)
    }

    /// The same, in the existing driver task `slot`.
    pub fn in_task(slot: usize, line: Option<u8>) -> Result<MsiRig, String> {
        MsiRig::claim(slot, msi_device(line)?)
    }

    /// Claim and arm the existing MSI device `dev` in driver task `slot`.
    pub fn claim(slot: usize, dev: DeviceId) -> Result<MsiRig, String> {
        enter(slot)?;
        let (endpoint, peer) = irq_channel()?;
        let handle = expect_ok(claim_irq(dev, endpoint, false), "claim")?;
        let mode = expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
        check!(mode == MODE_MSI, "irq_enable answered mode {mode}, not MSI");
        let index = vector_of(dev)?;
        Ok(MsiRig {
            slot,
            dev,
            handle,
            endpoint,
            peer,
            index,
        })
    }

    pub fn ack(&self) -> i64 {
        enter(self.slot).ok();
        sys(OP_IRQ_ACK, self.handle, 0, 0, 0)
    }

    pub fn queued(&self) -> Result<u64, String> {
        enter(self.slot)?;
        queued(self.endpoint)
    }
}

/// The MSI vector index claim `dev` was routed to.
pub fn vector_of(dev: DeviceId) -> Result<u8, String> {
    CLAIMS
        .lock()
        .get(dev)
        .and_then(|claim| claim.msi)
        .ok_or_else(|| "the claim has no MSI vector".to_string())
}

/// Whether this boot takes message interrupts; logs why not.
pub fn available(test: &str) -> bool {
    let on = msi::enabled();
    if !on {
        serial_println!("TEST:{test}:INFO:message interrupts are off (LAZYOS_MSI=0 or no APIC)");
    }
    on
}

/// The vector handler masks and defers; the bottom half posts one message; a
/// message while the ack is owed is latched, not delivered, and becomes one
/// fresh message after the ack.
pub fn msi_raise_ack_ordering() -> Result<(), String> {
    let fx = Fixture::new()?;
    if !available("dev_msi_raise_ack_ordering") {
        return Ok(());
    }
    let r = MsiRig::new(None)?;
    check!(
        !msi::is_masked(r.index),
        "irq_enable left the vector masked"
    );

    msi::dispatch(r.index);
    check!(
        msi::is_masked(r.index),
        "the handler left the vector unmasked"
    );
    check!(r.queued()? == 0, "a message was posted from the handler");
    intx::service_at(1000);
    check!(r.queued()? == 1, "the bottom half posted {}", r.queued()?);
    let (sender, dev, _, _) = take_irq(r.endpoint)?;
    check!(
        sender == task::KERNEL_TASK && dev == u32::from(r.dev.0),
        "message from {sender} for device {dev}"
    );

    let before = msi::stats();
    msi::dispatch(r.index);
    intx::service_at(1001);
    check!(r.queued()? == 0, "a second message while the ack is owed");
    check!(
        msi::stats().latched == before.latched + 1,
        "the masked vector did not latch: {before:?} -> {:?}",
        msi::stats()
    );
    expect_ok(r.ack(), "ack")?;
    check!(r.queued()? == 1, "the latched message was not delivered");
    check!(
        msi::is_masked(r.index),
        "the vector unmasked with a round open"
    );
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "second ack")?;
    check!(
        !msi::is_masked(r.index),
        "the vector stayed masked when idle"
    );
    expect_errno(r.ack(), EINVAL, "an ack with nothing owed")?;
    drop(fx);
    Ok(())
}

/// A storm on one vector: the queue never holds more than one message and
/// exactly one more follows the ack.
pub fn msi_storm_queue_depth_one() -> Result<(), String> {
    let fx = Fixture::new()?;
    if !available("dev_msi_storm_queue_depth_one") {
        return Ok(());
    }
    let r = MsiRig::new(Some(LINE_A))?;
    for round in 0..10_000u64 {
        msi::dispatch(r.index);
        if round % 97 == 0 {
            intx::service_at(round);
        }
    }
    intx::service_at(10_000);
    check!(
        r.queued()? == 1,
        "{} messages queued in a storm",
        r.queued()?
    );
    check!(masked(LINE_A), "the INTx line was armed on an MSI claim");
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "ack")?;
    check!(r.queued()? == 1, "the storm left {} owed", r.queued()?);
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "ack")?;
    check!(r.queued()? == 0, "messages kept coming after the storm");
    drop(fx);
    Ok(())
}

/// A vector in flight when its claim goes (release, or the driver dying):
/// nothing is delivered, the vector is free, and a late message on it is
/// spurious.
pub fn msi_teardown_vector_in_flight() -> Result<(), String> {
    let fx = Fixture::new()?;
    if !available("dev_msi_teardown_vector_in_flight") {
        return Ok(());
    }
    let in_use = msi::in_use();
    for crash in [false, true] {
        let r = MsiRig::new(None)?;
        msi::dispatch(r.index);
        if crash {
            crate::dev::note_task_exited(r.slot);
            crate::dev::silence_exited();
            check!(
                msi::in_use() == in_use,
                "a dead driver's vector stayed allocated"
            );
            leave(&fx);
            crate::dev::teardown_task(r.slot, 0);
        } else {
            expect_ok(sys(OP_RELEASE, r.handle, 0, 0, 0), "release")?;
        }
        check!(msi::in_use() == in_use, "the vector leaked (crash={crash})");
        intx::service_at(5);
        check!(claim_count() == 0, "a claim survived (crash={crash})");
        let before = msi::stats();
        msi::dispatch(r.index);
        intx::service_at(6);
        check!(
            msi::stats().spurious == before.spurious + 1,
            "a message on a freed vector was not spurious"
        );
    }
    drop(fx);
    Ok(())
}

/// Messages on vectors nobody owns, and indices past the pool, are counted
/// and dropped.
pub fn msi_spurious_vectors() -> Result<(), String> {
    let fx = Fixture::new()?;
    let before = (msi::stats(), crate::dev::irq::stats());
    for index in 0..msi::VECTORS {
        msi::dispatch(index);
    }
    msi::dispatch(msi::VECTORS);
    msi::dispatch(u8::MAX);
    let after = (msi::stats(), crate::dev::irq::stats());
    check!(
        after.0.spurious == before.0.spurious + u32::from(msi::VECTORS),
        "spurious accounting: {before:?} -> {after:?}"
    );
    check!(after.0.raised == before.0.raised, "a free vector raised");
    check!(
        !crate::dev::irq::raised_pending(),
        "a spurious message was queued"
    );
    drop(fx);
    Ok(())
}

/// With every vector taken, a claim falls back to its INTx line, and one
/// with neither is told to poll; freeing a vector lets the next claim have it.
pub fn msi_falls_back_to_intx() -> Result<(), String> {
    let fx = Fixture::new()?;
    if !available("dev_msi_falls_back_to_intx") {
        // Off: a device with both still works, on its line.
        let dev = msi_device(Some(LINE_A))?;
        let slot = spawn_driver(driver_cred())?;
        enter(slot)?;
        let (endpoint, _peer) = irq_channel()?;
        let handle = expect_ok(claim_irq(dev, endpoint, false), "claim")?;
        let mode = expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
        check!(mode == 0 && !masked(LINE_A), "MSI off but mode {mode}");
        return Ok(());
    }
    let slot = spawn_driver(driver_cred())?;
    let mut rigs = Vec::new();
    while msi::in_use() < usize::from(msi::VECTORS) {
        rigs.push(MsiRig::in_task(slot, None)?);
    }
    let both = msi_device(Some(LINE_B))?;
    let only = msi_device(None)?;
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    let handle = expect_ok(claim_irq(both, endpoint, false), "claim both")?;
    let mode = expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable both")?;
    check!(mode == 0, "no vector left, yet mode {mode}");
    check!(!masked(LINE_B), "the fallback line was not unmasked");
    let (endpoint2, _peer2) = irq_channel()?;
    let lone = expect_ok(claim_irq(only, endpoint2, false), "claim msi-only")?;
    expect_errno(
        sys(OP_IRQ_ENABLE, lone, 0, 0, 0),
        ENOSYS,
        "no vector and no line",
    )?;
    let freed = rigs.pop().ok_or("no rig")?;
    expect_ok(sys(OP_RELEASE, freed.handle, 0, 0, 0), "release one")?;
    let mode = expect_ok(sys(OP_IRQ_ENABLE, lone, 0, 0, 0), "irq_enable again")?;
    check!(
        mode == MODE_MSI,
        "a freed vector was not reused (mode {mode})"
    );
    drop(fx);
    Ok(())
}

/// Claim, arm, take interrupts and end the claim (release, or the driver
/// dying with a message in flight), 2000 times over: vectors never leak, the
/// task and its channels are reaped, every interrupt is delivered once.
pub fn msi_soak() -> Result<(), String> {
    let fx = Fixture::new()?;
    if !available("dev_msi_soak") {
        return Ok(());
    }
    let in_use = msi::in_use();
    let delivered = intx::counters().0;
    let mut expected = 0;
    let dev = msi_device(Some(LINE_C))?;
    for cycle in 0..2_000u64 {
        let slot = spawn_driver(driver_cred())?;
        let r = MsiRig::claim(slot, dev)?;
        for _ in 0..(cycle % 5) + 1 {
            msi::dispatch(r.index);
            intx::service_at(cycle);
            check!(r.queued()? == 1, "cycle {cycle}: not exactly one message");
            take_irq(r.endpoint)?;
            expect_ok(r.ack(), "ack")?;
            expected += 1;
        }
        if cycle % 3 == 0 {
            // Left in flight for the teardown to drop.
            msi::dispatch(r.index);
        }
        if cycle % 2 == 0 {
            expect_ok(sys(OP_RELEASE, r.handle, 0, 0, 0), "release")?;
        }
        leave(&fx);
        task::harness::finish(slot, 0);
        check!(
            task::reap_child().map(|reaped| reaped.0) == Some(slot),
            "cycle {cycle}: the driver was not reaped"
        );
        check!(msi::in_use() == in_use, "cycle {cycle}: a vector leaked");
        check!(claim_count() == 0, "cycle {cycle}: a claim survived");
        check!(masked(LINE_C), "cycle {cycle}: the INTx line was unmasked");
    }
    intx::service_at(1_000_000);
    check!(
        intx::counters().0 - delivered == expected,
        "{} messages for {expected} interrupts",
        intx::counters().0 - delivered
    );
    drop(fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_msi_raise_ack_ordering", msi_raise_ack_ordering),
    ("dev_msi_storm_queue_depth_one", msi_storm_queue_depth_one),
    (
        "dev_msi_teardown_vector_in_flight",
        msi_teardown_vector_in_flight,
    ),
    ("dev_msi_spurious_vectors", msi_spurious_vectors),
    ("dev_msi_falls_back_to_intx", msi_falls_back_to_intx),
    ("dev_msi_soak", msi_soak),
];
