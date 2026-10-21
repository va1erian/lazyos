//! Developer-tool labels: children and parentage, the dev-spawn scope and the
//! topic verdicts (`kernel/src/ipc/{children,devspawn,tool}.rs`). The label
//! table and the rule engine are in `tool.rs`.

use super::tool::*;
use super::*;
use crate::ipc::acl::{reason, Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::credentials::{LabelStamp, TransitionError, CAP_SETUID};
use crate::ipc::policy::NameOp;
use crate::ipc::topics::{self, fnv1a32, fnv1a64};
use crate::ipc::{authorize, children, devspawn};

/// Children and parentage: a child of a tool task keeps the tool label, a tool
/// is never a parent, an `execve` binding of the app of the same id does not
/// reach a tool task, and a tool cannot hop to another label.
pub fn children_inherit_and_never_parent() -> Result<(), String> {
    fresh()?;
    children::reset_for_tests();
    let outcome = in_space(children_cases);
    children::reset_for_tests();
    outcome
}

fn children_cases() -> Result<(), String> {
    let (tool, tool_id) = tool_task(TOOL, 1000)?;
    let child = labelled_task("", 1000, 0)?;
    credentials::inherit(tool, child);
    check!(
        credentials::of(child).label_id == tool_id,
        "a child of a tool lost the label"
    );
    check!(
        name_reason(child, NameOp::Register, "demo.pirate.child")?
            == Ok(reason::ALLOWED_BY_NAMESPACE),
        "the child of a tool has less than the tool"
    );
    check!(
        authorize(child, fnv1a64(DENIED[0]), 1, 1).denied(),
        "the child of a tool reached a denied interface"
    );
    check!(labels::parent_of(tool_id).is_none(), "tool has a parent");

    // An app of the same id declares a helper; its program does not enter for
    // the tool task.
    let rules = [policy::wire::LabelRule {
        interface_id: ANY_INTERFACE,
        method: ANY_METHOD,
        allow: true,
    }];
    check!(load_current(APP, &rules)? == 0, "load the app");
    check!(
        load_with_program("app:demo.pirate/helper", &[], Some("/system/bin/helper"))? == 0,
        "bind the app's helper"
    );
    let helper = labels::lookup("app:demo.pirate/helper").ok_or("helper")?;
    let app_cred = Cred::new(1000, 1000, 0, labels::lookup(APP).ok_or("app")?, 0);
    check!(
        children::label_for_exec(&app_cred, "/system/bin/helper") == Some(helper),
        "the app does not enter its own helper"
    );
    check!(
        children::label_for_exec(&credentials::of(tool), "/system/bin/helper").is_none(),
        "a tool task found a child to enter"
    );
    check!(
        children::enter_on_exec(tool, &["/system/bin/helper"]) == Ok(None),
        "execve moved a tool task"
    );
    check!(
        credentials::of(tool).label_id == tool_id && !children::is_child(tool),
        "the tool label changed"
    );
    check!(
        children::may_start(tool, "execve") && children::may_start(tool, "fork"),
        "a tool may not start programs"
    );
    let forged = Cred {
        label_id: helper,
        ..credentials::of(tool)
    };
    check!(
        credentials::check_stamp(tool, LabelStamp::Child, forged)
            == Err(TransitionError::LabelLocked),
        "a tool stamped itself with a child label"
    );
    check!(
        children::unbind_children_of(tool_id).is_empty(),
        "a tool owns bindings"
    );

    // Revoking the tool leaves the app's children alone.
    let bound = children::count();
    check!(load_current(TOOL, &[])? == 0, "revoke the tool");
    check!(
        children::count() == bound,
        "revoking a tool unbound children"
    );

    // No self-relabelling, with or without the credential capability.
    let other = labels::intern(OTHER_TOOL).map_err(|_| "intern")?;
    let hop = Cred {
        label_id: other,
        ..credentials::of(tool)
    };
    check!(
        credentials::check_stamp(tool, LabelStamp::Assign, hop)
            == Err(TransitionError::NotPrivileged),
        "a tool without CAP_SETUID relabelled itself"
    );
    credentials::set(tool, Cred::new(1000, 1000, CAP_SETUID, tool_id, 0));
    let hop = Cred {
        label_id: other,
        ..credentials::of(tool)
    };
    check!(
        credentials::check_stamp(tool, LabelStamp::Assign, hop)
            == Err(TransitionError::LabelLocked),
        "a tool hopped to another label"
    );
    Ok(())
}

/// Spawning into `dev:` is the app's rule: the tool's rules must allow the
/// spawn scope (the deny list refuses it, `develop = true` leaves it out), the
/// target must be approved, and the child never gains anything.
pub fn dev_spawn_scope_still_required() -> Result<(), String> {
    fresh()?;
    let dev = "dev:org.test.proj";
    let dev_id = labels::intern(dev).map_err(|_| "intern dev")?;
    acl::load_label(dev_id, &[deny_rule(ANY_INTERFACE)]).map_err(|e| format!("{e:?}"))?;
    let requested = Cred::new(1000, 1000, 0, 0, 0);

    // Deny list as shipped: the scope is refused.
    let (tool, tool_id) = tool_task(TOOL, 1000)?;
    let actor = credentials::of(tool);
    check!(devspawn::applies(&actor, dev), "dev: target not recognised");
    check!(
        !devspawn::applies(&actor, "app:org.test.proj")
            && !devspawn::applies(&actor, "tool:org.test.proj"),
        "a non-dev target took the dev path"
    );
    check!(
        devspawn::approve(tool, requested, dev) == Err(devspawn::Refusal::NotAllowed),
        "a tool spawned into dev: despite the deny"
    );
    let event = *audit::recent(1).first().ok_or("no audit record")?;
    check!(
        !event.allow
            && event.label_id == tool_id
            && event.interface_id == devspawn::SPAWN_SCOPE
            && event.method == fnv1a32(dev),
        "the refusal was audited as {event:?}"
    );

    check!(
        fnv1a64(super::tool::UNLESS_DEVELOP[0]) == devspawn::SPAWN_SCOPE,
        "the stand-in's `develop` entry is not the dev-spawn scope"
    );
    // `develop = true`: no deny for the scope, so the wildcard allows it.
    let without_scope: Vec<u64> = default_denied()
        .into_iter()
        .filter(|&id| id != devspawn::SPAWN_SCOPE)
        .collect();
    acl::load_label(tool_id, &tool_rules(&without_scope)).map_err(|e| format!("{e:?}"))?;
    let stamped = devspawn::approve(tool, requested, dev).map_err(|e| format!("{e:?}"))?;
    check!(stamped.label_id == dev_id, "the child got another label");
    check!(
        devspawn::recheck(&actor, &stamped).is_ok(),
        "the re-check at application failed"
    );
    // Still no widening of the identity, and no unapproved target.
    let widened = Cred::new(1001, 1000, 0, 0, 0);
    check!(
        devspawn::approve(tool, widened, dev) == Err(devspawn::Refusal::Widening),
        "a dev spawn changed the uid"
    );
    labels::intern("dev:org.test.never").map_err(|_| "intern")?;
    check!(
        devspawn::approve(tool, requested, "dev:org.test.never")
            == Err(devspawn::Refusal::NotAllowed),
        "an unapproved dev label was entered"
    );
    check!(
        devspawn::approve(tool, requested, "dev:org.test.unknown")
            == Err(devspawn::Refusal::NotAllowed),
        "an unknown dev label was entered"
    );
    // Revoked tool: the scope is no longer allowed.
    acl::load_label(tool_id, &[]).map_err(|e| format!("{e:?}"))?;
    check!(
        devspawn::approve(tool, requested, dev) == Err(devspawn::Refusal::NotAllowed),
        "a revoked tool spawned into dev:"
    );
    Ok(())
}

/// The topic matrix. `user/<uid>/` is the uid's, the tool's own `app/<id>/`
/// is granted, other apps' `app/...`, root wildcards and `system/` publishing
/// are reserved, the rest is the rules'; an unlabelled task is the control.
pub fn topic_verdicts() -> Result<(), String> {
    fresh()?;
    let (tool, id) = tool_task(TOOL, 1000)?;
    let legacy = labelled_task("", 1000, 0)?;
    let publish = topics::MODE_PUBLISH;
    let subscribe = topics::MODE_SUBSCRIBE;
    let ok = |slot, mode, name: &str| topics::authorize(slot, mode, name, 1).is_ok();

    for (mode, name) in [
        (publish, "app/demo.pirate/state"),
        (subscribe, "app/demo.pirate/#"),
        (publish, "playground/x"),
        (publish, "demo/chat/lobby"),
        (subscribe, "demo/#"),
        (subscribe, "demo/+/lobby"),
        (subscribe, "system/events/#"),
        (subscribe, "system"),
        // Reading the clock's tick is fine, and only the exact root `time`
        // is the clock's: `timeline` and `times/x` are free names.
        (subscribe, "time/tick"),
        (subscribe, "time/#"),
        (publish, "timeline/x"),
        (publish, "times/tick"),
        (publish, "user/1000/confd/changed/x"),
        (publish, "applications/x"),
        (publish, "apple"),
    ] {
        check!(ok(tool, mode, name), "{name} (mode {mode}) was refused");
    }
    let reserved = [
        (publish, "app/com.other/state"),
        (subscribe, "app/com.other/#"),
        (subscribe, "app/+/state"),
        (subscribe, "app/#"),
        (publish, "app/demo.pirate.x/state"),
        (publish, "app"),
        (publish, "system/events/power"),
        (publish, "system"),
        // `timed`'s retained tick: a tool could otherwise set every clock
        // reader's idea of the time.
        (publish, "time/tick"),
        (publish, "time"),
    ];
    for (mode, name) in reserved {
        check!(
            topics::authorize(tool, mode, name, 2) == Err(topics::Error::Denied),
            "{name} (mode {mode}) was reachable by a tool"
        );
        last_denial(id, reason::RESERVED_NAMESPACE)?;
    }
    // Another uid's private tree is the uid's, whatever the rules say.
    for (mode, name) in [
        (publish, "user/1001/x"),
        (subscribe, "user/+/x"),
        (subscribe, "user/#"),
        (publish, "user/01000/x"),
        (subscribe, "#"),
        (subscribe, "+/state"),
    ] {
        check!(
            topics::authorize(tool, mode, name, 3) == Err(topics::Error::Denied),
            "{name} was reachable by a tool"
        );
        last_denial(id, reason::PRIVATE_NAMESPACE)?;
    }
    // A topic-level deny ahead of the wildcard binds a tool like an app.
    let publish_segment = |text: &str| Rule {
        actor: 0,
        interface_id: topics::PUBLISH_INTERFACE,
        method: topics::segment_method(text),
        allow: false,
    };
    acl::load_label(
        id,
        &[
            publish_segment("secret"),
            allow_rule(ANY_INTERFACE, ANY_METHOD),
        ],
    )
    .map_err(|e| format!("{e:?}"))?;
    check!(
        topics::authorize(tool, publish, "secret/x", 4) == Err(topics::Error::Denied),
        "a topic deny lost to the wildcard"
    );
    last_denial(id, reason::EXPLICIT_DENY)?;
    check!(
        ok(tool, subscribe, "secret/x"),
        "the deny covered subscribing"
    );

    // Root passes the per-uid rule, so the tool rule is what holds a root
    // tool's root wildcards back.
    credentials::set(tool, Cred::new(0, 0, 0, id, 0));
    for name in ["#", "+/state", "app/#"] {
        check!(
            topics::authorize(tool, subscribe, name, 5) == Err(topics::Error::Denied),
            "a root tool subscribed to {name}"
        );
        last_denial(id, reason::RESERVED_NAMESPACE)?;
    }
    check!(ok(tool, subscribe, "demo/#"), "a root tool lost demo/#");
    credentials::set(tool, Cred::new(1000, 1000, 0, id, 0));

    // Revoked: only the tool's own tree is left.
    acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
    check!(
        topics::authorize(tool, publish, "playground/x", 6) == Err(topics::Error::Denied),
        "a revoked tool published"
    );
    last_denial(id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        ok(tool, publish, "app/demo.pirate/still"),
        "revoking removed the namespace grant"
    );

    // Controls: the unlabelled task is not limited by this module.
    check!(
        ok(legacy, publish, "app/com.other/state") && ok(legacy, publish, "system/events/power"),
        "the unlabelled bootstrap window changed"
    );
    Ok(())
}
