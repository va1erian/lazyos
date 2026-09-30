//! `claim` and `release` through the syscall path (issue #240): the grant rule,
//! class-specific ACL, audit records, quotas, and hostile handles.

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::dev::TaskSlot;
use crate::ipc::acl::Rule;
use crate::ipc::handles::{rights, HandleKind};
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

/// claim -> `Device` handle with the resource-derived rights -> release, with
/// every side effect and audit record checked in both directions.
pub fn sys_claim_release_roundtrip() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let slot = driver_in(driver_cred())?;
    let handles_before = handles::count_for_task(slot);
    let (_, generation) = table_state(dev);

    let handle = expect_ok(claim_plain(dev), "claim")?;
    let entry = handles::get(handle).map_err(|e| e.message().to_string())?;
    check!(
        entry.kind == HandleKind::Device,
        "the handle is a {:?}",
        entry.kind
    );
    check!(
        entry.rights == rights::DEV_ALL,
        "rights {:#x}, expected every family for a full PCI NIC",
        entry.rights
    );
    check!(
        table_state(dev).0 == Some(TaskSlot(slot)),
        "the owner was not recorded"
    );
    check!(
        usage(Resource::DeviceClaims) == 1,
        "the claim was not charged"
    );
    check!(
        handles::count_for_task(slot) == handles_before + 1,
        "the handle was not counted"
    );
    let record = latest(dev, method::CLAIM).ok_or("the claim was not audited")?;
    check!(
        record.allow
            && record.reason_code == reason::CLAIMED | rights::DEV_ALL << 8
            && record.interface_id == class::NET.interface_id
            && record.actor_slot == slot
            && record.uid == DRIVER_UID,
        "claim record is {record:?}"
    );

    // Device handles are neither duplicable nor closable through the
    // Messenger calls: only `release` ends a claim.
    check!(
        handles::duplicate(handle, rights::DEV_ALL).is_err(),
        "a Device handle was duplicated"
    );
    check!(
        channels::close_endpoint(handle).is_err() && table_state(dev).0.is_some(),
        "the Messenger close call ended a device claim"
    );

    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    check!(
        table_state(dev) == (None, generation + 1),
        "release left {:?}",
        table_state(dev)
    );
    check!(handles::get(handle).is_err(), "the handle survived release");
    check!(
        usage(Resource::DeviceClaims) == 0,
        "the charge survived release"
    );
    check!(
        handles::count_for_task(slot) == handles_before,
        "handle count drifted"
    );
    let record = latest(dev, method::RELEASE).ok_or("the release was not audited")?;
    check!(
        record.allow && record.reason_code == reason::RELEASED,
        "release record {record:?}"
    );
    expect_errno(sys(OP_RELEASE, handle, 0, 0, 0), EBADF, "second release")?;
    Ok(())
}

/// Every refusal path leaves no owner, quota charge, or handle, and the
/// permission failures are audited.
pub fn sys_claim_refusals_are_atomic() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let kernel_owned = add_device(Spec::nic(None))?;
    check!(
        crate::dev::table()
            .lock()
            .claim(kernel_owned, TaskSlot::KERNEL)
            .is_ok(),
        "cannot stage a kernel-owned device"
    );

    let _plain = driver_in(crate::ipc::credentials::Cred::new(DRIVER_UID, 1, 0, 0, 1))?;
    expect_errno(claim_plain(dev), EPERM, "claim without CAP_DEV_CLAIM")?;
    let denial = latest(dev, method::CLAIM).ok_or("the denial was not audited")?;
    check!(
        !denial.allow && denial.reason_code == reason::NO_CAP,
        "no-cap record is {denial:?}"
    );
    leave(&fx);
    expect_errno(claim_plain(dev), EPERM, "claim by the kernel task")?;

    let slot = driver_in(driver_cred())?;
    let handles_before = handles::count_for_task(slot);
    let bogus = crate::dev::table().lock().len() as u64 + 40;
    for (what, id, endpoint, flags, errno) in [
        ("id beyond u16", 1 << 20, NO_ENDPOINT, 0, ENODEV),
        ("id of an empty slot", bogus, NO_ENDPOINT, 0, ENODEV),
        ("unknown flag", u64::from(dev.0), NO_ENDPOINT, 2, EINVAL),
        (
            "shared without endpoint",
            u64::from(dev.0),
            NO_ENDPOINT,
            1,
            EINVAL,
        ),
        ("endpoint is no handle", u64::from(dev.0), 9999, 0, EBADF),
        (
            "device already owned by the kernel",
            u64::from(kernel_owned.0),
            NO_ENDPOINT,
            0,
            EBUSY,
        ),
    ] {
        expect_errno(sys(OP_CLAIM, id, endpoint, flags, 0), errno, what)?;
    }
    let bad = latest(dev, method::CLAIM).ok_or("the bad endpoint was not audited")?;
    check!(
        bad.reason_code == reason::BAD_ENDPOINT,
        "bad-endpoint record {bad:?}"
    );
    let busy = latest(kernel_owned, method::CLAIM).ok_or("the busy claim was not audited")?;
    check!(busy.reason_code == reason::BUSY, "busy record {busy:?}");
    check!(
        table_state(dev).0.is_none(),
        "a refused claim left an owner"
    );
    check!(
        usage(Resource::DeviceClaims) == 0,
        "a refused claim was charged"
    );
    check!(
        handles::count_for_task(slot) == handles_before,
        "a refused claim leaked a handle"
    );
    check!(
        table_state(kernel_owned).0 == Some(TaskSlot::KERNEL),
        "a refused claim disturbed the kernel's device"
    );

    // Double claim by a second task.
    expect_ok(claim_plain(dev), "first claim")?;
    let rival = driver_in(driver_cred())?;
    expect_errno(claim_plain(dev), EBUSY, "double claim")?;
    check!(
        table_state(dev).0 == Some(TaskSlot(slot)) && rival != slot,
        "the double claim changed the owner"
    );
    check!(
        usage(Resource::DeviceClaims) == 1,
        "the double claim was charged"
    );
    Ok(())
}

/// Policy names a *class*: a rule for net cannot claim storage or audio, the
/// generic `os.kernel.dev` interface authorizes nothing, and `map`/`dma` rules
/// widen the rights the handle gets.
pub fn sys_acl_is_class_specific() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let nic = add_device(Spec::nic(Some(LINE_A)))?;
    let storage = add_device(Spec::nic(None).with_class(0x01, 0))?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x01))?;
    let allow = |interface_id, method| Rule {
        actor: DRIVER_UID,
        interface_id,
        method,
        allow: true,
    };
    driver_in(driver_cred())?;

    acl::load(&[allow(class::NET.interface_id, method::CLAIM)]);
    let handle = expect_ok(claim_plain(nic), "net claim under a net rule")?;
    let entry = handles::get(handle).map_err(|e| e.message().to_string())?;
    check!(
        entry.rights == rights::DEV_CONFIG | rights::DEV_IRQ,
        "claim-only policy granted {:#x}",
        entry.rights
    );
    expect_errno(
        claim_plain(storage),
        EACCES,
        "storage claim under a net rule",
    )?;
    expect_errno(claim_plain(audio), EACCES, "audio claim under a net rule")?;
    let denial = latest(storage, method::CLAIM).ok_or("the class denial was not audited")?;
    check!(
        !denial.allow
            && denial.interface_id == class::STORAGE.interface_id
            && denial.reason_code == crate::ipc::acl::reason::DEFAULT_DENY,
        "denial record {denial:?}"
    );
    check!(
        table_state(storage).0.is_none() && table_state(audio).0.is_none(),
        "a denied claim left an owner"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    // The generic interface is not a class.
    acl::load(&[allow(class::DEV_INTERFACE, method::CLAIM)]);
    expect_errno(
        claim_plain(nic),
        EACCES,
        "claim under a generic os.kernel.dev rule",
    )?;

    // `map` adds MMIO and PIO; `dma` adds DMA.
    acl::load(&[
        allow(class::NET.interface_id, method::CLAIM),
        allow(class::NET.interface_id, method::MAP),
    ]);
    let handle = expect_ok(claim_plain(nic), "claim with map")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == rights::DEV_ALL & !rights::DEV_DMA,
        "claim+map granted {granted:#x}"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    acl::load(&[
        allow(class::NET.interface_id, method::CLAIM),
        allow(class::NET.interface_id, method::MAP),
        allow(class::NET.interface_id, method::DMA),
    ]);
    let handle = expect_ok(claim_plain(nic), "claim with map and dma")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == rights::DEV_ALL,
        "full policy granted {granted:#x}"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    // An explicit deny ahead of a broad allow wins.
    acl::load(&[
        Rule {
            actor: DRIVER_UID,
            interface_id: class::NET.interface_id,
            method: method::CLAIM,
            allow: false,
        },
        Rule {
            actor: crate::ipc::acl::ANY_ACTOR,
            interface_id: crate::ipc::acl::ANY_INTERFACE,
            method: crate::ipc::acl::ANY_METHOD,
            allow: true,
        },
    ]);
    expect_errno(claim_plain(nic), EACCES, "claim under an explicit deny")?;
    let denial = latest(nic, method::CLAIM).ok_or("explicit deny not audited")?;
    check!(
        denial.reason_code == crate::ipc::acl::reason::EXPLICIT_DENY,
        "deny record {denial:?}"
    );
    Ok(())
}

/// A grant of nothing is `EPERM`, decided before any owner is recorded.
pub fn sys_empty_rights_is_eperm() -> Result<(), String> {
    let _fx = Fixture::new()?;
    // A platform device with a memory BAR and no interrupt has only `MMIO`
    // to give; class policy that allows just `claim` grants CONFIG|IRQ.
    let dev = add_device(Spec::nic(None).platform().with_bars(vec![mem_bar(
        0,
        0xFED0_0000,
        0x1000,
    )]))?;
    driver_in(driver_cred())?;
    acl::load(&[Rule {
        actor: DRIVER_UID,
        interface_id: class::NET.interface_id,
        method: method::CLAIM,
        allow: true,
    }]);
    let (_, generation) = table_state(dev);
    let handles_before = handles::count();
    expect_errno(claim_plain(dev), EPERM, "empty rights intersection")?;
    check!(
        table_state(dev) == (None, generation),
        "an empty grant touched the table: {:?}",
        table_state(dev)
    );
    check!(
        usage(Resource::DeviceClaims) == 0,
        "an empty grant was charged"
    );
    check!(
        handles::count() == handles_before,
        "an empty grant leaked a handle"
    );
    let record = latest(dev, method::CLAIM).ok_or("the empty grant was not audited")?;
    check!(record.reason_code == reason::NO_RIGHTS, "record {record:?}");

    // With `map` allowed the intersection is MMIO and the claim goes through.
    acl::load(&[
        Rule {
            actor: DRIVER_UID,
            interface_id: class::NET.interface_id,
            method: method::CLAIM,
            allow: true,
        },
        Rule {
            actor: DRIVER_UID,
            interface_id: class::NET.interface_id,
            method: method::MAP,
            allow: true,
        },
    ]);
    let handle = expect_ok(claim_plain(dev), "claim once map is allowed")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(granted == rights::DEV_MMIO, "granted {granted:#x}");
    Ok(())
}

/// The interrupt endpoint must be the claimant's own private inbox (issue
/// #283): a side another handle also names (as every client of a resolved
/// service does) is refused, and a bound side loses the rights to be spread.
pub fn sys_irq_endpoint_must_be_private() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(Some(LINE_A)))?;
    let slot = driver_in(driver_cred())?;
    let (endpoint, _peer) = irq_channel()?;
    let entry = handles::get(endpoint).map_err(|error| error.message().to_string())?;

    // A second handle to the same side stands in for a resolved service.
    let shared = handles::open_for_task(
        slot,
        handles::HandleKind::Channel,
        handles::rights::ALL,
        entry.object_id,
    )
    .map_err(|error| error.message().to_string())?;
    expect_errno(
        claim_irq(dev, endpoint, false),
        EBADF,
        "claim on a shared side",
    )?;
    check!(
        table_state(dev).0.is_none(),
        "a refused claim left an owner"
    );
    let _ = handles::close(shared);

    // Not a channel at all.
    let handle = expect_ok(claim_plain(dev), "plain claim")?;
    let other = add_device(Spec::nic(Some(LINE_B)))?;
    expect_errno(
        claim_irq(other, handle, false),
        EBADF,
        "a device handle as endpoint",
    )?;
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    // The private side binds and is sealed against duplication and transfer.
    expect_ok(claim_irq(dev, endpoint, false), "claim on a private side")?;
    let rights = handles::rights(endpoint).ok_or("the endpoint handle vanished")?;
    check!(
        rights & (handles::rights::DUPLICATE | handles::rights::TRANSFER) == 0,
        "a bound endpoint can still be spread: rights {rights:#x}"
    );
    check!(
        handles::duplicate(endpoint, handles::rights::CALL).is_err(),
        "a bound endpoint was duplicated"
    );
    leave(&fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_sys_claim_release_roundtrip",
        sys_claim_release_roundtrip,
    ),
    (
        "dev_sys_claim_refusals_are_atomic",
        sys_claim_refusals_are_atomic,
    ),
    ("dev_sys_acl_is_class_specific", sys_acl_is_class_specific),
    ("dev_sys_empty_rights_is_eperm", sys_empty_rights_is_eperm),
    (
        "dev_sys_irq_endpoint_must_be_private",
        sys_irq_endpoint_must_be_private,
    ),
];
