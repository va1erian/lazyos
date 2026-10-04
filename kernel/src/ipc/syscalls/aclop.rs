//! The `acl_load` op: how label-keyed policy gets into the kernel.

use super::*;
use crate::ipc::acl::{self, reason, LoadError, Rule};
use crate::ipc::audit::{self, AuditEvent};
use crate::ipc::credentials::CAP_IPC_CONTROL;
use crate::ipc::{labels, policy};

/// Record a refused load. The interface is the policy loader's, so `auditd`
/// can tell a failed policy push from an application's denied call.
fn audit_refusal(slot: usize, reason_code: u32) {
    let cred = credentials::of(slot);
    audit::record(AuditEvent {
        ticks: task::ticks(),
        actor_slot: slot,
        uid: cred.uid,
        label_id: cred.label_id,
        interface_id: policy::LOADER_INTERFACE,
        method: policy::LOAD_METHOD,
        allow: false,
        reason_code,
        txn_id: 0,
    });
}

/// `OP_ACL_LOAD`: replace every rule of one label with the request's list.
///
/// The request parcel's body is the generated `LoadLabelArgs`
/// (`idl/policy.midl`). The caller must hold `CAP_IPC_CONTROL` -- no proxy
/// form exists, the rules are the *caller's* decision -- and must also pass
/// the ordinary ACL hook, so a labelled task can never load policy even if it
/// somehow held the capability. An empty rule list revokes the label. On
/// success `value` is the number of rules now held by the label.
pub(super) fn op_acl_load(args: &MsgArgs) -> Result<MsgResult, i64> {
    let me = task::current();
    if !credentials::of(me).has_cap(CAP_IPC_CONTROL) {
        audit_refusal(me, reason::LOADER_NOT_PRIVILEGED);
        return Err(errno::EPERM);
    }
    if crate::ipc::authorize(me, policy::LOADER_INTERFACE, policy::LOAD_METHOD, 0).denied() {
        return Err(errno::EACCES);
    }
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    let request = policy::wire::decode_load_label_args(parcel.body()).map_err(|_| errno::EINVAL)?;
    // Size before interning: a refused load must not consume a slot of the
    // append-only label table.
    if request.rules.len() > acl::MAX_RULES_PER_LABEL {
        return Err(errno::EINVAL);
    }
    let label = labels::intern(&request.label).map_err(|error| match error {
        labels::Error::Malformed => errno::EINVAL,
        labels::Error::TableFull => errno::ENOMEM,
    })?;
    let rules: Vec<Rule> = request
        .rules
        .iter()
        .map(|rule| Rule {
            actor: label,
            interface_id: rule.interface_id,
            method: rule.method,
            allow: rule.allow,
        })
        .collect();
    acl::load_label(label, &rules).map_err(|error| match error {
        LoadError::Unlabelled => errno::EINVAL,
        LoadError::TooManyRules => errno::EINVAL,
        LoadError::PolicyFull => errno::ENOMEM,
    })?;
    Ok(MsgResult {
        value: rules.len() as u64,
        ..MsgResult::default()
    })
}
