//! The test suite's stand-ins for time and for a daemon task: with a server
//! installed, a waiting requester calls it instead of parking, and the
//! clock can be moved forward to reach a deadline. Outside the test build
//! the clock never moves and there is no server.

#[cfg(lazyos_tests)]
mod fake {
    use core::sync::atomic::{AtomicU64, Ordering};
    use spin::Mutex;

    use super::super::{Phase, Stats, SLOTS};

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

    /// Run the fake daemon for slot `index`; false when none is installed.
    pub fn serve(index: usize) -> bool {
        let server = *SERVER.lock();
        server.map(|server| server(index)).is_some()
    }

    /// Give every slot back without touching the mount tables (between
    /// tests, which install their own tables).
    pub fn recycle_all() {
        for slot in SLOTS.iter() {
            let mut state = slot.state.lock();
            state.registered = false;
            state.alive = false;
            state.mounted = false;
            state.busy = false;
            state.phase = Phase::Idle;
            state.timeouts = 0;
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
