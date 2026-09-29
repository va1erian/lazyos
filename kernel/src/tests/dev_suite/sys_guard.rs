//! Hostile handles, quota rollback and audit-chain integrity of the device
//! syscall (issue #240).

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::dev::TaskSlot;
use crate::ipc::acl::Rule;
use crate::ipc::handles::{rights, HandleKind};
use crate::quota;
use crate::quota::Resource;

/// Newest audit record about `dev` with `method`.
fn latest(dev: DeviceId, method: u32) -> Option<audit::AuditEvent> {
    audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| event.method == method && report::device_of(event.txn_id) == Some(dev))
}

/// A driver task with `cred`, entered.
fn driver_in(cred: crate::ipc::credentials::Cred) -> Result<usize, String> {
    let slot = spawn_driver(cred)?;
    enter(slot)?;
    Ok(slot)
}

/// A handle from another task, a forged one, a stale one, one of the wrong
/// kind, or one lacking the right must never act on a device.
pub fn sys_hostile_handles() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let owner = driver_in(driver_cred())?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let (_, generation) = table_state(dev);
    let object = crate::dev::claims::object_id(dev, generation);
    leave(&fx);

    let ops = [
        (OP_MAP_BAR, 0u64, 0u64, 0u64),
        (OP_PIO, 1, 0, pio_word(1, false, 0)),
        (OP_CFG_READ, 0, 2, 0),
        (OP_CFG_WRITE, 4, 2, 0),
        (OP_IRQ_ENABLE, 0, 0, 0),
        (OP_IRQ_ACK, 0, 0, 0),
    ];
    let try_all = |h: u64, what: &str| -> Result<(), String> {
        for (op, a2, a3, a4) in ops {
            expect_errno(sys(op, h, a2, a3, a4), EBADF, &format!("{what}: op {op}"))?;
        }
        expect_errno(
            sys(OP_RELEASE, h, 0, 0, 0),
            EBADF,
            &format!("{what}: release"),
        )
    };

    // Another task, by number and by a forged table entry.
    let thief = driver_in(driver_cred())?;
    try_all(handle, "a foreign handle number")?;
    try_all(u64::MAX, "handle u64::MAX")?;
    try_all(9999, "handle 9999")?;
    let forged = handles::open_for_task(thief, HandleKind::Device, rights::DEV_ALL, object)
        .map_err(|e| e.message().to_string())?;
    try_all(forged, "a forged Device handle")?;
    // A handle of the wrong kind.
    let (channel, _peer) = irq_channel()?;
    try_all(channel, "a channel handle")?;
    check!(
        table_state(dev).0 == Some(TaskSlot(owner)),
        "a hostile call disturbed the owner"
    );

    // A forged alias in the owner's own table is not the claim's handle either.
    enter(owner)?;
    let alias = handles::open_for_task(owner, HandleKind::Device, rights::DEV_ALL, object)
        .map_err(|e| e.message().to_string())?;
    try_all(alias, "an alias of the owner's own handle")?;

    // Stale: after release, and after a re-claim at a new generation.
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    try_all(handle, "the released handle")?;
    let fresh = expect_ok(claim_plain(dev), "re-claim")?;
    check!(
        table_state(dev).1 == generation + 1,
        "re-claim did not advance the generation"
    );
    let stale = handles::open_for_task(owner, HandleKind::Device, rights::DEV_ALL, object)
        .map_err(|e| e.message().to_string())?;
    try_all(stale, "a handle of the previous generation")?;
    expect_ok(sys(OP_CFG_READ, fresh, 0, 2, 0), "the new handle works")?;
    Ok(())
}

/// The per-uid claim and handle quotas are charged last and rolled back.
pub fn sys_quota_charges() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let first = add_device(Spec::nic(None))?;
    let second = add_device(Spec::nic(None))?;
    let slot = driver_in(driver_cred())?;

    quota::set_limit(DRIVER_UID, Resource::DeviceClaims, 1);
    let handle = expect_ok(claim_plain(first), "first claim")?;
    expect_errno(
        claim_plain(second),
        EDQUOT,
        "claim over the DeviceClaims limit",
    )?;
    check!(
        table_state(second).0.is_none(),
        "an over-quota claim left an owner"
    );
    check!(
        usage(Resource::DeviceClaims) == 1,
        "the refused claim was charged"
    );
    let record = latest(second, method::CLAIM).ok_or("the quota refusal was not audited")?;
    check!(record.reason_code == reason::QUOTA, "record {record:?}");
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    expect_ok(
        claim_plain(second),
        "claim after the release freed the quota",
    )?;
    check!(
        usage(Resource::DeviceClaims) == 1,
        "quota not held by the second claim"
    );

    // Handle quota exhausted: the claim rolls back its ownership and charge.
    quota::set_limit(DRIVER_UID, Resource::DeviceClaims, 8);
    let held = usage(Resource::Handles);
    quota::set_limit(DRIVER_UID, Resource::Handles, held);
    let (_, generation) = table_state(first);
    expect_errno(claim_plain(first), EMFILE, "claim with no handle quota")?;
    check!(
        table_state(first).0.is_none(),
        "a handle-quota failure left an owner"
    );
    check!(
        usage(Resource::DeviceClaims) == 1,
        "a handle-quota failure kept its charge"
    );
    check!(
        table_state(first).1 > generation && handles::count_for_task(slot) as u64 == held,
        "rollback state: generation {} handles {}",
        table_state(first).1,
        handles::count_for_task(slot)
    );
    Ok(())
}

/// The claim/deny/release records verify against the hash chain.
pub fn sys_audit_chain_verifies() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let other = add_device(Spec::nic(None))?;
    let before = audit::total();
    driver_in(driver_cred())?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    expect_errno(claim_plain(dev), EBUSY, "double claim")?;
    acl::load(&[Rule {
        actor: DRIVER_UID,
        interface_id: class::STORAGE.interface_id,
        method: method::CLAIM,
        allow: true,
    }]);
    expect_errno(claim_plain(other), EACCES, "denied claim")?;
    acl::load(&[]);
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    check!(
        audit::total() >= before + 4,
        "only {} records were added",
        audit::total() - before
    );

    let mut events = audit::recent(audit::AUDIT_CAPACITY);
    events.reverse();
    let mut hash = audit::GENESIS_HASH;
    for event in &events {
        hash = audit::chain(hash, event);
    }
    check!(
        hash == audit::last_hash(),
        "the hash chain no longer verifies"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_sys_hostile_handles", sys_hostile_handles),
    ("dev_sys_quota_charges", sys_quota_charges),
    ("dev_sys_audit_chain_verifies", sys_audit_chain_verifies),
];
