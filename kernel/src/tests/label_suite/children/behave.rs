//! What a child label may do, and how a task gets into one.

use super::*;

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
        let entered = children::enter_on_exec(slot, &["/system/bin/busybox"]);
        check!(entered == Ok(None), "an unrelated path: {entered:?}");
        check!(
            credentials::of(slot).label_id == app,
            "an unrelated exec changed the label"
        );
        let entered = children::enter_on_exec(slot, &[NET_PATH]);
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
            children::enter_on_exec(slot, &[WEB_PATH]) == Ok(None),
            "a child entered another child"
        );
        check!(
            credentials::of(slot).label_id == net,
            "the label moved again"
        );
        reap(slot)?;

        // A bound script: the file that runs is its interpreter, and the
        // path as spelled is the one that is bound.
        let slot = task_of(app)?;
        check!(
            children::enter_on_exec(slot, &["/system/bin/busybox", WEB_PATH]) == Ok(Some(web)),
            "a bound script did not enter its child"
        );
        reap(slot)?;

        // Another app's task, or an unlabelled one, is left as it was.
        for label in [other, 0] {
            let slot = task_of(label)?;
            check!(
                children::enter_on_exec(slot, &[WEB_PATH]) == Ok(None)
                    && credentials::of(slot).label_id == label,
                "label {label} entered a child"
            );
            reap(slot)?;
        }

        // A revoked child cannot be entered.
        check!(load_with_program(WEB, &[], None)? == 0, "revoke");
        let slot = task_of(app)?;
        check!(
            children::enter_on_exec(slot, &[WEB_PATH]) == Ok(None)
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

/// An `execve` into a child label is refused while the task's slot holds
/// Messenger state (a channel, a buffer, an endpoint survive an `execve`),
/// and goes through when it holds none: a helper is started by a fork whose
/// slot is empty.
pub fn child_not_entered_with_handles() -> Result<(), String> {
    use crate::ipc::handles::{self, rights, HandleKind};

    fresh_children()?;
    in_space(|| -> Result<(), String> {
        let app = load_app()?;
        let (web, _) = load_children()?;
        let slot = task_of(app)?;
        check!(
            handles::entries_for_task(slot).is_empty(),
            "setup: a fresh task holds handles"
        );
        let handle = handles::open_for_task(slot, HandleKind::Channel, rights::CALL, 0x5eed)
            .map_err(|error| String::from(error.message()))?;
        let before = audit::total();

        let entered = children::enter_on_exec(slot, &[WEB_PATH]);
        check!(
            entered == Err(TransitionError::Widening),
            "an app task holding a handle entered its child: {entered:?}"
        );
        check!(
            credentials::of(slot).label_id == app,
            "a refused entry moved the label"
        );
        check!(
            audit::total() == before,
            "a refusal before the gate was audited as a transition"
        );
        check!(
            handles::entries_for_task(slot).len() == 1,
            "the refusal touched the handles"
        );

        // With the state gone the same exec is allowed.
        handles::close_for_task(slot, handle).map_err(|error| String::from(error.message()))?;
        check!(
            handles::entries_for_task(slot).is_empty(),
            "the handle was not closed"
        );
        check!(
            children::enter_on_exec(slot, &[WEB_PATH]) == Ok(Some(web)),
            "a task with no handles did not enter its child"
        );
        check!(credentials::of(slot).label_id == web, "the stamp was lost");
        reap(slot)
    })
}
