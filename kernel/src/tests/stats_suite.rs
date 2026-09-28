//! Fabric observability snapshot (issue #70).

use super::*;
use crate::ipc::stats::{self, FABRIC_STATS_VERSION};
use crate::ipc::{acl, audit, channels, handles, shared};
use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

const IFACE: u64 = 0x0abc_0def_1234_5678;

/// Friendly-message adapter for channel errors.
fn reason(error: channels::Error) -> String {
    error.message().into()
}

/// Friendly-message adapter for shared-buffer errors.
fn buffer_reason(error: shared::Error) -> String {
    error.message().into()
}

/// Every stats test starts from an empty fabric with the kernel task
/// current and runnable.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    stats::reset();
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    Ok(())
}

/// Encode a parcel whose body carries one string field.
fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: IFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// A snapshot reports exactly the handles, channels, buffer, policy rules
/// and audited denial the test created, with the live chain head.
pub fn snapshot_reflects_objects() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let buffer =
        shared::create(4096, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;
    acl::load(&[
        acl::Rule {
            actor: 0,
            interface_id: IFACE,
            method: 3,
            allow: false,
        },
        acl::Rule {
            actor: acl::ANY_ACTOR,
            interface_id: acl::ANY_INTERFACE,
            method: acl::ANY_METHOD,
            allow: true,
        },
    ]);
    let before = stats::snapshot();
    check!(
        before.audit_denies == 0,
        "denials start at {}",
        before.audit_denies
    );
    let decision = crate::ipc::authorize(task::KERNEL_TASK, IFACE, 3, 42);
    check!(decision.denied(), "the explicit deny rule was not applied");

    let snap = stats::snapshot();
    check!(
        snap.version == FABRIC_STATS_VERSION,
        "snapshot version is {}",
        snap.version
    );
    check!(
        snap.services == 0,
        "no bootstrap was created but services is {}",
        snap.services
    );
    check!(
        snap.channels == 1 && snap.endpoints == 2,
        "channel counts are channels {} endpoints {}",
        snap.channels,
        snap.endpoints
    );
    check!(
        snap.handles == 3 && snap.handles_per_task[task::KERNEL_TASK] == 3,
        "handle counts are total {} slot {}",
        snap.handles,
        snap.handles_per_task[task::KERNEL_TASK]
    );
    check!(
        snap.tasks[task::KERNEL_TASK].live == 1,
        "the kernel slot is not marked live"
    );
    check!(
        snap.tasks[task::KERNEL_TASK].buffers == 1
            && snap.tasks[task::KERNEL_TASK].buffer_bytes == 4096,
        "slot usage is {:?}",
        snap.tasks[task::KERNEL_TASK]
    );
    check!(
        snap.buffers == 1 && snap.buffer_bytes == 4096 && snap.buffer_mappings == 1,
        "buffer counts are {:?}",
        (snap.buffers, snap.buffer_bytes, snap.buffer_mappings)
    );
    check!(
        snap.acl_rules == 2 && snap.acl_loaded == 1,
        "ACL state is rules {} loaded {}",
        snap.acl_rules,
        snap.acl_loaded
    );
    check!(
        snap.audit_count == 1
            && snap.audit_total == 1
            && snap.audit_denies == 1
            && snap.audit_allows == 0,
        "audit state is {:?}",
        (snap.audit_count, snap.audit_total, snap.audit_denies)
    );
    check!(
        snap.audit_last_hash == audit::last_hash() && snap.audit_last_hash != audit::GENESIS_HASH,
        "the snapshot hash is not the live chain head"
    );

    handles::close(client).ok();
    handles::close(server).ok();
    shared::close(buffer).ok();
    Ok(())
}

/// `stats::reset` restores every counter — and the audit chain — to the
/// bring-up state.
pub fn reset_restores_zeros() -> Result<(), String> {
    fresh()?;
    channels::create().map_err(reason)?;
    shared::create(4096, shared::flags::READ).map_err(buffer_reason)?;
    acl::load(&[acl::Rule {
        actor: acl::ANY_ACTOR,
        interface_id: acl::ANY_INTERFACE,
        method: acl::ANY_METHOD,
        allow: true,
    }]);
    audit::record(audit::AuditEvent {
        ticks: 1,
        actor_slot: task::KERNEL_TASK,
        uid: 0,
        label_id: 0,
        interface_id: IFACE,
        method: 1,
        allow: false,
        reason_code: acl::reason::DEFAULT_DENY,
        txn_id: 7,
    });
    let before = stats::snapshot();
    check!(
        before.channels == 1 && before.buffers == 1 && before.audit_total == 1,
        "the fabric was not populated before reset: {before:?}"
    );

    stats::reset();
    let snap = stats::snapshot();
    check!(
        snap.version == FABRIC_STATS_VERSION,
        "snapshot version is {}",
        snap.version
    );
    check!(
        snap.services == 0 && snap.channels == 0 && snap.endpoints == 0,
        "reset left services/channels/endpoints: {snap:?}"
    );
    check!(
        snap.handles == 0 && snap.handles_per_task.iter().all(|held| *held == 0),
        "reset left handles: {snap:?}"
    );
    check!(
        snap.buffers == 0 && snap.buffer_bytes == 0 && snap.buffer_mappings == 0,
        "reset left buffers: {snap:?}"
    );
    check!(
        snap.acl_rules == 0 && snap.acl_loaded == 0,
        "reset left the ACL loaded: {snap:?}"
    );
    check!(
        snap.audit_count == 0
            && snap.audit_total == 0
            && snap.audit_denies == 0
            && snap.audit_allows == 0,
        "reset left audit counters: {snap:?}"
    );
    check!(
        snap.audit_last_hash == audit::GENESIS_HASH,
        "reset left the chain head at {:#x}",
        snap.audit_last_hash
    );
    Ok(())
}

/// An accepted one-way message and a synchronous call/reply move the
/// message counters; a policy denial moves the audit deny counter without
/// touching the channel counters.
pub fn counters_follow_calls_and_denials() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let before = stats::snapshot();

    let note = parcel(7, flags::ONE_WAY, "note")?;
    channels::send(client, &note).map_err(reason)?;
    let message = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the one-way message was not delivered")?;
    check!(
        message.sender == task::KERNEL_TASK,
        "sender is {}, expected {}",
        message.sender,
        task::KERNEL_TASK
    );
    let after_send = stats::snapshot();
    check!(
        after_send.one_way == before.one_way + 1,
        "one-way counter is {}, expected {}",
        after_send.one_way,
        before.one_way + 1
    );
    check!(
        after_send.calls == before.calls,
        "the one-way send counted as a call"
    );

    let request = parcel(9, flags::SYNC, "ping")?;
    let txn = channels::begin_call(client, 9, &request, None).map_err(reason)?;
    let _ = channels::recv(server, None).map_err(reason)?;
    channels::reply(txn, &request).map_err(reason)?;
    let _ = task::harness::take_wake_reason(task::current());
    let _ = channels::await_reply(txn).map_err(reason)?;
    let after_call = stats::snapshot();
    check!(
        after_call.calls == after_send.calls + 1 && after_call.replies == after_send.replies + 1,
        "call counters are calls {} replies {}",
        after_call.calls,
        after_call.replies
    );
    check!(
        after_call.outstanding == 0,
        "outstanding is {} after the reply",
        after_call.outstanding
    );

    acl::load(&[acl::Rule {
        actor: 9999,
        interface_id: IFACE,
        method: 9,
        allow: true,
    }]);
    let denied = crate::ipc::authorize(task::KERNEL_TASK, IFACE, 9, 77);
    check!(denied.denied(), "the unmatched call was not denied");
    let after_denial = stats::snapshot();
    check!(
        after_denial.audit_denies == after_call.audit_denies + 1,
        "deny counter is {}, expected {}",
        after_denial.audit_denies,
        after_call.audit_denies + 1
    );
    check!(
        after_denial.calls == after_call.calls && after_denial.drops == after_call.drops,
        "a denied call touched the channel counters"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "ipc_stats_snapshot_reflects_objects",
        snapshot_reflects_objects,
    ),
    ("ipc_stats_reset_restores_zeros", reset_restores_zeros),
    (
        "ipc_stats_counters_follow_traffic",
        counters_follow_calls_and_denials,
    ),
];
