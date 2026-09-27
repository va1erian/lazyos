//! Messenger kernel IPC core (issues #64+).
//!
//! This module owns the kernel-side objects the Messenger fabric is built on:
//! the per-process handle table that gives every object reference an
//! unforgeable, rights-carrying name; the channels that carry one-way messages
//! and synchronous transactions; the shared buffers and fences that make
//! handoff copy-free (issue #67); and the security core: kernel-stamped
//! credentials, the default-deny ACL hook, and the hash-chained audit ring
//! (issue #68). Syscalls land in later issues.

pub mod acl;
pub mod audit;
pub mod channels;
pub mod credentials;
pub mod handles;
pub mod shared;

/// The fabric's single policy choke point (issue #68).
///
/// `channels` (#66) and the native syscall dispatch (#69) call this once per
/// call, before touching any object or queue. It:
///
/// 1. reads the actor's kernel-stamped credentials by slot (never a
///    userspace-supplied identity),
/// 2. resolves the ACL authority from them (the uid today; see
///    [`credentials::Cred::authority`]),
/// 3. evaluates the compiled policy (default deny; see [`acl`]), and
/// 4. records an audit event: every denial, and allows when tracing is on
///    ([`audit::set_trace`]).
///
/// `txn_id` is copied into the audit record so a denied transaction can be
/// traced with `messengerctl why <txn>`. The return value carries the friendly
/// denial text; #69 maps it to `ERR_DENIED`.
pub fn authorize(actor_slot: usize, interface_id: u64, method: u32, txn_id: u64) -> acl::Decision {
    let cred = credentials::of(actor_slot);
    let (decision, reason_code) = acl::evaluate_verdict(cred.authority(), interface_id, method);
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
