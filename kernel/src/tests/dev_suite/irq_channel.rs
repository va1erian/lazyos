//! The kernel-made interrupt channel (issue #496).
//!
//! A driver no longer names the channel side the kernel posts into: `claim`
//! with `KERNEL_CHANNEL` makes a fresh channel whose sending side only the
//! kernel holds and hands the claimant a receive-only handle. These tests
//! check that a caller-named side (private or not) is refused, that the
//! receive handle cannot be spread, sent into, or published, that a failed
//! claim leaves no channel behind, and that releasing the claim ends the
//! channel for its reader. The soak claims and releases with interrupts in
//! flight many times over and must leak nothing.

use super::fixture::*;
use super::irq::rig;
use super::*;
use crate::dev::class::method;
use crate::dev::errno::*;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::ipc::handles::{self, rights};
use crate::quota::Resource;

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_irq_channel_caller_side_refused", caller_side_refused),
    ("dev_irq_channel_receive_only", receive_only),
    (
        "dev_irq_channel_bad_out_pointer_leaves_nothing",
        bad_out_pointer_leaves_nothing,
    ),
    ("dev_irq_channel_release_ends_it", release_ends_it),
    ("dev_irq_channel_soak_no_leaks", soak_no_leaks),
];

fn text(error: crate::ipc::channels::Error) -> String {
    error.message().to_string()
}

/// A claim naming any side of its own (a fresh private pair included) is
/// refused with `EBADF` and audited; nothing is owned afterwards.
pub fn caller_side_refused() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (mine, _peer) = irq_channel()?;
    for (what, endpoint) in [("a private pair", mine), ("handle 9999", 9999), ("zero", 0)] {
        expect_errno(sys(OP_CLAIM, u64::from(dev.0), endpoint, 0, 0), EBADF, what)?;
        check!(
            table_state(dev).0.is_none(),
            "{what}: a refused claim left an owner"
        );
    }
    let refused = audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| {
            event.method == method::CLAIM && report::device_of(event.txn_id) == Some(dev)
        });
    check!(
        refused.is_some_and(|event| !event.allow && event.reason_code == reason::BAD_ENDPOINT),
        "the refusal was not audited as BAD_ENDPOINT: {refused:?}"
    );
    check!(
        usage(Resource::DeviceClaims) == 0,
        "a refused claim was charged"
    );
    leave(&fx);
    Ok(())
}

/// The claimant's handle receives and waits, and nothing more: it cannot be
/// duplicated or transferred, nothing can be sent into the kernel's side, the
/// registry refuses to publish it, and the kernel's interrupt message reaches
/// it.
pub fn receive_only() -> Result<(), String> {
    let fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    enter(r.slot)?;
    let entry = handles::get(r.endpoint).map_err(|e| e.message().to_string())?;
    check!(
        entry.rights == rights::CALL,
        "the interrupt channel handle has rights {:#x}",
        entry.rights
    );
    check!(
        handles::duplicate(r.endpoint, rights::CALL).is_err(),
        "the interrupt channel was duplicated"
    );
    check!(
        crate::ipc::channels::is_irq_channel(entry.object_id),
        "the channel is not marked as an interrupt channel"
    );
    let sent = stuff_inbox_from_driver(r.endpoint);
    check!(
        sent == Err(crate::ipc::channels::Error::MissingRight),
        "a send into the kernel's side answered {sent:?}"
    );
    fire(LINE_A, 10);
    let (sender, dev, _, _) = take_irq(r.endpoint)?;
    check!(
        sender == task::KERNEL_TASK && dev == u32::from(r.dev.0),
        "the interrupt came from {sender} for device {dev}"
    );
    leave(&fx);
    Ok(())
}

/// A one-way message from the driver itself into its interrupt channel.
fn stuff_inbox_from_driver(endpoint: u64) -> Result<(), crate::ipc::channels::Error> {
    let mut body = libmessenger::Encoder::new();
    body.u32(1, 1)
        .map_err(|_| crate::ipc::channels::Error::BadParcel)?;
    let parcel = libmessenger::Parcel {
        header: libmessenger::Header {
            version: libmessenger::VERSION,
            flags: libmessenger::flags::ONE_WAY,
            interface_id: crate::dev::class::DEV_INTERFACE,
            method: method::IRQ,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel
        .encode(&mut bytes)
        .map_err(|_| crate::ipc::channels::Error::BadParcel)?;
    crate::ipc::channels::send(endpoint, &bytes)
}

/// `KERNEL_CHANNEL` with an unwritable out pointer fails with `EFAULT` and
/// undoes everything: no owner, no charge, no handle, no channel.
pub fn bad_out_pointer_leaves_nothing() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handles_before = handles::count_for_task(slot);
    let channels_before = crate::ipc::channels::counts().channels;
    let _strict = Strict::on();
    expect_errno(
        sys(OP_CLAIM, u64::from(dev.0), KERNEL_CHANNEL, 0, 0),
        EFAULT,
        "a null out pointer",
    )?;
    check!(
        table_state(dev).0.is_none(),
        "a faulted claim left an owner"
    );
    check!(
        usage(Resource::DeviceClaims) == 0,
        "a faulted claim was charged"
    );
    check!(
        handles::count_for_task(slot) == handles_before,
        "a faulted claim left a handle"
    );
    check!(
        crate::ipc::channels::counts().channels == channels_before,
        "a faulted claim left a channel"
    );
    leave(&fx);
    Ok(())
}

/// Releasing the claim closes the kernel's side: what was queued can still
/// be read, then the reader sees `PeerDied`; closing its handle frees the
/// channel.
pub fn release_ends_it() -> Result<(), String> {
    let fx = Fixture::new()?;
    let channels_before = crate::ipc::channels::counts().channels;
    let r = rig(LINE_A, false, true)?;
    fire(LINE_A, 10);
    enter(r.slot)?;
    expect_ok(sys(OP_RELEASE, r.handle, 0, 0, 0), "release")?;
    check!(
        crate::ipc::channels::try_recv(r.endpoint)
            .map_err(text)?
            .is_some(),
        "the queued interrupt was lost at release"
    );
    let after = crate::ipc::channels::try_recv(r.endpoint);
    check!(
        after == Err(crate::ipc::channels::Error::PeerDied),
        "after release the reader saw {:?}",
        after.map(|message| message.is_some())
    );
    crate::ipc::channels::close_endpoint(r.endpoint).map_err(text)?;
    check!(
        crate::ipc::channels::counts().channels == channels_before,
        "the interrupt channel outlived its claim and its reader"
    );
    leave(&fx);
    Ok(())
}

/// Soak: 300 rounds of claim with a kernel channel, arm, fire, read, then
/// release or die, leave no channel, handle, claim or charge behind.
pub fn soak_no_leaks() -> Result<(), String> {
    let fx = Fixture::new()?;
    let channels_before = crate::ipc::channels::counts().channels;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    for round in 0..300u64 {
        let slot = spawn_driver(driver_cred())?;
        enter(slot)?;
        let mut endpoint = 0u64;
        let handle = expect_ok(claim_irq(dev, &mut endpoint, round % 2 == 0), "claim")?;
        expect_ok(sys(OP_IRQ_ENABLE, handle, 0, 0, 0), "irq_enable")?;
        fire(LINE_A, 10 + round);
        enter(slot)?;
        if round % 3 != 0 {
            take_irq(endpoint)?;
            expect_ok(sys(OP_IRQ_ACK, handle, 0, 0, 0), "ack")?;
        }
        if round % 2 == 0 {
            expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        }
        // Off the driver's page tables before they can be freed.
        leave(&fx);
        task::harness::finish(slot, 0);
        check!(
            table_state(dev).0.is_none(),
            "round {round}: a dead driver still owns the device"
        );
        task::reap_child().ok_or("the driver was not reapable")?;
    }
    check!(
        crate::ipc::channels::counts().channels == channels_before,
        "{} channels leaked",
        crate::ipc::channels::counts().channels - channels_before
    );
    check!(usage(Resource::DeviceClaims) == 0, "claims leaked");
    check!(
        masked(LINE_A),
        "the line was left unmasked with nobody armed"
    );
    leave(&fx);
    Ok(())
}
