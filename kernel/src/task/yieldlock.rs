//! [`YieldMutex`]: a spin lock whose contenders yield the CPU
//! (`task::relax`), that knows its name, who holds it and who waits for it.
//!
//! Uncontended it costs a spin lock plus one store (the holder). Contended, a
//! task records which lock it waits for and since when, so `LOCK:WAIT`
//! (`task::lockwait`) can say "task 28 has waited 3 s for `ext2`, held by task
//! 12" instead of summing parks it cannot tell apart. The record is made when
//! the first try fails and cleared when the lock is taken, so any number of
//! short waits never add up.

use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use super::relax::Yield;
use super::MAX_TASKS;

/// What a waiter and the reporter need to know of a lock.
pub struct LockInfo {
    name: &'static str,
    /// Slot of the holder plus one; 0 when free.
    holder: AtomicUsize,
}

impl LockInfo {
    /// The task holding the lock right now, if any.
    pub fn holder(&self) -> Option<usize> {
        self.holder.load(Ordering::Relaxed).checked_sub(1)
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
    // Declared first: dropped last, after `Drop` has cleared the holder.
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
        YieldMutex {
            inner: spin::mutex::Mutex::new(value),
            info: LockInfo {
                name,
                holder: AtomicUsize::new(0),
            },
        }
    }

    /// Take the lock, parking while another task holds it.
    pub fn lock(&self) -> YieldGuard<'_, T> {
        if let Some(guard) = self.try_lock() {
            return guard;
        }
        let me = super::current();
        let slot = WAITING_FOR.get(me);
        if let Some(slot) = slot {
            WAIT_SINCE_NS[me].store(crate::arch::clock::monotonic_ns(), Ordering::Relaxed);
            slot.store(
                &self.info as *const LockInfo as *mut LockInfo,
                Ordering::Release,
            );
        }
        let guard = self.inner.lock();
        if let Some(slot) = slot {
            slot.store(core::ptr::null_mut(), Ordering::Release);
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
/// The record points into a lock the waiter borrows for as long as it waits
/// (it clears the record before `lock` returns), so while the record is set
/// the lock is alive; only its atomics are read.
pub fn waiting(slot: usize, now_ns: u64) -> Option<(&'static str, Option<usize>, u64)> {
    let lock = WAITING_FOR.get(slot)?.load(Ordering::Acquire);
    if lock.is_null() {
        return None;
    }
    // SAFETY: see above: set by `YieldMutex::lock` while it borrows the lock
    // and cleared before it returns; the caller holds the task table with
    // interrupts off on one CPU, so the waiter cannot have moved on in
    // between. Only atomics and a `&'static str` are read.
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
