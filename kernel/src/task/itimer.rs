//! Interval timers and POSIX timers (issue #535, gap 3): `setitimer`
//! (`ITIMER_REAL`, `ITIMER_VIRTUAL`, `ITIMER_PROF`), `alarm` and
//! `timer_create`; the syscall ABI is `process::linux::timers`.
//!
//! **Where time comes from.** Wall-clock style timers (`ITIMER_REAL`, every
//! POSIX timer) hold an absolute deadline on the monotonic clock, ordered in
//! an indexed min-heap (the same [`TimerQueue`] the sleeping tasks use, keyed
//! by timer index instead of task slot). The earliest deadline is mirrored in
//! [`NEXT_DEADLINE`], which the one-shot deadline timer
//! (`arch::event_timer`, via `sched::next_event`) folds in, so a timer fires
//! at its instant when the APIC timer exists and at the next 10 ms tick when
//! it does not. Nothing polls: with no timer armed the scheduler pays one
//! relaxed atomic load.
//!
//! CPU-time timers (`ITIMER_VIRTUAL`, `ITIMER_PROF`) are driven by the
//! scheduler's tick accounting, so they are approximations: each timer tick
//! is booked, whole, to the task it interrupted, and `ITIMER_VIRTUAL` counts
//! the ticks that landed in ring 3 while `ITIMER_PROF` counts every tick
//! charged to the process (user and system). Time a task spends inside a
//! syscall with interrupts off is folded into the next tick (the catch-up
//! periods of issue #344), and a process's threads add up, like Linux's.
//!
//! **Firing context.** Expiry runs at a scheduler entry or the deadline
//! interrupt *before* the task table is locked (`fire_due`): it pops the due
//! timers under [`TABLE`], drops that lock, then queues each signal through
//! the signal layer like any other sender, which interrupts a blocking
//! syscall (`EINTR`, or `SA_RESTART`) exactly as `kill` does. Nothing in the
//! path allocates.
//!
//! **Ownership.** A timer belongs to a process (its address space, the key
//! the signal layer uses) and dies with it: `fork` gives the child none,
//! `execve` keeps the three interval timers and deletes POSIX timers (as
//! Linux does), and the last thread's exit deletes the rest. Lock order:
//! task table, then [`TABLE`]; the signal layer is never called with
//! [`TABLE`] held.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use spin::Mutex;

use super::signal;
use super::timerq::TimerQueue;
use super::MAX_TASKS;

/// Timers the system can hold at once, across every process.
pub const MAX_TIMERS: usize = MAX_TASKS;
/// POSIX timers one process may own (`RLIMIT_SIGPENDING` stands in on Linux).
pub const MAX_POSIX_PER_PROCESS: usize = 64;
/// Shortest repeat interval honoured (nanoseconds). A shorter one would turn
/// into an interrupt storm; Linux rounds up to its clock resolution too.
pub const MIN_INTERVAL_NS: u64 = 100_000;

/// Which timer a process-level slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `ITIMER_REAL`: wall time, `SIGALRM`.
    Real,
    /// `ITIMER_VIRTUAL`: user CPU time, `SIGVTALRM`.
    Virtual,
    /// `ITIMER_PROF`: user and system CPU time, `SIGPROF`.
    Prof,
    /// A `timer_create` timer.
    Posix,
}

impl Kind {
    fn is_cpu(self) -> bool {
        matches!(self, Kind::Virtual | Kind::Prof)
    }
}

/// What a POSIX timer does on expiry (`sigev_notify`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notify {
    /// `SIGEV_NONE`: the timer only counts.
    None,
    /// `SIGEV_SIGNAL`: the signal goes to the process.
    Process,
    /// `SIGEV_THREAD_ID`: the signal goes to this thread alone.
    Thread(usize),
}

/// The remaining time and interval of a timer, in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub remaining_ns: u64,
    pub interval_ns: u64,
}

/// Why a timer call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The table or the process's quota is full (`EAGAIN`).
    NoSpace,
    /// No such timer id (`EINVAL`).
    NoSuchTimer,
}

#[derive(Clone, Copy)]
struct Timer {
    used: bool,
    armed: bool,
    kind: Kind,
    pml4: u64,
    /// POSIX timer id, unique inside the process.
    id: i32,
    notify: Notify,
    signo: u8,
    /// `sigev_value`, returned in `si_value`.
    value: u64,
    /// The clock a POSIX timer was created on (`CLOCK_REALTIME`/`MONOTONIC`).
    clock: u8,
    interval_ns: u64,
    /// Monotonic expiry (`Real`, `Posix`).
    deadline_ns: u64,
    /// CPU time left (`Virtual`, `Prof`).
    remaining_ns: u64,
    /// Expirations missed between the last two signals (`timer_getoverrun`).
    overrun: u32,
    /// The thread that created the timer: the preferred signal target.
    owner: usize,
}

impl Timer {
    const EMPTY: Timer = Timer {
        used: false,
        armed: false,
        kind: Kind::Real,
        pml4: 0,
        id: 0,
        notify: Notify::Process,
        signo: 0,
        value: 0,
        clock: 0,
        interval_ns: 0,
        deadline_ns: 0,
        remaining_ns: 0,
        overrun: 0,
        owner: 0,
    };

    fn reading(&self, now: u64) -> Reading {
        let remaining_ns = match (self.armed, self.kind.is_cpu()) {
            (false, _) => 0,
            (true, true) => self.remaining_ns,
            // A timer that is due but not yet fired still has time left:
            // report the smallest nonzero value so it reads as armed.
            (true, false) => self.deadline_ns.saturating_sub(now).max(1),
        };
        Reading {
            remaining_ns,
            interval_ns: self.interval_ns,
        }
    }
}

struct Table {
    timers: [Timer; MAX_TIMERS],
    /// Expiry order of the wall-clock timers (key: timer index).
    queue: TimerQueue,
    /// Armed CPU-time timers.
    cpu_armed: usize,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    timers: [Timer::EMPTY; MAX_TIMERS],
    queue: TimerQueue::new(),
    cpu_armed: 0,
});

/// The earliest wall-clock deadline (`u64::MAX` when none), readable without
/// the lock by the scheduler's hot paths.
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Armed CPU-time timers, for the tick path's one-load fast exit.
static CPU_ARMED: AtomicUsize = AtomicUsize::new(0);
/// Signals the timers have sent since boot (the test suites count with it).
static FIRED: AtomicU64 = AtomicU64::new(0);
/// Timers in use, so exit paths skip the table scan when there are none.
static IN_USE: AtomicUsize = AtomicUsize::new(0);

/// Run `f` on the table with interrupts off (the lock is taken from
/// interrupt context too).
fn with_table<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let result = f(&mut table);
        NEXT_DEADLINE.store(
            table
                .queue
                .peek()
                .map_or(u64::MAX, |(deadline, _)| deadline),
            Ordering::Relaxed,
        );
        CPU_ARMED.store(table.cpu_armed, Ordering::Relaxed);
        result
    })
}

/// The earliest armed wall-clock deadline, for the one-shot deadline timer.
pub fn next_deadline() -> Option<u64> {
    match NEXT_DEADLINE.load(Ordering::Relaxed) {
        u64::MAX => None,
        deadline => Some(deadline),
    }
}

/// Timer signals sent since boot.
#[allow(dead_code)] // test hook
pub fn fired_total() -> u64 {
    FIRED.load(Ordering::Relaxed)
}

/// Armed CPU-time timers (the tick path's cheap pre-check).
pub fn cpu_timers_armed() -> usize {
    CPU_ARMED.load(Ordering::Relaxed)
}

/// Timers in use (the test suites' leak check).
#[allow(dead_code)] // test hook
pub fn in_use() -> usize {
    IN_USE.load(Ordering::Relaxed)
}

impl Table {
    fn find(&self, pml4: u64, kind: Kind, id: i32) -> Option<usize> {
        self.timers.iter().position(|timer| {
            timer.used
                && timer.pml4 == pml4
                && timer.kind == kind
                && (kind != Kind::Posix || timer.id == id)
        })
    }

    fn alloc(&mut self, timer: Timer) -> Result<usize, Error> {
        let index = self
            .timers
            .iter()
            .position(|timer| !timer.used)
            .ok_or(Error::NoSpace)?;
        self.timers[index] = Timer {
            used: true,
            ..timer
        };
        IN_USE.fetch_add(1, Ordering::Relaxed);
        Ok(index)
    }

    fn free(&mut self, index: usize) {
        self.disarm(index);
        if self.timers[index].used {
            self.timers[index] = Timer::EMPTY;
            IN_USE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn disarm(&mut self, index: usize) {
        let timer = &mut self.timers[index];
        if timer.armed && timer.kind.is_cpu() {
            self.cpu_armed -= 1;
        }
        timer.armed = false;
        timer.remaining_ns = 0;
        self.queue.cancel(index);
    }

    /// Arm timer `index` to expire `value_ns` of its clock from now (CPU
    /// timers) or at the monotonic instant `deadline_ns` (wall timers).
    fn arm(&mut self, index: usize, deadline_ns: u64, value_ns: u64, interval_ns: u64) {
        self.disarm(index);
        let timer = &mut self.timers[index];
        timer.interval_ns = interval_ns;
        timer.armed = true;
        timer.overrun = 0;
        if timer.kind.is_cpu() {
            timer.remaining_ns = value_ns.max(1);
            self.cpu_armed += 1;
        } else {
            timer.deadline_ns = deadline_ns;
            self.queue.arm(index, deadline_ns);
        }
    }
}

/// Clamp a repeat interval: zero stays zero (one-shot).
fn clamp_interval(interval_ns: u64) -> u64 {
    if interval_ns == 0 {
        0
    } else {
        interval_ns.max(MIN_INTERVAL_NS)
    }
}

/// Make the deadline timer notice a new earliest deadline.
fn changed() {
    super::schedule::itimer_changed();
}

/// `setitimer`/`alarm`: set process `pml4`'s interval timer `kind` to expire
/// in `value_ns` (zero disarms) and then every `interval_ns`, as thread
/// `owner`. Returns what the timer held before.
pub fn set_interval(
    pml4: u64,
    owner: usize,
    kind: Kind,
    now_ns: u64,
    value_ns: u64,
    interval_ns: u64,
) -> Result<Reading, Error> {
    let old = with_table(|table| -> Result<Reading, Error> {
        let existing = table.find(pml4, kind, 0);
        let old = existing.map_or(Reading::default(), |i| table.timers[i].reading(now_ns));
        if value_ns == 0 {
            if let Some(index) = existing {
                table.free(index);
            }
            return Ok(old);
        }
        let index = match existing {
            Some(index) => index,
            None => table.alloc(Timer {
                kind,
                pml4,
                owner,
                signo: match kind {
                    Kind::Real => signal::SIGALRM,
                    Kind::Virtual => signal::SIGVTALRM,
                    _ => signal::SIGPROF,
                },
                notify: Notify::Process,
                ..Timer::EMPTY
            })?,
        };
        table.timers[index].owner = owner;
        table.arm(
            index,
            now_ns.saturating_add(value_ns),
            value_ns,
            clamp_interval(interval_ns),
        );
        Ok(old)
    })?;
    changed();
    Ok(old)
}

/// `getitimer`: the remaining time and interval of `kind` (zeros when unset).
pub fn get_interval(pml4: u64, kind: Kind, now_ns: u64) -> Reading {
    with_table(|table| {
        table
            .find(pml4, kind, 0)
            .map_or(Reading::default(), |i| table.timers[i].reading(now_ns))
    })
}

/// Delete every timer of process `pml4` (the last thread left, or its
/// address space is freed).
pub fn forget(pml4: u64) {
    if IN_USE.load(Ordering::Relaxed) == 0 {
        return;
    }
    with_table(|table| {
        for index in 0..MAX_TIMERS {
            if table.timers[index].used && table.timers[index].pml4 == pml4 {
                table.free(index);
            }
        }
    });
}

/// `execve` moved a process from address space `old` to `new`: the interval
/// timers carry over, POSIX timers are deleted (POSIX.1: `exec` removes
/// them).
pub fn exec(old: u64, new: u64) {
    if IN_USE.load(Ordering::Relaxed) == 0 {
        return;
    }
    with_table(|table| {
        for index in 0..MAX_TIMERS {
            let timer = table.timers[index];
            if !timer.used || timer.pml4 != old {
                continue;
            }
            if timer.kind == Kind::Posix {
                table.free(index);
            } else {
                table.timers[index].pml4 = new;
            }
        }
    });
}

mod fire;
mod posix;
pub use fire::{charge_cpu, fire_due};
pub use posix::{clock_of, create, delete, gettime, overrun, settime};

/// Test hook: forget every timer.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    with_table(|table| {
        for index in 0..MAX_TIMERS {
            if table.timers[index].used {
                table.free(index);
            }
        }
    });
}

/// Test hook: the expiry heap's invariants and its agreement with the table.
#[cfg(lazyos_tests)]
pub fn check_for_test() -> Result<(), &'static str> {
    with_table(|table| {
        table.queue.check()?;
        let wall_armed = table
            .timers
            .iter()
            .filter(|t| t.used && t.armed && !t.kind.is_cpu())
            .count();
        if wall_armed != table.queue.len() {
            return Err("armed wall-clock timers and queue entries disagree");
        }
        let cpu_armed = table
            .timers
            .iter()
            .filter(|t| t.used && t.armed && t.kind.is_cpu())
            .count();
        if cpu_armed != table.cpu_armed {
            return Err("armed CPU timers and the counter disagree");
        }
        Ok(())
    })
}
