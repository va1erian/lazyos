//! Task registration, fork/reap churn, thread exit and slot recycling,
//! futex mismatches, and the fd table.

use super::*;

/// Registering the kernel task sets the current slot and snapshot fields.
pub fn kernel_registered() -> Result<(), String> {
    task::register_kernel();
    check!(
        task::current() == task::KERNEL_TASK,
        "current task is {}, expected the kernel slot",
        task::current()
    );
    let (name, _, done) =
        task::snapshot(task::KERNEL_TASK).ok_or("kernel task is not registered")?;
    check!(name == "kernel", "kernel task is named {name:?}");
    check!(!done, "kernel task starts done");
    check!(
        !task::has_children(),
        "kernel task unexpectedly has children"
    );
    check!(
        task::reap_child().is_none(),
        "kernel task reaped a nonexistent child"
    );
    Ok(())
}

/// The futex park/wake mechanism: `set_blocked` parks, `wake_task` resumes.
pub fn block_wake_roundtrip() -> Result<(), String> {
    check!(!task::blocked(), "task starts blocked");
    task::set_blocked(true);
    check!(task::blocked(), "set_blocked(true) did not park the task");
    task::wake_task(task::current());
    check!(!task::blocked(), "wake_task did not resume the task");
    Ok(())
}

/// `spawn_fork` bookkeeping, exit, and reaping across many rounds.
pub fn fork_reap_churn() -> Result<(), String> {
    task::harness::reset();
    for round in 0..8u32 {
        let slot =
            task::spawn_fork().map_err(|error| format!("round {round}: spawn_fork: {error}"))?;
        check!(
            (1..task::MAX_TASKS).contains(&slot),
            "round {round}: fork slot {slot} is out of range"
        );
        check!(
            task::reap_child().is_none(),
            "round {round}: reaped a child that is still running"
        );
        task::harness::finish(slot, 0x40 + round as u64);
        let (reaped, status) =
            task::reap_child().ok_or_else(|| format!("round {round}: child is not reapable"))?;
        check!(
            reaped == slot,
            "round {round}: reaped slot {reaped}, expected {slot}"
        );
        check!(
            status == 0x40 + round as u64,
            "round {round}: exit status {status:#x}"
        );
    }
    task::harness::reset();
    Ok(())
}

/// Every slot but the kernel's can hold a task, and the table is empty
/// again once they are reaped (issue #204).
pub fn slots_fill_table() -> Result<(), String> {
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let spawned = fill_and_drain()?;
    check!(
        spawned == task::MAX_TASKS - 1,
        "filled {spawned} slots, expected {}",
        task::MAX_TASKS - 1
    );
    check!(
        task::process::process_list().len() == 1,
        "tasks other than the kernel's survive the drain"
    );
    task::harness::reset();
    Ok(())
}

/// Soak: fill and drain the whole table several times, so more tasks are
/// spawned than there are slots. Slots and frames must be recycled, not
/// leaked (issue #204).
pub fn slots_soak_recycle() -> Result<(), String> {
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    const ROUNDS: usize = 6;
    let baseline = mem::frame_stats().live();
    let mut total = 0;
    for round in 0..ROUNDS {
        let spawned = fill_and_drain().map_err(|error| format!("round {round}: {error}"))?;
        check!(
            spawned == task::MAX_TASKS - 1,
            "round {round}: only {spawned} slots were free again"
        );
        total += spawned;
        let live = mem::frame_stats().live();
        check!(
            live <= baseline,
            "round {round}: {} frames leaked after the drain",
            live - baseline
        );
    }
    serial_println!(
        "TEST:task_slots_soak_recycle:INFO:spawned={total} slots={}",
        task::MAX_TASKS
    );
    task::harness::reset();
    Ok(())
}

/// An exited `clone(CLONE_VM)` thread (parentless) releases its slot once
/// the scheduler has switched away from it; a task with a parent stays a
/// waitable zombie until `wait4` reaps it (issue #133).
pub fn thread_exit_reclaim() -> Result<(), String> {
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    // A process to own the thread: forking from init gives it a table of
    // its own, which the thread then shares.
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let leader_pml4 = task::harness::pml4(leader).ok_or("the leader has no address space")?;
    task::harness::switch_current(leader);
    let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    check!(
        task::harness::pml4(thread) == Some(leader_pml4),
        "the thread does not share the leader's address space"
    );

    // Exit the thread and run the tick that switches away from it. The
    // slot is only freed after the scheduler flags it, so it must still be
    // present until `reclaim_pending` runs.
    task::harness::finish(thread, 0x33);
    task::harness::switch_current(thread);
    let next = task::harness::simulate_tick();
    check!(next != thread, "the finished thread was selected again");
    check!(
        task::harness::state(thread) == Some(task::TaskState::Done),
        "the thread was reclaimed while still on the scheduler stack"
    );
    task::reclaim_pending();
    check!(
        task::harness::state(thread).is_none(),
        "the exited thread still holds its slot"
    );
    check!(
        task::process::find_by_pid(thread).is_none(),
        "a reclaimed thread is still findable by pid"
    );
    check!(
        task::harness::pml4(leader) == Some(leader_pml4),
        "reclaiming the thread tore down the shared address space"
    );
    check!(
        task::signal::send_tid(
            leader,
            thread,
            task::signal::SIGTERM,
            task::signal::SigInfo::user(leader, 0)
        ) == Err(task::signal::SignalError::NoSuchProcess),
        "a reclaimed thread's tid still accepts signals"
    );

    // A task with a parent is not reclaimed: `wait4` must still collect it.
    task::harness::switch_current(leader);
    let child = task::spawn_fork().map_err(|error| format!("child: {error}"))?;
    task::harness::finish(child, 0x44);
    task::harness::switch_current(child);
    let next = task::harness::simulate_tick();
    check!(next != child, "the finished child was selected again");
    task::reclaim_pending();
    check!(
        task::harness::state(child) == Some(task::TaskState::Done),
        "a waitable child was reclaimed without wait4"
    );
    task::harness::switch_current(leader);
    let (reaped, status) = task::reap_child().ok_or("the child is not reapable")?;
    check!(
        reaped == child && status == 0x44,
        "wait4 collected {reaped}/{status:#x}, expected {child}/0x44"
    );

    // The parentless leader itself is reclaimed once it leaves the CPU.
    task::harness::finish(leader, 0);
    task::harness::switch_current(leader);
    task::harness::simulate_tick();
    task::reclaim_pending();
    check!(
        task::harness::state(leader).is_none(),
        "the exited leader still holds its slot"
    );
    task::harness::reset();
    Ok(())
}

/// 64 spawn/exit generations in one process: the freed slot is recycled
/// every round, past the ~14 raw spawns a 16-slot table allows.
pub fn thread_churn_generations() -> Result<(), String> {
    const ROUNDS: usize = 64;
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let mut slots = Vec::new();
    for round in 0..ROUNDS {
        task::harness::switch_current(leader);
        let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("round {round}: spawn: {error}"))?;
        slots.push(thread);
        task::harness::finish(thread, 0);
        task::harness::switch_current(thread);
        let next = task::harness::simulate_tick();
        check!(
            next != thread,
            "round {round}: finished thread was selected"
        );
        task::reclaim_pending();
        check!(
            task::harness::state(thread).is_none(),
            "round {round}: slot {thread} was not reclaimed"
        );
    }
    check!(
        slots.iter().all(|&slot| slot == slots[0]),
        "the thread slot was not recycled: {slots:?}"
    );
    task::harness::finish(leader, 0);
    task::harness::switch_current(leader);
    task::harness::simulate_tick();
    task::reclaim_pending();
    task::harness::reset();
    Ok(())
}

/// Soak (issue #133): thousands of short-lived threads, plus repeated
/// whole thread-group teardowns with a mapped address space. Occupied slots
/// and frame accounting must return exactly to the baseline.
pub fn soak_thread_exit_generations() -> Result<(), String> {
    const THREADS: usize = 4096;
    const GROUPS: usize = 64;
    /// A deliberately generous ceiling (roughly a minute of wall clock);
    /// the loop is expected to take well under a second even under TCG.
    const MAX_CYCLES: u64 = 400_000_000_000;

    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let baseline_frames = mem::frame_stats().live();
    let baseline_slots = task::process::process_list().len();
    let start = unsafe { core::arch::x86_64::_rdtsc() };

    // One long-lived process whose threads churn: every exit must recycle
    // the slot and drop the task's own buffers.
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let leader_pml4 =
        PhysAddr::new(task::harness::pml4(leader).ok_or("the leader has no address space")?);
    process::map_range(leader_pml4, TEST_VA, TEST_VA + 4 * 4096)
        .map_err(|error| format!("leader map: {error}"))?;
    let steady_frames = mem::frame_stats().live();
    let mut thread_slots = Vec::new();
    for round in 0..THREADS {
        task::harness::switch_current(leader);
        let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("round {round}: spawn: {error}"))?;
        thread_slots.push(thread);

        // Give the thread task-owned heap state, so reclamation has real
        // buffers to drop, not just an empty shell.
        task::harness::switch_current(thread);
        task::write_output(b"thread output\n");
        task::fd_open(task::Fd::File {
            data: alloc::sync::Arc::new(alloc::vec![0x5a; 64]),
            offset: 0,
        })
        .ok_or_else(|| format!("round {round}: fd_open failed"))?;
        task::harness::finish(thread, 0);
        let next = task::harness::simulate_tick();
        check!(
            next != thread,
            "round {round}: finished thread was selected"
        );
        task::reclaim_pending();
        check!(
            task::harness::state(thread).is_none(),
            "round {round}: slot {thread} was not reclaimed"
        );
        if round % 1024 == 0 {
            let live = mem::frame_stats().live();
            check!(
                live == steady_frames,
                "round {round}: frames leaked ({} over baseline)",
                live.saturating_sub(steady_frames)
            );
            serial_println!(
                "TEST:task_soak_thread_exit_generations:PROGRESS:thread {round}/{THREADS}"
            );
        }
    }
    check!(
        thread_slots.iter().all(|&slot| slot == thread_slots[0]),
        "the churn did not recycle one slot"
    );
    check!(
        task::process::process_list().len() == baseline_slots + 1,
        "slots leaked during thread churn: {} rows",
        task::process::process_list().len()
    );

    // Whole generations: each builds its own address space with a mapped
    // page, spawns a thread in it, exits both, and must return the frames.
    for round in 0..GROUPS {
        task::harness::switch_current(task::KERNEL_TASK);
        let group = task::spawn_fork().map_err(|error| format!("group {round}: {error}"))?;
        let group_pml4 =
            PhysAddr::new(task::harness::pml4(group).ok_or("the group has no address space")?);
        process::map_range(group_pml4, TEST_VA, TEST_VA + 4096)
            .map_err(|error| format!("group {round} map: {error}"))?;
        task::harness::switch_current(group);
        let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("group {round}: thread spawn: {error}"))?;
        task::harness::finish(thread, 0);
        task::harness::finish(group, 0);

        // Switch away from the thread: its slot goes, the shared address
        // space stays (the group still references it).
        task::harness::switch_current(thread);
        task::harness::simulate_tick();
        task::reclaim_pending();
        check!(
            task::harness::state(thread).is_none(),
            "group {round}: thread slot was not reclaimed"
        );
        // Switch away from the group: the last user is gone, so the whole
        // address space is torn down.
        task::harness::switch_current(group);
        task::harness::simulate_tick();
        task::reclaim_pending();
        check!(
            task::harness::state(group).is_none(),
            "group {round}: group slot was not reclaimed"
        );
        let live = mem::frame_stats().live();
        check!(
            live == steady_frames,
            "group {round}: address-space frames leaked ({} over baseline)",
            live.saturating_sub(steady_frames)
        );
    }

    // Reclaiming the long-lived leader must return everything to the
    // pre-soak baseline.
    task::harness::finish(leader, 0);
    task::harness::switch_current(leader);
    task::harness::simulate_tick();
    task::reclaim_pending();
    let after_frames = mem::frame_stats().live();
    check!(
        after_frames == baseline_frames,
        "soak leaked {} frames",
        after_frames.saturating_sub(baseline_frames)
    );
    let after_slots = task::process::process_list().len();
    check!(
        after_slots == baseline_slots,
        "soak leaked {} task slots",
        after_slots.saturating_sub(baseline_slots)
    );
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!(
        "TEST:task_soak_thread_exit_generations:INFO:threads={THREADS} groups={GROUPS} cycles={cycles}"
    );
    check!(
        cycles < MAX_CYCLES,
        "soak used {cycles} cycles, over the {MAX_CYCLES} budget"
    );
    task::harness::reset();
    Ok(())
}

/// `futex(FUTEX_WAIT)` on a mismatched word returns EAGAIN without blocking;
/// `FUTEX_WAKE` with no waiters returns 0.
pub fn futex_wait_mismatch() -> Result<(), String> {
    let mut word: u32 = 7;
    let addr = core::ptr::addr_of_mut!(word) as u64;
    let eagain = (-11i64) as u64;
    let result = process::linux::dispatch_for_test(202, addr, 0, 99);
    check!(
        result == eagain,
        "futex WAIT on a mismatched word returned {result:#x}, expected EAGAIN"
    );
    let woken = process::linux::dispatch_for_test(202, addr, 1, 1);
    check!(woken == 0, "futex WAKE with no waiters returned {woken}");
    check!(word == 7, "futex syscall modified the word");
    Ok(())
}

/// Open, size, read, seek, duplicate and close a descriptor.
pub fn fd_table() -> Result<(), String> {
    task::register_kernel();
    let fd = task::fd_open(task::Fd::File {
        data: alloc::sync::Arc::new(b"hello".to_vec()),
        offset: 0,
    })
    .ok_or("fd_open failed")?;
    check!(fd >= 3, "fd_open returned reserved slot {fd}");
    check!(
        task::fd_kind(fd) == task::FdKind::File,
        "opened fd is not a file"
    );
    check!(
        task::fd_size(fd) == Some(5),
        "file size is {:?}, expected 5",
        task::fd_size(fd)
    );

    let mut buffer = [0u8; 8];
    let chunk = task::fd_read(fd, buffer.len()).ok_or("fd_read failed")?;
    buffer[..chunk.len()].copy_from_slice(&chunk);
    let read = chunk.len();
    check!(
        read == 5 && &buffer[..5] == b"hello",
        "fd_read got {read} bytes: {:?}",
        &buffer[..read.min(buffer.len())]
    );

    check!(task::fd_seek(fd, 0, 0) == Some(0), "fd_seek(SET 0) failed");
    let duplicate = task::fd_dup(fd).ok_or("fd_dup failed")?;
    check!(duplicate != fd, "fd_dup reused the same slot");
    check!(
        task::fd_size(duplicate) == Some(5),
        "duplicated fd lost its data"
    );
    check!(task::fd_close(duplicate), "fd_close(duplicate) failed");
    check!(
        task::fd_kind(duplicate) == task::FdKind::Closed,
        "closed fd is still open"
    );
    check!(task::fd_close(fd), "fd_close(fd) failed");
    check!(
        !task::fd_close(fd),
        "closing an already closed fd succeeded"
    );
    Ok(())
}
