//! Messenger kernel IPC core (issues #64+).
//!
//! This module owns the kernel-side objects the Messenger fabric is built on:
//! the per-process handle table that gives every object reference an
//! unforgeable, rights-carrying name; the channels that carry one-way messages
//! and synchronous transactions; the shared buffers and fences that make
//! handoff copy-free (issue #67); and the security core: kernel-stamped
//! credentials, the default-deny ACL hook, and the hash-chained audit ring
//! (issue #68). The name registry in [`registry`] turns those objects into
//! discoverable services: names carry an owner, an interface list and an
//! optional lease, and `resolve` opens the registered endpoint in the
//! receiving task. The native syscall surface lives in [`syscalls`], and
//! [`stats`] aggregates every subsystem into the versioned observability
//! snapshot (issue #70).

pub mod acl;
pub mod audit;
pub mod channels;
pub mod credentials;
pub mod epoll;
pub mod eventfd;
pub mod handles;
pub mod inet;
pub mod labels;
pub mod pipe;
pub mod policy;
pub mod registry;
pub mod shared;
pub mod shared_va;
pub mod stats;
pub mod syscalls;
pub mod topics;
pub mod unix;

/// Tear down everything a reclaimed task slot still holds in the fabric.
///
/// Called by the scheduler when it frees the slot of a finished task
/// (`task::reap_child` / `task::reclaim_pending`), *before* the address space
/// is released. `table` is the slot's PML4 and `table_shared` says whether
/// another live task still uses it.
///
/// Without this a dead task's handle table stayed behind: the next task to reuse
/// the slot inherited its channel endpoints and buffers (a capability leak), its
/// peers never saw `PeerDied` and blocked forever, its per-uid handle and buffer
/// charges were never released, and a shared-buffer mapping recorded against the
/// dead PML4 was later unmapped through freed page-table memory.
pub fn teardown_task(slot: usize, table: u64, table_shared: bool) {
    // Quiesce and release every device the task claimed first (issue #240):
    // mask its IRQs, stop DMA, unmap MMIO from the dying address space, and free
    // the device for the next driver, before anything else can observe it.
    crate::dev::teardown_task(slot, table);
    registry::release_owner(slot);
    for (handle, entry) in handles::entries_for_task(slot) {
        match entry.kind {
            handles::HandleKind::Channel => {
                let _ = channels::close_endpoint_for(slot, handle, true);
            }
            // Buffers are closed (and unmapped) by `shared::teardown_task`.
            handles::HandleKind::Buffer => {}
            // `dev::teardown_task` already released the claim; this only drops
            // the leftover handle entry.
            handles::HandleKind::Endpoint
            | handles::HandleKind::Object
            | handles::HandleKind::Device => {
                let _ = handles::close_for_task(slot, handle);
            }
        }
    }
    shared::teardown_task(slot, table, table_shared);
    channels::forget_task(slot);
    // Anything a subsystem refused to close still must not outlive the slot.
    handles::reset_for_task(slot);
}

/// The fabric's single policy choke point (issue #68).
///
/// `channels` (#66) and the native syscall dispatch (#69) call this once per
/// call, before touching any object or queue. It:
///
/// 1. reads the actor's kernel-stamped credentials by slot (never a
///    userspace-supplied identity),
/// 2. resolves the ACL authority from them (the label when the task has one,
///    otherwise the uid; see [`policy`] and [`credentials::Cred::authority`]),
/// 3. evaluates the compiled policy (default deny; see [`acl`]), and
/// 4. records an audit event: every denial, and allows when tracing is on
///    ([`audit::set_trace`]).
///
/// `txn_id` is copied into the audit record so a denied transaction can be
/// traced with `messengerctl why <txn>`. The return value carries the friendly
/// denial text; #69 maps it to `ERR_DENIED`.
pub fn authorize(actor_slot: usize, interface_id: u64, method: u32, txn_id: u64) -> acl::Decision {
    let cred = credentials::of(actor_slot);
    let (decision, reason_code) = if cred.label_id != 0 {
        // A labelled task is judged by its label's rules, never its uid.
        policy::evaluate_labelled(&cred, interface_id, method)
    } else {
        acl::evaluate_verdict(cred.authority(), interface_id, method)
    };
    if decision.denied() || audit::trace() {
        audit::record(audit::AuditEvent {
            ticks: crate::task::ticks(),
            actor_slot,
            uid: cred.uid,
            label_id: cred.label_id,
            interface_id,
            method,
            allow: !decision.denied(),
            reason_code,
            txn_id,
        });
    }
    decision
}
