//! Messenger credentials, ACL, and audit (issue #68).

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::ipc::{acl, audit};

const IFACE: u64 = 0x0102_0304_0506_0708;

const OTHER_IFACE: u64 = 0x1111_2222_3333_4444;

/// Every ACL test starts from the bring-up state: root credentials, empty
/// policy (the bootstrap window), empty audit ring, tracing off.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    credentials::reset_for_task(task::current());
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
    Ok(())
}

/// With a non-empty policy, a call that matches no rule is denied by
/// default, with friendly text and the default-deny reason code.
pub fn acl_default_deny() -> Result<(), String> {
    fresh()?;
    acl::load(&[acl::Rule {
        actor: 1000,
        interface_id: OTHER_IFACE,
        method: 1,
        allow: true,
    }]);
    let decision = acl::evaluate(2000, IFACE, 7);
    check!(decision.denied(), "an unmatched call was allowed");
    let reason = decision.reason().ok_or("denial carried no reason")?;
    check!(!reason.is_empty(), "the denial reason is empty");
    let (decision, code) = acl::evaluate_verdict(2000, IFACE, 7);
    check!(
        decision.denied(),
        "evaluate_verdict allowed an unmatched call"
    );
    check!(
        code == acl::reason::DEFAULT_DENY,
        "unmatched call has reason code {code}, expected default deny"
    );
    Ok(())
}

/// A matching allow rule permits exactly its `(actor, interface, method)`
/// triple; neighbours still fall through to default deny.
pub fn acl_allow_rule() -> Result<(), String> {
    fresh()?;
    acl::load(&[acl::Rule {
        actor: 1000,
        interface_id: IFACE,
        method: 7,
        allow: true,
    }]);
    check!(
        acl::evaluate(1000, IFACE, 7) == acl::Decision::Allow,
        "a matching allow rule was not honored"
    );
    check!(
        acl::evaluate(1001, IFACE, 7).denied(),
        "the rule leaked to a different uid"
    );
    check!(
        acl::evaluate(1000, IFACE, 8).denied(),
        "the rule leaked to a different method"
    );
    check!(
        acl::evaluate(1000, OTHER_IFACE, 7).denied(),
        "the rule leaked to a different interface"
    );
    Ok(())
}

/// An explicit deny rule wins over a later allow-all, and the wildcard
/// allow still covers actors the deny does not name.
pub fn acl_explicit_deny() -> Result<(), String> {
    fresh()?;
    acl::load(&[
        acl::Rule {
            actor: 1000,
            interface_id: IFACE,
            method: 7,
            allow: false,
        },
        acl::Rule {
            actor: acl::ANY_ACTOR,
            interface_id: acl::ANY_INTERFACE,
            method: acl::ANY_METHOD,
            allow: true,
        },
    ]);
    let (decision, code) = acl::evaluate_verdict(1000, IFACE, 7);
    check!(decision.denied(), "an explicit deny rule was overridden");
    check!(
        code == acl::reason::EXPLICIT_DENY,
        "explicit deny has reason code {code}"
    );
    check!(
        acl::evaluate(2000, IFACE, 7) == acl::Decision::Allow,
        "the wildcard allow rule was not honored"
    );
    Ok(())
}

/// Credentials default to root and `set`/`set_current` replace them
/// per slot; the ACL authority is the uid.
pub fn credentials_default_and_set() -> Result<(), String> {
    fresh()?;
    check!(
        credentials::of(task::current()) == Cred::ROOT,
        "default credentials are not root: {:?}",
        credentials::of(task::current())
    );
    check!(
        Cred::ROOT.uid == 0 && Cred::ROOT.has_cap(credentials::CAP_SYS_ADMIN),
        "root is not uid 0 with CAP_SYS_ADMIN"
    );
    let cred = Cred::new(1000, 100, credentials::CAP_AUDIT_READ, 7, 0xabc);
    credentials::set_current(cred);
    check!(
        credentials::of(task::current()) == cred,
        "set_current did not replace the credentials"
    );
    check!(
        credentials::of(task::current()).authority() == 1000,
        "the ACL authority is not the uid"
    );
    credentials::reset_for_task(task::current());
    check!(
        credentials::of(task::current()) == Cred::ROOT,
        "reset did not restore root"
    );
    Ok(())
}

/// `authorize` reads the actor's credentials, denies by default, and always
/// records the denial with its correlation id; the hash chain advances.
/// Untraced allows are not recorded; traced ones are.
pub fn authorize_denial_audited() -> Result<(), String> {
    fresh()?;
    let slot = task::current();
    credentials::set(slot, Cred::new(1000, 100, 0, 3, 0));
    acl::load(&[acl::Rule {
        actor: 1000,
        interface_id: OTHER_IFACE,
        method: 1,
        allow: true,
    }]);
    check!(!audit::trace(), "tracing starts enabled");

    let before = audit::last_hash();
    let count_before = audit::count();
    let decision = crate::ipc::authorize(slot, IFACE, 7, 0xfeed);
    check!(decision.denied(), "authorize allowed an unpermitted call");
    check!(
        audit::count() == count_before + 1,
        "authorize did not record the denial"
    );
    let event = *audit::recent(1)
        .first()
        .ok_or("the denial left no audit event")?;
    check!(
        event.uid == 1000 && event.actor_slot == slot && event.txn_id == 0xfeed,
        "the audit event lost the actor or correlation id: {event:?}"
    );
    check!(
        !event.allow,
        "the recorded denial says the call was allowed"
    );
    check!(
        event.reason_code == acl::reason::DEFAULT_DENY,
        "recorded reason code is {}",
        event.reason_code
    );
    let hash = audit::last_hash();
    check!(hash != before, "the hash chain did not advance");
    check!(
        hash == audit::chain(before, &event),
        "the chain head does not match the recorded event"
    );

    // An allow with tracing off is not recorded.
    credentials::set(slot, Cred::ROOT);
    acl::load(&[acl::Rule {
        actor: acl::ANY_ACTOR,
        interface_id: acl::ANY_INTERFACE,
        method: acl::ANY_METHOD,
        allow: true,
    }]);
    check!(
        !crate::ipc::authorize(slot, IFACE, 7, 1).denied(),
        "root was denied by an allow-all policy"
    );
    check!(
        audit::count() == count_before + 1,
        "an untraced allow was recorded"
    );

    // With tracing on, the allow is recorded too.
    audit::set_trace(true);
    check!(
        !crate::ipc::authorize(slot, IFACE, 7, 2).denied(),
        "the traced allow was denied"
    );
    check!(
        audit::count() == count_before + 2,
        "a traced allow was not recorded"
    );
    let last = *audit::recent(1).first().ok_or("no audit event")?;
    check!(
        last.allow && last.txn_id == 2,
        "the traced allow record is wrong: {last:?}"
    );
    Ok(())
}

/// The ring overwrites the oldest event when it wraps, always keeps the
/// newest entries in order, and the chain keeps advancing.
pub fn audit_ring_wraps() -> Result<(), String> {
    fresh()?;
    let extra = 3usize;
    for index in 0..(audit::AUDIT_CAPACITY + extra) {
        audit::record(audit::AuditEvent {
            ticks: index as u64,
            actor_slot: 1,
            uid: 1000,
            label_id: 0,
            interface_id: IFACE,
            method: index as u32,
            allow: false,
            reason_code: acl::reason::DEFAULT_DENY,
            txn_id: index as u64,
        });
    }
    check!(
        audit::count() == audit::AUDIT_CAPACITY,
        "the ring holds {} events, expected {}",
        audit::count(),
        audit::AUDIT_CAPACITY
    );
    check!(
        audit::total() == (audit::AUDIT_CAPACITY + extra) as u64,
        "the ring total is {}",
        audit::total()
    );
    let recent = audit::recent(extra);
    check!(
        recent.len() == extra,
        "recent returned {} events, expected {extra}",
        recent.len()
    );
    for (i, event) in recent.iter().enumerate() {
        let expected = (audit::AUDIT_CAPACITY + extra - 1 - i) as u64;
        check!(
            event.ticks == expected && event.txn_id == expected,
            "recent[{i}] is ticks {} txn {}, expected {expected}",
            event.ticks,
            event.txn_id
        );
    }
    check!(
        audit::recent(audit::AUDIT_CAPACITY + 10).len() == audit::AUDIT_CAPACITY,
        "recent returned more events than the ring holds"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_acl_default_deny", acl_default_deny),
    ("ipc_acl_allow_rule", acl_allow_rule),
    ("ipc_acl_explicit_deny", acl_explicit_deny),
    (
        "ipc_credentials_default_and_set",
        credentials_default_and_set,
    ),
    ("ipc_authorize_denial_audited", authorize_denial_audited),
    ("ipc_audit_ring_wraps", audit_ring_wraps),
];
