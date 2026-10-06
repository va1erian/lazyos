//! The descriptor and CPU quotas, charged where the resources are used
//! (issue #483): `Resource::Fds` by the descriptor table, `Resource::CpuTicks`
//! by the scheduler.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::quota::{self, Resource};
use crate::task::{Fd, PriorityClass, TaskState};

/// A regular uid for the descriptor tests.
const FD_UID: u32 = 48_301;
/// The capped and the uncapped uid of the CPU tests.
const CPU_CAPPED: u32 = 48_302;
const CPU_FREE: u32 = 48_303;

/// Fresh ledger, the kernel task as root with a clean table.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    quota::reset();
}

/// Run the kernel task as `uid` (no capabilities) until [`as_root`].
fn as_user(uid: u32) {
    credentials::set(task::KERNEL_TASK, Cred::new(uid, uid, 0, 0, 0));
}

fn as_root() {
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
}

fn fds_of(uid: u32) -> u64 {
    quota::usage(uid, Resource::Fds)
}

/// Close every descriptor in `fds`, then go back to root.
fn close_all(fds: &[usize]) {
    for &fd in fds {
        task::fd_close(fd);
    }
    as_root();
}

/// A uid capped at N descriptors opens N and gets `EMFILE` (`None`) on the
/// next open or `dup`; `dup2` over an open descriptor still works at the cap,
/// a close makes room again, and closing everything returns the charge.
pub fn quota_fds_cap_gives_emfile() -> Result<(), String> {
    const CAP: u64 = 6;
    fresh();
    quota::set_limit(FD_UID, Resource::Fds, CAP);
    as_user(FD_UID);
    let mut opened = Vec::new();
    for index in 0..CAP {
        match task::fd_open(Fd::Terminal) {
            Some(fd) => opened.push(fd),
            None => {
                close_all(&opened);
                return Err(format!("open {index} of {CAP} was refused"));
            }
        }
    }
    let refused = task::fd_open(Fd::Terminal).is_none()
        && task::fd_dup(opened[0]).is_none()
        && task::fd_dup_min(opened[0], 40).is_none();
    let at_cap = fds_of(FD_UID);
    // `dup2` onto an open descriptor replaces it: no new charge is needed.
    let replaced = task::fd_dup2(opened[0], opened[1]) == Some(opened[1]);
    let stats = quota::stats(FD_UID);
    task::fd_close(opened.pop().unwrap_or(0));
    let reopened = task::fd_open(Fd::Terminal);
    if let Some(fd) = reopened {
        opened.push(fd);
    }
    let full = fds_of(FD_UID);
    close_all(&opened);
    check!(refused, "fd {} was granted past a cap of {CAP}", CAP + 1);
    check!(
        at_cap == CAP,
        "usage at the cap is {at_cap}, expected {CAP}"
    );
    check!(replaced, "dup2 over an open descriptor failed at the cap");
    check!(stats.denials >= 3, "denials were not counted: {stats:?}");
    check!(reopened.is_some(), "a close did not make room for an open");
    check!(full == CAP, "usage after reopening is {full}");
    check!(
        fds_of(FD_UID) == 0,
        "closing everything left {} charged",
        fds_of(FD_UID)
    );
    check!(
        quota::stats(FD_UID).over_releases == 0,
        "a descriptor was released twice"
    );
    Ok(())
}

/// `fork` charges a copy of every descriptor to the uid, is refused when the
/// copy does not fit, and the child's charge goes back when it is reaped.
pub fn quota_fds_fork_charges_the_copy() -> Result<(), String> {
    fresh();
    let base = task::harness::fd_open_count(task::KERNEL_TASK) as u64;
    as_user(FD_UID);
    let mut opened = Vec::new();
    for _ in 0..4 {
        opened.extend(task::fd_open(Fd::Terminal));
    }
    let own = fds_of(FD_UID);
    // Room for exactly one copy of the whole table (inherited root-charged
    // descriptors are copied, and charged, to the forking uid).
    quota::set_limit(FD_UID, Resource::Fds, own + own + base);
    let first = task::spawn_fork();
    let after_fork = fds_of(FD_UID);
    let second = task::spawn_fork();
    task::harness::switch_current(task::KERNEL_TASK);
    if let Ok(child) = first {
        task::harness::finish(child, 0);
    }
    if let Ok(child) = second {
        task::harness::finish(child, 0);
    }
    while task::reap_child().is_some() {}
    task::harness::reset();
    let reaped = fds_of(FD_UID);
    close_all(&opened);
    check!(own == 4, "four opens charged {own}");
    check!(first.is_ok(), "a fork that fits was refused: {first:?}");
    check!(
        after_fork == own + own + base,
        "the fork charged {} for a {}-descriptor table",
        after_fork - own,
        own + base
    );
    check!(
        second.is_err(),
        "a fork past the descriptor quota succeeded"
    );
    check!(
        reaped == own,
        "reaping left {reaped} charged, expected {own}"
    );
    check!(fds_of(FD_UID) == 0, "{} still charged", fds_of(FD_UID));
    Ok(())
}

/// 100,000 open/close (and dup/dup2) rounds against a small cap leak no
/// charge and never release one twice.
pub fn quota_fds_soak_open_close() -> Result<(), String> {
    const ROUNDS: usize = 100_000;
    fresh();
    quota::set_limit(FD_UID, Resource::Fds, 3);
    as_user(FD_UID);
    for round in 0..ROUNDS {
        let Some(fd) = task::fd_open(Fd::Terminal) else {
            as_root();
            return Err(format!("round {round}: open refused below the cap"));
        };
        if round % 7 == 0 {
            let copy = task::fd_dup(fd);
            let moved = copy.and_then(|copy| task::fd_dup2(fd, copy));
            if let Some(copy) = copy {
                task::fd_close(copy);
            }
            if copy.is_none() || moved.is_none() {
                task::fd_close(fd);
                as_root();
                return Err(format!("round {round}: dup/dup2 failed under the cap"));
            }
        }
        task::fd_close(fd);
        if fds_of(FD_UID) != 0 {
            as_root();
            return Err(format!("round {round}: {} left charged", fds_of(FD_UID)));
        }
    }
    as_root();
    let stats = quota::stats(FD_UID);
    check!(
        stats.usage[Resource::Fds.index()] == 0 && stats.over_releases == 0,
        "after the soak: {stats:?}"
    );
    serial_println!(
        "TEST:quota_fds_soak_open_close:INFO:rounds={ROUNDS} peak={}",
        stats.peak[Resource::Fds.index()]
    );
    Ok(())
}

/// Park the kernel and start two `Normal` tasks of equal weight, one per
/// uid; the CPU tests simulate ticks between them.
fn cpu_pair() -> Result<(usize, usize), String> {
    fresh();
    task::set_blocked(true);
    let capped = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let free = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    for slot in [capped, free] {
        check!(
            task::set_priority(slot, PriorityClass::Normal) && task::set_weight(slot, 4),
            "slot {slot} refused its class"
        );
    }
    credentials::set(capped, Cred::new(CPU_CAPPED, CPU_CAPPED, 0, 0, 0));
    credentials::set(free, Cred::new(CPU_FREE, CPU_FREE, 0, 0, 0));
    task::harness::switch_current(capped);
    Ok((capped, free))
}

/// Finish the pair, wake the kernel and reset the table.
fn cpu_cleanup() {
    for slot in 1..task::MAX_TASKS {
        if task::harness::state(slot).is_some_and(|state| state != TaskState::Done) {
            task::harness::finish(slot, 0);
        }
    }
    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    task::set_blocked(false);
    task::harness::reset();
    quota::reset();
}

/// Ticks per slot over `ticks` simulated timer decisions.
fn run_ticks(ticks: usize, a: usize, b: usize) -> (u64, u64) {
    let (mut first, mut second) = (0u64, 0u64);
    for _ in 0..ticks {
        let picked = task::harness::simulate_tick();
        if picked == a {
            first += 1;
        } else if picked == b {
            second += 1;
        }
    }
    (first, second)
}

/// Every tick is booked to the running task's uid, and a uid past its CPU
/// budget gets markedly fewer ticks than an uncapped peer of the same class
/// and weight, without being starved.
pub fn quota_cpu_capped_uid_gets_fewer_ticks() -> Result<(), String> {
    const TICKS: usize = 2000;
    let (capped, free) = cpu_pair()?;
    quota::set_limit(CPU_CAPPED, Resource::CpuTicks, 20);
    let (capped_runs, free_runs) = run_ticks(TICKS, capped, free);
    let capped_ticks = task::cpu_ticks(capped);
    let free_ticks = task::cpu_ticks(free);
    let capped_usage = quota::usage(CPU_CAPPED, Resource::CpuTicks);
    let free_usage = quota::usage(CPU_FREE, Resource::CpuTicks);
    let over = quota::cpu::over_cap(capped);
    cpu_cleanup();
    check!(
        capped_usage == capped_ticks && free_usage == free_ticks,
        "quota usage {capped_usage}/{free_usage} != task ticks {capped_ticks}/{free_ticks}"
    );
    check!(over, "the capped uid was never marked over its budget");
    check!(
        capped_runs > 0 && capped_runs * 4 < free_runs,
        "capped uid ran {capped_runs} ticks, uncapped peer {free_runs}"
    );
    serial_println!(
        "TEST:quota_cpu_capped_uid_gets_fewer_ticks:INFO:capped={capped_runs} free={free_runs}"
    );
    Ok(())
}

/// Equal uncapped peers still split evenly (the penalty applies only past a
/// budget), and raising the budget restores an even split.
pub fn quota_cpu_uncapped_peers_split_evenly() -> Result<(), String> {
    let (capped, free) = cpu_pair()?;
    let (even_a, even_b) = run_ticks(1000, capped, free);
    quota::set_limit(CPU_CAPPED, Resource::CpuTicks, 0);
    let (slow, fast) = run_ticks(1000, capped, free);
    quota::set_limit(CPU_CAPPED, Resource::CpuTicks, u64::MAX);
    // The flag follows the next booked tick of the capped task.
    let (again_a, again_b) = run_ticks(1000, capped, free);
    cpu_cleanup();
    check!(
        even_a.abs_diff(even_b) <= 2,
        "uncapped peers split {even_a}/{even_b}"
    );
    check!(slow * 4 < fast, "a zero budget still split {slow}/{fast}");
    check!(
        again_a.abs_diff(again_b) <= 80,
        "a raised budget split {again_a}/{again_b}"
    );
    Ok(())
}

/// A long run of ticks across budget changes books every tick exactly once
/// to the uid that consumed it and never starves the capped task.
pub fn quota_cpu_soak_booking() -> Result<(), String> {
    const ROUNDS: usize = 40;
    let (capped, free) = cpu_pair()?;
    let mut budget = 0u64;
    for round in 0..ROUNDS {
        budget += 50 * (round as u64 % 3);
        quota::set_limit(CPU_CAPPED, Resource::CpuTicks, budget);
        let (a, b) = run_ticks(500, capped, free);
        if a == 0 || a + b != 500 {
            cpu_cleanup();
            return Err(format!("round {round}: split {a}/{b}"));
        }
    }
    let booked =
        quota::usage(CPU_CAPPED, Resource::CpuTicks) + quota::usage(CPU_FREE, Resource::CpuTicks);
    let consumed = task::cpu_ticks(capped) + task::cpu_ticks(free);
    cpu_cleanup();
    check!(
        booked == consumed,
        "booked {booked} ticks for {consumed} consumed"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("quota_fds_cap_gives_emfile", quota_fds_cap_gives_emfile),
    (
        "quota_fds_fork_charges_the_copy",
        quota_fds_fork_charges_the_copy,
    ),
    ("quota_fds_soak_open_close", quota_fds_soak_open_close),
    (
        "quota_cpu_capped_uid_gets_fewer_ticks",
        quota_cpu_capped_uid_gets_fewer_ticks,
    ),
    (
        "quota_cpu_uncapped_peers_split_evenly",
        quota_cpu_uncapped_peers_split_evenly,
    ),
    ("quota_cpu_soak_booking", quota_cpu_soak_booking),
];
