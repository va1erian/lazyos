//! The test suite's stand-ins for time and for a daemon task: with a server
//! installed, a waiting requester calls it instead of parking, and the
//! clock can be moved forward to reach a deadline. Outside the test build
//! the clock never moves and there is no server.

#[cfg(lazyos_tests)]
mod fake {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use fused::wire::Request;
    use spin::Mutex;

    use super::super::{Phase, Stats, SLICE_TICKS, SLOTS};

    static OFFSET: AtomicU64 = AtomicU64::new(0);
    static PARKLESS: AtomicBool = AtomicBool::new(false);
    static PARKS: AtomicU64 = AtomicU64::new(0);
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

    /// Make every park a counted tick of the fake clock instead of a sleep:
    /// a test that expects a daemon *not* to park observes [`parks`] stay
    /// put, and one that regressed ends at its deadline instead of hanging.
    pub fn set_parkless(on: bool) {
        PARKLESS.store(on, Ordering::Relaxed);
        PARKS.store(0, Ordering::Relaxed);
    }

    /// Parks counted since [`set_parkless`].
    pub fn parks() -> u64 {
        PARKS.load(Ordering::Relaxed)
    }

    /// Whether this park is replaced by a tick of the fake clock.
    pub fn parked() -> bool {
        if !PARKLESS.load(Ordering::Relaxed) {
            return false;
        }
        PARKS.fetch_add(1, Ordering::Relaxed);
        advance(SLICE_TICKS);
        true
    }

    /// Put a request in slot `index`'s hands-to-be, as if a requester had
    /// just queued it and the daemon had not yet asked.
    pub fn hold_queued(index: usize) {
        let mut state = SLOTS[index].state.lock();
        state.busy = true;
        state.phase = Phase::Queued;
        state.request = Request::EMPTY;
        state.payload_len = 0;
        state.reply_cap = 0;
    }

    /// Whether a request still waits in slot `index` for its daemon.
    pub fn queued(index: usize) -> bool {
        SLOTS[index].state.lock().phase == Phase::Queued
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
        set_parkless(false);
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

#[cfg(not(lazyos_tests))]
pub fn parked() -> bool {
    false
}
