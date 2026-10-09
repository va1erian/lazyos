//! The test suite's stand-ins for time and for a provider task: with a
//! server installed, a waiting requester calls it instead of parking, and the
//! clock can be moved forward to reach a deadline. Outside the test build the
//! clock never moves and there is no server.

#[cfg(lazyos_tests)]
mod fake {
    use core::sync::atomic::{AtomicU64, Ordering};
    use spin::Mutex;

    use super::super::{Phase, Stats, DISKS};

    static OFFSET: AtomicU64 = AtomicU64::new(0);
    static SERVER: Mutex<Option<fn(usize)>> = Mutex::new(None);

    pub fn offset() -> u64 {
        OFFSET.load(Ordering::Relaxed)
    }

    pub fn advance(ticks: u64) {
        OFFSET.fetch_add(ticks, Ordering::Relaxed);
    }

    pub fn set_server(server: Option<fn(usize)>) {
        *SERVER.lock() = server;
    }

    pub fn serving() -> bool {
        SERVER.lock().is_some()
    }

    /// Run the fake provider for disk `index`; false when none is installed.
    pub fn serve(index: usize) -> bool {
        let server = *SERVER.lock();
        server.map(|server| server(index)).is_some()
    }

    /// Put a request of disk `index` in its provider's hands, as if taken
    /// now and never answered (the provider busy on that disk).
    pub fn hold_taken(index: usize) {
        let mut state = DISKS[index].state.lock();
        state.busy = true;
        state.phase = Phase::Taken;
        state.taken_at = super::super::now();
    }

    /// Give every slot back (between tests). The registry keeps its
    /// entries, so a recycled slot comes back under the same name.
    pub fn recycle_all() {
        for disk in DISKS.iter() {
            let mut state = disk.state.lock();
            state.registered = false;
            state.alive = false;
            state.busy = false;
            state.phase = Phase::Idle;
            state.timeouts = 0;
            state.scanned = true;
            state.stats = Stats::default();
        }
        OFFSET.store(0, Ordering::Relaxed);
        set_server(None);
    }
}

#[cfg(lazyos_tests)]
pub use fake::*;

#[cfg(not(lazyos_tests))]
pub fn offset() -> u64 {
    0
}

#[cfg(not(lazyos_tests))]
pub fn serving() -> bool {
    false
}

#[cfg(not(lazyos_tests))]
pub fn serve(_index: usize) -> bool {
    false
}
