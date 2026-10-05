//! Soak: thousands of register/resolve/unregister/load cycles across labels,
//! including tasks that die mid-registration and registrations that advertise
//! another app's interface (issue #495), must leave the label table, the ACL
//! storage, the registry and the handle tables exactly where they started.

use super::*;

const EACCES: i64 = 13;
const APPS: usize = 8;
const PASSES: usize = 2;
const CYCLES_PER_PASS: usize = 1500;
/// One cycle in this many ends with the owning task exiting while it still
/// holds a registered name.
const EXIT_EVERY: usize = 100;
/// One cycle in this many also tries to serve the peer app's interface.
const FOREIGN_EVERY: usize = 4;

/// App `app`'s own interface, `soak<app>.svc.v1`.
fn interface_of(app: usize) -> String {
    format!("soak{app}.svc.v1")
}

fn name_of(app: usize) -> String {
    format!("app.soak{app}.svc")
}

/// Close every handle `slot` holds, then require the table to be empty.
fn drain_handles(slot: usize) -> Result<(), String> {
    task::harness::switch_current(slot);
    for (handle, _) in handles::entries_for_task(slot) {
        let _ = channels::close_endpoint(handle);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        handles::count_for_task(slot) == 0,
        "slot {slot} still holds {} handles",
        handles::count_for_task(slot)
    );
    Ok(())
}

fn as_task<R>(slot: usize, f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    task::harness::switch_current(slot);
    let outcome = f();
    task::harness::switch_current(task::KERNEL_TASK);
    outcome
}

/// One full cycle for app `k`: register, resolve (own, foreign denied, foreign
/// allowed by a loaded rule, then revoked), unregister.
fn cycle(index: usize, tasks: &[usize]) -> Result<(), String> {
    let k = index % APPS;
    let (owner, peer) = (tasks[k], tasks[(k + 1) % APPS]);
    let name = name_of(k);
    let peer_label = format!("app:soak{}", (k + 1) % APPS);

    if index % FOREIGN_EVERY == 0 {
        let foreign = interface_of((k + 1) % APPS);
        let id = crate::ipc::topics::fnv1a64(&foreign);
        let code = as_task(owner, || {
            let (code, pair) = register_current_with(&name, &[id], &[&foreign])?;
            close_pair(pair);
            Ok(code)
        })?;
        check!(
            code == failed(EACCES),
            "cycle {index}: served {foreign} -> {code:#x}"
        );
        check!(
            registry::list().is_empty(),
            "cycle {index}: refusal registered"
        );
    }
    let own = interface_of(k);
    let own_id = crate::ipc::topics::fnv1a64(&own);
    let code = as_task(owner, || {
        let (code, pair) = register_current_with(&name, &[own_id], &[&own])?;
        close_pair(pair);
        Ok(code)
    })?;
    check!(code == 0, "cycle {index}: register -> {code:#x}");
    check!(registry::list().len() == 1, "cycle {index}: registry size");
    check!(
        registry::list()[0].interfaces == [own_id],
        "cycle {index}: interfaces not kept"
    );
    check!(
        as_task(owner, || resolve_current(&name))? == 0,
        "cycle {index}: an app could not resolve its own name"
    );
    check!(
        as_task(peer, || resolve_current(&name))? == failed(EACCES),
        "cycle {index}: a foreign resolve was not denied"
    );

    if index % 3 == 0 {
        let rule = policy::wire::LabelRule {
            interface_id: policy::RESOLVE_SCOPE,
            method: crate::ipc::topics::fnv1a32(&name),
            allow: true,
        };
        check!(
            load_current(&peer_label, &[rule])? == 0,
            "cycle {index}: load"
        );
        check!(
            as_task(peer, || resolve_current(&name))? == 0,
            "cycle {index}: a granted resolve was refused"
        );
        check!(
            load_current(&peer_label, &[])? == 0,
            "cycle {index}: revoke"
        );
        check!(
            as_task(peer, || resolve_current(&name))? == failed(EACCES),
            "cycle {index}: a revoked resolve was allowed"
        );
        drain_handles(peer)?;
    }

    check!(
        as_task(owner, || unregister_current(&name))? == 0,
        "cycle {index}: unregister"
    );
    check!(
        registry::list().is_empty(),
        "cycle {index}: name left behind"
    );
    drain_handles(owner)
}

/// A task registers a name and exits without unregistering; teardown and
/// pruning must release the name and its handles.
fn exit_mid_registration(index: usize) -> Result<(), String> {
    let k = index % APPS;
    let child = labelled_task(&format!("app:soak{k}"), 3000, 0)?;
    let code = as_task(child, || {
        let (code, _pair) = register_current(&name_of(k))?;
        Ok(code)
    })?;
    check!(code == 0, "exit cycle {index}: register -> {code:#x}");
    check!(
        registry::list().len() == 1,
        "exit cycle {index}: not registered"
    );
    task::harness::finish(child, 0);
    check!(
        registry::list().is_empty(),
        "exit cycle {index}: a dead owner's name survived"
    );
    check!(
        task::reap_child().is_some(),
        "exit cycle {index}: not reapable"
    );
    check!(
        handles::count_for_task(child) == 0,
        "exit cycle {index}: the dead task kept {} handles",
        handles::count_for_task(child)
    );
    Ok(())
}

pub fn register_load_cycles() -> Result<(), String> {
    fresh()?;
    let mut tasks = Vec::new();
    for app in 0..APPS {
        tasks.push(labelled_task(
            &format!("app:soak{app}"),
            2000 + app as u32,
            0,
        )?);
    }
    let denials_before = audit::denials();
    let mut label_counts = Vec::new();
    in_space(|| -> Result<(), String> {
        let mut index = 0;
        for _ in 0..PASSES {
            for _ in 0..CYCLES_PER_PASS {
                cycle(index, &tasks)?;
                if index % EXIT_EVERY == EXIT_EVERY - 1 {
                    exit_mid_registration(index)?;
                }
                index += 1;
            }
            label_counts.push(labels::count());
            check!(
                acl::label_rules_total() == 0,
                "{} label rules leaked",
                acl::label_rules_total()
            );
            check!(registry::list().is_empty(), "names leaked");
            let stats = registry::stats();
            check!(
                stats.entries == 0 && stats.leases == 0,
                "registry stats {stats:?}"
            );
        }
        Ok(())
    })?;
    check!(
        label_counts.iter().all(|count| *count == APPS),
        "the label table grew across passes: {label_counts:?}"
    );
    let denied = audit::denials() - denials_before;
    // One foreign resolve per cycle, plus the foreign interfaces.
    let cycles = PASSES * CYCLES_PER_PASS;
    let expected = cycles + cycles / FOREIGN_EVERY;
    check!(
        denied >= expected as u64,
        "only {denied} denials audited (expected at least {expected})"
    );
    // Hash chain still verifies end to end after the churn.
    check!(audit::count() > 0, "audit ring is empty");
    Ok(())
}
