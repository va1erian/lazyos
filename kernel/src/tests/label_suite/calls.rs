//! Topics, interface calls and audit records for labelled tasks.

use super::*;
use crate::ipc::acl::{reason, Rule};
use crate::ipc::{authorize, topics};

fn rule(interface_id: u64, method: u32, allow: bool) -> Rule {
    Rule {
        actor: 0,
        interface_id,
        method,
        allow,
    }
}

/// An app owns `app/<id>/` for publish and subscribe; nothing else is
/// reachable until rules for its label name the segments, and unlabelled
/// tasks are unaffected.
pub fn topic_namespace() -> Result<(), String> {
    fresh()?;
    let x = labelled_task("app:com.x", 1000, 0)?;
    let x_id = credentials::of(x).label_id;
    let legacy = labelled_task("", 1000, 0)?;
    let publish = topics::MODE_PUBLISH;
    let subscribe = topics::MODE_SUBSCRIBE;

    check!(
        topics::authorize(x, publish, "app/com.x/state/ready", 1) == Ok(4),
        "the app's own publish was refused"
    );
    check!(
        topics::authorize(x, publish, "app/com.x", 1).is_ok(),
        "the namespace root itself was refused"
    );
    check!(
        topics::authorize(x, subscribe, "app/com.x/#", 1).is_ok(),
        "the app's own subscription was refused"
    );
    check!(
        topics::authorize(x, publish, "app/com.y/state", 2) == Err(topics::Error::Denied),
        "an app published into another app's namespace"
    );
    last_denial(x_id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        topics::authorize(x, subscribe, "app/+/state", 2) == Err(topics::Error::Denied),
        "a wildcard filter crossed into other apps' namespaces"
    );
    check!(
        topics::authorize(x, publish, "system/events/power", 3) == Err(topics::Error::Denied),
        "an app published a system topic with no rule"
    );
    check!(
        topics::authorize(x, publish, "app.com.x/state", 3) == Err(topics::Error::Denied),
        "a lookalike first segment was accepted"
    );
    // A prefix of the id is not the id.
    check!(
        topics::authorize(x, publish, "app/com.x.y/state", 3) == Err(topics::Error::Denied),
        "an id-prefix topic was accepted"
    );

    // Rules for the label name literal segments of the topic.
    let segment = |text: &str| {
        rule(
            topics::PUBLISH_INTERFACE,
            topics::segment_method(text),
            true,
        )
    };
    acl::load_label(
        x_id,
        &[segment("system"), segment("events"), segment("power")],
    )
    .map_err(|_| "load failed")?;
    check!(
        topics::authorize(x, publish, "system/events/power", 4).is_ok(),
        "a rule-granted topic was refused"
    );
    check!(
        topics::authorize(x, subscribe, "system/events/power", 4) == Err(topics::Error::Denied),
        "a publish grant allowed subscribing"
    );
    acl::load_label(x_id, &[]).map_err(|_| "revoke failed")?;
    check!(
        topics::authorize(x, publish, "system/events/power", 5) == Err(topics::Error::Denied),
        "a revoked topic grant still works"
    );
    check!(
        topics::authorize(x, publish, "app/com.x/still", 5).is_ok(),
        "revoking rules removed the namespace grant"
    );

    // Unlabelled: the bootstrap window, exactly as before.
    check!(
        topics::authorize(legacy, publish, "system/events/power", 6).is_ok(),
        "an unlabelled task lost its bootstrap access"
    );
    Ok(())
}

/// Every interface call of a labelled task is default-deny; first match wins
/// among its label's rules; the registry's register/resolve/unregister are
/// the implicit exceptions (their names are checked separately) but listing
/// is not; uid rules never apply to a labelled task.
pub fn calls_default_deny() -> Result<(), String> {
    fresh()?;
    let x = labelled_task("app:com.x", 1000, 0)?;
    let x_id = credentials::of(x).label_id;
    let legacy = labelled_task("", 1000, 0)?;
    const IFACE: u64 = 0x7777;

    check!(
        authorize(x, IFACE, 5, 10).denied(),
        "a labelled call passed with no rules while the policy was empty"
    );
    last_denial(x_id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        !authorize(legacy, IFACE, 5, 10).denied(),
        "the unlabelled bootstrap window closed"
    );

    // A uid-keyed allow-everything rule does not help a labelled task.
    acl::load(&[Rule {
        actor: 1000,
        interface_id: acl::ANY_INTERFACE,
        method: acl::ANY_METHOD,
        allow: true,
    }]);
    check!(
        authorize(x, IFACE, 5, 11).denied(),
        "a uid rule granted a labelled task access"
    );
    check!(
        !authorize(legacy, IFACE, 5, 11).denied(),
        "the uid rule stopped working for unlabelled tasks"
    );
    acl::load(&[]);

    // Label rules: explicit deny first shadows the wildcard allow after it.
    acl::load_label(
        x_id,
        &[
            rule(IFACE, 6, false),
            rule(IFACE, acl::ANY_METHOD, true),
            rule(0x8888, 1, true),
        ],
    )
    .map_err(|_| "load failed")?;
    check!(
        !authorize(x, IFACE, 5, 12).denied(),
        "an allow rule was ignored"
    );
    check!(
        authorize(x, IFACE, 6, 12).denied(),
        "a deny rule lost to a later allow"
    );
    last_denial(x_id, reason::EXPLICIT_DENY)?;
    check!(
        !authorize(x, 0x8888, 1, 12).denied(),
        "second interface allow"
    );
    check!(
        authorize(x, 0x8888, 2, 12).denied(),
        "an exact rule matched another method"
    );
    check!(
        authorize(x, 0x9999, 1, 12).denied(),
        "an unlisted interface was allowed"
    );

    // Registry: the three name ops are attempted freely, listing is not.
    check!(
        !authorize(x, registry::INTERFACE, registry::method::REGISTER, 13).denied()
            && !authorize(x, registry::INTERFACE, registry::method::RESOLVE, 13).denied()
            && !authorize(x, registry::INTERFACE, registry::method::UNREGISTER, 13).denied(),
        "the implicit registry calls were denied"
    );
    check!(
        authorize(x, registry::INTERFACE, registry::method::LIST, 13).denied(),
        "a labelled task could list every service without a rule"
    );

    // Another label's rules are not this label's.
    let y = labelled_task("app:com.y", 1001, 0)?;
    check!(
        authorize(y, IFACE, 5, 14).denied(),
        "label x's rules applied to y"
    );
    Ok(())
}

/// A refusal lands in the audit ring with the label id and a machine-readable
/// reason, allows appear only while tracing, and the friendly text names the
/// namespace the app may use.
pub fn denial_audit() -> Result<(), String> {
    fresh()?;
    let x = labelled_task("app:com.x", 1000, 0)?;
    let x_id = credentials::of(x).label_id;

    let before = audit::denials();
    check!(authorize(x, 0x7777, 1, 77).denied(), "expected a denial");
    check!(
        audit::denials() == before + 1,
        "the denial counter did not move"
    );
    let event = last_denial(x_id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        event.interface_id == 0x7777
            && event.method == 1
            && event.txn_id == 77
            && event.uid == 1000,
        "the record is {event:?}"
    );

    acl::load_label(x_id, &[rule(0x7777, 1, true)]).map_err(|_| "load failed")?;
    let count = audit::count();
    check!(!authorize(x, 0x7777, 1, 78).denied(), "expected an allow");
    check!(
        audit::count() == count,
        "an allow was audited with tracing off"
    );
    audit::set_trace(true);
    check!(!authorize(x, 0x7777, 1, 79).denied(), "expected an allow");
    let event = *audit::recent(1).first().ok_or("no event")?;
    check!(
        event.allow && event.label_id == x_id && event.reason_code == reason::ALLOWED_BY_RULE,
        "the traced allow is {event:?}"
    );
    audit::set_trace(false);

    // Name denials record the registry method and the name hash.
    let denied = policy::check_name(x, policy::NameOp::Register, "os.lazy.evil");
    check!(denied.is_err(), "squatting was allowed");
    let event = last_denial(x_id, reason::RESERVED_NAMESPACE)?;
    check!(
        event.interface_id == registry::INTERFACE
            && event.method == registry::method::REGISTER
            && event.txn_id == topics::fnv1a64("os.lazy.evil"),
        "the name record is {event:?}"
    );

    let text = policy::explain(x_id, reason::RESERVED_NAMESPACE);
    check!(
        text.contains("app.com.x.<name>") && text.contains("app/com.x/"),
        "the friendly text does not name the namespace: {text}"
    );
    Ok(())
}
