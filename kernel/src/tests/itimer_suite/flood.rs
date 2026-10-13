//! What an unprivileged program can make the timer path do (a flood of
//! expiries, the timer pools), and the identity rules: the signal number as
//! an `int`, a thread slot reused, `execve` in a threaded process.

use super::*;
use crate::ipc::credentials::{self, Cred, CAP_SYS_ADMIN};
use crate::task::itimer::{Caller, Kind, Notify};
use crate::task::signal::{Disposition, SIGALRM, SIGUSR1, SI_KERNEL};
use crate::task::{TaskState, WaitKind};

const SIGEV_SIGNAL: u64 = 0;
const ABSTIME: u64 = 1;

/// Give slot `slot` an identity with or without `CAP_SYS_ADMIN`.
fn identity(slot: usize, privileged: bool) {
    let cred = Cred {
        uid: 1000,
        caps: if privileged { CAP_SYS_ADMIN } else { 0 },
        ..credentials::of(slot)
    };
    credentials::set(slot, cred);
}

fn sigevent(signo: u32, notify: u64, tid: u32) -> [u64; 8] {
    let mut event = [0u64; 8];
    event[1] = u64::from(signo) | (notify << 32);
    event[2] = u64::from(tid);
    event
}

/// `timer_create(CLOCK_MONOTONIC)` as the current task.
fn create(event: &[u64; 8]) -> Result<i32, u64> {
    let mut id = -1i32;
    let ret = process::linux::dispatch_for_test(SYS_TIMER_CREATE, 1, ptr(event), ptr_mut(&mut id));
    if ret == 0 {
        Ok(id)
    } else {
        Err(ret)
    }
}

fn settime(id: i32, flags: u64, spec_ns: [i64; 4]) -> u64 {
    process::linux::dispatch_args_for_test(SYS_TIMER_SETTIME, id as u64, flags, ptr(&spec_ns), 0)
}

fn interval_of(id: i32) -> Result<u64, String> {
    let mut out = [0i64; 4];
    let ret = process::linux::dispatch_for_test(SYS_TIMER_GETTIME, id as u64, ptr_mut(&mut out), 0);
    check!(ret == 0, "timer_gettime {ret:#x}");
    Ok(out[0] as u64 * NS + out[1] as u64)
}

fn overrun_of(id: i32) -> u64 {
    process::linux::dispatch_for_test(SYS_TIMER_GETOVERRUN, id as u64, 0, 0)
}

/// 64 timers at the shortest interval an unprivileged program may ask for
/// share one blocked signal: 500 ms of expiries send it once, the rest only
/// count, and `timer_getoverrun` agrees with the count.
pub fn pending_signal_is_cheap() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    identity(p, false);
    let event = sigevent(u32::from(SIGUSR1), SIGEV_SIGNAL, 0);
    let mut ids = alloc::vec::Vec::new();
    for _ in 0..64 {
        let id = create(&event).map_err(|e| format!("create {e:#x}"))?;
        // Ask for 100 us: below the unprivileged floor.
        check!(
            settime(id, 0, [0, 100_000, 0, 1_000_000]) == 0,
            "timer_settime"
        );
        ids.push(id);
    }
    let floor = interval_of(ids[0])?;
    check!(floor == 1_000_000, "the unprivileged floor is {floor} ns");
    let before = itimer::fired_total();
    let start = now();
    for k in 1..=500u64 {
        itimer::fire_due(start + k * 1_000_000);
    }
    let sent = itimer::fired_total() - before;
    check!(
        sent == 1,
        "{} expiries of a pending signal queued it {sent} times",
        64 * 500
    );
    check!(
        pending(p) & bit(SIGUSR1) != 0,
        "the signal was never queued"
    );
    let over = overrun_of(ids[5]);
    check!(
        (480..=500).contains(&over),
        "timer_getoverrun counted {over} of ~500 expiries"
    );
    itimer::check_for_test()?;
    finish(&[p])
}

/// An ignored signal costs no send either; a privileged caller keeps the
/// short floor; a one-timer run counts the overrun exactly (one signal, then
/// one per expiry that found it still pending).
pub fn ignored_and_floors() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    identity(p, false);
    signal::set_action(p, SIGUSR1, Disposition::Ignore).map_err(|e| format!("{e:?}"))?;
    let event = sigevent(u32::from(SIGUSR1), SIGEV_SIGNAL, 0);
    let id = create(&event).map_err(|e| format!("create {e:#x}"))?;
    settime(id, 0, [0, 1_000_000, 0, 1_000_000]);
    let before = itimer::fired_total();
    let start = now();
    for k in 1..=100u64 {
        itimer::fire_due(start + k * 1_000_000);
    }
    check!(
        itimer::fired_total() == before,
        "an ignored signal went through the send path"
    );
    check!(pending(p) & bit(SIGUSR1) == 0, "an ignored signal queued");

    // Privileged: the floor is 100 us.
    identity(p, true);
    let fast = create(&event).map_err(|e| format!("create {e:#x}"))?;
    check!(settime(fast, 0, [0, 100_000, 0, 100_000]) == 0, "settime");
    let floor = interval_of(fast)?;
    check!(floor == 100_000, "the privileged floor is {floor} ns");
    check!(itimer::in_use() == 2, "{} timers", itimer::in_use());

    // Exact overrun accounting with one timer and a blocked signal.
    itimer::forget(task::pml4_of(p).ok_or("no pml4")?);
    signal::set_action(p, SIGUSR1, Disposition::Default).map_err(|e| format!("{e:?}"))?;
    let one = create(&event).map_err(|e| format!("create {e:#x}"))?;
    settime(one, 0, [0, 1_000_000, 0, 1_000_000]);
    let before = itimer::fired_total();
    let start = now();
    for k in 1..=3u64 {
        itimer::fire_due(start + k * 1_000_000);
    }
    check!(
        itimer::fired_total() == before + 1,
        "three expiries sent {} signals",
        itimer::fired_total() - before
    );
    let over = overrun_of(one);
    check!(over == 2, "two suppressed expiries read as overrun {over}");
    finish(&[p])
}

/// `timer_create` can exhaust only its own pool: `alarm` and `setitimer`
/// still work, and one user cannot take more than its share.
pub fn pools() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let per = itimer::MAX_POSIX_PER_PROCESS;
    let spaces = itimer::POSIX_POOL / per;
    for space in 0..spaces {
        for _ in 0..per {
            itimer::create(
                0x100_0000 + space as u64 * 0x1000,
                1,
                Caller::KERNEL,
                Notify::Process,
                SIGUSR1,
                None,
                1,
            )
            .map_err(|e| format!("space {space}: {e:?}"))?;
        }
    }
    check!(
        itimer::create(
            0x200_0000,
            1,
            Caller::KERNEL,
            Notify::Process,
            SIGUSR1,
            None,
            1
        ) == Err(itimer::Error::NoSpace),
        "the POSIX pool did not run out"
    );
    // The system-wide exhaustion is `EAGAIN` for timer_create alone.
    let event = sigevent(u32::from(SIGUSR1), SIGEV_SIGNAL, 0);
    check!(create(&event) == Err(EAGAIN), "timer_create in a full pool");
    let ret = process::linux::dispatch_for_test(SYS_ALARM, 5, 0, 0);
    check!(ret == 0, "alarm returned {ret}");
    let (left, _) = getitimer(0)?;
    check!(
        left > 4 * NS,
        "alarm was dropped with the POSIX pool full: {left} ns"
    );
    let t = spec((0, 0), (9, 0));
    check!(setitimer(1, &t, None) == 0, "setitimer with the pool full");
    itimer::reset_for_test();

    // One uid: 128 timers over two processes, the 129th refused; another uid
    // is unaffected.
    let user = Caller {
        uid: 4242,
        privileged: false,
    };
    for space in 0..2u64 {
        for _ in 0..per {
            itimer::create(
                0x300_0000 + space * 0x1000,
                1,
                user,
                Notify::Process,
                SIGUSR1,
                None,
                1,
            )
            .map_err(|e| format!("uid space {space}: {e:?}"))?;
        }
    }
    let third = itimer::create(0x300_2000, 1, user, Notify::Process, SIGUSR1, None, 1);
    check!(
        third == Err(itimer::Error::NoSpace),
        "the per-user cap was not enforced: {third:?}"
    );
    let other = Caller {
        uid: 4243,
        privileged: false,
    };
    check!(
        itimer::create(0x300_3000, 1, other, Notify::Process, SIGUSR1, None, 1).is_ok(),
        "another user was refused"
    );
    itimer::reset_for_test();
    finish(&[p])
}

/// The signal number is checked as the `int` user space passed, not after a
/// cast to a byte (270 is not `SIGALRM`), and job-control signals are refused.
pub fn signo_validated() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    for (signo, ok) in [
        (270u32, false),
        (u32::MAX, false),
        (0, false),
        (65, false),
        (18, false), // SIGCONT
        (20, false), // SIGTSTP
        (9, false),
        (64, true),
        (u32::from(SIGALRM), true),
    ] {
        let got = create(&sigevent(signo, SIGEV_SIGNAL, 0));
        check!(
            got.is_ok() == ok,
            "signo {signo}: timer_create returned {got:?}, wanted ok={ok}"
        );
    }
    finish(&[p])
}

/// A `SIGEV_THREAD_ID` timer remembers the thread, not just its slot: a
/// later task in that slot is not signalled.
pub fn generation() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let pml4 = task::pml4_of(p).ok_or("no pml4")?;
    let thread = task::spawn_thread("itimer-gen", process::USER_STACK_TOP, 0, 0)
        .map_err(|e| format!("thread: {e}"))?;
    let generation = signal::thread_generation(thread);
    check!(generation != 0, "a live thread has no generation");
    let stale = Notify::Thread {
        slot: thread,
        generation: generation + 1,
    };
    let live = Notify::Thread {
        slot: thread,
        generation,
    };
    let id = |notify| {
        itimer::create(pml4, p, Caller::KERNEL, notify, SIGUSR1, None, 1)
            .map_err(|e| format!("{e:?}"))
    };
    let start = now();
    let stale_id = id(stale)?;
    itimer::settime(pml4, stale_id, start, Some(start + 1), 0).map_err(|e| format!("{e:?}"))?;
    let before = itimer::fired_total();
    itimer::fire_due(start + NS);
    check!(
        itimer::fired_total() == before && signal::pending_thread(thread) & bit(SIGUSR1) == 0,
        "a timer for an earlier task in the slot signalled the thread"
    );
    let live_id = id(live)?;
    itimer::settime(pml4, live_id, start, Some(start + 1), 0).map_err(|e| format!("{e:?}"))?;
    itimer::fire_due(start + NS);
    check!(
        signal::pending_thread(thread) & bit(SIGUSR1) != 0,
        "the live thread was not signalled"
    );
    finish(&[p, thread])
}

/// `setitimer(which, NULL, old)` disarms and reports; an itimer's signal
/// carries `SI_KERNEL` and no sending pid.
pub fn null_new_and_si_pid() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let t = spec((0, 0), (5, 0));
    check!(setitimer(0, &t, None) == 0, "setitimer");
    let mut old = [-1i64; 4];
    let ret = process::linux::dispatch_for_test(SYS_SETITIMER, 0, 0, ptr_mut(&mut old));
    check!(ret == 0, "setitimer(REAL, NULL, old) returned {ret:#x}");
    check!(old[2] == 4 || old[2] == 5, "old value {old:?}");
    let (left, _) = getitimer(0)?;
    check!(left == 0, "NULL did not disarm: {left} ns left");

    process::linux::dispatch_for_test(SYS_ALARM, 1, 0, 0);
    itimer::fire_due(now() + 2 * NS);
    check!(pending(p) & bit(SIGALRM) != 0, "no SIGALRM");
    let info = signal::harness::recorded_process_info(p, SIGALRM).ok_or("no info")?;
    check!(
        info.code == SI_KERNEL && info.pid == 0,
        "ITIMER_REAL's siginfo: code {} pid {}",
        info.code,
        info.pid
    );
    finish(&[p])
}

/// `execve` by one thread of a process whose other thread still lives keeps
/// the process's timers where they are (nothing moves, nothing is deleted).
pub fn exec_threaded() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let pml4 = task::pml4_of(p).ok_or("no pml4")?;
    process::linux::dispatch_for_test(SYS_ALARM, 30, 0, 0);
    let id = create(&sigevent(u32::from(SIGUSR1), SIGEV_SIGNAL, 0))
        .map_err(|e| format!("create {e:#x}"))?;
    check!(settime(id, 0, [0, 0, 20, 0]) == 0, "settime");
    let thread = task::spawn_thread("itimer-exec", process::USER_STACK_TOP, 0, 0)
        .map_err(|e| format!("thread: {e}"))?;
    // The thread "execs" (the kernel table stands in for the new image).
    task::harness::switch_current(thread);
    let kernel = crate::mem::kernel_table().as_u64();
    task::set_pml4(kernel);
    let real = itimer::get_interval(pml4, Kind::Real, now());
    check!(
        real.remaining_ns > 25 * NS,
        "the sibling's alarm moved with the exec: {real:?}"
    );
    check!(
        itimer::gettime(pml4, id, now()).is_ok(),
        "the sibling's POSIX timer was deleted by the exec"
    );
    task::set_pml4(pml4);
    task::harness::switch_current(p);
    finish(&[p, thread])
}

/// A due timer is fired by a real scheduler entry (`decide`, through a yield),
/// not only by `expire_due`.
pub fn decide_fires() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let mut id = -1i32;
    let ret = process::linux::dispatch_for_test(SYS_TIMER_CREATE, 1, 0, ptr_mut(&mut id));
    check!(ret == 0, "timer_create returned {ret:#x}");
    check!(settime(id, ABSTIME, [0, 0, 0, 1]) == 0, "timer_settime");
    check!(
        pending(p) & bit(SIGALRM) == 0,
        "fired before a scheduler entry"
    );
    // Keep the process out of the run queue, and enter the scheduler as the
    // kernel task would.
    task::harness::set_state(
        p,
        TaskState::Blocked {
            wait: WaitKind::Signal,
            deadline: None,
        },
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::switch::yield_now();
    check!(
        pending(p) & bit(SIGALRM) != 0,
        "a scheduler entry did not fire the due timer"
    );
    finish(&[p])
}
