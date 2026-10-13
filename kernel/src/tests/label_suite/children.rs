//! Child labels (issue #832, `kernel/src/ipc/children.rs`): `app:<id>/<name>`
//! labels for an installed app's helper programs.
//!
//! The app plays a browser: `app:org.test.browser` holds the display and the
//! network, and declares a page process (`/system/bin/webprocess`, no
//! interface at all) and a network process (`/system/bin/networkprocess`,
//! just the network). The loader (`OP_ACL_LOAD`) is the real one; the entering
//! `execve` is exercised through the function `sys_execve` calls
//! ([`children::enter_on_exec`]), since a successful `execve` would replace
//! the test task's image.

use super::*;
use crate::ipc::acl::{reason, Rule};
use crate::ipc::credentials::{LabelStamp, TransitionError};
use crate::ipc::labels::{Error as LabelError, Kind};
use crate::ipc::topics::{self, fnv1a32, fnv1a64};
use crate::ipc::{authorize, children, devspawn};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EACCES: i64 = 13;
const EINVAL: i64 = 22;

const APP: &str = "app:org.test.browser";
const OTHER: &str = "app:org.test.other";
const WEB: &str = "app:org.test.browser/webprocess";
const NET: &str = "app:org.test.browser/networkprocess";
const OTHER_WEB: &str = "app:org.test.other/webprocess";
const WEB_PATH: &str = "/system/bin/webprocess";
const NET_PATH: &str = "/system/bin/networkprocess";

const DISPLAY: &str = "os.lazy.display.v1";
const SOCKETS: &str = "os.lazy.net.socket.v1";
const CLIPBOARD: &str = "os.lazy.clipboard.v1";
/// The service name the socket interface is also served under.
const STACK: &str = "os.lazy.net.stack";

const UID: u32 = 1000;
const SESSION: u64 = 7;
/// A capability the app's tasks hold, to see the child keeps it as it is.
const CAPS: u32 = credentials::CAP_KILL;

fn any(interface: &str) -> policy::wire::LabelRule {
    policy::wire::LabelRule {
        interface_id: fnv1a64(interface),
        method: u32::MAX,
        allow: true,
    }
}

fn resolve(name: &str) -> policy::wire::LabelRule {
    policy::wire::LabelRule {
        interface_id: policy::RESOLVE_SCOPE,
        method: fnv1a32(name),
        allow: true,
    }
}

fn kernel_rule(interface: u64, method: u32) -> Rule {
    Rule {
        actor: 0,
        interface_id: interface,
        method,
        allow: true,
    }
}

/// The bring-up state of the label suite, and no program bound.
fn fresh_children() -> Result<(), String> {
    fresh()?;
    children::reset_for_tests();
    Ok(())
}

/// Load the app's own label (display, sockets and the name the socket
/// interface is served under) through the real loader.
fn load_app() -> Result<u32, String> {
    let rules = [any(DISPLAY), any(SOCKETS), resolve(STACK)];
    check!(
        load_current(APP, &rules)? == 0,
        "the app's rules were refused"
    );
    labels::lookup(APP).ok_or_else(|| String::from("the app label was not interned"))
}

/// Load both children of the browser, after [`load_app`].
fn load_children() -> Result<(u32, u32), String> {
    check!(
        load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
        "the page process was refused"
    );
    check!(
        load_with_program(NET, &[any(SOCKETS), resolve(STACK)], Some(NET_PATH))? == 0,
        "the network process was refused"
    );
    let web = labels::lookup(WEB).ok_or("the page label was not interned")?;
    let net = labels::lookup(NET).ok_or("the network label was not interned")?;
    Ok((web, net))
}

/// A task of `label_id`, with the app's identity.
fn task_of(label_id: u32) -> Result<usize, String> {
    let slot = task::spawn_child("child", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::set(slot, Cred::new(UID, UID, CAPS, label_id, SESSION));
    Ok(slot)
}

fn reap(slot: usize) -> Result<(), String> {
    task::harness::finish(slot, 0);
    check!(
        task::reap_child_slot(slot).is_some(),
        "could not reap {slot}"
    );
    Ok(())
}

fn allowed(slot: usize, interface: &str) -> bool {
    !authorize(slot, fnv1a64(interface), 3, 0).denied()
}

/// The label grammar: the child form, its parent link and every way the
/// string can be wrong.
pub fn child_label_grammar() -> Result<(), String> {
    fresh_children()?;
    let (kind, name) = labels::parse(WEB).map_err(|_| "parse")?;
    check!(
        kind == Kind::Child && name == "org.test.browser/webprocess",
        "parsed as {kind:?} {name:?}"
    );
    check!(labels::parent_label(WEB) == Some(APP), "parent label");
    for plain in [
        APP,
        "dev:org.test.browser",
        "system:netdrv",
        "app:a.b/",
        "x",
    ] {
        check!(
            labels::parent_label(plain).is_none(),
            "{plain:?} has a parent label"
        );
    }

    // The app first: a child of a label nobody interned is refused, and the
    // refusal takes no slot.
    check!(
        labels::intern(WEB) == Err(LabelError::NoParent),
        "a child was interned before its app"
    );
    check!(labels::count() == 0, "a refused child changed the table");
    let app = labels::intern(APP).map_err(|_| "intern the app")?;
    let web = labels::intern(WEB).map_err(|_| "intern the child")?;
    check!(
        labels::kind_of(web) == Some(Kind::Child) && labels::kind_of(app) == Some(Kind::App),
        "kinds"
    );
    check!(labels::parent_of(web) == Some(app), "the parent link");
    check!(
        labels::parent_of(app).is_none() && labels::parent_of(0).is_none(),
        "an app or the unlabelled has a parent"
    );
    check!(labels::intern(WEB) == Ok(web), "re-interning a child");
    check!(
        labels::name_of(web).as_deref() == Some(WEB),
        "the id does not round-trip"
    );

    let long = format!("{APP}/{}", "a".repeat(labels::MAX_CHILD_NAME_BYTES + 1));
    let longest = format!("{APP}/{}", "a".repeat(labels::MAX_CHILD_NAME_BYTES));
    let bad = [
        "app:org.test.browser/",
        "app:org.test.browser/Web",
        "app:org.test.browser/we b",
        "app:org.test.browser/a/b",
        "app:org.test.browser//a",
        "app:org.test.browser/-a",
        "app:org.test.browser/_a",
        "app:org.test.browser/a.b",
        "app:org.test.browser/a:b",
        "app:/a",
        "app:.org/a",
        "dev:org.test.browser/a",
        "system:netdrv/a",
        long.as_str(),
    ];
    for label in bad {
        check!(
            labels::intern(label) == Err(LabelError::Malformed),
            "{label:?} was accepted"
        );
    }
    check!(labels::intern(&longest).is_ok(), "the longest child name");
    check!(
        labels::intern("app:org.test.browser/a_b-1").is_ok(),
        "underscore and digits"
    );
    // The longest app id plus the longest child fits a label.
    let id = format!(
        "app:{}",
        ["a".repeat(40), "b".repeat(40), "c".repeat(46)].join(".")
    );
    check!(id.len() == 4 + 128, "setup: id is {} bytes", id.len());
    labels::intern(&id).map_err(|_| "intern the longest app")?;
    let child = format!("{id}/{}", "z".repeat(labels::MAX_CHILD_NAME_BYTES));
    check!(
        child.len() <= labels::MAX_LABEL_BYTES && labels::intern(&child).is_ok(),
        "the longest child of the longest app ({} bytes) was refused",
        child.len()
    );
    Ok(())
}

/// Loading a child through `OP_ACL_LOAD`: the app first, its program, no
/// widening, and every malformed request refused without a trace.
pub fn child_load_rules() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        // No app label yet: refused, and the child takes no slot.
        let before = labels::count();
        check!(
            load_with_program(WEB, &[], Some(WEB_PATH))? == failed(ENOENT),
            "a child of an unloaded app was accepted"
        );
        check!(
            labels::count() == before && labels::lookup(WEB).is_none(),
            "a refused child interned a label"
        );

        let app = load_app()?;
        // A page process asks for nothing: bound, with no rules.
        check!(
            load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
            "a child with no rules was refused"
        );
        let web = labels::lookup(WEB).ok_or("not interned")?;
        check!(
            children::program_of(web).as_deref() == Some(WEB_PATH)
                && acl::label_rule_count(web) == 0,
            "the binding or the rules are wrong"
        );
        check!(labels::parent_of(web) == Some(app), "link");

        // Widening is refused with `EACCES`, audited, and changes nothing.
        let wider = [any(SOCKETS), any(CLIPBOARD)];
        check!(
            load_with_program(NET, &wider, Some(NET_PATH))? == failed(EACCES),
            "a child wider than the app was accepted"
        );
        let event = *audit::recent(1).first().ok_or("no audit record")?;
        check!(
            !event.allow
                && event.reason_code == reason::CHILD_WIDENS_APP
                && event.interface_id == policy::LOADER_INTERFACE,
            "the refusal was audited as {event:?}"
        );
        check!(
            labels::lookup(NET).is_none() && children::count() == 1,
            "a refused child left a label or a binding"
        );
        // A wildcard the app does not hold is wider than any one of its rules.
        let wildcard = policy::wire::LabelRule {
            interface_id: u64::MAX,
            method: u32::MAX,
            allow: true,
        };
        check!(
            load_with_program(NET, &[wildcard], Some(NET_PATH))? == failed(EACCES),
            "a wildcard child was accepted"
        );
        // A deny rule never widens anything.
        let deny = policy::wire::LabelRule {
            interface_id: fnv1a64(CLIPBOARD),
            method: u32::MAX,
            allow: false,
        };
        check!(
            load_with_program(NET, &[deny, any(SOCKETS)], Some(NET_PATH))? == 0,
            "a child with a deny rule was refused"
        );
        check!(
            load_with_program(NET, &[any(SOCKETS), resolve(STACK)], Some(NET_PATH))? == 0,
            "the network process was refused"
        );

        // Malformed requests change nothing.
        let bound = children::count();
        let used = labels::count();
        for program in ["relative/x", "/a/../b", "/a//b", "/a/./b", "/a/", "/"] {
            let code = load_with_program(WEB, &[], Some(program))?;
            check!(code == failed(EINVAL), "program {program:?} -> {code:#x}");
        }
        check!(
            load_with_program(WEB, &[any(SOCKETS)], None)? == failed(EINVAL),
            "a child with rules and no program was accepted"
        );
        check!(
            load_with_program("app:org.test.browser/other", &[], Some(WEB_PATH))? == failed(EINVAL),
            "two children of one app share a program"
        );
        check!(
            load_with_program(APP, &[], Some("/system/bin/x"))? == failed(EINVAL),
            "a program on an app label was accepted"
        );
        check!(
            load_with_program("system:netdrv", &[], Some("/system/bin/x"))? == failed(EINVAL),
            "a program on a system label was accepted"
        );
        check!(
            load_with_program("app:org.test.browser/Web", &[], Some("/x"))? == failed(EINVAL),
            "a malformed child was accepted"
        );
        check!(
            children::count() == bound && labels::count() == used,
            "a refused load changed the bindings or the label table"
        );
        // Another app may bind the same path to its own child.
        check!(
            load_current(OTHER, &[any(DISPLAY)])? == 0,
            "load the other app"
        );
        check!(
            load_with_program(OTHER_WEB, &[], Some(WEB_PATH))? == 0,
            "the same path under another app was refused"
        );
        // A new program replaces the old one.
        check!(
            load_with_program(WEB, &[], Some("/system/bin/webprocess2"))? == 0,
            "rebind"
        );
        check!(
            children::program_of(web).as_deref() == Some("/system/bin/webprocess2"),
            "the program was not replaced"
        );
        Ok(())
    })
}

/// A child leaves with its own revocation (no rules, no program) and with the
/// app's: the bindings and the rules go, and nothing else is touched.
pub fn child_revocation() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        load_app()?;
        let (web, net) = load_children()?;
        check!(load_current(OTHER, &[any(DISPLAY)])? == 0, "other app");
        check!(
            load_with_program(OTHER_WEB, &[], Some(WEB_PATH))? == 0,
            "other child"
        );
        let other_web = labels::lookup(OTHER_WEB).ok_or("other child")?;
        check!(children::count() == 3, "bindings: {}", children::count());

        // One child, by itself.
        check!(
            load_with_program(WEB, &[], None)? == 0,
            "revoking a child was refused"
        );
        check!(
            children::program_of(web).is_none() && children::program_of(net).is_some(),
            "the wrong binding went"
        );
        check!(acl::label_rule_count(net) == 2, "a sibling lost rules");
        // Revoking a child that was never loaded interns nothing.
        let used = labels::count();
        check!(
            load_with_program("app:org.test.browser/never", &[], None)? == 0,
            "revoking an unknown child"
        );
        check!(labels::count() == used, "a revocation interned a label");

        // The app, which takes the rest of its children with it and no one
        // else's.
        check!(load_current(APP, &[])? == 0, "revoking the app");
        check!(
            children::program_of(net).is_none() && acl::label_rule_count(net) == 0,
            "a child outlived its app"
        );
        check!(
            children::program_of(other_web).as_deref() == Some(WEB_PATH),
            "another app's child went too"
        );
        check!(children::count() == 1, "bindings: {}", children::count());
        // An upgrade reloads the app and then the children: the app's own
        // non-empty load does not drop anything.
        load_app()?;
        load_children()?;
        load_app()?;
        check!(children::count() == 3, "an app reload dropped a child");
        Ok(())
    })
}

/// A child is allowed what its own rules *and* its app's allow: the kernel
/// narrows it even when the rules were loaded some other way, and revoking
/// the app silences it. It owns no names and no topics.
pub fn child_calls_are_the_intersection() -> Result<(), String> {
    fresh_children()?;
    let app = labels::intern(APP).map_err(|_| "intern the app")?;
    let net = labels::intern(NET).map_err(|_| "intern the child")?;
    // Straight into the ACL, as a loader bug would: the app has the display
    // and the sockets, the child asks for the sockets and the clipboard.
    acl::load_label(
        app,
        &[
            kernel_rule(fnv1a64(DISPLAY), u32::MAX),
            kernel_rule(fnv1a64(SOCKETS), u32::MAX),
            kernel_rule(policy::RESOLVE_SCOPE, fnv1a32(STACK)),
        ],
    )
    .map_err(|e| format!("{e:?}"))?;
    acl::load_label(
        net,
        &[
            kernel_rule(fnv1a64(SOCKETS), u32::MAX),
            kernel_rule(fnv1a64(CLIPBOARD), u32::MAX),
            kernel_rule(policy::RESOLVE_SCOPE, fnv1a32(STACK)),
        ],
    )
    .map_err(|e| format!("{e:?}"))?;

    let parent = task_of(app)?;
    let child = task_of(net)?;
    check!(allowed(parent, SOCKETS) && allowed(parent, DISPLAY), "app");
    check!(
        !allowed(parent, CLIPBOARD),
        "the app was given the clipboard"
    );
    check!(allowed(child, SOCKETS), "the child lost its own sockets");
    check!(
        !allowed(child, CLIPBOARD),
        "the child used a grant its app lacks"
    );
    check!(
        !allowed(child, DISPLAY),
        "the child used a grant it never asked for"
    );
    last_denial(net, reason::LABEL_DEFAULT_DENY)?;

    // Names: the shared grant resolves for both, and only the app owns the
    // `app.<id>.*` namespace.
    let resolves = |slot| policy::check_name(slot, policy::NameOp::Resolve, STACK).is_ok();
    check!(resolves(parent) && resolves(child), "a granted resolve");
    check!(
        !policy::check_name(child, policy::NameOp::Resolve, "os.lazy.display").is_ok(),
        "the child resolved a name it has no rule for"
    );
    let register = |slot| {
        policy::check_name(slot, policy::NameOp::Register, "app.org.test.browser.svc").is_ok()
    };
    check!(register(parent), "the app lost its own name");
    check!(!register(child), "a child registered the app's name");
    // Topics: the app's own namespace is the app's alone.
    let own = "app/org.test.browser/state";
    check!(
        topics::authorize(parent, topics::MODE_PUBLISH, own, 1).is_ok(),
        "the app lost its own topic"
    );
    check!(
        topics::authorize(child, topics::MODE_PUBLISH, own, 1) == Err(topics::Error::Denied),
        "a child published into the app's namespace"
    );

    // Revoking the app silences the child in the same instant.
    acl::load_label(app, &[]).map_err(|e| format!("{e:?}"))?;
    check!(
        !allowed(child, SOCKETS) && !resolves(child),
        "a child kept working after its app was revoked"
    );
    reap(parent)?;
    reap(child)
}

/// Only an `execve` of a child's program by a task of the app's own label
/// enters the child: not another path, not another app, not an unlabelled
/// task, not a child.
pub fn child_entered_only_by_its_program() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        let app = load_app()?;
        let (web, net) = load_children()?;
        check!(load_current(OTHER, &[any(DISPLAY)])? == 0, "other app");
        let other = labels::lookup(OTHER).ok_or("other")?;
        let app_cred = Cred::new(UID, UID, CAPS, app, SESSION);

        check!(
            children::label_for_exec(&app_cred, WEB_PATH) == Some(web),
            "the page program does not map to the page label"
        );
        check!(
            children::label_for_exec(&app_cred, NET_PATH) == Some(net),
            "the network program does not map to the network label"
        );
        for path in [
            "/system/bin/busybox",
            "/system/bin/webprocess/",
            "webprocess",
        ] {
            check!(
                children::label_for_exec(&app_cred, path).is_none(),
                "{path:?} enters a child"
            );
        }
        let foreign = Cred::new(UID, UID, CAPS, other, SESSION);
        check!(
            children::label_for_exec(&foreign, WEB_PATH).is_none(),
            "another app entered this app's child"
        );
        check!(
            children::label_for_exec(&Cred::new(UID, UID, CAPS, 0, SESSION), WEB_PATH).is_none(),
            "an unlabelled task entered a child"
        );
        check!(
            children::label_for_exec(&Cred::new(UID, UID, CAPS, web, SESSION), NET_PATH).is_none(),
            "a child entered its sibling"
        );

        // Entering stamps the label and nothing else, and audits it.
        let slot = task_of(app)?;
        let entered = children::enter_on_exec(slot, "/system/bin/busybox");
        check!(entered == Ok(None), "an unrelated path: {entered:?}");
        check!(
            credentials::of(slot).label_id == app,
            "an unrelated exec changed the label"
        );
        let entered = children::enter_on_exec(slot, NET_PATH);
        check!(entered == Ok(Some(net)), "entering: {entered:?}");
        check!(
            credentials::of(slot) == Cred::new(UID, UID, CAPS, net, SESSION),
            "the stamp changed more than the label: {:?}",
            credentials::of(slot)
        );
        let event = *audit::recent(1).first().ok_or("no audit record")?;
        check!(
            event.allow
                && event.label_id == app
                && event.reason_code == credentials::reason::TRANSITION_ALLOWED
                && event.txn_id == slot as u64,
            "the entry was audited as {event:?}"
        );
        // From inside there is nowhere further to go.
        check!(
            children::enter_on_exec(slot, WEB_PATH) == Ok(None),
            "a child entered another child"
        );
        check!(
            credentials::of(slot).label_id == net,
            "the label moved again"
        );
        reap(slot)?;

        // Another app's task, or an unlabelled one, is left as it was.
        for label in [other, 0] {
            let slot = task_of(label)?;
            check!(
                children::enter_on_exec(slot, WEB_PATH) == Ok(None)
                    && credentials::of(slot).label_id == label,
                "label {label} entered a child"
            );
            reap(slot)?;
        }

        // A revoked child cannot be entered.
        check!(load_with_program(WEB, &[], None)? == 0, "revoke");
        let slot = task_of(app)?;
        check!(
            children::enter_on_exec(slot, WEB_PATH) == Ok(None)
                && credentials::of(slot).label_id == app,
            "a revoked child was entered"
        );
        reap(slot)
    })
}

/// The credential gate refuses every other way into a child label, and
/// refuses the child stamp to anyone but the app's own task.
pub fn child_stamp_is_narrow() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        let app = load_app()?;
        let (web, net) = load_children()?;
        check!(load_current(OTHER, &[any(DISPLAY)])? == 0, "other app");
        check!(
            load_with_program(OTHER_WEB, &[], Some(WEB_PATH))? == 0,
            "other child"
        );
        let other = labels::lookup(OTHER).ok_or("other")?;
        let other_web = labels::lookup(OTHER_WEB).ok_or("other child")?;
        let slot = task_of(app)?;
        let own = credentials::of(slot);
        let stamp = |requested: Cred| {
            credentials::transition_with(slot, slot, requested, LabelStamp::Child)
        };
        let refused = |what: &str, result: Result<Cred, TransitionError>| -> Result<(), String> {
            check!(
                result == Err(TransitionError::LabelLocked),
                "{what}: {result:?}"
            );
            check!(credentials::of(slot) == own, "{what} changed the task");
            Ok(())
        };
        refused(
            "another app's child",
            stamp(Cred {
                label_id: other_web,
                ..own
            }),
        )?;
        refused(
            "the app's own label",
            stamp(Cred {
                label_id: app,
                ..own
            }),
        )?;
        refused(
            "another app",
            stamp(Cred {
                label_id: other,
                ..own
            }),
        )?;
        refused("no label", stamp(Cred { label_id: 0, ..own }))?;
        refused(
            "another uid",
            stamp(Cred {
                uid: 0,
                label_id: web,
                ..own
            }),
        )?;
        refused(
            "a new capability",
            stamp(Cred {
                caps: CAPS | credentials::CAP_SETUID,
                label_id: web,
                ..own
            }),
        )?;
        refused(
            "another session",
            stamp(Cred {
                session: 0,
                label_id: web,
                ..own
            }),
        )?;
        // The ordinary stamp still keeps the label.
        check!(
            credentials::transition(
                slot,
                slot,
                Cred {
                    label_id: web,
                    ..own
                }
            ) == Err(TransitionError::NotPrivileged),
            "the plain gate let a task relabel itself"
        );
        // The dev path refuses a child like any `app:` label.
        check!(!devspawn::applies(&own, WEB), "a child is a dev label");
        // An unbound child cannot be stamped either.
        check!(load_with_program(WEB, &[], None)? == 0, "revoke");
        refused(
            "a revoked child",
            stamp(Cred {
                label_id: web,
                ..own
            }),
        )?;
        // And the bound one is the one way in.
        check!(
            stamp(Cred {
                label_id: net,
                ..own
            })
            .is_ok(),
            "the bound child"
        );
        check!(credentials::of(slot).label_id == net, "the stamp was lost");
        reap(slot)
    })
}

/// A task in a child label starts no program: `execve`, `fork`, a
/// `vfork`-style `clone` and `spawnv` are all `EPERM`, audited.
pub fn child_may_not_start_programs() -> Result<(), String> {
    const SYS_CLONE: u64 = 56;
    const SYS_FORK: u64 = 57;
    const SYS_VFORK: u64 = 58;
    const SYS_EXECVE: u64 = 59;
    const SYS_SPAWNV: u64 = 31;
    const CLONE_VM: u64 = 0x100;

    fresh_children()?;
    in_space(|| -> Result<(), String> {
        let app = load_app()?;
        let (web, _) = load_children()?;
        // The kernel task plays the page process.
        credentials::set(task::KERNEL_TASK, Cred::new(UID, UID, CAPS, web, SESSION));
        let outcome = (|| -> Result<(), String> {
            let path = b"/system/bin/busybox\0";
            let argv = [path.as_ptr() as u64, 0];
            let words = [1u64; 20];
            let free = task::free_slots();
            // Each call is made, then its audit record read: the ring's newest
            // record is the one the call just wrote.
            let refused = |what: &str, code: u64| -> Result<(), String> {
                check!(code == failed(EPERM), "{what} gave {code:#x}");
                let event = *audit::recent(1).first().ok_or("no audit record")?;
                check!(
                    !event.allow
                        && event.label_id == web
                        && event.reason_code == reason::CHILD_MAY_NOT_START
                        && event.method == fnv1a32(what)
                        && event.interface_id == devspawn::SPAWN_SCOPE,
                    "{what} was audited as {event:?}"
                );
                Ok(())
            };
            let linux = process::linux::dispatch_for_test;
            refused(
                "execve",
                linux(SYS_EXECVE, path.as_ptr() as u64, argv.as_ptr() as u64, 0),
            )?;
            refused("fork", linux(SYS_FORK, 0, 0, 0))?;
            // `vfork(2)` is served by the same copy-on-write fork.
            refused("fork", linux(SYS_VFORK, 0, 0, 0))?;
            refused("vfork", linux(SYS_CLONE, CLONE_VM | 0x11, 0, 0))?;
            refused(
                "spawnv",
                process::dispatch_for_test(SYS_SPAWNV, words.as_ptr() as u64, 0, 0),
            )?;
            check!(task::free_slots() == free, "a refused start took a slot");
            Ok(())
        })();
        credentials::reset_for_task(task::KERNEL_TASK);
        outcome?;

        // The same calls are not refused for the app or an unlabelled task.
        for (label, who) in [(app, "the app"), (0, "an unlabelled task")] {
            let slot = task_of(label)?;
            check!(children::may_start(slot, "execve"), "{who} may not start");
            check!(!children::is_child(slot), "{who} is a child");
            reap(slot)?;
        }
        let slot = task_of(web)?;
        check!(children::is_child(slot), "a child is not a child");
        check!(!children::may_start(slot, "fork"), "a child may start");
        reap(slot)
    })
}

/// A thousand tasks enter and leave a child label, and a thousand
/// load/revoke cycles bind and unbind one: no label, binding, rule or task
/// slot is left behind.
pub fn child_label_soak() -> Result<(), String> {
    const CYCLES: usize = 1000;

    fresh_children()?;
    in_space(|| -> Result<(), String> {
        let app = load_app()?;
        let (web, net) = load_children()?;
        let labels_before = labels::count();
        let bindings_before = children::count();
        let rules_before = acl::label_rules_total();
        let slots_before = task::free_slots();
        let frames_before = mem::frame_stats().live();

        for cycle in 0..CYCLES {
            // A process of the app execs a helper and exits.
            let slot = task_of(app)?;
            let (path, want) = if cycle % 2 == 0 {
                (WEB_PATH, web)
            } else {
                (NET_PATH, net)
            };
            check!(
                children::enter_on_exec(slot, path) == Ok(Some(want)),
                "cycle {cycle}: no entry"
            );
            check!(
                credentials::of(slot).label_id == want,
                "cycle {cycle}: label {}",
                credentials::of(slot).label_id
            );
            reap(slot)?;
            // The reused slot starts with a clean identity.
            if cycle % 100 == 0 {
                let fresh = task_of(app)?;
                check!(
                    credentials::of(fresh).label_id == app,
                    "cycle {cycle}: a reused slot kept a child label"
                );
                reap(fresh)?;
            }
            // And the helper's label is reloaded now and then, as an upgrade
            // does, and revoked as a removal does.
            if cycle % 10 == 0 {
                check!(
                    load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
                    "cycle {cycle}: reload"
                );
            }
            if cycle % 250 == 249 {
                check!(load_with_program(WEB, &[], None)? == 0, "revoke");
                check!(
                    load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
                    "cycle {cycle}: rebind"
                );
            }
        }
        check!(
            labels::count() == labels_before,
            "labels grew: {labels_before} -> {}",
            labels::count()
        );
        check!(
            children::count() == bindings_before,
            "bindings grew: {bindings_before} -> {}",
            children::count()
        );
        check!(
            acl::label_rules_total() == rules_before,
            "rules grew: {rules_before} -> {}",
            acl::label_rules_total()
        );
        check!(
            task::free_slots() == slots_before,
            "slots leaked: {slots_before} -> {}",
            task::free_slots()
        );
        let frames = mem::frame_stats().live();
        check!(
            frames <= frames_before,
            "frames leaked: {frames_before} -> {frames}"
        );
        Ok(())
    })
}
