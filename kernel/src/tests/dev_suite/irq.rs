//! Interrupt delivery to a single claimant (issue #240, driver-plan D2):
//! raise/ack ordering, exclusive lines, unclaimed and spurious interrupts,
//! kernel handlers, the polling fallback, and endpoint failure modes.
//!
//! The ISR half is `irq::dispatch(line)` called directly (the suite runs with
//! interrupts off); the bottom half is `intx::service_at(now)` with an explicit
//! clock, so deadlines are deterministic.

use super::fixture::*;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::dev::errno::*;
use crate::dev::syscall::{OP_IRQ_ACK, OP_IRQ_ENABLE, OP_RELEASE};
use crate::dev::{intx, irq};
use crate::quota;

/// One driver task that claimed one synthetic device with an interrupt
/// endpoint.
pub struct Rig {
    pub slot: usize,
    pub dev: DeviceId,
    pub handle: u64,
    pub endpoint: u64,
    pub peer: u64,
}

/// Spawn a driver and claim a fresh NIC-like device on `line`.
pub fn rig(line: u8, shared: bool, arm: bool) -> Result<Rig, String> {
    let dev = add_device(Spec::nic(Some(line)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (endpoint, peer) = irq_channel()?;
    let handle = expect_ok(claim_irq(dev, endpoint, shared), "claim")?;
    if arm {
        expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
    }
    Ok(Rig {
        slot,
        dev,
        handle,
        endpoint,
        peer,
    })
}

impl Rig {
    pub fn ack(&self) -> i64 {
        sys(OP_IRQ_ACK, self.handle, 0, 0, 0)
    }

    /// Messages waiting for this driver (switches into its task).
    pub fn queued(&self) -> Result<u64, String> {
        enter(self.slot)?;
        queued(self.endpoint)
    }

    /// `(armed, pending, missed)` from the claim table.
    pub fn flags(&self) -> Result<(bool, bool, bool), String> {
        let claims = CLAIMS.lock();
        let claim = claims.get(self.dev).ok_or("the claim is gone")?;
        Ok((claim.armed, claim.pending, claim.missed))
    }
}

/// The ISR half masks and defers; the bottom half posts exactly one message
/// from the kernel identity; the line stays masked until the ack; a second
/// interrupt while the ack is owed posts nothing (one outstanding per claim).
pub fn irq_raise_ack_ordering() -> Result<(), String> {
    let fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    check!(!masked(LINE_A), "irq_enable did not unmask the line");

    irq::dispatch(LINE_A);
    check!(masked(LINE_A), "the ISR half left the line unmasked");
    check!(r.queued()? == 0, "a message was posted from the ISR half");

    intx::service_at(1000);
    check!(
        r.queued()? == 1,
        "the bottom half posted {} messages",
        r.queued()?
    );
    check!(masked(LINE_A), "the line unmasked before the ack");
    let (sender, dev, index, generation) = take_irq(r.endpoint)?;
    check!(
        sender == task::KERNEL_TASK,
        "the message came from slot {sender}, not the kernel"
    );
    let (_, table_generation) = table_state(r.dev);
    check!(
        dev == u32::from(r.dev.0) && index == 0 && generation == table_generation,
        "message body ({dev}, {index}, {generation}) does not name the claim"
    );

    // Consuming the message does not clear the debt: only the ack does.
    fire(LINE_A, 1001);
    check!(
        r.queued()? == 0,
        "a second message was posted while an ack was owed"
    );
    check!(
        r.flags()? == (true, true, true),
        "state after a missed interrupt: {:?}",
        r.flags()?
    );
    check!(masked(LINE_A), "the line unmasked while the round is open");

    expect_ok(r.ack(), "ack")?;
    check!(
        r.queued()? == 1,
        "the missed interrupt was not re-posted after the ack"
    );
    check!(
        !masked(LINE_A),
        "the line stayed masked after the round ended"
    );
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "second ack")?;
    check!(
        r.flags()? == (true, false, false),
        "not idle after the last ack"
    );
    expect_errno(r.ack(), EINVAL, "an ack with nothing owed")?;
    drop(fx);
    Ok(())
}

/// Two exclusive claimants on one line: the second fails `EBUSY` and leaves no
/// owner, charge, or handle; sharing needs every claimant to opt in; a claim
/// with no endpoint (a polling driver) does not occupy the line.
pub fn irq_exclusive_line_ebusy() -> Result<(), String> {
    let fx = Fixture::new()?;
    let first = rig(LINE_A, false, false)?;
    let second_dev = add_device(Spec::nic(Some(LINE_A)))?;
    let other = spawn_driver(driver_cred())?;
    enter(other)?;
    let (endpoint, _peer) = irq_channel()?;
    let handles_before = handles::count_for_task(other);
    let claims_before = usage(quota::Resource::DeviceClaims);

    expect_errno(
        claim_irq(second_dev, endpoint, false),
        EBUSY,
        "exclusive on exclusive",
    )?;
    expect_errno(
        claim_irq(second_dev, endpoint, true),
        EBUSY,
        "shared on exclusive",
    )?;
    check!(
        table_state(second_dev).0.is_none(),
        "a refused claim left an owner"
    );
    check!(
        handles::count_for_task(other) == handles_before,
        "a refused claim leaked a handle"
    );
    check!(
        usage(quota::Resource::DeviceClaims) == claims_before,
        "a refused claim leaked a quota charge"
    );
    // A polling claimant does not occupy the line.
    let polling = expect_ok(claim_plain(second_dev), "polling claim on an occupied line")?;
    expect_ok(sys(OP_RELEASE, polling, 0, 0, 0), "release polling claim")?;

    // Once the first claim is gone the line is free again.
    enter(first.slot)?;
    expect_ok(sys(OP_RELEASE, first.handle, 0, 0, 0), "release")?;
    enter(other)?;
    expect_ok(
        claim_irq(second_dev, endpoint, false),
        "claim after release",
    )?;
    drop(fx);
    Ok(())
}

/// A shared line needs every claimant to have opted in.
pub fn irq_shared_needs_all_opt_in() -> Result<(), String> {
    let fx = Fixture::new()?;
    let _a = rig(LINE_A, true, false)?;
    let device = add_device(Spec::nic(Some(LINE_A)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    expect_errno(
        claim_irq(device, endpoint, false),
        EBUSY,
        "exclusive on shared",
    )?;
    expect_ok(claim_irq(device, endpoint, true), "shared on shared")?;
    // Sharing without an endpoint is meaningless.
    let lone = add_device(Spec::nic(Some(LINE_A)))?;
    expect_errno(
        claim_irq(lone, NO_ENDPOINT, true),
        EINVAL,
        "shared without endpoint",
    )?;
    drop(fx);
    Ok(())
}

/// An interrupt on a line nobody claimed is masked and counted, never
/// delivered, and cannot storm; the kernel's own lines are never touched.
pub fn irq_unclaimed_line_safe() -> Result<(), String> {
    let fx = Fixture::new()?;
    let before = irq::stats();
    irq::dispatch(LINE_C);
    check!(masked(LINE_C), "an unclaimed line was left unmasked");
    intx::service_at(10);
    check!(masked(LINE_C), "the bottom half unmasked an unclaimed line");
    let after = irq::stats();
    check!(
        after.stray == before.stray + 1 && after.raised == before.raised + 1,
        "stray accounting: {before:?} -> {after:?}"
    );
    check!(
        irq::take_raised() == 0,
        "the raised bit survived the bottom half"
    );

    // A claim that never armed the line does not listen either.
    let r = rig(LINE_B, false, false)?;
    fire(LINE_B, 20);
    check!(r.queued()? == 0, "an unarmed claim was sent an interrupt");
    check!(masked(LINE_B), "an unarmed claim's line was unmasked");

    // Timer, keyboard, cascade and mouse are not the device core's to mask.
    let lines = [0u8, 1, 2, 12];
    let states = lines.map(pic::is_masked);
    for line in lines {
        irq::dispatch(line);
    }
    irq::dispatch(16);
    irq::dispatch(255);
    check!(
        lines.map(pic::is_masked) == states,
        "a reserved line's mask changed"
    );
    check!(
        irq::take_raised() == 0,
        "a reserved line was queued for delivery"
    );
    drop(fx);
    Ok(())
}

/// IRQ 7 and 15 fire spuriously when a request vanishes mid-acknowledge; the
/// handler must swallow them without masking, queueing or delivering.
pub fn irq_spurious_7_and_15() -> Result<(), String> {
    let fx = Fixture::new()?;
    let masks = [7u8, 15].map(pic::is_masked);
    // A claimant armed on line 7 must not hear a spurious interrupt either.
    let r = rig(7, false, true)?;
    let before = irq::stats();
    irq::dispatch(7);
    irq::dispatch(15);
    let after = irq::stats();
    check!(
        after.spurious == before.spurious + 2 && after.raised == before.raised,
        "spurious accounting: {before:?} -> {after:?}"
    );
    check!(irq::take_raised() == 0, "a spurious interrupt was queued");
    intx::service_at(1);
    check!(r.queued()? == 0, "a spurious interrupt was delivered");
    check!(!masked(7), "a spurious interrupt masked the armed line");
    drop(fx);
    check!(pic::is_masked(7) == masks[0], "line 7 mask not restored");
    check!(pic::is_masked(15) == masks[1], "line 15 mask changed");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_irq_raise_ack_ordering", irq_raise_ack_ordering),
    ("dev_irq_exclusive_line_ebusy", irq_exclusive_line_ebusy),
    (
        "dev_irq_shared_needs_all_opt_in",
        irq_shared_needs_all_opt_in,
    ),
    ("dev_irq_unclaimed_line_safe", irq_unclaimed_line_safe),
    ("dev_irq_spurious_7_and_15", irq_spurious_7_and_15),
];
