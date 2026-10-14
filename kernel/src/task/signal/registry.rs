//! Where signal state lives, and the one rule about who owns what (issue #772).
//!
//! POSIX splits a process's signal state, and so does this module:
//!
//! * [`Signals`] is **per process**, keyed by the address space (`pml4`; tasks
//!   that share one are threads of a process): the dispositions
//!   (`rt_sigaction`), the *shared* pending set that process-directed signals
//!   (`kill`, `SIGCHLD`, the terminal's `^C`) wait in until some thread that
//!   does not block them takes one, and the group-exit status.
//! * [`ThreadSig`] is **per thread**, keyed by the task slot: the blocked mask
//!   (`rt_sigprocmask`, `ppoll`'s temporary mask, `sigsuspend`), the thread's
//!   own pending set that thread-directed signals (`tkill`/`tgkill`, `SIGPIPE`,
//!   synchronous faults) wait in and are delivered from on that thread alone,
//!   and the alternate stack (`sigaltstack`).
//!
//! The thread table is a fixed array in `.bss` (the scheduler reads it, and
//! signal paths must not allocate there). Both tables sit behind separate
//! locks, always taken process table first, thread table second, and both after
//! the task table. A slot's [`ThreadSig`] is reset when its task is created
//! ([`thread_born`], [`fork_inherit`]), so a recycled slot never inherits a
//! dead thread's mask or pending signals.

use super::*;

/// Process signal state, keyed by address space. A process's PML4 is created at
/// `spawn`/`fork`, freed at reap/exec, and never changes while live, so the key
/// is stable exactly as long as the process is.
pub(super) struct Signals {
    pub(super) pml4: u64,
    /// Process-directed signals waiting for a thread that does not block them.
    pub(super) pending: u64,
    pub(super) actions: [Disposition; NSIG],
    /// The `siginfo` of each signal in [`Signals::pending`].
    pub(super) infos: [SigInfo; NSIG],
    /// The exit status of a group exit in progress (a `SIGKILL`, or a fatal
    /// default that one thread already acted on): Linux's
    /// `signal->group_exit_code`. Threads parked inside the kernel when it
    /// began finish with this status once they leave the kernel, whatever
    /// signal they meet on the way out.
    pub(super) group_exit: Option<u64>,
    /// When the kill began (`arch::clock::monotonic_ns`; 0: none), so
    /// [`killwatch`](super::killwatch) can name a process that has not
    /// finished a second later, and whether it already did.
    pub(super) kill_at_ns: u64,
    pub(super) slow_reported: bool,
}

impl Signals {
    fn new(pml4: u64) -> Self {
        Signals {
            pml4,
            pending: 0,
            actions: [Disposition::Default; NSIG],
            infos: [SigInfo::user(0, SI_USER); NSIG],
            group_exit: None,
            kill_at_ns: 0,
            slow_reported: false,
        }
    }
}

/// One thread's signal state. Every field is zero when the slot is unused.
pub(super) struct ThreadSig {
    pub(super) blocked: u64,
    /// Thread-directed signals: delivered on this thread only, whatever the
    /// other threads of the process block.
    pub(super) pending: u64,
    /// The `siginfo` of each signal in [`ThreadSig::pending`].
    pub(super) infos: [SigInfo; NSIG],
    pub(super) altstack: AltStack,
    /// True while a handler delivered with `SA_ONSTACK` runs, so a nested
    /// signal reuses the current stack instead of re-entering the alt stack.
    pub(super) on_altstack: bool,
    /// The mask `rt_sigsuspend` (or `ppoll`'s temporary mask) replaced:
    /// restored when the handler it woke for returns (it becomes that frame's
    /// saved mask), or at once when no handler ran.
    pub(super) suspend_restore: Option<u64>,
    /// Which task has lived in this slot: a fresh value for every task, so
    /// something that remembers a thread by slot (a `SIGEV_THREAD_ID` timer)
    /// can tell it from a successor.
    pub(super) generation: u64,
}

impl ThreadSig {
    const NEW: ThreadSig = ThreadSig {
        blocked: 0,
        pending: 0,
        infos: [SigInfo::user(0, SI_USER); NSIG],
        altstack: AltStack::DISABLED,
        on_altstack: false,
        suspend_restore: None,
        generation: 0,
    };
}

/// The last generation handed to a task.
static GENERATION: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1
}

/// The generation of the task now in `slot` (0 for a slot never used).
pub fn thread_generation(slot: usize) -> u64 {
    THREADS
        .lock()
        .get(slot)
        .map_or(0, |thread| thread.generation)
}

/// The process table. Lock order: task table, then this, then [`THREADS`].
static PROCS: Mutex<Vec<Signals>> = Mutex::new(Vec::new());

/// The thread table, indexed by task slot.
static THREADS: Mutex<[ThreadSig; MAX_TASKS]> = Mutex::new([ThreadSig::NEW; MAX_TASKS]);

/// Run `f` with the signal state of process `pml4`, creating it on first use.
pub(super) fn with_signals<R>(pml4: u64, f: impl FnOnce(&mut Signals) -> R) -> R {
    let mut all = PROCS.lock();
    let index = proc_index(&mut all, pml4);
    f(&mut all[index])
}

/// Run `f` with the signal state of thread `slot`.
pub(super) fn with_thread<R>(slot: usize, f: impl FnOnce(&mut ThreadSig) -> R) -> R {
    f(&mut THREADS.lock()[slot])
}

/// Run `f` with the state of process `pml4` and of its thread `slot` together,
/// the view delivery needs: one atomic look at the shared and the private set.
pub(super) fn with_both<R>(
    pml4: u64,
    slot: usize,
    f: impl FnOnce(&mut Signals, &mut ThreadSig) -> R,
) -> R {
    let mut all = PROCS.lock();
    let index = proc_index(&mut all, pml4);
    let mut threads = THREADS.lock();
    f(&mut all[index], &mut threads[slot])
}

/// Run `f` over both tables without creating anything: the process list and
/// the thread array. For the scheduler's scans.
pub(super) fn peek<R>(f: impl FnOnce(&[Signals], &[ThreadSig; MAX_TASKS]) -> R) -> R {
    let procs = PROCS.lock();
    let threads = THREADS.lock();
    f(&procs, &threads)
}

fn proc_index(all: &mut Vec<Signals>, pml4: u64) -> usize {
    match all.iter().position(|state| state.pml4 == pml4) {
        Some(index) => index,
        None => {
            all.push(Signals::new(pml4));
            all.len() - 1
        }
    }
}

/// Make sure process `pml4` has its signal state. A timer fires from the
/// scheduler's interrupt, where nothing may allocate, and the registry grows
/// its process list on first use: the syscall that arms a timer calls this
/// first so the interrupt only ever finds an entry.
pub fn ensure_process(pml4: u64) {
    with_signals(pml4, |_| {});
}

/// Whether a signal table lock is held right now (the NMI hang report, issue #382).
pub fn registry_locked() -> bool {
    PROCS.is_locked() || THREADS.is_locked()
}

/// Drop a torn-down address space's signal state.
pub fn forget(pml4: u64) {
    PROCS.lock().retain(|state| state.pml4 != pml4);
}

/// A task came to life in `slot`. A thread (or a program started by one)
/// begins with `creator`'s blocked mask, as Linux's `clone` copies it; any
/// other new task starts with nothing blocked. Nothing is pending and no
/// alternate stack is set up: a thread does not inherit its creator's.
pub fn thread_born(slot: usize, creator: Option<usize>) {
    let mut threads = THREADS.lock();
    let blocked = creator.map_or(0, |creator| threads[creator].blocked);
    threads[slot] = ThreadSig {
        blocked,
        generation: next_generation(),
        ..ThreadSig::NEW
    };
}

/// Inherit signal state across `fork`: the process gets the dispositions, and
/// its one thread the calling thread's blocked mask and alternate stack (POSIX
/// `fork` copies the calling thread's). A fresh process starts with nothing
/// pending. Ignored signals stay ignored, exactly like Linux; installed
/// handlers are inherited too.
pub fn fork_inherit(parent_slot: usize, parent: u64, child_slot: usize, child: u64) {
    let mut all = PROCS.lock();
    let mut inherited = Signals::new(child);
    if let Some(state) = all.iter().find(|state| state.pml4 == parent) {
        inherited.actions = state.actions;
    }
    all.retain(|state| state.pml4 != child);
    all.push(inherited);
    let mut threads = THREADS.lock();
    let (blocked, altstack) = {
        let calling = &threads[parent_slot];
        (calling.blocked, calling.altstack)
    };
    threads[child_slot] = ThreadSig {
        blocked,
        altstack,
        generation: next_generation(),
        ..ThreadSig::NEW
    };
}

/// `execve` replaced the address space of the thread in `slot`: the blocked
/// mask and the thread's pending signals survive (Linux keeps both), the
/// alternate stack and any suspend in progress do not.
pub fn exec_thread(slot: usize) {
    let mut threads = THREADS.lock();
    let thread = &mut threads[slot];
    thread.altstack = AltStack::DISABLED;
    thread.on_altstack = false;
    thread.suspend_restore = None;
}

/// Test-harness hooks (issue #62): reset the registry between tests.
#[cfg(lazyos_tests)]
pub mod harness {
    use super::*;

    /// Clear all process and thread signal state.
    pub fn reset() {
        PROCS.lock().clear();
        let mut threads = THREADS.lock();
        for thread in threads.iter_mut() {
            *thread = ThreadSig::NEW;
        }
    }

    /// Number of process entries (leak check for tests).
    pub fn registry_len() -> usize {
        PROCS.lock().len()
    }

    /// Number of slots holding any thread signal state (leak check for tests).
    pub fn threads_in_use() -> usize {
        THREADS
            .lock()
            .iter()
            .filter(|thread| {
                thread.blocked != 0
                    || thread.pending != 0
                    || thread.altstack.enabled
                    || thread.suspend_restore.is_some()
            })
            .count()
    }
    /// The `siginfo` the kernel last recorded for `sig` on the current
    /// thread: what a `SA_SIGINFO` handler is shown (a kernel-thread test
    /// task has no user frame to read it from). A synchronous fault is
    /// queued on the faulting thread, so its record is the thread's.
    pub fn recorded_info(sig: u8) -> Option<SigInfo> {
        let (slot, _) = super::super::current_info()?;
        Some(with_thread(slot, |thread| thread.infos[sig as usize]))
    }

    /// Drop every pending signal of `slot` and of its process (the mask is
    /// kept): a test that counts a timer's signals one at a time.
    pub fn clear_pending(slot: usize) {
        if let Some((pml4, _)) = super::super::slot_info(slot) {
            with_signals(pml4, |state| state.pending = 0);
        }
        with_thread(slot, |thread| thread.pending = 0);
    }

    /// The `siginfo` recorded for a process-directed `sig` (`kill`, an
    /// interval timer) of `slot`'s process.
    pub fn recorded_process_info(slot: usize, sig: u8) -> Option<SigInfo> {
        let (pml4, _) = super::super::slot_info(slot)?;
        Some(with_signals(pml4, |state| state.infos[sig as usize]))
    }
}
