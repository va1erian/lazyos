//! Application labels and label-keyed Messenger policy (application package
//! system, phase 1): the interned label table, the credential gate rules that
//! keep a label write-once, the `os.lazy.*` / `app.<id>.*` / `app/<id>/`
//! namespaces, default-deny for labelled tasks, the `acl_load` op and a soak
//! over register/unregister/load cycles.

use super::*;
use crate::ipc::syscalls::{
    errno, MsgArgs, MsgResult, OP_ACL_LOAD, OP_REGISTER, OP_RESOLVE, OP_UNREGISTER,
};
use crate::ipc::{acl, audit, channels, credentials, handles, labels, policy, registry};
use credentials::Cred;
use libmessenger::{Header, Parcel, VERSION};

mod calls;
mod identity;
mod names;
mod session_topics;
mod soak;

/// Scratch user address space for the syscall-level tests.
const SPACE: u64 = 0x0060_0000;
const SPACE_PAGES: u64 = 8;
const ARGS: u64 = SPACE;
const RESULT: u64 = SPACE + 0x100;
const REQUEST: u64 = SPACE + 0x1000;
/// Credential-gate scratch: command line, labelled-spawn block, label bytes,
/// and the label-name read-back buffer.
const CMDLINE: u64 = SPACE + 0x3000;
const BLOCK: u64 = SPACE + 0x3100;
const LABEL_BYTES: u64 = SPACE + 0x3200;
const LABEL_OUT: u64 = SPACE + 0x3400;

/// Every test starts from the bring-up state: kernel task current, all slots
/// reset, empty registry, empty uid policy, no labels, no label rules, empty
/// audit ring, tracing off.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 0..task::MAX_TASKS {
        handles::reset_for_task(slot);
        credentials::reset_for_task(slot);
    }
    channels::reset();
    registry::reset();
    acl::load(&[]);
    acl::reset_labels_for_tests();
    labels::reset_for_tests();
    audit::reset();
    audit::set_trace(false);
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    Ok(())
}

/// Run `f` with [`SPACE`] mapped into a fresh address space installed as CR3,
/// exactly as a real syscall from a user task would find it.
fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
    mem::switch_to(table);
    let outcome = f();
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// Two's-complement `-errno` as the syscall returns it in `rax`.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

fn write_bytes(va: u64, bytes: &[u8]) {
    // Safety: the scratch pages are mapped writable while installed.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
}

fn read_bytes(va: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    // Safety: the scratch pages are mapped readable while installed.
    unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
    out
}

/// One Messenger op through the native gate with the args block at [`ARGS`].
fn dispatch(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
    write_bytes(ARGS, &args.to_bytes());
    let code = process::dispatch_for_test(5, op, ARGS, RESULT);
    let result = MsgResult::from_bytes(&read_bytes(RESULT, crate::ipc::syscalls::RESULT_SIZE))
        .expect("the kernel wrote a malformed result block");
    (code, result)
}

/// Encode a request parcel around an already-encoded body.
fn parcel_bytes(interface_id: u64, method: u32, body: Vec<u8>) -> Result<Vec<u8>, String> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel
        .encode(&mut bytes)
        .map_err(|error| String::from(error.message()))?;
    Ok(bytes)
}

/// Send a parcel through `op` and return the raw return code.
fn send(op: u64, bytes: &[u8]) -> u64 {
    write_bytes(REQUEST, bytes);
    let args = MsgArgs {
        txn_id: crate::ipc::syscalls::REGISTRY_TARGET_SELF,
        parcel_ptr: REQUEST,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    dispatch(op, &args).0
}

/// Register `name` as the current task, publishing a fresh channel and no
/// interfaces. Returns the syscall code and the two handles the test must
/// close afterwards.
fn register_current(name: &str) -> Result<(u64, (u64, u64)), String> {
    register_current_with(name, &[], &[])
}

/// [`register_current`] advertising `interfaces`, spelled out by `names`.
fn register_current_with(
    name: &str,
    interfaces: &[u64],
    names: &[&str],
) -> Result<(u64, (u64, u64)), String> {
    let (service, callable) = channels::create().map_err(|error| String::from(error.message()))?;
    let body = registry::wire::encode_register_args(&registry::wire::RegisterArgs {
        name: name.into(),
        endpoint: Some(callable),
        interfaces: interfaces.to_vec(),
        lease_ticks: 0,
        interface_names: names.iter().map(|&n| n.into()).collect(),
    })
    .map_err(|error| String::from(error.message()))?;
    let bytes = parcel_bytes(registry::INTERFACE, registry::method::REGISTER, body)?;
    Ok((send(OP_REGISTER, &bytes), (service, callable)))
}

/// Resolve or unregister `name` as the current task.
fn name_op(op: u64, method: u32, name: &str) -> Result<u64, String> {
    let body =
        registry::wire::encode_resolve_args(&registry::wire::ResolveArgs { name: name.into() })
            .map_err(|error| String::from(error.message()))?;
    let bytes = parcel_bytes(registry::INTERFACE, method, body)?;
    Ok(send(op, &bytes))
}

fn resolve_current(name: &str) -> Result<u64, String> {
    name_op(OP_RESOLVE, registry::method::RESOLVE, name)
}

fn unregister_current(name: &str) -> Result<u64, String> {
    name_op(OP_UNREGISTER, registry::method::UNREGISTER, name)
}

/// Load `rules` for `label` through `OP_ACL_LOAD` as the current task.
fn load_current(label: &str, rules: &[policy::wire::LabelRule]) -> Result<u64, String> {
    let body = policy::wire::encode_load_label_args(&policy::wire::LoadLabelArgs {
        label: label.into(),
        rules: rules.to_vec(),
    })
    .map_err(|error| String::from(error.message()))?;
    let bytes = parcel_bytes(policy::LOADER_INTERFACE, policy::LOAD_METHOD, body)?;
    Ok(send(OP_ACL_LOAD, &bytes))
}

/// A live child task stamped (kernel-side) with `label`, `uid` and `caps`.
fn labelled_task(label: &str, uid: u32, caps: u32) -> Result<usize, String> {
    let slot = task::spawn_child("label", &service_suite::minimal_elf()).map_err(to_string)?;
    let id = if label.is_empty() {
        0
    } else {
        labels::intern(label).map_err(|_| String::from("intern failed"))?
    };
    credentials::set(slot, Cred::new(uid, uid, caps, id, 0));
    Ok(slot)
}

/// Close both channel handles a helper created in the current task.
fn close_pair(pair: (u64, u64)) {
    let _ = channels::close_endpoint(pair.0);
    let _ = channels::close_endpoint(pair.1);
}

/// Whether the newest audit event is a denial of `reason` by `label_id`.
fn last_denial(label_id: u32, reason: u32) -> Result<audit::AuditEvent, String> {
    let event = *audit::recent(1).first().ok_or("the audit ring is empty")?;
    check!(
        !event.allow && event.label_id == label_id && event.reason_code == reason,
        "the newest audit event is {event:?} (wanted a denial of label {label_id}, reason {reason})"
    );
    Ok(event)
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "label_intern_dedup_and_charset",
        identity::intern_dedup_and_charset,
    ),
    ("label_intern_capacity", identity::intern_capacity),
    ("label_gate_assigns_once", identity::gate_assigns_once),
    (
        "label_not_inherited_across_labelled_spawn",
        identity::not_inherited_across_labelled_spawn,
    ),
    ("label_gate_syscalls", identity::gate_syscalls),
    ("label_reserved_os_lazy_names", names::reserved_os_lazy),
    ("label_app_namespace_names", names::app_namespace),
    (
        "label_interfaces_own_domain_allowed",
        names::own_domain_interfaces,
    ),
    (
        "label_interfaces_foreign_denied",
        names::foreign_interfaces_denied,
    ),
    (
        "label_interfaces_unlabelled_and_system",
        names::interfaces_unlabelled_and_system,
    ),
    (
        "label_cross_app_resolve_needs_rule",
        names::cross_app_resolve,
    ),
    (
        "label_acl_load_gate_and_revoke",
        names::load_gate_and_revoke,
    ),
    ("label_topic_namespace_and_rules", calls::topic_namespace),
    (
        "label_session_topics_own_session_only",
        session_topics::own_session_only,
    ),
    ("label_session_topics_soak", session_topics::session_soak),
    ("label_calls_default_deny", calls::calls_default_deny),
    ("label_denial_audit_and_message", calls::denial_audit),
    (
        "label_soak_register_load_cycles",
        soak::register_load_cycles,
    ),
];
