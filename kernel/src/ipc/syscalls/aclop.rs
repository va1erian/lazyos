//! The `acl_load` op: how label-keyed policy gets into the kernel.

use super::*;
use crate::ipc::acl::{self, reason, LoadError, Rule};
use crate::ipc::audit::{self, AuditEvent};
use crate::ipc::credentials::CAP_IPC_CONTROL;
use crate::ipc::{children, labels, policy};

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
///
/// A child label (`app:<id>/<name>`, [`crate::ipc::children`]) also carries
/// the `program` that enters it: see [`load_child`].
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
    let program = request.program.as_deref().filter(|path| !path.is_empty());
    let loaded = match labels::parse(&request.label) {
        Ok((labels::Kind::Child, _)) => load_child(me, &request, program)?,
        Ok(_) if program.is_some() => return Err(errno::EINVAL),
        Ok(_) => load_label(&request)?,
        Err(_) => return Err(errno::EINVAL),
    };
    Ok(MsgResult {
        value: loaded as u64,
        ..MsgResult::default()
    })
}

/// The rules of `request` as the ACL stores them for `label` (the loader
/// rewrites the actor of each, so the value given here is not trusted).
fn rules_for(label: u32, request: &policy::wire::LoadLabelArgs) -> Vec<Rule> {
    request
        .rules
        .iter()
        .map(|rule| Rule {
            actor: label,
            interface_id: rule.interface_id,
            method: rule.method,
            allow: rule.allow,
        })
        .collect()
}

fn intern_error(error: labels::Error) -> i64 {
    match error {
        labels::Error::Malformed => errno::EINVAL,
        labels::Error::TableFull => errno::ENOMEM,
        labels::Error::NoParent => errno::ENOENT,
    }
}

fn load_error(error: LoadError) -> i64 {
    match error {
        LoadError::Unlabelled | LoadError::TooManyRules => errno::EINVAL,
        LoadError::PolicyFull => errno::ENOMEM,
    }
}

/// Load the rules of an `app:`, `system:` or `dev:` label. An empty list is
/// a revocation, and revoking an app takes its children with it (their
/// bindings and rules: `ipc::children`), so a package's helpers never outlive
/// it, whatever its manifest says.
fn load_label(request: &policy::wire::LoadLabelArgs) -> Result<usize, i64> {
    let label = labels::intern(&request.label).map_err(intern_error)?;
    let rules = rules_for(label, request);
    acl::load_label(label, &rules).map_err(load_error)?;
    if rules.is_empty() {
        for child in children::unbind_children_of(label) {
            // Emptying a label's rules cannot fail.
            let _ = acl::load_label(child, &[]);
        }
    }
    Ok(rules.len())
}

/// Load a child label (`app:<id>/<name>`, issue #832): its rules, which must
/// be ones the app's loaded rules allow, and the program that enters it.
/// Without a `program` and with no rules it is a revocation; with rules and
/// no program the request is malformed (nothing could ever enter the label).
fn load_child(
    me: usize,
    request: &policy::wire::LoadLabelArgs,
    program: Option<&str>,
) -> Result<usize, i64> {
    let parent_name = labels::parent_label(&request.label).ok_or(errno::EINVAL)?;
    // The app first: a child of a label the loader never loaded is refused
    // before it can take a slot of the append-only label table.
    let parent = labels::lookup(parent_name).ok_or(errno::ENOENT)?;
    let Some(program) = program else {
        if !request.rules.is_empty() {
            return Err(errno::EINVAL);
        }
        if let Some(child) = labels::lookup(&request.label) {
            children::unbind(child);
            let _ = acl::load_label(child, &[]);
        }
        return Ok(0);
    };
    let rules = rules_for(0, request);
    if !acl::covered_by(parent, &rules) {
        audit_refusal(me, reason::CHILD_WIDENS_APP);
        return Err(errno::EACCES);
    }
    let existing = labels::lookup(&request.label);
    children::check_bind(existing, parent, program).map_err(|error| match error {
        children::BindError::BadProgram | children::BindError::Duplicate => errno::EINVAL,
        children::BindError::Full => errno::ENOMEM,
    })?;
    let child = labels::intern(&request.label).map_err(intern_error)?;
    acl::load_label(child, &rules).map_err(load_error)?;
    if children::bind(child, program).is_err() {
        // Only a race with another loader can get here; leave nothing half-made.
        let _ = acl::load_label(child, &[]);
        return Err(errno::ENOMEM);
    }
    Ok(rules.len())
}
