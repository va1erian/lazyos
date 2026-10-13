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
//! relaxed atomic load (and does not read the clock).
//!
//! CPU-time timers (`ITIMER_VIRTUAL`, `ITIMER_PROF`) are driven by the
//! scheduler's tick accounting, so they are approximations: each timer tick
//! is booked, whole, to the task it interrupted, and `ITIMER_VIRTUAL` counts
//! the ticks that landed in ring 3 while `ITIMER_PROF` counts every tick
//! charged to the process (user and system). Syscalls run with interrupts
//! off, so a tick almost always lands in ring 3 or in a wait: system time is
//! folded into the next tick (the catch-up periods of issue #344) and the
//! two timers advance nearly alike (`ITIMER_PROF` ~ `ITIMER_VIRTUAL`). A
//! process's threads add up, like Linux's.
//!
//! **Firing context.** Expiry runs at a scheduler entry or the deadline
//! interrupt *before* the task table is locked (`fire_due`): it pops the due
//! timers under [`TABLE`], drops that lock, then queues each signal through
//! the signal layer like any other sender, which interrupts a blocking
//! syscall (`EINTR`, or `SA_RESTART`) exactly as `kill` does. Nothing in the
//! path allocates.
//!
//! **Approximations.** A relative `CLOCK_REALTIME` timer and an absolute one
//! are converted to a monotonic deadline when armed, so a later
//! `clock_settime` step does not move them (Linux re-evaluates absolute
//! realtime timers). `overrun` counts the expirations that did not get a
//! signal of their own (one was still pending, or periods passed unserved)
//! since the last signal was sent; the `si_overrun` of a signal already
//! queued is not updated. Repeat intervals under [`MIN_INTERVAL_NS`]
//! (privileged) or [`MIN_INTERVAL_UNPRIVILEGED_NS`] are raised to it. `SIGEV_THREAD` is
//! user space's (musl builds it on `SIGEV_THREAD_ID` and `rt_sigtimedwait`,
//! which this kernel does not have).
//!
//! **Flooding.** An expiry whose signal is already pending for its target, or
//! ignored, only re-arms and counts an overrun: no table walk, no wakeup. Each
//! user is capped at [`MAX_POSIX_PER_UID`] POSIX timers (unless privileged)
//! and the interval floor is 1 ms unprivileged, so an unprivileged program
//! can cost the interrupt path a bounded amount. The interval timers have a
//! pool of their own ([`INTERVAL_POOL`]) and cannot be starved by
//! `timer_create`.
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

/// Slots for the three interval timers of every process that can exist (a
/// process holds at least one task): `alarm` and `setitimer` never fail for
/// want of room, whatever POSIX timers do to the other pool.
pub const INTERVAL_POOL: usize = 3 * MAX_TASKS;
/// Slots for `timer_create` timers, system-wide.
pub const POSIX_POOL: usize = 1024;
/// Every timer slot; also the capacity of the expiry heap.
pub const MAX_TIMERS: usize = INTERVAL_POOL + POSIX_POOL;
/// POSIX timers one process may own (`RLIMIT_SIGPENDING` stands in on Linux).
pub const MAX_POSIX_PER_PROCESS: usize = 64;
/// POSIX timers one user may own across all its processes, unless it holds
/// `CAP_SYS_ADMIN`: a user cannot take the whole pool by forking.
pub const MAX_POSIX_PER_UID: usize = 128;
/// Shortest repeat interval of a privileged caller (nanoseconds). A shorter
/// one would turn into an interrupt storm; Linux rounds up to its clock
/// resolution too.
pub const MIN_INTERVAL_NS: u64 = 100_000;
/// Shortest repeat interval of everyone else: with the per-user cap it bounds
/// what an unprivileged program can make the interrupt path do.
pub const MIN_INTERVAL_UNPRIVILEGED_NS: u64 = 1_000_000;

/// Who arms a timer: the identity its quota and its interval floor follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    /// Holds `CAP_SYS_ADMIN`: no per-user cap, the short interval floor.
    pub privileged: bool,
}

impl Caller {
    /// The kernel itself (the test suites).
    #[allow(dead_code)] // test hook
    pub const KERNEL: Caller = Caller {
        uid: 0,
        privileged: true,
    };

    fn min_interval_ns(self) -> u64 {
        if self.privileged {
            MIN_INTERVAL_NS
        } else {
            MIN_INTERVAL_UNPRIVILEGED_NS
        }
    }
}

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
    /// `SIGEV_THREAD_ID`: the signal goes to this thread alone. The slot is
    /// the thread id; the generation (`signal::thread_generation`) tells it
    /// from a later task that was given the same slot.
    Thread { slot: usize, generation: u64 },
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
    /// Expirations since the last signal was sent that got none of their
    /// own because it was still pending or ignored.
    overrun: u32,
    /// The overrun count the last signal sent carried (periods that passed
    /// unserved before it): `timer_getoverrun` is this plus `overrun`.
    overrun_last: u32,
    /// The owner's user id, for the per-user quota.
    uid: u32,
    /// The repeat-interval floor (`Caller::min_interval_ns`).
    min_interval_ns: u64,
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
        overrun_last: 0,
        uid: 0,
        min_interval_ns: MIN_INTERVAL_NS,
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
    queue: TimerQueue<MAX_TIMERS>,
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
    /// The slots `kind` lives in: interval timers and POSIX timers have a
    /// pool each.
    fn pool(kind: Kind) -> core::ops::Range<usize> {
        if kind == Kind::Posix {
            INTERVAL_POOL..MAX_TIMERS
        } else {
            0..INTERVAL_POOL
        }
    }

    fn find(&self, pml4: u64, kind: Kind, id: i32) -> Option<usize> {
        let pool = Self::pool(kind);
        let start = pool.start;
        self.timers[pool]
            .iter()
            .position(|timer| {
                timer.used
                    && timer.pml4 == pml4
                    && timer.kind == kind
                    && (kind != Kind::Posix || timer.id == id)
            })
            .map(|at| at + start)
    }

    fn alloc(&mut self, timer: Timer) -> Result<usize, Error> {
        let pool = Self::pool(timer.kind);
        let start = pool.start;
        let index = self.timers[pool]
            .iter()
            .position(|timer| !timer.used)
            .ok_or(Error::NoSpace)?
            + start;
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
        timer.overrun_last = 0;
        if timer.kind.is_cpu() {
            timer.remaining_ns = value_ns.max(1);
            self.cpu_armed += 1;
        } else {
            timer.deadline_ns = deadline_ns;
            self.queue.arm(index, deadline_ns);
        }
    }
}

/// Clamp a repeat interval to the floor `min_ns`: zero stays zero (one-shot).
fn clamp_interval(interval_ns: u64, min_ns: u64) -> u64 {
    if interval_ns == 0 {
        0
    } else {
        interval_ns.max(min_ns)
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
    caller: Caller,
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
        let timer = &mut table.timers[index];
        timer.owner = owner;
        timer.uid = caller.uid;
        timer.min_interval_ns = caller.min_interval_ns();
        let interval_ns = clamp_interval(interval_ns, timer.min_interval_ns);
        table.arm(
            index,
            now_ns.saturating_add(value_ns),
            value_ns,
            interval_ns,
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
    // The new address space has no signal state yet, and a timer's signal is
    // queued from an interrupt that must not allocate it.
    signal::ensure_process(new);
}

mod fire;
mod posix;
#[cfg(lazyos_tests)]
pub use fire::fire_due;
pub use fire::{charge_cpu, fire_due_now};
pub use posix::{clock_of, create, delete, gettime, overrun, settime};

#[cfg(lazyos_tests)]
mod hooks;
#[cfg(lazyos_tests)]
pub use hooks::{check_for_test, reset_for_test};
