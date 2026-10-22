//! Process-group and session numbers outlive their leader (issue #880): a
//! task slot is a pid, so a slot whose number some task still uses as its
//! pgid or sid must not be handed out again, or its new holder could join the
//! old group with `setpgid(0, 0)`.

use super::*;

/// Every live task whose process group is `group`, as slots.
fn members(group: usize) -> Vec<usize> {
    task::process::process_list()
        .iter()
        .filter(|row| row.pgid == group)
        .map(|row| row.slot)
        .collect()
}

/// Fork from `parent` (the harness forks the current task).
fn fork_from(parent: usize, what: &str) -> Result<usize, String> {
    task::harness::switch_current(parent);
    task::spawn_fork().map_err(|error| format!("{what}: {error}"))
}

/// A group whose leader is gone keeps its members, and no new task receives
/// the leader's number while one of them lives; when the last member is
/// reaped the slot comes back.
pub fn group_number_is_not_reused() -> Result<(), String> {
    task::harness::reset();
    // Two fillers, so that the leader's slot is not slot 1.
    let filler = fork_from(task::KERNEL_TASK, "filler")?;
    fork_from(task::KERNEL_TASK, "filler")?;
    let leader = fork_from(task::KERNEL_TASK, "leader")?;
    let member = fork_from(leader, "member")?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        task::process::pgid_of(member) == leader && task::process::sid_of(member) == leader,
        "the member did not inherit the leader's group and session"
    );

    // The leader exits and is reaped; its slot is free, its group is not.
    task::harness::finish(leader, 0);
    check!(
        task::reap_child().map(|(slot, _)| slot) == Some(leader),
        "the leader was not reaped"
    );
    check!(
        task::process::find_by_pid(leader).is_none(),
        "the reaped leader still occupies its slot"
    );
    check!(
        task::process::pgid_of(member) == leader,
        "the member left the group with its leader"
    );

    // Nothing receives the number while the member lives, however many
    // tasks are created. The newcomers are forked by a filler (the kernel
    // task would give every child a group of its own) and form their own
    // group, as WebKit's launcher does.
    let mut spawned = Vec::new();
    for _ in 0..8 {
        let slot = fork_from(filler, "spawn")?;
        check!(
            slot != leader,
            "slot {leader} was handed out while its group still has a member"
        );
        task::harness::switch_current(slot);
        task::process::setpgid(slot, 0, 0).map_err(|e| format!("setpgid(0, 0): {e:?}"))?;
        check!(
            members(leader) == [member],
            "group {leader} holds {:?}, expected only the member {member}",
            members(leader)
        );
        spawned.push(slot);
    }

    // A session behaves the same: its leader is gone, a member is not.
    let session = fork_from(task::KERNEL_TASK, "session")?;
    let inside = fork_from(session, "inside")?;
    task::harness::switch_current(task::KERNEL_TASK);
    // `inside` gets a group of its own: the session number then lives only in
    // its `sid`.
    task::process::setpgid(session, inside as i64, inside as i64)
        .map_err(|e| format!("setpgid(inside): {e:?}"))?;
    task::harness::finish(session, 0);
    check!(
        task::reap_child().map(|(slot, _)| slot) == Some(session),
        "the session leader was not reaped"
    );
    check!(
        task::process::sid_of(inside) == session,
        "the session member left its session"
    );
    let mut more = Vec::new();
    for _ in 0..8 {
        let slot = fork_from(task::KERNEL_TASK, "spawn")?;
        check!(
            slot != session,
            "slot {session} was handed out while its session still has a member"
        );
        more.push(slot);
    }

    // The last members go: both numbers are free again, lowest first.
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in [member, inside].into_iter().chain(more) {
        task::harness::finish(slot, 0);
    }
    while task::reap_child().is_some() {}
    for &slot in &spawned {
        task::harness::finish(slot, 0);
    }
    task::harness::switch_current(filler);
    while task::reap_child().is_some() {}
    let first = fork_from(task::KERNEL_TASK, "respawn")?;
    let second = fork_from(task::KERNEL_TASK, "respawn")?;
    check!(
        first == leader && second == leader + 1,
        "the freed group number did not come back first: got {first} and {second}, leader {leader}"
    );
    let mut got = vec![first, second];
    for _ in 0..16 {
        got.push(fork_from(task::KERNEL_TASK, "respawn")?);
    }
    check!(
        got.contains(&session),
        "the freed session number {session} did not come back: {got:?}"
    );
    task::harness::reset();
    Ok(())
}

/// `setpgid` and `setsid` refuse to found a group or session whose number
/// another process already uses; without such a holder they succeed.
pub fn setpgid_refuses_a_stale_group() -> Result<(), String> {
    task::harness::reset();
    let root = fork_from(task::KERNEL_TASK, "root")?;
    let child = fork_from(root, "child")?;
    let other = fork_from(root, "other")?;
    // A task outside the family that, by a forged state, already names
    // `child`'s number as its group.
    let squatter = fork_from(task::KERNEL_TASK, "squatter")?;
    task::harness::set_group(squatter, child, squatter);
    check!(
        task::process::setpgid(child, 0, 0) == Err(GroupError::NotPermitted),
        "setpgid(0, 0) joined a group that another process already has"
    );
    check!(
        task::process::pgid_of(child) == root,
        "a refused setpgid moved the process to {}",
        task::process::pgid_of(child)
    );
    // The same number as the session of someone else.
    task::harness::set_group(squatter, squatter, other);
    check!(
        task::process::setpgid(other, 0, 0) == Err(GroupError::NotPermitted),
        "setpgid(0, 0) formed a group named like an existing session"
    );
    // A session cannot take the number of a group that exists either.
    task::harness::set_group(squatter, child, squatter);
    check!(
        task::process::setsid(child) == Err(GroupError::NotPermitted),
        "setsid took the number of an existing group"
    );
    check!(
        task::process::sid_of(child) == root,
        "a refused setsid changed the session"
    );

    // Without the squatter both work.
    task::harness::set_group(squatter, squatter, squatter);
    task::process::setpgid(child, 0, 0).map_err(|e| format!("setpgid after: {e:?}"))?;
    check!(
        task::process::pgid_of(child) == child,
        "setpgid(0, 0) did not form the group"
    );
    task::process::setsid(other).map_err(|e| format!("setsid after: {e:?}"))?;
    check!(
        task::process::sid_of(other) == other && task::process::pgid_of(other) == other,
        "setsid did not found the session"
    );
    task::harness::reset();
    Ok(())
}

/// Soak: rounds of a launcher that forks a group leader, the leader forks
/// members and exits, and the launcher goes on forking children that form
/// groups of their own (as WebKit's does). Slots are reused all the time,
/// yet a group of the old leader's number never gains a task it was not
/// formed with, and its number is reusable once the last member is reaped.
pub fn stale_group_soak() -> Result<(), String> {
    const ROUNDS: usize = 400;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..ROUNDS {
        task::harness::reset();
        // A different slot for the leader every round.
        for _ in 0..(next() % 5) {
            fork_from(task::KERNEL_TASK, "filler").map_err(|e| format!("round {round}: {e}"))?;
        }
        let launcher = fork_from(task::KERNEL_TASK, "launcher")?;
        let leader = fork_from(launcher, "leader")?;
        task::process::setpgid(launcher, leader as i64, leader as i64)
            .map_err(|e| format!("round {round}: leader setpgid: {e:?}"))?;
        let mut formed = Vec::new();
        for _ in 0..(1 + next() % 3) {
            formed.push(fork_from(leader, "member").map_err(|e| format!("round {round}: {e}"))?);
        }
        // The leader exits; the launcher reaps it.
        task::harness::finish(leader, 0);
        task::harness::switch_current(launcher);
        check!(
            task::reap_child().map(|(slot, _)| slot) == Some(leader),
            "round {round}: the leader was not reaped by the launcher"
        );
        check!(
            members(leader) == formed,
            "round {round}: group {leader} holds {:?}, expected {formed:?}",
            members(leader)
        );
        // The launcher keeps forking engines that form their own groups.
        for _ in 0..(2 + next() % 6) {
            let engine =
                fork_from(launcher, "engine").map_err(|e| format!("round {round}: {e}"))?;
            check!(
                engine != leader,
                "round {round}: slot {leader} reused while its group lives"
            );
            task::harness::switch_current(engine);
            task::process::setpgid(engine, 0, 0)
                .map_err(|e| format!("round {round}: engine setpgid: {e:?}"))?;
            check!(
                members(leader) == formed,
                "round {round}: group {leader} gained a task: {:?}",
                members(leader)
            );
        }
        // The members go (the kernel task adopted them) and are reaped: the
        // group is over and its number is the next one a fork receives.
        task::harness::switch_current(task::KERNEL_TASK);
        for &slot in &formed {
            task::harness::finish(slot, 0);
        }
        while task::reap_child().is_some() {}
        check!(
            members(leader).is_empty(),
            "round {round}: group {leader} survived its members"
        );
        let again = fork_from(launcher, "again").map_err(|e| format!("round {round}: {e}"))?;
        check!(
            again == leader,
            "round {round}: slot {leader} did not come back after its group ended (got {again})"
        );
    }
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    crate::trace::test!("task_stale_group_soak:INFO:rounds={ROUNDS}");
    Ok(())
}

/// With the table full, a slot pinned by a stale group stays free but unused:
/// the allocation fails (`EAGAIN`) rather than hand it out.
pub fn pinned_slot_is_never_the_last_resort() -> Result<(), String> {
    task::harness::reset();
    let leader = fork_from(task::KERNEL_TASK, "leader")?;
    let member = fork_from(leader, "member")?;
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(leader, 0);
    check!(
        task::reap_child().map(|(slot, _)| slot) == Some(leader),
        "the leader was not reaped"
    );
    let mut count = 0;
    while let Ok(slot) = task::spawn_fork() {
        check!(
            slot != leader,
            "slot {leader} handed out with its group alive"
        );
        count += 1;
        check!(count < task::MAX_TASKS, "the table never filled");
    }
    check!(
        task::process::find_by_pid(leader).is_none() && task::free_slots() == 1,
        "expected exactly the pinned slot {leader} to stay free ({} free)",
        task::free_slots()
    );
    check!(
        task::process::pgid_of(member) == leader,
        "the group lost its member"
    );
    // The member goes: the pinned slot is the one free slot, and it is usable.
    task::harness::finish(member, 0);
    while task::reap_child().is_some() {}
    check!(
        task::spawn_fork() == Ok(leader),
        "the slot {leader} was not usable once its group ended"
    );
    task::harness::reset();
    Ok(())
}

/// A process that formed a group, left it and comes back with
/// `setpgid(0, 0)` while a child still holds the group: legal on Linux, and
/// not a stale group (the child descends from the process).
pub fn setpgid_back_into_own_group() -> Result<(), String> {
    task::harness::reset();
    let root = fork_from(task::KERNEL_TASK, "root")?;
    let other = fork_from(root, "other")?;
    let leader = fork_from(root, "leader")?;
    task::process::setpgid(root, other as i64, other as i64)
        .map_err(|e| format!("form {other}: {e:?}"))?;
    task::process::setpgid(root, leader as i64, leader as i64)
        .map_err(|e| format!("form {leader}: {e:?}"))?;
    let child = fork_from(leader, "child")?;
    check!(
        task::process::pgid_of(child) == leader,
        "the child is not in the leader's group"
    );
    // The leader leaves its own group for `other`'s, and comes back.
    task::process::setpgid(root, leader as i64, other as i64)
        .map_err(|e| format!("leave: {e:?}"))?;
    task::process::setpgid(leader, 0, 0).map_err(|e| format!("come back: {e:?}"))?;
    check!(
        task::process::pgid_of(leader) == leader,
        "the process did not return to its group"
    );
    task::harness::reset();
    Ok(())
}
