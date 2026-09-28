//! Fork trees, process groups and sessions, reparenting, group kill,
//! and the process-list / task-snapshot introspection surface.

use super::*;

/// `spawn_fork` chains form a tree: parent links, children derivation and
/// the introspection rows all agree.
pub fn process_tree_fork() -> Result<(), String> {
    let chain = fork_chain(3)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child, leaf) = (chain[0], chain[1], chain[2]);

    check!(
        task::process::ppid_of(root) == 0,
        "root ppid is {}, expected init",
        task::process::ppid_of(root)
    );
    check!(
        task::process::ppid_of(child) == root,
        "child ppid is {}, expected {root}",
        task::process::ppid_of(child)
    );
    check!(
        task::process::ppid_of(leaf) == child,
        "leaf ppid is {}, expected {child}",
        task::process::ppid_of(leaf)
    );
    check!(
        task::process::children_of(task::KERNEL_TASK) == [root],
        "init children are {:?}, expected [{root}]",
        task::process::children_of(task::KERNEL_TASK)
    );
    check!(
        task::process::children_of(root) == [child],
        "root children are {:?}, expected [{child}]",
        task::process::children_of(root)
    );
    check!(
        task::process::children_of(leaf).is_empty(),
        "leaf unexpectedly has children: {:?}",
        task::process::children_of(leaf)
    );
    check!(
        task::process::find_by_pid(child) == Some(child),
        "find_by_pid({child}) missed the live child"
    );

    // The same tree, seen through the introspection API.
    let list = task::process::process_list();
    check!(
        list.len() == 4,
        "process_list has {} rows, expected 4",
        list.len()
    );
    let row = list
        .iter()
        .find(|row| row.pid == leaf)
        .ok_or("leaf missing from process_list")?;
    check!(
        row.slot == leaf
            && row.ppid == child
            && row.pgid == root
            && row.sid == root
            && row.uid == 0
            && row.state == task::TaskState::Runnable
            && row.name == "fork",
        "leaf row is {row:?}"
    );
    finish_and_reap_all(&chain)
}

/// `fork` inherits the parent's pgid/sid; a non-leader child can form its
/// own group, and the group/session errors match Linux.
pub fn pgid_sid_inherit() -> Result<(), String> {
    let chain = fork_chain(3)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child, leaf) = (chain[0], chain[1], chain[2]);

    check!(
        task::process::pgid_of(root) == root && task::process::sid_of(root) == root,
        "root is not its own leader (pgid {}, sid {})",
        task::process::pgid_of(root),
        task::process::sid_of(root)
    );
    for slot in [child, leaf] {
        check!(
            task::process::pgid_of(slot) == root && task::process::sid_of(slot) == root,
            "slot {slot} did not inherit the root group/session (pgid {}, sid {})",
            task::process::pgid_of(slot),
            task::process::sid_of(slot)
        );
    }

    // A child (not a session leader) can form a new group in the session.
    task::process::setpgid(root, child as i64, child as i64)
        .map_err(|error| format!("setpgid(child, child): {error:?}"))?;
    check!(
        task::process::pgid_of(child) == child,
        "child pgid is {}, expected {child}",
        task::process::pgid_of(child)
    );
    check!(
        task::process::sid_of(child) == root,
        "forming a group changed the session: {}",
        task::process::sid_of(child)
    );

    // Setting the group the target is already in is a successful no-op.
    task::process::setpgid(child, 0, 0).map_err(|error| format!("setpgid(0, 0): {error:?}"))?;
    check!(
        task::process::pgid_of(child) == child,
        "idempotent setpgid moved the child"
    );

    // A session leader cannot leave its group; a non-child cannot be moved;
    // a group outside the session does not exist; a negative pgid is EINVAL.
    check!(
        task::process::setpgid(root, root as i64, child as i64) == Err(GroupError::NotPermitted),
        "moved a session leader into another group"
    );
    check!(
        task::process::setpgid(child, root as i64, root as i64) == Err(GroupError::NotPermitted),
        "moved a process that is not the caller or its child"
    );
    check!(
        task::process::setpgid(root, child as i64, 9999) == Err(GroupError::NoSuchProcess),
        "joined a group that does not exist in the session"
    );
    check!(
        task::process::setpgid(child, 0, -1) == Err(GroupError::Invalid),
        "a negative pgid was accepted"
    );
    finish_and_reap_all(&chain)
}

/// `setsid` moves a non-leader into a fresh session and is `EPERM` for a
/// group leader (so it cannot be called twice).
pub fn setsid_new_session() -> Result<(), String> {
    let chain = fork_chain(2)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child) = (chain[0], chain[1]);

    // Already a group leader: Linux returns EPERM and changes nothing.
    check!(
        task::process::setsid(root) == Err(GroupError::NotPermitted),
        "setsid succeeded for a group leader"
    );
    check!(
        task::process::sid_of(root) == root,
        "a failed setsid changed the sid"
    );

    check!(
        task::process::setsid(child) == Ok(child),
        "setsid did not return the new sid {child}"
    );
    check!(
        task::process::sid_of(child) == child,
        "child sid is {}, expected {child}",
        task::process::sid_of(child)
    );
    check!(
        task::process::pgid_of(child) == child,
        "child pgid is {}, expected {child}",
        task::process::pgid_of(child)
    );
    check!(
        task::process::sid_of(root) == root,
        "the parent session changed"
    );

    // The new leader cannot call setsid again.
    check!(
        task::process::setsid(child) == Err(GroupError::NotPermitted),
        "setsid succeeded twice"
    );
    finish_and_reap_all(&chain)
}

/// A dying task's children are adopted by the kernel/init task, and only
/// the old parent can reap the corpse.
pub fn reparent_on_death() -> Result<(), String> {
    let chain = fork_chain(3)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child, leaf) = (chain[0], chain[1], chain[2]);
    check!(
        task::process::ppid_of(leaf) == child,
        "leaf ppid is {}, expected {child}",
        task::process::ppid_of(leaf)
    );

    check!(
        task::process::finish(child, 42),
        "finish(child) was a no-op"
    );
    check!(
        task::harness::state(child) == Some(task::TaskState::Done),
        "finished child is not Done"
    );
    check!(
        task::process::ppid_of(leaf) == 0,
        "orphan ppid is {}, expected init",
        task::process::ppid_of(leaf)
    );
    check!(
        task::process::children_of(task::KERNEL_TASK).contains(&leaf),
        "init's children do not include the orphan: {:?}",
        task::process::children_of(task::KERNEL_TASK)
    );

    // The live grandparent reaps the corpse, but not the adopted orphan:
    // that one is init's to collect.
    task::harness::switch_current(root);
    let (slot, status) = task::reap_child().ok_or("root could not reap its child")?;
    check!(
        slot == child && status == 42,
        "reaped slot {slot} with status {status}, expected {child}/42"
    );
    check!(
        task::reap_child().is_none(),
        "root reaped a task that is not its child"
    );
    task::harness::switch_current(task::KERNEL_TASK);

    check!(task::process::finish(leaf, 0), "finish(leaf) was a no-op");
    check!(task::process::finish(root, 0), "finish(root) was a no-op");
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(reaped == 2, "init reaped {reaped} orphans, expected 2");
    task::harness::reset();
    Ok(())
}

/// `kill_group` marks every member `Done` (including blocked ones, which a
/// later wake must not resurrect), spares init and other groups, and makes
/// the corpses reapable by init.
pub fn kill_group_terminates() -> Result<(), String> {
    let chain = fork_chain(3)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child, leaf) = (chain[0], chain[1], chain[2]);

    // A sibling in its own group must survive the kill.
    task::harness::switch_current(root);
    let outsider = task::spawn_fork().map_err(|error| format!("outsider: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    task::process::setpgid(root, outsider as i64, outsider as i64)
        .map_err(|error| format!("setpgid(outsider): {error:?}"))?;
    check!(
        task::process::pgid_of(outsider) == outsider,
        "outsider did not leave the group"
    );

    // Park a member first: a killed sleeper must stay Done (#57).
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(child, None);

    let killed = task::kill_group(root);
    check!(killed == 3, "kill_group killed {killed}, expected 3");
    for slot in [root, child, leaf] {
        check!(
            task::harness::state(slot) == Some(task::TaskState::Done),
            "group member {slot} survived: {:?}",
            task::harness::state(slot)
        );
    }
    check!(
        task::harness::state(outsider) == Some(task::TaskState::Runnable),
        "outsider was killed with the group"
    );
    check!(
        queue.notify_one() == 0,
        "a killed waiter was woken back to Runnable"
    );
    check!(
        task::harness::state(child) == Some(task::TaskState::Done),
        "a killed waiter was resurrected"
    );

    // init is exempt: only init terminates itself.
    check!(
        task::kill_group(task::KERNEL_TASK) == 0,
        "kill_group(0) killed init"
    );
    check!(
        task::harness::state(task::KERNEL_TASK) == Some(task::TaskState::Runnable),
        "init is no longer runnable"
    );

    // Every corpse was adopted by init: all four tasks are reapable there.
    task::harness::finish(outsider, 0);
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(reaped == 4, "init reaped {reaped}, expected 4");
    task::harness::reset();
    Ok(())
}

/// `process_list` reports the kernel as pid 0 and every live task with its
/// tree/group/session ids.
pub fn process_list_snapshot() -> Result<(), String> {
    let chain = fork_chain(2)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child) = (chain[0], chain[1]);

    let list = task::process::process_list();
    check!(
        list.len() == 3,
        "process_list has {} rows, expected 3 (init + 2)",
        list.len()
    );
    let init = list
        .iter()
        .find(|row| row.pid == 0)
        .ok_or("init is not listed")?;
    check!(
        init.slot == task::KERNEL_TASK
            && init.ppid == 0
            && init.pgid == 0
            && init.sid == 0
            && init.uid == 0
            && init.state == task::TaskState::Runnable
            && init.name == "kernel",
        "init row is {init:?}"
    );
    let row = list
        .iter()
        .find(|row| row.pid == child)
        .ok_or("forked child is not listed")?;
    check!(
        row.slot == child
            && row.ppid == root
            && row.pgid == root
            && row.sid == root
            && row.state == task::TaskState::Runnable
            && row.name == "fork",
        "child row is {row:?}"
    );
    finish_and_reap_all(&chain)
}

/// `task::introspect::TaskSnapshot` (MCP debug bridge Phase 2,
/// `docs/mcp-debug-bridge.md`) agrees with `process_list` on every live
/// slot, and round-trips through its wire encoding byte for byte.
pub fn task_snapshot_matches_process_list() -> Result<(), String> {
    use crate::task::introspect::TaskSnapshot;

    let chain = fork_chain(2)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let (root, child) = (chain[0], chain[1]);

    let snapshot = TaskSnapshot::snapshot();
    let processes = task::process::process_list();
    check!(
        snapshot.rows.len() == task::MAX_TASKS,
        "snapshot has {} rows, expected MAX_TASKS ({})",
        snapshot.rows.len(),
        task::MAX_TASKS
    );

    for info in &processes {
        let row = snapshot
            .rows
            .get(info.slot)
            .ok_or_else(|| alloc::format!("slot {} missing from snapshot", info.slot))?;
        check!(
            row.live
                && row.pid as usize == info.pid
                && row.ppid as usize == info.ppid
                && row.pgid as usize == info.pgid
                && row.sid as usize == info.sid
                && row.name == info.name,
            "snapshot row {row:?} does not match process_list row {info:?}"
        );
    }
    let live_slots = processes.len();
    let live_rows = snapshot.rows.iter().filter(|row| row.live).count();
    check!(
        live_rows == live_slots,
        "snapshot has {live_rows} live rows, process_list has {live_slots}"
    );

    // The root (a fresh fork) and its child both show up with the parent
    // link intact.
    let root_row = &snapshot.rows[root];
    check!(
        root_row.live && root_row.ppid as usize != root,
        "root row is {root_row:?}"
    );
    let child_row = &snapshot.rows[child];
    check!(
        child_row.live && child_row.ppid as usize == root,
        "child row is {child_row:?}"
    );

    // Wire round trip: encode then decode must reproduce every row.
    let bytes = snapshot.to_bytes();
    check!(
        bytes.len() == TaskSnapshot::SIZE,
        "encoded {} bytes, expected {}",
        bytes.len(),
        TaskSnapshot::SIZE
    );
    let decoded = TaskSnapshot::from_bytes(&bytes).ok_or("from_bytes rejected a valid block")?;
    check!(
        decoded.rows == snapshot.rows && decoded.version == snapshot.version,
        "decoded snapshot does not match the original"
    );

    finish_and_reap_all(&chain)
}

/// Soak: repeatedly fork/reap and snapshot the task table many times,
/// checking the snapshot is always internally consistent (live count
/// matches `process_list`, every live row round-trips) and that nothing
/// leaks a stale row once a task is reaped.
pub fn task_snapshot_soak_fork_churn() -> Result<(), String> {
    use crate::task::introspect::TaskSnapshot;

    const ITERATIONS: usize = 500;
    for iteration in 0..ITERATIONS {
        let chain = fork_chain(2)?;
        task::harness::switch_current(task::KERNEL_TASK);

        let snapshot = TaskSnapshot::snapshot();
        let processes = task::process::process_list();
        let live_rows = snapshot.rows.iter().filter(|row| row.live).count();
        check!(
            live_rows == processes.len(),
            "iteration {iteration}: {live_rows} live rows, process_list has {}",
            processes.len()
        );
        let bytes = snapshot.to_bytes();
        let decoded = TaskSnapshot::from_bytes(&bytes).ok_or_else(|| {
            alloc::format!("iteration {iteration}: from_bytes rejected a valid block")
        })?;
        check!(
            decoded.rows == snapshot.rows,
            "iteration {iteration}: decoded snapshot does not match the original"
        );

        finish_and_reap_all(&chain)?;
    }

    // After the last reap, every forked slot is gone: only init remains.
    let processes = task::process::process_list();
    check!(
        processes.len() == 1 && processes[0].pid == 0,
        "leaked task rows after {ITERATIONS} fork/reap cycles: {processes:?}"
    );
    serial_println!("TEST:task_snapshot_soak_fork_churn:INFO:iterations={ITERATIONS}");
    Ok(())
}
