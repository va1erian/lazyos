//! Topic ACL hooks (issue #92): stable segment hashes, per-segment
//! policy, wildcard filters, and the `authorize_topic` syscall gate.
//! The broker itself is userspace; these tests pin the kernel half of
//! the contract.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::ipc::syscalls::{errno, MsgArgs, MsgResult, OP_AUTHORIZE_TOPIC, REGISTRY_TARGET_SELF};
use crate::ipc::{acl, audit, topics};
use libmessenger::{Encoder, Header, Parcel, VERSION};

/// Scratch user address space for the syscall-level test: `dispatch`
/// validates pointers against the active CR3.
const SPACE: u64 = 0x0040_0000;

const SPACE_PAGES: u64 = 4;

const ARGS: u64 = SPACE;

const RESULT: u64 = SPACE + 0x100;

const REQUEST: u64 = SPACE + 0x1000;

/// Every topics test starts from the bring-up state: kernel task current,
/// root credentials, empty policy (the bootstrap window), empty audit ring.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    Ok(())
}

/// Run `f` with [`SPACE`] mapped into a fresh address space installed as
/// CR3, exactly as a real syscall from a user task would find it.
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

fn write_bytes(va: u64, bytes: &[u8]) {
    // Safety: the scratch pages are mapped writable while installed.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
}

fn read_bytes(va: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.resize(len, 0);
    // Safety: the scratch pages are mapped readable while installed.
    unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
    out
}

/// Run one op through the native gate with the args block at [`ARGS`].
fn dispatch(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
    write_bytes(ARGS, &args.to_bytes());
    let code = process::dispatch_for_test(5, op, ARGS, RESULT);
    let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
        .expect("the kernel wrote a malformed result block");
    (code, result)
}

/// Two's-complement `-errno` as the syscall returns it in `rax`.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Wrap an encoded `authorize_topic` body in a parcel and serialize it.
fn wrap_body(body: Vec<u8>) -> Result<Vec<u8>, String> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0,
            method: 0,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// Encode an `authorize_topic` request parcel with the generated codec.
fn auth_parcel(name: &str, mode: u32, txn: u64) -> Result<Vec<u8>, String> {
    let request = topics::publish_scope::AuthorizeTopicArgs {
        name: String::from(name),
        mode,
        txn,
    };
    let body = topics::publish_scope::encode_authorize_topic_args(&request)
        .map_err(|error| error.message())?;
    wrap_body(body)
}

/// Run `authorize_topic` on the caller itself with `request` (a whole parcel).
fn authorize_raw(request: &[u8]) -> (u64, MsgResult) {
    write_bytes(REQUEST, request);
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        parcel_ptr: REQUEST,
        parcel_len: request.len() as u64,
        ..MsgArgs::default()
    };
    dispatch(OP_AUTHORIZE_TOPIC, &args)
}

/// A policy that denies `method` on `interface` for uid 1000 ahead of an
/// allow-all rule, so neighbours and other actors stay allowed.
fn deny_rule(interface: u64, method: u32) -> [acl::Rule; 2] {
    [
        acl::Rule {
            actor: 1000,
            interface_id: interface,
            method,
            allow: false,
        },
        acl::Rule {
            actor: acl::ANY_ACTOR,
            interface_id: acl::ANY_INTERFACE,
            method: acl::ANY_METHOD,
            allow: true,
        },
    ]
}

/// The interface ids and segment methods are derived, not guessed: the
/// constants must keep matching the documented names, and the validation
/// rules must match the userspace broker.
pub fn segment_methods_stable() -> Result<(), String> {
    check!(
        topics::fnv1a64("os.lazy.messenger.topics.publish.v1") == topics::PUBLISH_INTERFACE,
        "the publish interface constant drifted from its name"
    );
    check!(
        topics::fnv1a64("os.lazy.messenger.topics.subscribe.v1") == topics::SUBSCRIBE_INTERFACE,
        "the subscribe interface constant drifted from its name"
    );
    check!(
        topics::PUBLISH_INTERFACE == topics::publish_scope::INTERFACE_ID
            && topics::SUBSCRIBE_INTERFACE == topics::subscribe_scope::INTERFACE_ID
            && topics::PUBLISH_INTERFACE != topics::SUBSCRIBE_INTERFACE,
        "the scope ids are not the generated interface ids"
    );
    check!(
        topics::MODE_PUBLISH == 0 && topics::MODE_SUBSCRIBE == 1,
        "the mode codes drifted from the IDL enum"
    );
    check!(
        topics::segment_method("system") == 1_226_705_564,
        "the segment hash for `system` changed: {}",
        topics::segment_method("system")
    );
    check!(
        topics::segment_method("+") != topics::segment_method("#")
            && topics::segment_method("+") != 0,
        "wildcard segment ids are not distinct"
    );
    check!(
        topics::interface(topics::MODE_SUBSCRIBE) == Some(topics::SUBSCRIBE_INTERFACE)
            && topics::interface(99).is_none(),
        "the mode-to-interface map is wrong"
    );

    check!(
        topics::validate("system/events/network/up", topics::MODE_PUBLISH) == Ok(4),
        "a literal four-segment topic was rejected"
    );
    check!(
        topics::validate("system/+", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
        "publish accepted a wildcard"
    );
    check!(
        topics::validate("system/+", topics::MODE_SUBSCRIBE) == Ok(2),
        "subscribe rejected a `+` segment"
    );
    check!(
        topics::validate("system/#", topics::MODE_SUBSCRIBE) == Ok(2),
        "subscribe rejected a trailing `#`"
    );
    check!(
        topics::validate("system/#/up", topics::MODE_SUBSCRIBE) == Err(topics::Error::BadName),
        "subscribe accepted a non-trailing `#`"
    );
    check!(
        topics::validate("", topics::MODE_PUBLISH) == Err(topics::Error::BadName)
            && topics::validate("a//b", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
        "empty segments were accepted"
    );
    check!(
        topics::validate("a/b/c/d/e/f/g/h/i", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
        "a nine-segment topic was accepted"
    );
    check!(
        topics::validate("system events", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
        "a topic with a space was accepted"
    );
    Ok(())
}

/// Policy is evaluated per segment: one denied segment blocks the whole
/// name, its neighbours pass, and the denial lands in the audit ring with
/// the broker's correlation id.
pub fn acl_segments_enforced() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    credentials::set(slot, Cred::new(1000, 100, 0, 0, 0));
    acl::load(&deny_rule(
        topics::PUBLISH_INTERFACE,
        topics::segment_method("secret"),
    ));

    let allowed = topics::authorize(slot, topics::MODE_PUBLISH, "public/data", 0x11)
        .map_err(|error| String::from(error.message()))?;
    check!(
        allowed == 2,
        "the allowed topic reported {allowed} segments"
    );

    let count_before = audit::count();
    let denied = topics::authorize(slot, topics::MODE_PUBLISH, "public/secret/data", 0xabc);
    check!(
        denied == Err(topics::Error::Denied),
        "a denied middle segment was allowed: {denied:?}"
    );
    check!(
        audit::count() == count_before + 1,
        "the denial did not reach the audit ring"
    );
    let event = *audit::recent(1)
        .first()
        .ok_or("the denial left no audit event")?;
    check!(
        event.uid == 1000
            && event.interface_id == topics::PUBLISH_INTERFACE
            && event.method == topics::segment_method("secret")
            && event.txn_id == 0xabc
            && !event.allow,
        "the audit event lost the topic segment or actor: {event:?}"
    );

    // A publish-mode rule must not leak into subscribe mode.
    check!(
        topics::authorize(slot, topics::MODE_SUBSCRIBE, "public/secret/data", 0).is_ok(),
        "the publish deny leaked into subscribe"
    );

    // Bad names and modes never touch policy.
    check!(
        topics::authorize(slot, topics::MODE_PUBLISH, "public/+", 0) == Err(topics::Error::BadName),
        "a publish wildcard reached policy"
    );
    check!(
        topics::authorize(slot, 9, "public/data", 0) == Err(topics::Error::BadMode),
        "an unknown mode reached policy"
    );
    Ok(())
}

/// Wildcard subscription segments are policy-checked like literals, so a
/// filter cannot bypass a namespace rule.
pub fn acl_wildcard_filter() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    credentials::set(slot, Cred::new(1000, 100, 0, 0, 0));
    acl::load(&deny_rule(
        topics::SUBSCRIBE_INTERFACE,
        topics::segment_method("#"),
    ));
    check!(
        topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/#", 0)
            == Err(topics::Error::Denied),
        "a `#` filter bypassed the wildcard deny"
    );
    check!(
        topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/events", 0).is_ok(),
        "the literal prefix was denied with the wildcard"
    );

    // `+` is a distinct method id and can be denied on its own.
    acl::load(&deny_rule(
        topics::SUBSCRIBE_INTERFACE,
        topics::segment_method("+"),
    ));
    check!(
        topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/+/up", 0)
            == Err(topics::Error::Denied),
        "a `+` filter bypassed the wildcard deny"
    );
    check!(
        topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/up", 0).is_ok(),
        "`+` denied a literal neighbour"
    );
    Ok(())
}

/// The syscall gate: allow returns the segment count, deny is `-EACCES`
/// with an audit record, proxy targets require `CAP_IPC_CONTROL`, and
/// malformed requests are `-EINVAL`.
pub fn syscall_gate() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        // Empty policy (the bootstrap window): the whole topic is allowed
        // and the op reports how many segments it checked.
        let request = auth_parcel("system/events", topics::MODE_PUBLISH, 0x5)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: REGISTRY_TARGET_SELF,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(code == 0, "authorize -> {code:#x}");
        check!(
            result.value == 2,
            "the gate reported {} segments",
            result.value
        );

        // A deny rule for one segment turns the whole request into -EACCES
        // and records the denial (with the request's correlation id). The
        // rule keys on uid 1000, so drop the caller's root identity.
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        acl::load(&deny_rule(
            topics::PUBLISH_INTERFACE,
            topics::segment_method("secret"),
        ));
        let count_before = audit::count();
        let request = auth_parcel("secret/data", topics::MODE_PUBLISH, 0x77)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: REGISTRY_TARGET_SELF,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(
            code == failed(errno::EACCES),
            "denied authorize -> {code:#x}"
        );
        check!(
            result.status == -errno::EACCES,
            "the denial status is {}",
            result.status
        );
        check!(
            audit::count() == count_before + 1,
            "the syscall denial was not audited"
        );

        // A proxy target needs CAP_IPC_CONTROL. The kernel task is root
        // with every cap by default, so drop to an unprivileged identity.
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        let request = auth_parcel("system/events", topics::MODE_PUBLISH, 0)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: 1,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(
            code == failed(errno::EPERM),
            "proxy without cap -> {code:#x}"
        );

        // With the cap the same call evaluates the other actor (root, so
        // the deny rule above does not match it).
        credentials::set(
            task::current(),
            Cred::new(1000, 100, credentials::CAP_IPC_CONTROL, 0, 0),
        );
        let args = MsgArgs {
            txn_id: 1,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(code == 0, "proxy with cap -> {code:#x}");
        check!(
            result.value == 2,
            "the proxy checked {} segments",
            result.value
        );

        // Malformed: publish wildcard, unknown mode, and an empty body.
        let request = auth_parcel("system/+", topics::MODE_PUBLISH, 0)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: REGISTRY_TARGET_SELF,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(
            code == failed(errno::EINVAL),
            "publish wildcard -> {code:#x}"
        );

        let request = auth_parcel("system/events", 9, 0)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            txn_id: REGISTRY_TARGET_SELF,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(code == failed(errno::EINVAL), "unknown mode -> {code:#x}");

        let args = MsgArgs {
            txn_id: REGISTRY_TARGET_SELF,
            ..MsgArgs::default()
        };
        let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
        check!(code == failed(errno::E2BIG), "empty request -> {code:#x}");
        Ok(())
    })
}

/// The request decoder is the generated one: unknown trailing fields are
/// ignored (forward compatibility), and a truncated body is `-EINVAL`, never
/// a panic or a silent allow.
pub fn gate_codec_edges() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let request = topics::publish_scope::AuthorizeTopicArgs {
            name: String::from("system/events"),
            mode: topics::MODE_PUBLISH,
            txn: 9,
        };
        let mut body = topics::publish_scope::encode_authorize_topic_args(&request)
            .map_err(|error| error.message())?;
        // Field 9 is unknown to the request layout and must be skipped.
        let mut extra = Encoder::new();
        extra.u64(9, 0xdead).map_err(|error| error.message())?;
        body.extend_from_slice(&extra.finish());
        let (code, result) = authorize_raw(&wrap_body(body.clone())?);
        check!(code == 0 && result.value == 2, "unknown field -> {code:#x}");

        // Every strict prefix of a valid body is either decoded as a shorter
        // valid body or refused; none may crash the gate.
        let whole = topics::publish_scope::encode_authorize_topic_args(&request)
            .map_err(|error| error.message())?;
        for cut in 0..whole.len() {
            let (code, _) = authorize_raw(&wrap_body(whole[..cut].to_vec())?);
            check!(
                code == 0 || code == failed(errno::EINVAL),
                "truncated at {cut} -> {code:#x}"
            );
        }
        // Cutting mid-field is a codec error, not an allow.
        let (code, _) = authorize_raw(&wrap_body(whole[..3].to_vec())?);
        check!(code == failed(errno::EINVAL), "mid-field cut -> {code:#x}");
        Ok(())
    })
}

/// Sustained load through the gate: thousands of allowed and denied requests
/// must keep answering correctly, audit exactly the denials, and leak
/// nothing (the ring is bounded, so the monotonic total is the invariant).
pub fn gate_stress() -> Result<(), String> {
    const ROUNDS: u64 = 3000;
    fresh()?;
    in_space(|| -> Result<(), String> {
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        // Deny only the *subscribe* scope's `secret`; publish stays allowed,
        // pinning that each mode authorizes on its own interface id.
        acl::load(&deny_rule(
            topics::SUBSCRIBE_INTERFACE,
            topics::segment_method("secret"),
        ));
        let allowed = auth_parcel("public/secret/x", topics::MODE_PUBLISH, 1)?;
        let denied = auth_parcel("public/secret/x", topics::MODE_SUBSCRIBE, 2)?;
        let before = audit::total();
        for round in 0..ROUNDS {
            let (code, result) = authorize_raw(&allowed);
            check!(
                code == 0 && result.value == 3,
                "round {round}: allowed publish -> {code:#x}"
            );
            let (code, _) = authorize_raw(&denied);
            check!(
                code == failed(errno::EACCES),
                "round {round}: denied subscribe -> {code:#x}"
            );
        }
        check!(
            audit::total() == before + ROUNDS,
            "expected {ROUNDS} audited denials, saw {}",
            audit::total() - before
        );
        Ok(())
    })
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_topic_segment_methods_stable", segment_methods_stable),
    ("ipc_topic_acl_segments_enforced", acl_segments_enforced),
    ("ipc_topic_acl_wildcard_filter", acl_wildcard_filter),
    ("ipc_topic_acl_syscall_gate", syscall_gate),
    ("ipc_topic_gate_codec_edges", gate_codec_edges),
    ("ipc_topic_gate_stress", gate_stress),
];
