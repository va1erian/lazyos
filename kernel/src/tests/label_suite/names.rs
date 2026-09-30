//! Service-name namespaces, cross-app resolve and the `acl_load` op.

use super::*;
use crate::ipc::acl::reason;
use crate::ipc::credentials::{CAP_DEV_CLAIM, CAP_IPC_CONTROL};

const EPERM: i64 = 1;
const EACCES: i64 = 13;
const EINVAL: i64 = 22;

/// Register `name` as `slot` and return the syscall code; the test's handles
/// are closed again so nothing accumulates.
fn register_as(slot: usize, name: &str) -> Result<u64, String> {
    task::harness::switch_current(slot);
    let (code, pair) = register_current(name)?;
    close_pair(pair);
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(code)
}

/// `os.lazy.*` is the platform's: a labelled app cannot squat it, a
/// `system:` task and a privileged unlabelled task can, and a refusal leaves
/// the registry untouched and an audit record naming the label.
pub fn reserved_os_lazy() -> Result<(), String> {
    fresh()?;
    let app = labelled_task("app:com.evil.app", 1000, 0)?;
    let app_id = credentials::of(app).label_id;
    in_space(|| -> Result<(), String> {
        let code = register_as(app, "os.lazy.confd")?;
        check!(
            code == failed(EACCES),
            "app squatted os.lazy.confd -> {code:#x}"
        );
        check!(
            registry::list().is_empty(),
            "a denied register touched the table"
        );
        last_denial(app_id, reason::RESERVED_NAMESPACE)?;

        // Even with root and every capability: the label wins over the uid.
        credentials::set(app, Cred::new(0, 0, credentials::CAP_ALL, app_id, 0));
        let code = register_as(app, "os.lazy.timed")?;
        check!(
            code == failed(EACCES),
            "a root-uid app registered os.lazy.* -> {code:#x}"
        );

        let system = labelled_task("system:confd", 0, 0)?;
        check!(
            register_as(system, "os.lazy.confd")? == 0,
            "system label refused"
        );
        let root = labelled_task("", 0, credentials::CAP_ALL)?;
        check!(
            register_as(root, "os.lazy.timed")? == 0,
            "unlabelled root refused"
        );
        let admin = labelled_task("", 1000, CAP_IPC_CONTROL)?;
        check!(
            register_as(admin, "os.lazy.a")? == 0,
            "CAP_IPC_CONTROL refused"
        );
        let driver = labelled_task("", 901, CAP_DEV_CLAIM)?;
        check!(
            register_as(driver, "os.lazy.drv")? == 0,
            "a provisioned driver refused"
        );
        // An unlabelled, capability-less service (how `init` runs `netd`)
        // keeps the legacy uid rules: allowed while no uid policy is loaded.
        let plain = labelled_task("", 1000, 0)?;
        let code = register_as(plain, "os.lazy.plain")?;
        check!(
            code == 0,
            "an unlabelled capability-less service lost os.lazy.* -> {code:#x}"
        );
        check!(
            registry::list().len() == 5,
            "{} names",
            registry::list().len()
        );
        Ok(())
    })
}

/// `app.<id>.<name>` belongs to `app:<id>`; a labelled app registers nothing
/// else; unlabelled tasks keep registering ordinary names.
pub fn app_namespace() -> Result<(), String> {
    fresh()?;
    let x = labelled_task("app:com.x", 1000, 0)?;
    let y = labelled_task("app:com.y", 1001, 0)?;
    let xy = labelled_task("app:com.x.y", 1002, 0)?;
    let legacy = labelled_task("", 1000, 0)?;
    in_space(|| -> Result<(), String> {
        check!(
            register_as(x, "app.com.x.svc")? == 0,
            "app:com.x refused its own name"
        );
        check!(
            register_as(y, "app.com.x.svc2")? == failed(EACCES),
            "app:com.y registered in app:com.x's namespace"
        );
        // `app.com.x.y.svc` is com.x.y's; com.x cannot claim it, and the
        // prefix relationship does not let com.x.y into com.x's either.
        check!(
            register_as(x, "app.com.x.y.svc")? == failed(EACCES),
            "app:com.x claimed app:com.x.y's name"
        );
        check!(
            register_as(xy, "app.com.x.y.svc")? == 0,
            "app:com.x.y refused its name"
        );
        check!(
            register_as(xy, "app.com.x.other")? == failed(EACCES),
            "app:com.x.y registered a name of app:com.x"
        );
        // A deeper tail is ambiguous, so it is refused outright.
        check!(
            register_as(x, "app.com.x.a.b")? == failed(EACCES),
            "a dotted tail was accepted"
        );
        // Outside both namespaces a labelled app gets nothing.
        check!(
            register_as(x, "example.service")? == failed(EACCES),
            "a labelled app registered a free-form name"
        );
        let id = credentials::of(x).label_id;
        last_denial(id, reason::OUTSIDE_NAMESPACE)?;
        // Unlabelled tasks are unchanged for ordinary names, but may not
        // squat an app's namespace.
        check!(
            register_as(legacy, "example.legacy")? == 0,
            "legacy register broke"
        );
        check!(
            register_as(legacy, "app.com.legacy.svc")? == failed(EACCES),
            "an unlabelled user squatted app.*"
        );
        check!(
            registry::list().len() == 3,
            "{} names",
            registry::list().len()
        );
        Ok(())
    })
}

/// Resolving a name is default-deny for a labelled task, except inside its own
/// namespace, until a rule for the exact name is loaded; revoking is loading
/// an empty list.
pub fn cross_app_resolve() -> Result<(), String> {
    fresh()?;
    let x = labelled_task("app:com.x", 1000, 0)?;
    let y = labelled_task("app:com.y", 1001, 0)?;
    let y_id = credentials::of(y).label_id;
    let legacy = labelled_task("", 1000, 0)?;
    in_space(|| -> Result<(), String> {
        check!(register_as(x, "app.com.x.svc")? == 0, "register");
        let resolve = |slot: usize, name: &str| -> Result<u64, String> {
            task::harness::switch_current(slot);
            let code = resolve_current(name);
            task::harness::switch_current(task::KERNEL_TASK);
            code
        };
        check!(
            resolve(x, "app.com.x.svc")? == 0,
            "an app could not resolve itself"
        );
        let before = handles::count_for_task(y);
        check!(
            resolve(y, "app.com.x.svc")? == failed(EACCES),
            "cross-app resolve was allowed without a rule"
        );
        last_denial(y_id, reason::LABEL_DEFAULT_DENY)?;
        check!(
            handles::count_for_task(y) == before,
            "a denied resolve opened a handle"
        );
        // A name that does not exist is denied the same way: no existence oracle.
        check!(
            resolve(y, "app.com.nobody.svc")? == failed(EACCES),
            "resolve of a missing name leaked existence"
        );
        check!(
            resolve(legacy, "app.com.x.svc")? == 0,
            "an unlabelled task lost resolve"
        );

        // Load a rule for exactly that name, from the privileged kernel task.
        let allow = policy::wire::LabelRule {
            interface_id: policy::RESOLVE_SCOPE,
            method: crate::ipc::topics::fnv1a32("app.com.x.svc"),
            allow: true,
        };
        check!(
            load_current("app:com.y", &[allow.clone()])? == 0,
            "load refused"
        );
        check!(
            resolve(y, "app.com.x.svc")? == 0,
            "an allowed resolve was refused"
        );
        check!(
            resolve(y, "app.com.x.other")? == failed(EACCES),
            "the rule granted more than one name"
        );
        // Revoke: an empty load removes every grant.
        check!(load_current("app:com.y", &[])? == 0, "revoke refused");
        check!(
            acl::label_rule_count(y_id) == 0,
            "rules survived the revoke"
        );
        check!(
            resolve(y, "app.com.x.svc")? == failed(EACCES),
            "a revoked app still resolves"
        );
        Ok(())
    })
}

/// `acl_load` needs `CAP_IPC_CONTROL`, replaces rather than appends, rejects
/// malformed labels and oversized batches atomically, and is refused to any
/// labelled task.
pub fn load_gate_and_revoke() -> Result<(), String> {
    fresh()?;
    let allow = |method| policy::wire::LabelRule {
        interface_id: 0x5151,
        method,
        allow: true,
    };
    in_space(|| -> Result<(), String> {
        // Unprivileged caller: -EPERM, audited, nothing interned or loaded.
        let plain = labelled_task("", 1000, 0)?;
        task::harness::switch_current(plain);
        let code = load_current("app:com.load", &[allow(1)])?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(code == failed(EPERM), "unprivileged load -> {code:#x}");
        check!(labels::lookup("app:com.load").is_none(), "label interned");
        let event = *audit::recent(1).first().ok_or("no audit event")?;
        check!(
            !event.allow
                && event.reason_code == reason::LOADER_NOT_PRIVILEGED
                && event.interface_id == policy::LOADER_INTERFACE,
            "the refusal record is {event:?}"
        );

        // A privileged but *labelled* task is still refused by the ACL hook.
        let labelled = labelled_task("app:com.root", 0, CAP_IPC_CONTROL)?;
        task::harness::switch_current(labelled);
        let code = load_current("app:com.load", &[allow(1)])?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(code == failed(EACCES), "labelled load -> {code:#x}");

        // The kernel task (root, unlabelled) may.
        check!(
            load_current("app:com.load", &[allow(1), allow(2)])? == 0,
            "load"
        );
        let id = labels::lookup("app:com.load").ok_or("label not interned by load")?;
        check!(acl::label_rule_count(id) == 2, "two rules expected");
        // Replace-all: a second load replaces, never appends.
        check!(load_current("app:com.load", &[allow(3)])? == 0, "reload");
        check!(acl::label_rule_count(id) == 1, "reload appended");
        // Other labels are untouched by a load.
        check!(
            load_current("app:com.other", &[allow(9)])? == 0,
            "load other"
        );
        check!(
            acl::label_rule_count(id) == 1,
            "a load leaked across labels"
        );
        // Bad input fails without changing anything.
        check!(
            load_current("Not A Label", &[allow(1)])? == failed(EINVAL),
            "malformed label accepted"
        );
        let many: Vec<_> = (0..=acl::MAX_RULES_PER_LABEL as u32).map(allow).collect();
        check!(
            load_current("app:com.load", &many)? == failed(EINVAL),
            "an oversized batch was accepted"
        );
        check!(
            acl::label_rule_count(id) == 1,
            "a failed load changed the label"
        );
        // An oversized batch for a label nobody has seen must not intern it:
        // a refused load changes nothing, including the append-only table.
        let before = labels::count();
        check!(
            load_current("app:com.oversized", &many)? == failed(EINVAL),
            "an oversized batch for a new label was accepted"
        );
        check!(
            labels::count() == before && labels::lookup("app:com.oversized").is_none(),
            "a refused load interned its label"
        );
        // Revoke by loading nothing.
        check!(load_current("app:com.load", &[])? == 0, "revoke");
        check!(acl::label_rule_count(id) == 0, "revoke left rules");
        Ok(())
    })
}
