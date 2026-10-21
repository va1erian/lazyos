//! [`YieldMutex`]: a spin lock whose contenders yield the CPU
//! (`task::relax`), that knows its name, who holds it and who waits for it.
//!
//! Uncontended it costs a spin lock plus one store (the holder). Contended, a
//! task records which lock it waits for and since when, so `LOCK:WAIT`
//! (`task::lockwait`) can say "task 28 has waited 3 s for `ext2`, held by task
//! 12" instead of summing parks it cannot tell apart. The record is made when
//! the first try fails and cleared when the lock is taken, so any number of
//! short waits never add up.
//!
//! # Fair locks (hand-off)
//!
//! A plain lock lets the releaser take it again at once: a task doing
//! back-to-back long critical sections (a SQLite thread's fsyncs on the ext2
//! gate) wins every race against waiters that are parked for [`PARK_NS`]
//! between looks, so a waiter sat out several holds (`LOCK:WAIT ms=740`
//! against holds of 200 ms at most, issue #940). A lock made with
//! [`YieldMutex::named_fair`] hands off instead: a release that leaves
//! waiters sets `handoff`, and a task arriving while it is set is a newcomer
//! that must queue behind the waiters (it parks once before it tries). The
//! waiters, parked earlier, look first. Nothing waits on the flag: when no
//! waiter is left to clear it, the next newcomer takes the lock after its one
//! park and clears it, so a waiter that was ended costs one park, never a
//! hang. The waiter count is kept exact by [`forget_wait`].

use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use super::relax::Yield;
use super::MAX_TASKS;

/// What a waiter and the reporter need to know of a lock.
pub struct LockInfo {
    name: &'static str,
    /// Slot of the holder plus one; 0 when free.
    holder: AtomicUsize,
    /// Tasks parked in `lock` for it (only counted for a fair lock).
    waiting: AtomicUsize,
    /// A fair lock was released with waiters: newcomers queue behind them.
    handoff: AtomicBool,
    fair: bool,
}

impl LockInfo {
    /// The task holding the lock right now, if any.
    pub fn holder(&self) -> Option<usize> {
        self.holder.load(Ordering::Relaxed).checked_sub(1)
    }

    /// Test builds: tasks counted as waiting.
    #[cfg(lazyos_tests)]
    pub fn waiting_for_test(&self) -> usize {
        self.waiting.load(Ordering::Relaxed)
    }

    /// A waiter is gone (it took the lock or was ended).
    fn left_queue(&self) {
        if self.fair {
            self.waiting.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Forget that task `slot` waits for a lock: the task is ended (or the slot
/// is born anew) and will never leave `lock` by itself.
pub fn forget_wait(slot: usize) {
    if let Some(record) = WAITING_FOR.get(slot) {
        let lock = record.swap(core::ptr::null_mut(), Ordering::AcqRel);
        if !lock.is_null() {
            // SAFETY: a record is set only while its task is parked in
            // `YieldMutex::lock` borrowing the lock (see `waiting`), so the
            // lock is alive; only an atomic is touched.
            unsafe { &*lock }.left_queue();
        }
    }
}

/// Per slot, the lock it is waiting for (null: none) and since when
/// (monotonic ns).
static WAITING_FOR: [AtomicPtr<LockInfo>; MAX_TASKS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_TASKS];
static WAIT_SINCE_NS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// A spin lock whose contenders yield the CPU (see `task::relax`).
pub struct YieldMutex<T> {
    inner: spin::mutex::Mutex<T, Yield>,
    info: LockInfo,
}

/// The guard of a [`YieldMutex`]; dropping it frees the lock and clears the
/// holder.
pub struct YieldGuard<'a, T> {
    // `Drop::drop` clears the holder first; then the fields drop in
    // declaration order, so the spin guard (the lock itself) goes next.
    inner: spin::mutex::MutexGuard<'a, T>,
    info: &'a LockInfo,
}

impl<T> YieldMutex<T> {
    /// An unnamed lock.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub const fn new(value: T) -> Self {
        Self::named("lock", value)
    }

    /// A lock that `LOCK:WAIT` can name.
    pub const fn named(name: &'static str, value: T) -> Self {
        Self::build(name, value, false)
    }

    /// A named lock that hands off to its waiters (see the module header).
    pub const fn named_fair(name: &'static str, value: T) -> Self {
        Self::build(name, value, true)
    }

    const fn build(name: &'static str, value: T, fair: bool) -> Self {
        YieldMutex {
            inner: spin::mutex::Mutex::new(value),
            info: LockInfo {
                name,
                holder: AtomicUsize::new(0),
                waiting: AtomicUsize::new(0),
                handoff: AtomicBool::new(false),
                fair,
            },
        }
    }

    /// Take the lock, parking while another task holds it.
    pub fn lock(&self) -> YieldGuard<'_, T> {
        let queue_behind = self.info.fair && self.info.handoff.load(Ordering::Acquire);
        if !queue_behind {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
        }
        let me = super::current();
        let slot = WAITING_FOR.get(me);
        if let Some(slot) = slot {
            WAIT_SINCE_NS[me].store(crate::arch::clock::monotonic_ns(), Ordering::Relaxed);
            if self.info.fair {
                self.info.waiting.fetch_add(1, Ordering::AcqRel);
            }
            slot.store(
                &self.info as *const LockInfo as *mut LockInfo,
                Ordering::Release,
            );
        }
        if queue_behind {
            // The lock is free but owed to the tasks already parked: let
            // them (parked earlier, so they wake earlier) look first.
            <super::relax::Yield as spin::RelaxStrategy>::relax();
        }
        let guard = self.inner.lock();
        if let Some(slot) = slot {
            if !slot.swap(core::ptr::null_mut(), Ordering::AcqRel).is_null() {
                self.info.left_queue();
            }
        }
        if self.info.fair {
            // Taken by a waiter (or the newcomer that outwaited them): the
            // hand-off is done; the next release sets it again if needed.
            self.info.handoff.store(false, Ordering::Release);
        }
        self.held(guard)
    }

    /// Take the lock if it is free.
    pub fn try_lock(&self) -> Option<YieldGuard<'_, T>> {
        self.inner.try_lock().map(|guard| self.held(guard))
    }

    /// Whether the lock is held right now.
    #[allow(dead_code)]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    fn held<'a>(&'a self, inner: spin::mutex::MutexGuard<'a, T>) -> YieldGuard<'a, T> {
        self.info
            .holder
            .store(super::current() + 1, Ordering::Relaxed);
        YieldGuard {
            inner,
            info: &self.info,
        }
    }

    /// Release a lock whose guard was lost (a thread that was ended inside
    /// the critical section).
    ///
    /// # Safety
    ///
    /// Nobody may own the lock or use a guard of it afterwards, as for
    /// `spin::Mutex::force_unlock`.
    #[cfg(lazyos_tests)]
    pub unsafe fn force_unlock(&self) {
        self.info.holder.store(0, Ordering::Relaxed);
        // SAFETY: the caller's contract.
        unsafe { self.inner.force_unlock() };
    }

    /// Test builds: the lock's record.
    #[cfg(lazyos_tests)]
    pub fn info_for_test(&self) -> &LockInfo {
        &self.info
    }
}

impl<T> Drop for YieldGuard<'_, T> {
    fn drop(&mut self) {
        self.info.holder.store(0, Ordering::Relaxed);
        if self.info.fair && self.info.waiting.load(Ordering::Acquire) > 0 {
            self.info.handoff.store(true, Ordering::Release);
        }
    }
}

impl<T> Deref for YieldGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for YieldGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

/// The lock task `slot` is waiting for at `now_ns` (monotonic ns) and how long
/// it has waited, or `None` if it waits for none.
///
/// The record points into a lock the waiter borrows while it is parked in
/// `lock`; `lock` clears it when it returns, and `process::finish_locked`
/// ([`forget_wait`]) clears it when the task is ended wherever it stands (the
/// current thread of a fault, the OOM killer or `exit_group`; a thread parked
/// inside the kernel is not ended there, `signal::end_process` wakes it to
/// unwind, so one parked in `lock` leaves it first). Callers ask only about a slot whose task exists and is not
/// `Done`, so while the record is set the waiter is parked inside `lock`,
/// still borrowing the lock; only its atomics and name are read.
pub fn waiting(slot: usize, now_ns: u64) -> Option<(&'static str, Option<usize>, u64)> {
    let lock = WAITING_FOR.get(slot)?.load(Ordering::Acquire);
    if lock.is_null() {
        return None;
    }
    // SAFETY: see above: the record is set only while the task is parked in
    // `YieldMutex::lock` borrowing the lock, and is cleared on return and on
    // the task's end; the caller holds the task table with interrupts off on
    // one CPU and has checked the task is live, so the lock is still alive.
    // Only atomics and a `&'static str` are read.
    let info = unsafe { &*lock };
    let since = WAIT_SINCE_NS[slot].load(Ordering::Relaxed);
    Some((info.name, info.holder(), now_ns.saturating_sub(since)))
}

/// Test builds: make `slot` look like a waiter for `lock` since `since_ns`;
/// `None` clears it.
#[cfg(lazyos_tests)]
pub fn simulate_wait_for_test<T>(slot: usize, lock: Option<(&YieldMutex<T>, u64)>) {
    match lock {
        Some((lock, since_ns)) => {
            WAIT_SINCE_NS[slot].store(since_ns, Ordering::Relaxed);
            WAITING_FOR[slot].store(
                &lock.info as *const LockInfo as *mut LockInfo,
                Ordering::Release,
            );
        }
        None => WAITING_FOR[slot].store(core::ptr::null_mut(), Ordering::Release),
    }
}

/// Test builds: pretend task `slot` holds `lock` (`None`: nobody).
#[cfg(lazyos_tests)]
pub fn simulate_holder_for_test<T>(lock: &YieldMutex<T>, slot: Option<usize>) {
    lock.info
        .holder
        .store(slot.map_or(0, |slot| slot + 1), Ordering::Relaxed);
}
