//! The shared-INTx contract (issue #240, driver-plan section 3.3): one message
//! per armed claimant, unmask only after every notified claimant acked, a
//! bounded ack deadline, and the late-ack recovery rules with the `missed` bit.

use super::fixture::*;
use super::irq::{rig, Rig};
use super::*;
use crate::dev::class::method;
use crate::dev::intx::{self, ACK_DEADLINE_TICKS};
use crate::dev::report::{self, reason};
use crate::dev::syscall::OP_IRQ_ENABLE;

const T0: u64 = 1000;

/// Build claimants on one shared line, as separate driver tasks.
fn claimants(line: u8, count: usize, armed: usize) -> Result<Vec<Rig>, String> {
    let mut rigs = Vec::new();
    for index in 0..count {
        rigs.push(rig(line, true, index < armed)?);
    }
    leave_all();
    Ok(rigs)
}

fn leave_all() {
    task::harness::switch_current(task::KERNEL_TASK);
}

/// Take the pending message and ack it, as driver `r`.
fn service_and_ack(r: &Rig) -> Result<(), String> {
    enter(r.slot)?;
    take_irq(r.endpoint)?;
    expect_ok(r.ack(), "ack")?;
    Ok(())
}

/// Newest audit record about `dev` with `method`, if any.
fn latest_record(dev: DeviceId, method: u32) -> Option<audit::AuditEvent> {
    audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| event.method == method && report::device_of(event.txn_id) == Some(dev))
}

/// One interrupt reaches every armed claimant exactly once and none of the
/// others; the line unmasks only after the last of them acked.
pub fn irq_shared_one_message_per_armed_claimant() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 3, 2)?;
    check!(!masked(LINE_A), "arming left the line masked");
    fire(LINE_A, T0);
    check!(masked(LINE_A), "the line is not masked during the round");
    check!(
        rigs[0].queued()? == 1,
        "claimant 0 got {} messages",
        rigs[0].queued()?
    );
    check!(
        rigs[1].queued()? == 1,
        "claimant 1 got {} messages",
        rigs[1].queued()?
    );
    check!(rigs[2].queued()? == 0, "an unarmed claimant was notified");

    service_and_ack(&rigs[0])?;
    check!(
        masked(LINE_A),
        "the line unmasked while claimant 1 owes an ack"
    );
    service_and_ack(&rigs[1])?;
    check!(!masked(LINE_A), "the line stayed masked after every ack");
    check!(!rigs[2].flags()?.1, "the unarmed claimant is owed an ack");

    // A second interrupt starts a fresh round for both.
    fire(LINE_A, T0 + 1);
    check!(
        rigs[0].queued()? == 1 && rigs[1].queued()? == 1,
        "the next round did not notify both claimants"
    );
    Ok(())
}

/// A claimant that misses its deadline is dropped from the round: the line is
/// released for the others, the laggard is audited, and its queue stays at one.
pub fn irq_shared_deadline_drops_laggard() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 2, 2)?;
    let timeouts_before = intx::counters().1;
    fire(LINE_A, T0);
    service_and_ack(&rigs[0])?;

    intx::service_at(T0 + ACK_DEADLINE_TICKS - 1);
    check!(masked(LINE_A), "the line unmasked before the deadline");
    check!(
        intx::counters().1 == timeouts_before,
        "a timeout fired early"
    );

    intx::service_at(T0 + ACK_DEADLINE_TICKS);
    check!(!masked(LINE_A), "the line stayed masked past the deadline");
    check!(
        intx::counters().1 == timeouts_before + 1,
        "expected exactly one timeout"
    );
    let record = latest_record(rigs[1].dev, method::IRQ_TIMEOUT)
        .ok_or("the laggard's timeout was not audited")?;
    check!(
        !record.allow
            && record.reason_code == reason::IRQ_TIMEOUT
            && record.actor_slot == rigs[1].slot,
        "timeout record is {record:?}"
    );
    check!(
        rigs[1].flags()? == (true, true, false),
        "the laggard should stay owed an ack: {:?}",
        rigs[1].flags()?
    );

    // While it is owed, later interrupts do not grow its queue.
    rigs[1].queued()?;
    for round in 0..50u64 {
        fire(LINE_A, T0 + 200 + round);
        service_and_ack(&rigs[0])?;
    }
    check!(
        rigs[1].queued()? == 1,
        "the laggard's queue grew to {}",
        rigs[1].queued()?
    );
    Ok(())
}

/// Late-ack recovery: an ack after the drop makes the claim eligible again;
/// if it missed an interrupt meanwhile it is re-posted at once; and the late
/// ack never unmasks the line for the claimants of a newer round.
pub fn irq_shared_late_ack_recovery() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 2, 2)?;
    fire(LINE_A, T0);
    service_and_ack(&rigs[0])?;
    intx::service_at(T0 + ACK_DEADLINE_TICKS);
    enter(rigs[1].slot)?;
    take_irq(rigs[1].endpoint)?; // drained, but still owed an ack

    // A new interrupt: claimant 0 is notified, claimant 1 only marked missed.
    fire(LINE_A, T0 + 200);
    check!(
        rigs[1].queued()? == 0,
        "an owed claimant was sent a second message"
    );
    check!(
        rigs[1].flags()?.2,
        "the owed claimant's miss was not recorded"
    );
    check!(masked(LINE_A), "the new round is not holding the line");

    // Its late ack must not unmask the line: claimant 0 is still in the round.
    enter(rigs[1].slot)?;
    expect_ok(rigs[1].ack(), "late ack")?;
    check!(
        masked(LINE_A),
        "a late ack unmasked a line another claimant holds"
    );
    check!(
        rigs[1].queued()? == 1,
        "the missed interrupt was not re-posted after the late ack"
    );
    check!(
        rigs[1].flags()? == (true, true, false),
        "state {:?}",
        rigs[1].flags()?
    );

    service_and_ack(&rigs[0])?;
    check!(
        !masked(LINE_A),
        "the round did not end when its only waiter acked"
    );
    // The re-posted message's ack is outside any round: no effect on the line.
    service_and_ack(&rigs[1])?;
    check!(!masked(LINE_A), "a recovery ack changed the line");
    Ok(())
}

/// A late ack with no interrupt missed posts nothing and restores eligibility.
pub fn irq_shared_late_ack_without_miss() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 2, 2)?;
    fire(LINE_A, T0);
    service_and_ack(&rigs[0])?;
    intx::service_at(T0 + ACK_DEADLINE_TICKS);
    enter(rigs[1].slot)?;
    take_irq(rigs[1].endpoint)?;
    expect_ok(rigs[1].ack(), "late ack")?;
    check!(
        rigs[1].queued()? == 0,
        "a late ack with no miss posted a message"
    );
    check!(!masked(LINE_A), "a late ack changed the line");
    check!(
        rigs[1].flags()? == (true, false, false),
        "state {:?}",
        rigs[1].flags()?
    );

    fire(LINE_A, T0 + 300);
    check!(
        rigs[1].queued()? == 1,
        "the recovered claimant was not notified again"
    );
    Ok(())
}

/// When every claimant is a laggard the line is still released, and their
/// queues stay at one.
pub fn irq_shared_all_laggards_release_line() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 2, 2)?;
    fire(LINE_A, T0);
    intx::service_at(T0 + ACK_DEADLINE_TICKS);
    check!(!masked(LINE_A), "a round of laggards kept the line masked");
    fire(LINE_A, T0 + 200);
    check!(!masked(LINE_A), "all-owed interrupt left the line masked");
    check!(
        rigs[0].queued()? == 1 && rigs[1].queued()? == 1,
        "laggard queues: {} and {}",
        rigs[0].queued()?,
        rigs[1].queued()?
    );
    Ok(())
}

/// A claimant that arms during a round is not waited for and does not unmask
/// the line under the others.
pub fn irq_shared_arm_during_round() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let rigs = claimants(LINE_A, 2, 1)?;
    fire(LINE_A, T0);
    check!(masked(LINE_A), "round not open");
    enter(rigs[1].slot)?;
    expect_ok(
        sys(OP_IRQ_ENABLE, rigs[1].handle, 0, 0, 0),
        "late irq_enable",
    )?;
    check!(
        masked(LINE_A),
        "arming a claimant unmasked a line held by a round"
    );
    check!(
        rigs[1].queued()? == 0,
        "a claimant armed mid-round got a message"
    );
    service_and_ack(&rigs[0])?;
    check!(!masked(LINE_A), "the round did not end");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_irq_shared_one_message_per_armed_claimant",
        irq_shared_one_message_per_armed_claimant,
    ),
    (
        "dev_irq_shared_deadline_drops_laggard",
        irq_shared_deadline_drops_laggard,
    ),
    (
        "dev_irq_shared_late_ack_recovery",
        irq_shared_late_ack_recovery,
    ),
    (
        "dev_irq_shared_late_ack_without_miss",
        irq_shared_late_ack_without_miss,
    ),
    (
        "dev_irq_shared_all_laggards_release_line",
        irq_shared_all_laggards_release_line,
    ),
    (
        "dev_irq_shared_arm_during_round",
        irq_shared_arm_during_round,
    ),
];
