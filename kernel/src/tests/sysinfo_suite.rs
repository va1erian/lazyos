//! The fixed-layout read-only snapshot behind syscall 14: version/
//! layout stability, the strict buffer contract, live counters and the
//! task table. The soak drives snapshots through repeated fork/exit/
//! reclaim generations, where a per-snapshot allocation or a
//! translation leak would show as frame, slab or task-row growth.
//! System statistics snapshot (issue #144).

use super::*;
use crate::sysinfo;

/// Two's-complement `-errno`, the syscall error encoding.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Register the kernel task and empty the table, as a normal boot starts.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
}

/// Scratch user buffer for the snapshot. The syscall validates its
/// destination against the active CR3 as mapped, writable user memory, so
/// each call installs a fresh address space with this range mapped.
const SPACE: u64 = 0x0040_0000;

const SPACE_PAGES: u64 = (sysinfo::SIZE + 4095) / 4096;

/// Run `f` with [`SPACE`] mapped into a fresh user address space.
fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
    mem::switch_to(table);
    let outcome = f();
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// A full snapshot through the syscall entry, decoded as raw words.
/// Boxed: at 64 slots a block is over 5 KiB, and the soak below keeps two
/// of them live while the syscall path holds its own on the same stack.
fn snapshot() -> Result<alloc::boxed::Box<[u64; sysinfo::WORDS]>, String> {
    in_space(|| {
        let code = process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, SPACE, sysinfo::SIZE);
        check!(code == sysinfo::SIZE, "snapshot -> {code:#x}");
        let mut words = alloc::boxed::Box::new([0u64; sysinfo::WORDS]);
        for (index, word) in words.iter_mut().enumerate() {
            // Safety: the scratch pages are mapped readable while installed.
            *word = unsafe { core::ptr::read_volatile((SPACE as *const u64).add(index)) };
        }
        Ok(words)
    })
}

/// The size op reports the ABI block; a null or short buffer and an
/// unknown op are refused with the documented errno; a full buffer gets
/// exactly one versioned block.
pub fn snapshot_abi_contract() -> Result<(), String> {
    fresh();
    let size = process::dispatch_for_test(14, sysinfo::op::SIZE, 0, 0);
    check!(
        size == sysinfo::SIZE,
        "size op reported {size:#x}, expected {:#x}",
        sysinfo::SIZE
    );
    check!(
        process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, 0, 0) == failed(14),
        "a null snapshot buffer was not refused with -EFAULT"
    );
    let short = in_space(|| {
        Ok(process::dispatch_for_test(
            14,
            sysinfo::op::SNAPSHOT,
            SPACE,
            sysinfo::SIZE - 8,
        ))
    })?;
    check!(
        short == failed(7),
        "a short buffer returned {short:#x}, expected -E2BIG"
    );
    check!(
        process::dispatch_for_test(14, 99, 0, 0) == failed(22),
        "an unknown op was not refused with -EINVAL"
    );

    let words = snapshot()?;
    check!(
        words[sysinfo::H_VERSION] == sysinfo::SYSTEM_STATS_VERSION,
        "header version is {}, expected {}",
        words[sysinfo::H_VERSION],
        sysinfo::SYSTEM_STATS_VERSION
    );
    check!(
        words[sysinfo::H_WORDS] == sysinfo::WORDS as u64,
        "header word count is {}, expected {}",
        words[sysinfo::H_WORDS],
        sysinfo::WORDS
    );
    check!(
        words[sysinfo::H_TASK_ROW_WORDS] == sysinfo::TASK_ROW_WORDS as u64
            && words[sysinfo::H_TASK_SLOTS] == task::MAX_TASKS as u64,
        "row layout words are {} rows {} slots, expected {} and {}",
        words[sysinfo::H_TASK_ROW_WORDS],
        words[sysinfo::H_TASK_SLOTS],
        sysinfo::TASK_ROW_WORDS,
        task::MAX_TASKS
    );
    Ok(())
}

/// Both read-only monitor gates (13: task list, 14: system stats) are open
/// to every task, so a destination that is a kernel address, unmapped, or
/// runs off the end of the mapping must be refused with `-EFAULT` before a
/// byte is written — never turned into a kernel write.
pub fn snapshot_rejects_bad_destinations() -> Result<(), String> {
    fresh();
    let mut canary = [0x5a5a_5a5a_5a5a_5a5au64; sysinfo::WORDS];
    let kernel_buf = canary.as_mut_ptr() as u64;
    in_space(|| {
        let tail = SPACE + SPACE_PAGES * 4096 - 8;
        for (label, buf) in [
            ("kernel address", kernel_buf),
            ("unmapped", 0xdead_0000),
            ("non-canonical", 0x0000_8000_0000_0000),
            ("range past the mapping", tail),
            ("wrapping range", u64::MAX - 8),
        ] {
            let code = process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, buf, sysinfo::SIZE);
            check!(
                code == failed(14),
                "sysinfo {label} -> {code:#x}, expected -EFAULT"
            );
            let code = process::dispatch_for_test(13, buf, 0, 0);
            check!(
                code == failed(14),
                "sys_tasks {label} -> {code:#x}, expected -EFAULT"
            );
        }
        let code = process::dispatch_for_test(13, SPACE, 0, 0);
        check!(code == 0, "sys_tasks into a mapped buffer -> {code:#x}");
        Ok(())
    })?;
    check!(
        canary.iter().all(|&word| word == 0x5a5a_5a5a_5a5a_5a5a),
        "a refused snapshot still wrote into the kernel buffer"
    );
    Ok(())
}

/// Every reported field is live and self-consistent, the kernel task's row
/// matches its scheduler state, and empty slots are all-zero.
pub fn snapshot_fields_sane() -> Result<(), String> {
    fresh();
    let words = snapshot()?;

    check!(
        words[sysinfo::H_TICKS] == task::ticks(),
        "tick word {} is not the live clock {}",
        words[sysinfo::H_TICKS],
        task::ticks()
    );
    let total = words[sysinfo::H_FRAMES_TOTAL];
    let live = words[sysinfo::H_FRAMES_LIVE];
    let free = words[sysinfo::H_FRAMES_FREE];
    check!(total > 0, "the allocator reports zero frames");
    check!(
        free + live == total,
        "frame accounting is inconsistent: free {free} + live {live} != total {total}"
    );
    check!(
        words[sysinfo::H_FRAMES_ALLOCATED] >= live
            && words[sysinfo::H_FRAMES_FREED] <= words[sysinfo::H_FRAMES_ALLOCATED],
        "frame counters are inconsistent: live {live} allocated {} freed {}",
        words[sysinfo::H_FRAMES_ALLOCATED],
        words[sysinfo::H_FRAMES_FREED]
    );
    check!(
        words[sysinfo::H_SLAB_PEAK] >= words[sysinfo::H_SLAB_LIVE],
        "slab peak {} is below live {}",
        words[sysinfo::H_SLAB_PEAK],
        words[sysinfo::H_SLAB_LIVE]
    );
    check!(
        words[sysinfo::H_HEAP_USED] + words[sysinfo::H_HEAP_FREE] == words[sysinfo::H_HEAP_TOTAL]
            && words[sysinfo::H_HEAP_TOTAL] > 15 * 1024 * 1024
            && words[sysinfo::H_HEAP_TOTAL] <= mem::HEAP_SIZE,
        "heap words are inconsistent: used {} + free {} != total {}",
        words[sysinfo::H_HEAP_USED],
        words[sysinfo::H_HEAP_FREE],
        words[sysinfo::H_HEAP_TOTAL]
    );
    check!(
        words[sysinfo::H_TASKS_LIVE] == 1,
        "live task count is {}, expected the kernel task only",
        words[sysinfo::H_TASKS_LIVE]
    );

    // The kernel task's row: slot 0, pid 0, no parent, runnable, in the
    // interactive class with its default weight, named `kernel`.
    let base = sysinfo::HEADER_WORDS;
    check!(
        words[base + sysinfo::R_PRESENT] == 1
            && words[base + sysinfo::R_PID] == 0
            && words[base + sysinfo::R_PPID] == 0,
        "the kernel row presence/pid/ppid words are wrong"
    );
    check!(
        words[base + sysinfo::R_STATE] == sysinfo::state::RUNNABLE,
        "the kernel row state is {}, expected runnable",
        words[base + sysinfo::R_STATE]
    );
    check!(
        words[base + sysinfo::R_CLASS] == sysinfo::class::INTERACTIVE
            && words[base + sysinfo::R_WEIGHT] == 4,
        "the kernel row class/weight words are {} and {}",
        words[base + sysinfo::R_CLASS],
        words[base + sysinfo::R_WEIGHT]
    );
    check!(
        words[base + sysinfo::R_NAME8] == u64::from_le_bytes(*b"kernel\0\0"),
        "the kernel row short name is {:#x}",
        words[base + sysinfo::R_NAME8]
    );
    check!(
        words[base + sysinfo::R_NAME_HASH] == sysinfo::fnv1a64(b"kernel"),
        "the kernel row name hash is {:#x}",
        words[base + sysinfo::R_NAME_HASH]
    );

    // An empty slot's whole row stays zero, so a reader can rely on
    // presence alone.
    let empty = sysinfo::HEADER_WORDS + sysinfo::TASK_ROW_WORDS;
    check!(
        words[empty..empty + sysinfo::TASK_ROW_WORDS]
            .iter()
            .all(|word| *word == 0),
        "an empty task slot has non-zero row words"
    );
    Ok(())
}

/// A forked task shows up with its pid, ppid, name and CPU ticks, and the
/// live count follows the table.
pub fn snapshot_reflects_spawned_task() -> Result<(), String> {
    fresh();
    let slot = task::spawn_fork().map_err(to_string)?;
    check!(
        task::process::ppid_of(slot) == task::KERNEL_TASK,
        "the fork's ppid is {}",
        task::process::ppid_of(slot)
    );
    // Charge the child a CPU tick exactly as a timer tick would.
    task::harness::switch_current(slot);
    task::harness::simulate_tick();
    task::harness::switch_current(task::KERNEL_TASK);

    let words = snapshot()?;
    check!(
        words[sysinfo::H_TASKS_LIVE] == 2,
        "live task count is {}, expected 2",
        words[sysinfo::H_TASKS_LIVE]
    );
    let base = sysinfo::HEADER_WORDS + slot * sysinfo::TASK_ROW_WORDS;
    check!(
        words[base + sysinfo::R_PRESENT] == 1
            && words[base + sysinfo::R_PID] == slot as u64
            && words[base + sysinfo::R_PPID] == task::KERNEL_TASK as u64,
        "the spawned row presence/pid/ppid words are wrong"
    );
    check!(
        words[base + sysinfo::R_STATE] == sysinfo::state::RUNNABLE,
        "the spawned row state is {}",
        words[base + sysinfo::R_STATE]
    );
    check!(
        words[base + sysinfo::R_CPU_TICKS] >= 1,
        "the spawned row CPU ticks are {}",
        words[base + sysinfo::R_CPU_TICKS]
    );
    check!(
        words[base + sysinfo::R_NAME8] == u64::from_le_bytes(*b"fork\0\0\0\0")
            && words[base + sysinfo::R_NAME_HASH] == sysinfo::fnv1a64(b"fork"),
        "the spawned row name words are wrong"
    );
    task::harness::reset();
    Ok(())
}

/// Many snapshots under fork/exit/reclaim churn: the layout stays stable
/// and neither the frame allocator nor the slab allocator grows.
pub fn soak_snapshot_task_churn() -> Result<(), String> {
    fresh();
    const ROUNDS: usize = 96;

    // Warm up the address-space-derived tables (BUMPS, signal dispositions)
    // once, so the baseline is measured after their one-time growth.
    let warm = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(warm);
    task::harness::simulate_tick();
    task::harness::finish(warm, 0);
    task::harness::switch_current(warm);
    task::harness::simulate_tick();
    task::reclaim_pending();
    fresh();

    let baseline = snapshot()?;
    let frames_before = mem::frame_stats().live();
    let slab_before = mem::slab::stats().live_bytes;
    for round in 0..ROUNDS {
        let slot = task::spawn_fork().map_err(|error| format!("round {round}: fork: {error}"))?;
        task::harness::switch_current(slot);
        task::harness::simulate_tick();
        task::harness::finish(slot, round as u64);
        task::harness::switch_current(slot);
        task::harness::simulate_tick();
        task::reclaim_pending();

        let words = snapshot()?;
        check!(
            words[sysinfo::H_VERSION] == sysinfo::SYSTEM_STATS_VERSION
                && words[sysinfo::H_WORDS] == sysinfo::WORDS as u64
                && words[sysinfo::H_TASK_ROW_WORDS] == sysinfo::TASK_ROW_WORDS as u64,
            "round {round}: snapshot layout changed"
        );
        check!(
            words[sysinfo::H_TASKS_LIVE] == 1,
            "round {round}: {} live tasks after reclaim",
            words[sysinfo::H_TASKS_LIVE]
        );
    }

    // Every churned address space must be gone: no frame or slab growth
    // across 96 fork/exit/reclaim generations.
    let frames_after = mem::frame_stats().live();
    check!(
        frames_after == frames_before,
        "frame leak across {ROUNDS} generations: {frames_before} -> {frames_after}"
    );
    let slab_after = mem::slab::stats().live_bytes;
    check!(
        slab_after == slab_before,
        "slab leak across {ROUNDS} generations: {slab_before} -> {slab_after}"
    );
    check!(
        baseline[sysinfo::H_TASKS_LIVE] == 1,
        "the kernel task vanished before the soak"
    );
    fresh();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("sysinfo_snapshot_abi_contract", snapshot_abi_contract),
    (
        "sysinfo_snapshot_rejects_bad_destinations",
        snapshot_rejects_bad_destinations,
    ),
    ("sysinfo_snapshot_fields_sane", snapshot_fields_sane),
    (
        "sysinfo_snapshot_reflects_spawned_task",
        snapshot_reflects_spawned_task,
    ),
    ("sysinfo_soak_snapshot_task_churn", soak_snapshot_task_churn),
];
