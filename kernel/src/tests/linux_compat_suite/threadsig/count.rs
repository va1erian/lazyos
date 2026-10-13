//! The count of thread-directed signals a process has sent
//! (`LazyOS_ThreadSignals` in `/proc/self/status`): the evidence the
//! `jsc` fixture reads to show that a collector really suspended a parked
//! mutator with `tgkill` (tools/jsc/judge.py).

use super::group::*;
use super::*;
use crate::task::signal;

/// Block both test signals on every member: a thread that acts after
/// being signalled would take the signal on its syscall return, which needs
/// a real kernel stack, and the tests only count the sends.
fn block_all(g: &Group) {
    for slot in g.members() {
        signal::set_blocked(slot, kbit(USR1) | kbit(USR2));
    }
}

/// The count `slot` reads from its own `/proc/self/status`.
fn sent_by(slot: usize) -> Result<u64, String> {
    task::harness::switch_current(slot);
    let status = fabricated("/proc/self/status")?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("LazyOS_ThreadSignals:\t"))
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| format!("no LazyOS_ThreadSignals line in {status:?}"))
}

/// Every thread of a process reads the process's count; `tkill` and `tgkill`
/// with a real signal add one each; a probe (signal 0), a refused send and
/// the process-directed `kill` add nothing; a fork starts at zero and a
/// thread started later sees the process's count.
pub fn threadsig_counts_sent_signals() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    block_all(&g);
    for slot in g.members() {
        check!(sent_by(slot)? == 0, "member {slot} starts above zero");
    }

    check!(tgkill(&g, a, b, USR1) == 0, "tgkill from A to B");
    check!(
        as_thread(g.leader, TKILL, &[a as u64, USR2 as u64]) == 0,
        "tkill from the leader to A"
    );
    for slot in g.members() {
        check!(
            sent_by(slot)? == 2,
            "member {slot} reads {} after two sends",
            sent_by(slot)?
        );
    }

    // Nothing was sent by these.
    let wrong_group = [b as u64, a as u64, USR1 as u64];
    check!(as_thread(a, TKILL, &[b as u64, 0]) == 0, "probe");
    check!(as_thread(a, TGKILL, &wrong_group) == ESRCH, "wrong tgid");
    check!(as_thread(a, TKILL, &[b as u64, 65]) == EINVAL, "signal 65");
    check!(
        as_thread(a, TKILL, &[250, USR1 as u64]) == ESRCH,
        "unused tid"
    );
    let pid = g.leader as u64;
    check!(
        as_thread(task::KERNEL_TASK, KILL, &[pid, USR2 as u64]) == 0,
        "kill"
    );
    check!(
        sent_by(a)? == 2,
        "a probe, a refusal or kill moved the count to {}",
        sent_by(a)?
    );

    // A fork is a process of its own; a thread made later is the leader's.
    task::harness::switch_current(g.leader);
    let child = task::spawn_fork().map_err(to_string)?;
    let later = thread()?;
    check!(
        sent_by(child)? == 0 && sent_by(later)? == 2,
        "fork {}, new thread {}",
        sent_by(child)?,
        sent_by(later)?
    );
    // (A thread never signals itself here: delivery on its own syscall
    // return needs a real kernel stack.)
    check!(
        as_thread(child, TKILL, &[g.leader as u64, USR2 as u64]) == 0,
        "the child signals the parent"
    );
    check!(
        sent_by(child)? == 1 && sent_by(g.leader)? == 2,
        "the child's send reached the parent's count"
    );

    task::harness::finish(child, 0);
    task::harness::switch_current(g.leader);
    task::reap_child().ok_or("the fork was not reapable")?;
    reclaim_threads(&[later]);
    teardown(&g)
}

/// Soak: thousands of sends from rotating threads, mixed with probes and
/// refusals, are counted exactly, and nothing else (frames, slots) moves.
pub fn threadsig_count_soak() -> Result<(), String> {
    const ROUNDS: usize = 4000;
    let baseline_frames = mem::frame_stats().live();
    let baseline_slots = task::process::process_list().len();
    let g = group(4)?;
    let members = g.members();
    block_all(&g);
    let mut expected = 0u64;
    for round in 0..ROUNDS {
        let index = round % members.len();
        let from = members[index];
        // Never the sender itself (see the correctness test).
        let to =
            members[(index + 1 + (round / members.len()) % (members.len() - 1)) % members.len()];
        let sig = if round % 2 == 0 { USR1 } else { USR2 };
        match round % 4 {
            0 | 1 => {
                check!(tgkill(&g, from, to, sig) == 0, "round {round}: tgkill");
                expected += 1;
            }
            2 => {
                check!(
                    as_thread(from, TKILL, &[to as u64, sig as u64]) == 0,
                    "round {round}: tkill"
                );
                expected += 1;
            }
            _ => {
                check!(
                    as_thread(from, TKILL, &[to as u64, 0]) == 0
                        && as_thread(from, TKILL, &[250, sig as u64]) == ESRCH,
                    "round {round}: probe or refusal"
                );
            }
        }
        if round % 500 == 0 {
            let now = sent_by(members[(round / 500) % members.len()])?;
            check!(
                now == expected,
                "round {round}: reads {now}, sent {expected}"
            );
        }
    }
    check!(
        sent_by(g.leader)? == expected,
        "final count {} of {expected}",
        sent_by(g.leader)?
    );
    teardown(&g)?;
    check!(
        task::process::process_list().len() == baseline_slots
            && mem::frame_stats().live() <= baseline_frames,
        "the soak left slots or frames behind"
    );
    Ok(())
}
