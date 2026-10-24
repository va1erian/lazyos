//! The suite's nap hook (test builds only): a stand-in for a peer that acts
//! while the suite's only task waits (`task::harness` re-exports it).

/// What runs each time the CPU wakes from a halt while the suite's task
/// waits: the idle context's loop runs it (`task::idle`), and so does a
/// parked task's wait loop on a CPU without one. The suite has no second
/// task to play a peer, so this is how one acts *while* a blocking call
/// waits: bytes that arrive later, or the `Interrupted` wake a signal
/// brings. It runs as whatever is current then (usually the idle context),
/// so it names the waiter by [`nap_hook_owner`], never `current()`.
static NAP_HOOK: spin::Mutex<Option<fn()>> = spin::Mutex::new(None);
/// The task that installed the [`NAP_HOOK`]: the waiter it acts for.
static NAP_HOOK_OWNER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Install (or with `None` remove) the [`NAP_HOOK`], for the current task.
pub fn set_nap_hook(hook: Option<fn()>) {
    NAP_HOOK_OWNER.store(
        crate::task::current(),
        core::sync::atomic::Ordering::Relaxed,
    );
    *NAP_HOOK.lock() = hook;
}

/// The task the [`NAP_HOOK`] acts for (the one that installed it).
pub fn nap_hook_owner() -> usize {
    NAP_HOOK_OWNER.load(core::sync::atomic::Ordering::Relaxed)
}

/// Run the [`NAP_HOOK`], if any (reporting whether there was one), with its lock released (the hook may wake
/// the waiter, which takes queue and task-table locks).
pub(crate) fn run_nap_hook() -> bool {
    let hook = *NAP_HOOK.lock();
    if let Some(hook) = hook {
        hook();
    }
    hook.is_some()
}
