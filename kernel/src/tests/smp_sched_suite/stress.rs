//! The stress cases: a token handed back and forth across CPUs, and a churn
//! of threads that start, exit on whatever CPU ran them and are reaped.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::super::gs_suite::{spawn_hostile, Hostile, PASSED};
use super::rig::*;
use super::*;
use crate::task::wait::WaitQueue;
use crate::task::WaitKind;

/// Hand-offs per pair (`--soak`: the long run).
const PINGPONGS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    200_000
} else {
    4_000
};
/// Short-lived threads the churn starts (`--soak`: more).
const CHURN: usize = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    5_000
} else {
    300
};
/// Ping-pong pairs.
const PAIRS: usize = 2;

/// Per thread of the pairs (index = pair * 2 + side): its queue.
static QUEUES: [WaitQueue; PAIRS * 2] = [const { WaitQueue::new(WaitKind::Sleep) }; PAIRS * 2];
/// Per pair: whose turn it is (0 or 1), and hand-offs so far.
static TURN: [AtomicU64; PAIRS] = [const { AtomicU64::new(0) }; PAIRS];
static HANDOFFS: [AtomicU64; PAIRS] = [const { AtomicU64::new(0) }; PAIRS];
/// Which threads are which: the next role handed out.
static NEXT_ROLE: AtomicUsize = AtomicUsize::new(0);
/// CPUs any pair thread ran on, threads finished.
static SEEN: AtomicU64 = AtomicU64::new(0);
static FINISHED: AtomicUsize = AtomicUsize::new(0);

/// One side of a pair: wait for its turn, count the hand-off, pass the turn
/// and wake the other side, until the pair has done [`PINGPONGS`].
extern "C" fn player() -> ! {
    let me = task::current();
    let role = NEXT_ROLE.fetch_add(1, Ordering::Relaxed);
    let (pair, side) = (role / 2, role % 2);
    loop {
        // The check and the park run under this CPU's entry-lock hold, so
        // the other side's pass and wake cannot fall between them.
        while TURN[pair].load(Ordering::Acquire) != side as u64 {
            if HANDOFFS[pair].load(Ordering::Relaxed) >= PINGPONGS {
                FINISHED.fetch_add(1, Ordering::Relaxed);
                exit_thread();
            }
            QUEUES[role].wait_ns(me, Some(now() + 10_000_000));
        }
        // The other side made the last hand-off and passed the turn back.
        if HANDOFFS[pair].load(Ordering::Relaxed) >= PINGPONGS {
            FINISHED.fetch_add(1, Ordering::Relaxed);
            exit_thread();
        }
        SEEN.fetch_or(1 << cpu(), Ordering::Relaxed);
        let done = HANDOFFS[pair].fetch_add(1, Ordering::Relaxed) + 1 >= PINGPONGS;
        TURN[pair].store(1 - side as u64, Ordering::Release);
        QUEUES[pair * 2 + (1 - side)].notify_all();
        if done {
            FINISHED.fetch_add(1, Ordering::Relaxed);
            exit_thread();
        }
    }
}

/// [`PAIRS`] pairs hand a token back and forth [`PINGPONGS`] times each
/// while the suite naps on the boot CPU: every hand-off counted once, the
/// threads ran on the APs, all finish and are reaped.
pub fn pingpong_soak() -> Result<(), String> {
    with_aps(|_| {
        NEXT_ROLE.store(0, Ordering::Relaxed);
        SEEN.store(0, Ordering::Relaxed);
        FINISHED.store(0, Ordering::Relaxed);
        for pair in 0..PAIRS {
            TURN[pair].store(0, Ordering::Relaxed);
            HANDOFFS[pair].store(0, Ordering::Relaxed);
        }
        for _ in 0..PAIRS * 2 {
            spawn("smp-player", player)?;
        }
        let started = now();
        let done = nap_until(600_000_000_000, || {
            FINISHED.load(Ordering::Relaxed) == PAIRS * 2
        });
        let ms = (now() - started) / 1_000_000;
        let reaped = reap(PAIRS * 2, 2_000_000_000);
        let seen = SEEN.load(Ordering::Relaxed);
        let counted: u64 = HANDOFFS
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .sum();
        crate::trace::test!(
            "smp_sched_pingpong_soak:INFO:pairs={PAIRS} handoffs={counted} ms={ms} seen={seen:#x}"
        );
        check!(
            done,
            "{} of {} players finished",
            FINISHED.load(Ordering::Relaxed),
            PAIRS * 2
        );
        check!(
            HANDOFFS
                .iter()
                .all(|count| count.load(Ordering::Relaxed) == PINGPONGS),
            "hand-offs per pair: {:?}",
            HANDOFFS
                .each_ref()
                .map(|count| count.load(Ordering::Relaxed))
        );
        check!(reaped == PAIRS * 2, "{reaped} players reaped");
        check!(seen & !1 != 0, "no hand-off ran on an AP ({seen:#x})");
        Ok(())
    })
}

/// A thread that only exits.
extern "C" fn brief() -> ! {
    SEEN.fetch_or(1 << cpu(), Ordering::Relaxed);
    exit_thread()
}

/// [`CHURN`] threads, a few at a time, each exiting at once on whatever CPU
/// ran it and reaped on the boot CPU: all reaped, some exited on an AP, and
/// every kernel stack came back.
pub fn thread_churn_soak() -> Result<(), String> {
    with_aps(|_| {
        SEEN.store(0, Ordering::Relaxed);
        let stacks = task::diag::live_kstacks();
        let mut reaped = 0;
        let mut spawned = 0;
        while spawned < CHURN {
            for _ in 0..4.min(CHURN - spawned) {
                spawn("smp-brief", brief)?;
                spawned += 1;
            }
            reaped += reap(spawned - reaped, 2_000_000_000);
            check!(
                reaped == spawned,
                "{reaped} of {spawned} brief threads reaped"
            );
        }
        let seen = SEEN.load(Ordering::Relaxed);
        crate::trace::test!("smp_sched_thread_churn_soak:INFO:threads={CHURN} seen={seen:#x}");
        check!(seen & !1 != 0, "no brief thread ran on an AP ({seen:#x})");
        check!(
            task::diag::live_kstacks() == stacks,
            "{} kernel stacks left",
            task::diag::live_kstacks() - stacks
        );
        Ok(())
    })
}

/// User tasks the ring-3 case runs at once, and rounds each.
const USER_TASKS: usize = 4;
const USER_ROUNDS: u32 = 60;

/// Forked tasks run a ring-3 program (`gs_suite`'s: `syscall`, `int 0x80`
/// and a spin the tick preempts, every round checked) while the suite naps
/// on the boot CPU, so they run, enter the kernel and are preempted on the
/// APs: every task passes (its syscall stack, `RSP0`, `%gs` and `FS` base
/// were its own on whatever CPU it ran). Then the process of each is killed
/// while it spins in ring 3 on an AP: the kill reaches it there (its CPU is
/// kicked and its own sweep ends it), it leaves the CPU and is reclaimed.
pub fn ring3_tasks_on_aps() -> Result<(), String> {
    with_aps(|_| {
        let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
        let result = ring3_rounds(&mut alloc::vec::Vec::new());
        crate::user_ptr::set_trust_kernel_pointers(trusted);
        result
    })
}

/// The body of [`ring3_tasks_on_aps`], with user pointers checked for real.
fn ring3_rounds(tasks: &mut alloc::vec::Vec<Hostile>) -> Result<(), String> {
    let free = task::free_slots();
    for _ in 0..USER_TASKS {
        tasks.push(spawn_hostile(USER_ROUNDS, false)?);
    }
    let mut cpus = 0u64;
    let finished = nap_until(30_000_000_000, || {
        for hostile in tasks.iter() {
            if let Some(cpu) = crate::task::oncpu::cpu_of(hostile.slot) {
                SEEN.fetch_or(1 << cpu, Ordering::Relaxed);
            }
        }
        tasks.iter().all(|hostile| hostile.result() != 0)
    });
    cpus |= SEEN.swap(0, Ordering::Relaxed);
    let results: alloc::vec::Vec<u32> = tasks.iter().map(|hostile| hostile.result()).collect();
    // Now each spins on `pause` in ring 3: kill them where they run.
    let running_on_aps = tasks
        .iter()
        .filter(|hostile| crate::task::oncpu::cpu_of(hostile.slot).is_some_and(|cpu| cpu != 0))
        .count();
    for hostile in tasks.iter() {
        if let Some(pml4) = task::harness::pml4(hostile.slot) {
            crate::task::signal::terminate_process(pml4, 0);
        }
    }
    let gone = nap_until(5_000_000_000, || {
        task::reclaim_pending();
        task::free_slots() == free
    });
    crate::trace::test!(
        "smp_sched_ring3_tasks_on_aps:INFO:tasks={USER_TASKS} cpus={cpus:#x} killed_on_aps={running_on_aps} results={results:?}"
    );
    check!(finished, "the user tasks did not finish: {results:?}");
    check!(
        results.iter().all(|&r| r == PASSED),
        "results {results:?}, wanted {PASSED}"
    );
    check!(cpus & !1 != 0, "no user task ran on an AP ({cpus:#x})");
    check!(
        gone,
        "{} of {USER_TASKS} killed tasks were never reclaimed",
        free - task::free_slots()
    );
    Ok(())
}
