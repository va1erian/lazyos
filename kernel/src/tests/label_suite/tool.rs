//! Developer-tool labels (`tool:<id>`, `kernel/src/ipc/tool.rs`,
//! `docs/devtool-packages-plan.md` T2): the label grammar, the rule engine
//! with a default-allow list, the name and topic verdicts, children and the
//! dev-spawn scope. The soaks are in `tool_soak.rs`.
//!
//! The rules are what `pkgstore` compiles for a tool: explicit denies for the
//! credential-class interfaces, then `allow ANY_INTERFACE/ANY_METHOD`.

use super::*;
use crate::ipc::acl::{reason, Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::credentials::{LabelStamp, TransitionError, CAP_SETUID};
use crate::ipc::labels::{Error as LabelError, Kind};
use crate::ipc::policy::NameOp;
use crate::ipc::topics::{self, fnv1a32, fnv1a64};
use crate::ipc::{authorize, children, confine, devspawn};

const EINVAL: i64 = 22;
const EACCES: i64 = 13;

pub(super) const TOOL: &str = "tool:demo.pirate";
pub(super) const OTHER_TOOL: &str = "tool:demo.other";
pub(super) const APP: &str = "app:demo.pirate";
pub(super) const OTHER_APP: &str = "app:com.other";

/// A stand-in for `devtool::DENY` (`libs/devtool`, the deny list `pkgd` compiles
/// into a tool's rules; the kernel does not depend on that crate). It names
/// exactly the interfaces of the real list (a plain tool, no declaration):
/// `tools/devtool/check_interfaces.py`, a CI step, compares the two.
pub(super) const DENIED: [&str; 8] = [
    "os.lazy.keyd.v1",
    "os.lazy.keyd.appsecret.v1",
    "os.lazy.accounts.v1",
    "os.lazy.process.label.spawn.v1",
    "os.lazy.net.socket.v1",
    "os.lazy.net.stack.v1",
    "os.lazy.mount.v1",
    "os.lazy.messenger.policy.v1",
];

/// The `when` column of the stand-in, one list per condition that lifts an
/// entry (`devtool::DenyWhen`): `UnlessNetwork`, `UnlessDevelop`, `UnlessShell`.
/// An entry of `DENIED` in none of them is `Always`. `check_interfaces.py`
/// compares each list with the entries of `devtool::DENY` of that `when`, so a
/// change to which declaration lifts which entry cannot leave these tests
/// judging a different tool than the one `pkgstore` compiles.
pub(super) const UNLESS_NETWORK: [&str; 3] = [
    "os.lazy.net.socket.v1",
    "os.lazy.net.stack.v1",
    "os.lazy.mount.v1",
];
pub(super) const UNLESS_DEVELOP: [&str; 1] = ["os.lazy.process.label.spawn.v1"];
pub(super) const UNLESS_SHELL: [&str; 3] = [
    "os.lazy.keyd.v1",
    "os.lazy.keyd.appsecret.v1",
    "os.lazy.accounts.v1",
];

pub(super) fn deny_rule(interface_id: u64) -> Rule {
    Rule {
        actor: 0,
        interface_id,
        method: ANY_METHOD,
        allow: false,
    }
}

pub(super) fn allow_rule(interface_id: u64, method: u32) -> Rule {
    Rule {
        actor: 0,
        interface_id,
        method,
        allow: true,
    }
}

/// The compiled tool list: denies in order, then the wildcard allow.
pub(super) fn tool_rules(denied: &[u64]) -> Vec<Rule> {
    let mut rules: Vec<Rule> = denied.iter().map(|&id| deny_rule(id)).collect();
    rules.push(allow_rule(ANY_INTERFACE, ANY_METHOD));
    rules
}

pub(super) fn default_denied() -> Vec<u64> {
    DENIED.iter().map(|name| fnv1a64(name)).collect()
}

/// A live task of `label` with the user's identity and the tool's rules loaded.
pub(super) fn tool_task(label: &str, uid: u32) -> Result<(usize, u32), String> {
    let slot = labelled_task(label, uid, 0)?;
    let id = credentials::of(slot).label_id;
    acl::load_label(id, &tool_rules(&default_denied())).map_err(|e| format!("load: {e:?}"))?;
    Ok((slot, id))
}

/// `check_name`'s verdict as the reason the audit record carries (allows are
/// audited with tracing on): `Ok(reason)` or `Err(reason)`.
pub(super) fn name_reason(slot: usize, op: NameOp, name: &str) -> Result<Result<u32, u32>, String> {
    audit::set_trace(true);
    let verdict = policy::check_name(slot, op, name);
    audit::set_trace(false);
    let event = *audit::recent(1).first().ok_or("the audit ring is empty")?;
    match verdict {
        Ok(()) => {
            check!(event.allow, "{name}: allowed, audited as {event:?}");
            Ok(Ok(event.reason_code))
        }
        Err(denied) => {
            check!(
                !event.allow && event.reason_code == denied.reason_code,
                "{name}: denied {denied:?}, audited as {event:?}"
            );
            Ok(Err(denied.reason_code))
        }
    }
}
/// `tool:<id>` is a label form of its own: the grammar of `app:` without the
/// child suffix, equal strings share an id, `tool:x` and `app:x` are two
/// labels, and no tool ever has a parent.
pub fn grammar_and_interning() -> Result<(), String> {
    fresh()?;
    let (kind, name) = labels::parse("tool:org.lazy.playground").map_err(|_| "parse")?;
    check!(
        kind == Kind::Tool && name == "org.lazy.playground",
        "parsed as {kind:?} {name:?}"
    );
    let tool = labels::intern(TOOL).map_err(|_| "intern tool")?;
    let again = labels::intern(TOOL).map_err(|_| "re-intern tool")?;
    let app = labels::intern(APP).map_err(|_| "intern app")?;
    check!(tool == again, "an equal tool label got a second id");
    check!(tool != app, "tool:x and app:x share an id");
    check!(
        labels::kind_of(tool) == Some(Kind::Tool) && labels::kind_of(app) == Some(Kind::App),
        "kinds"
    );
    check!(
        labels::name_of(tool).as_deref() == Some(TOOL),
        "the id does not round-trip"
    );
    check!(labels::parent_of(tool).is_none(), "a tool has a parent");
    check!(labels::lookup(TOOL) == Some(tool), "lookup");
    check!(labels::count() == 2, "count is {}", labels::count());

    let too_long = format!("tool:{}", "a".repeat(labels::MAX_LABEL_BYTES - 4));
    let longest = format!("tool:{}", "a".repeat(labels::MAX_LABEL_BYTES - 5));
    let bad = [
        "tool:",
        "tool",
        "Tool:demo.x",
        "TOOL:demo.x",
        "tool:Demo.x",
        "tool:demo_x",
        "tool:demo x",
        "tool:.demo",
        "tool:demo.",
        "tool:-demo",
        "tool:demo-",
        "tool:demo..x",
        "tool:demo:x",
        "tool: demo",
        "tool:\u{e9}",
        "toolx:demo.x",
        "tools:demo.x",
        // A tool has no children, with or without the app interned.
        "tool:demo.pirate/helper",
        "tool:demo.pirate/",
        "tool:/helper",
        "tool:demo.pirate/a/b",
        &too_long,
    ];
    for label in bad {
        check!(
            labels::intern(label) == Err(LabelError::Malformed),
            "{label:?} was accepted"
        );
        check!(
            !matches!(labels::parse(label), Ok((Kind::Tool, _))),
            "{label:?} parsed as a tool"
        );
    }
    check!(
        labels::intern(&longest).is_ok(),
        "a tool label of exactly {} bytes was refused",
        labels::MAX_LABEL_BYTES
    );
    check!(labels::count() == 3, "a bad label changed the table");
    check!(
        labels::parent_label("tool:demo.pirate/helper").is_none(),
        "a tool label has a parent label"
    );

    // A child of the *app* `demo.pirate` attaches to the app, never to the
    // tool of the same id.
    check!(
        labels::intern("app:demo.pirate/helper").is_ok(),
        "child of an interned app"
    );
    let child = labels::lookup("app:demo.pirate/helper").ok_or("child")?;
    check!(
        labels::parent_of(child) == Some(app) && labels::parent_of(child) != Some(tool),
        "the child is linked to the wrong label"
    );
    check!(
        labels::child_label_count(tool) == 0,
        "the tool counts children"
    );
    fresh()?;
    check!(
        labels::intern("app:demo.pirate/helper") == Err(LabelError::NoParent),
        "a tool label stood in for the app of a child"
    );
    labels::intern(TOOL).map_err(|_| "intern tool")?;
    check!(
        labels::intern("app:demo.pirate/helper") == Err(LabelError::NoParent),
        "an interned tool made a child label possible"
    );
    Ok(())
}

/// The table is shared and bounded: tool labels fill it like any, a full table
/// refuses a new tool and keeps serving the existing ones.
pub fn table_capacity() -> Result<(), String> {
    fresh()?;
    let mut first = 0;
    for index in 0..labels::CAPACITY {
        let id = labels::intern(&format!("tool:cap.fill{index}")).map_err(|_| "fill failed")?;
        if index == 0 {
            first = id;
        }
    }
    check!(labels::count() == labels::CAPACITY, "table not full");
    check!(
        labels::intern("tool:cap.overflow") == Err(LabelError::TableFull),
        "a full table accepted a new tool label"
    );
    check!(
        labels::intern("app:cap.overflow") == Err(LabelError::TableFull),
        "a full table accepted an app label"
    );
    check!(
        labels::intern("tool:cap.fill0") == Ok(first),
        "an existing tool label stopped resolving"
    );
    check!(
        labels::kind_of(first) == Some(Kind::Tool),
        "the kind of a stored tool"
    );
    // The loader reports the same condition as ENOMEM and changes nothing.
    let loaded = in_space(|| load_current("tool:cap.overflow", &[]))?;
    check!(
        loaded == failed(12),
        "OP_ACL_LOAD of a tool label in a full table -> {loaded:#x}"
    );
    check!(
        labels::count() == labels::CAPACITY,
        "a refused load changed the count"
    );
    fresh()
}

/// A tool's list through the real loader: accepted for `tool:` labels,
/// replaced as a whole, revoked by an empty list; a program, a child form and
/// more than the per-label bound are `EINVAL` and leave nothing behind.
pub fn load_through_the_loader() -> Result<(), String> {
    fresh()?;
    in_space(load_cases)
}

fn load_cases() -> Result<(), String> {
    let rules: Vec<policy::wire::LabelRule> = DENIED
        .iter()
        .map(|name| policy::wire::LabelRule {
            interface_id: fnv1a64(name),
            method: ANY_METHOD,
            allow: false,
        })
        .chain(core::iter::once(policy::wire::LabelRule {
            interface_id: ANY_INTERFACE,
            method: ANY_METHOD,
            allow: true,
        }))
        .collect();
    check!(
        load_current(TOOL, &rules)? == 0,
        "the tool's rules were refused"
    );
    let id = labels::lookup(TOOL).ok_or("the tool label was not interned")?;
    check!(
        labels::kind_of(id) == Some(Kind::Tool),
        "interned as another kind"
    );
    check!(
        acl::label_rule_count(id) == rules.len(),
        "{} rules stored",
        acl::label_rule_count(id)
    );
    check!(
        load_with_program(TOOL, &rules, Some("/system/bin/x"))? == failed(EINVAL),
        "a tool label took a program"
    );
    check!(
        load_current("tool:demo.pirate/helper", &[])? == failed(EINVAL),
        "a tool child form was accepted"
    );
    check!(
        load_current("tool:", &[])? == failed(EINVAL),
        "an empty tool name was accepted"
    );
    let too_many = vec![
        policy::wire::LabelRule {
            interface_id: 1,
            method: 1,
            allow: false,
        };
        acl::MAX_RULES_PER_LABEL + 1
    ];
    check!(
        load_current(TOOL, &too_many)? == failed(EINVAL),
        "an oversized list was accepted"
    );
    check!(
        acl::label_rule_count(id) == rules.len(),
        "a refused load changed the stored rules"
    );
    check!(labels::count() == 1, "refused loads interned labels");

    // Replace, not append.
    check!(load_current(TOOL, &rules[..2])? == 0, "replace");
    check!(acl::label_rule_count(id) == 2, "the replaced list was kept");
    // Revoke.
    check!(load_current(TOOL, &[])? == 0, "revoke");
    check!(acl::label_rule_count(id) == 0, "rules survived the revoke");
    Ok(())
}

/// Rules are rules: denies ahead of the wildcard win, the wildcard decides the
/// rest, a method-level deny narrows only that method, an empty list is a
/// revocation, another label's list is not this one's, and the label is never
/// confined by a wildcard.
pub fn default_allow_rules() -> Result<(), String> {
    fresh()?;
    let (tool, id) = tool_task(TOOL, 1000)?;
    let app = labelled_task(OTHER_APP, 1000, 0)?;
    let app_id = credentials::of(app).label_id;
    for name in DENIED {
        check!(
            authorize(tool, fnv1a64(name), 3, 1).denied(),
            "{name} was reachable by a tool"
        );
        last_denial(id, reason::EXPLICIT_DENY)?;
    }
    // Each `when` list names entries of the deny list, and no entry twice.
    let columns = [&UNLESS_NETWORK[..], &UNLESS_DEVELOP[..], &UNLESS_SHELL[..]];
    for (index, column) in columns.iter().enumerate() {
        for name in *column {
            check!(DENIED.contains(name), "{name} is in a `when` list only");
            let others = columns
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .any(|(_, list)| list.contains(name));
            check!(!others, "{name} is in two `when` lists");
        }
    }
    for name in ["os.lazy.confd.v1", "os.lazy.echo.v1", "demo.pirate.chat.v1"] {
        check!(
            !authorize(tool, fnv1a64(name), 9, 2).denied(),
            "{name} was refused by the wildcard"
        );
    }
    check!(
        authorize(app, fnv1a64("os.lazy.confd.v1"), 9, 2).denied(),
        "the tool's wildcard leaked to an app"
    );
    last_denial(app_id, reason::LABEL_DEFAULT_DENY)?;

    // A method-level deny ahead of the wildcard narrows that method only.
    let confd = fnv1a64("os.lazy.confd.v1");
    acl::load_label(
        id,
        &[
            Rule {
                actor: 0,
                interface_id: confd,
                method: 5,
                allow: false,
            },
            allow_rule(ANY_INTERFACE, ANY_METHOD),
        ],
    )
    .map_err(|e| format!("{e:?}"))?;
    check!(
        authorize(tool, confd, 5, 3).denied() && !authorize(tool, confd, 6, 3).denied(),
        "a method deny was not exact"
    );

    // The ids the label is allowed with the wildcard method: the first rule
    // naming an id decides it and the wildcard ends the list.
    check!(
        acl::label_allowed_ids(id).is_empty(),
        "a deny-only prefix allowed ids"
    );
    acl::load_label(
        id,
        &[
            allow_rule(10, ANY_METHOD),
            deny_rule(11),
            allow_rule(11, ANY_METHOD),
            allow_rule(ANY_INTERFACE, ANY_METHOD),
            allow_rule(12, ANY_METHOD),
        ],
    )
    .map_err(|e| format!("{e:?}"))?;
    check!(
        acl::label_allowed_ids(id) == [10],
        "allowed ids are {:?}",
        acl::label_allowed_ids(id)
    );

    // A wildcard allow is not the confinement marker.
    check!(!confine::confined(id), "a tool label is confined");

    // Revoke: every call is denied again, the implicit registry calls apart.
    acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
    check!(
        authorize(tool, confd, 6, 4).denied(),
        "a revoked tool still calls"
    );
    last_denial(id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        !authorize(tool, registry::INTERFACE, registry::method::RESOLVE, 4).denied(),
        "the implicit registry calls were refused"
    );
    check!(
        authorize(tool, registry::INTERFACE, registry::method::LIST, 4).denied(),
        "a revoked tool listed every service"
    );
    Ok(())
}
