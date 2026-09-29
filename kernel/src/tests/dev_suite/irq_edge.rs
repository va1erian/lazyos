//! Edge cases of interrupt delivery (issue #240): kernel handlers, the polling
//! fallback, dead and full endpoints, and the wiring/INTx invariants.

use super::fixture::*;
use super::irq::rig;
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::{OP_IRQ_ACK, OP_IRQ_ENABLE};
use crate::dev::{intx, irq};
use core::sync::atomic::{AtomicU32, Ordering};

static KERNEL_HITS: AtomicU32 = AtomicU32::new(0);

fn kernel_handler(_line: u8) {
    KERNEL_HITS.fetch_add(1, Ordering::SeqCst);
}

/// A kernel driver's plain `fn(line)` runs instead of the message path, the
/// line stays unmasked, and the two tiers cannot share a line.
pub fn irq_kernel_handler() -> Result<(), String> {
    let fx = Fixture::new()?;
    KERNEL_HITS.store(0, Ordering::SeqCst);
    check!(
        irq::register_kernel(LINE_B, kernel_handler).is_ok(),
        "register failed"
    );
    check!(
        irq::register_kernel(LINE_B, kernel_handler) == Err(EBUSY),
        "a second kernel handler was accepted"
    );
    check!(!masked(LINE_B), "registering did not unmask the line");
    irq::dispatch(LINE_B);
    check!(
        KERNEL_HITS.load(Ordering::SeqCst) == 1,
        "the handler did not run"
    );
    check!(!masked(LINE_B), "dispatch masked a kernel-owned line");
    check!(
        irq::take_raised() == 0,
        "a kernel-owned line was queued for messages"
    );

    // A userspace claimant cannot arm a kernel-owned line.
    let dev = add_device(Spec::nic(Some(LINE_B)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    let handle = expect_ok(claim_irq(dev, endpoint, false), "claim")?;
    expect_errno(
        sys(OP_IRQ_ENABLE, handle, 0, 0, 0),
        EBUSY,
        "arm a kernel line",
    )?;
    leave(&fx);

    for line in [0u8, 1, 2, 12, 16, 200] {
        check!(
            irq::register_kernel(line, kernel_handler) == Err(EINVAL),
            "registered on unroutable line {line}"
        );
    }
    irq::unregister_kernel(LINE_B);
    check!(masked(LINE_B), "unregistering left the line unmasked");
    // The reverse: an armed claimant blocks a kernel handler.
    enter(slot)?;
    expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "arm after unregister")?;
    leave(&fx);
    check!(
        irq::register_kernel(LINE_B, kernel_handler) == Err(EBUSY),
        "a kernel handler took a line a claimant armed"
    );
    drop(fx);
    Ok(())
}

/// A line the PIC cannot deliver (reserved, or "not connected") falls back to
/// polling: the claim succeeds, `irq_enable` says `ENOSYS`, and nothing is
/// armed or unmasked.
pub fn irq_polling_fallback() -> Result<(), String> {
    let fx = Fixture::new()?;
    let mouse_mask = pic::is_masked(12);
    let unrouted = rig(12, false, false)?;
    expect_errno(
        sys(OP_IRQ_ENABLE, unrouted.handle, 0, 0, 0),
        ENOSYS,
        "irq_enable on the mouse line",
    )?;
    check!(
        unrouted.flags()?.0 == false,
        "an unroutable claim was armed"
    );
    check!(
        pic::is_masked(12) == mouse_mask,
        "the fallback touched the mouse line"
    );

    // A function whose Interrupt Line is 0xFF has no interrupt right at all.
    let none = add_device(Spec::nic(Some(0xFF)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    expect_errno(
        claim_irq(none, endpoint, false),
        EPERM,
        "endpoint without an irq right",
    )?;
    check!(
        table_state(none).0.is_none(),
        "the refused claim left an owner"
    );
    let handle = expect_ok(claim_plain(none), "polling claim")?;
    // No interrupt right at all, so the handle cannot even ask.
    expect_errno(
        sys(OP_IRQ_ENABLE, handle, 0, 0, 0),
        EPERM,
        "irq_enable without an irq right",
    )?;
    // With the right but no endpoint there is nowhere to deliver to.
    let pollable = add_device(Spec::nic(Some(LINE_C)))?;
    let polled = expect_ok(claim_plain(pollable), "polling claim with an irq right")?;
    expect_errno(
        sys(OP_IRQ_ENABLE, polled, 0, 0, 0),
        EINVAL,
        "irq_enable without endpoint",
    )?;

    // Index 0 is the only interrupt.
    let handle = unrouted.handle;
    enter(unrouted.slot)?;
    expect_errno(
        sys(OP_IRQ_ENABLE, handle, 1, 0, 0),
        EINVAL,
        "irq_enable index 1",
    )?;
    expect_errno(sys(OP_IRQ_ACK, handle, 1, 0, 0), EINVAL, "irq_ack index 1")?;
    drop(fx);
    Ok(())
}

/// An endpoint whose channel is gone cannot be notified: the claim is disarmed,
/// the line is not left masked, and nothing is left waiting.
pub fn irq_dead_endpoint() -> Result<(), String> {
    let fx = Fixture::new()?;
    let timeouts_before = intx::counters().1;
    let r = rig(LINE_A, false, true)?;
    enter(r.slot)?;
    channels::close_endpoint(r.endpoint).map_err(|e| e.message().to_string())?;
    channels::close_endpoint(r.peer).map_err(|e| e.message().to_string())?;
    fire(LINE_A, 5);
    check!(
        r.flags()? == (false, false, false),
        "dead endpoint left {:?}",
        r.flags()?
    );
    check!(
        masked(LINE_A),
        "a line with no armed claimant was left unmasked"
    );
    intx::service_at(10_000);
    check!(
        intx::counters().1 == timeouts_before,
        "a dead endpoint produced a timeout"
    );
    enter(r.slot)?;
    expect_errno(r.ack(), EINVAL, "ack after a failed post")?;
    drop(fx);
    Ok(())
}

/// A full inbox refuses the message; the interrupt is remembered and re-posted
/// once the driver makes room, instead of being lost.
pub fn irq_queue_full_retry() -> Result<(), String> {
    let fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    enter(r.slot)?;
    let mut stuffed = 0;
    while stuff_inbox(r.peer).is_ok() {
        stuffed += 1;
        check!(stuffed <= 1024, "the inbox never filled");
    }
    fire(LINE_A, 1);
    check!(
        r.flags()? == (true, false, true),
        "full inbox left {:?}",
        r.flags()?
    );
    check!(!masked(LINE_A), "a failed post left the line masked");
    let mut drained = 0;
    while channels::try_recv(r.endpoint)
        .map_err(|e| e.message().to_string())?
        .is_some()
    {
        drained += 1;
    }
    check!(drained == stuffed, "drained {drained} of {stuffed}");
    intx::service_at(2);
    check!(
        r.queued()? == 1,
        "the retry was not posted after room appeared"
    );
    let (sender, ..) = take_irq(r.endpoint)?;
    check!(sender == task::KERNEL_TASK, "retry came from slot {sender}");
    drop(fx);
    Ok(())
}

/// The interrupt resource mirrors the wiring: a PCI function has an `Irq` exactly
/// when it uses an INTx pin, and in-kernel drivers, which only poll, leave their
/// function's INTx disabled so it cannot hold a line a userspace driver shares.
pub fn irq_enumeration_matches_wiring() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let devices: Vec<DeviceInfo> = crate::dev::table().lock().iter().collect();
    for info in devices {
        let BusId::Pci(address) = info.bus else {
            continue;
        };
        let pin = pci::interrupt_pin(address);
        check!(
            (pin != 0) == info.resources.irq().is_some(),
            "{:04x}:{:04x} pin {pin} but irq {:?}",
            info.vendor,
            info.device,
            info.resources.irq()
        );
        let owner = crate::dev::table().lock().owner(info.id);
        if owner == Some(crate::dev::TaskSlot::KERNEL) {
            check!(
                pci::command(address) & pci::COMMAND_INTX_DISABLE != 0,
                "kernel-driven {:04x}:{:04x} may still assert INTx",
                info.vendor,
                info.device
            );
        }
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_irq_kernel_handler", irq_kernel_handler),
    ("dev_irq_polling_fallback", irq_polling_fallback),
    ("dev_irq_dead_endpoint", irq_dead_endpoint),
    ("dev_irq_queue_full_retry", irq_queue_full_retry),
    (
        "dev_irq_enumeration_matches_wiring",
        irq_enumeration_matches_wiring,
    ),
];
