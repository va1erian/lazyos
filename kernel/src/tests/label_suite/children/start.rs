//! A child label starts no program, and the soak.

use super::*;

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
