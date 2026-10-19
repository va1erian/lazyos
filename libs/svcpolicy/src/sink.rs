//! How the interim topic broker holds on to a subscriber's endpoint
//! (`user::messenger::router`, audit H12), as a state machine the host can
//! test with a counting transport.
//!
//! The broker resolves a subscriber's sink name to an endpoint **handle** in
//! its own table, caches it, and pushes events into it. A handle is a slot of
//! a 256-entry table the broker (in `init`, PID 1) shares with everything else
//! it does, and `Endpoint` is `Copy` with no `Drop`, so every handle the
//! broker stops using must be released by hand. The rules:
//!
//! * a send that fails because the sink's queue is full (`Backpressure`) means
//!   the sink is alive and not draining: the event is dropped and the handle
//!   kept. Resolving again would open a second handle to the same stalled
//!   endpoint, and a stalled subscriber would cost one per event (about 200
//!   events and `init` could not resolve a name or launch an app);
//! * any other failure (`Gone`: the sink died, or its name now belongs to
//!   another endpoint) releases the cached handle and resolves once more;
//!   a handle that cannot be sent to is released too, never forgotten.

use alloc::string::String;

/// Why a send into a sink's endpoint failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendFailure {
    /// The queue is full or the sink is over a quota: alive, just not
    /// draining (`EAGAIN`).
    Backpressure,
    /// The endpoint is closed or the handle is no good any more.
    Gone,
}

/// The kernel operations a sink needs; `user::messenger::router` implements
/// them over real handles, the tests over a counter.
pub trait Transport {
    /// A handle to an endpoint; `Copy` and without a destructor, like the
    /// real one, so a handle that is not passed to `release` is leaked.
    type Handle: Copy;

    /// Open a handle to the endpoint registered under `name`.
    fn resolve(&mut self, name: &str) -> Option<Self::Handle>;
    /// Push the event being delivered into `handle`'s endpoint.
    fn send(&mut self, handle: Self::Handle) -> Result<(), SendFailure>;
    /// Give `handle` back (a release, not a close of the sink for others).
    fn release(&mut self, handle: Self::Handle);
    /// Whether `name` is still registered (a lookup that leaves nothing
    /// open): the check that finds a subscriber which died while nothing was
    /// published to it.
    fn alive(&mut self, name: &str) -> bool;
}

/// What became of one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The event went into the sink.
    Delivered,
    /// The event was dropped: the sink is stalled or gone.
    Dropped,
}

/// One subscriber's sink: its registered name and the handle cached for it.
pub struct Sink<H> {
    name: String,
    handle: Option<H>,
}

impl<H: Copy> Sink<H> {
    /// A sink for `name` holding `handle` (opened when it subscribed).
    pub fn new(name: String, handle: H) -> Sink<H> {
        Sink {
            name,
            handle: Some(handle),
        }
    }

    /// The registered name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether a handle is cached.
    pub fn is_resolved(&self) -> bool {
        self.handle.is_some()
    }

    /// Push one event, following the rules in the module docs. At most one
    /// handle is open per sink when this returns.
    pub fn deliver<T: Transport<Handle = H>>(&mut self, transport: &mut T) -> Delivery {
        if let Some(handle) = self.handle {
            match transport.send(handle) {
                Ok(()) => return Delivery::Delivered,
                Err(SendFailure::Backpressure) => return Delivery::Dropped,
                Err(SendFailure::Gone) => {
                    transport.release(handle);
                    self.handle = None;
                }
            }
        }
        let Some(handle) = transport.resolve(&self.name) else {
            return Delivery::Dropped;
        };
        match transport.send(handle) {
            Ok(()) => {
                self.handle = Some(handle);
                Delivery::Delivered
            }
            // A freshly resolved sink that is already stalled keeps its handle
            // (the next event finds it cached); one that is gone gives it back.
            Err(SendFailure::Backpressure) => {
                self.handle = Some(handle);
                Delivery::Dropped
            }
            Err(SendFailure::Gone) => {
                transport.release(handle);
                Delivery::Dropped
            }
        }
    }

    /// Whether the sink's name is still registered; if not, the cached handle
    /// is given back and the sink ends unresolved (the roster drops it).
    pub fn check_alive<T: Transport<Handle = H>>(&mut self, transport: &mut T) -> bool {
        let Some(handle) = self.handle else {
            return false;
        };
        if transport.alive(&self.name) {
            return true;
        }
        transport.release(handle);
        self.handle = None;
        false
    }

    /// The subscriber is leaving: give the cached handle back.
    pub fn release<T: Transport<Handle = H>>(&mut self, transport: &mut T) {
        if let Some(handle) = self.handle.take() {
            transport.release(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// A transport that counts open handles and fails on demand.
    struct Counting {
        open: usize,
        next: u32,
        /// How `send` answers.
        answer: Result<(), SendFailure>,
        /// Whether the name still resolves.
        registered: bool,
    }

    impl Counting {
        fn new() -> Counting {
            Counting {
                open: 0,
                next: 0,
                answer: Ok(()),
                registered: true,
            }
        }

        fn open_handle(&mut self) -> u32 {
            self.open += 1;
            self.next += 1;
            self.next
        }
    }

    impl Transport for Counting {
        type Handle = u32;

        fn resolve(&mut self, _name: &str) -> Option<u32> {
            self.registered.then(|| self.open_handle())
        }

        fn send(&mut self, _handle: u32) -> Result<(), SendFailure> {
            self.answer
        }

        fn release(&mut self, _handle: u32) {
            assert!(self.open > 0, "a handle was released twice");
            self.open -= 1;
        }

        fn alive(&mut self, _name: &str) -> bool {
            self.registered
        }
    }

    fn sink(transport: &mut Counting) -> Sink<u32> {
        let handle = transport.open_handle();
        Sink::new("sink.1".to_string(), handle)
    }

    #[test]
    fn a_healthy_sink_keeps_its_one_handle() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        for _ in 0..1000 {
            assert_eq!(sink.deliver(&mut transport), Delivery::Delivered);
        }
        assert_eq!(transport.open, 1);
    }

    #[test]
    fn a_stalled_sink_costs_no_handles() {
        // 1000 events into a queue that stays full: every one is dropped and
        // the handle table is where it started (it was one per event).
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        transport.answer = Err(SendFailure::Backpressure);
        for _ in 0..1000 {
            assert_eq!(sink.deliver(&mut transport), Delivery::Dropped);
        }
        assert_eq!(transport.open, 1);
        assert!(sink.is_resolved());
        // It drains again: delivery resumes on the same handle.
        transport.answer = Ok(());
        assert_eq!(sink.deliver(&mut transport), Delivery::Delivered);
        assert_eq!(transport.open, 1);
    }

    #[test]
    fn a_dead_sink_gives_its_handle_back_and_ends_unresolved() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        transport.answer = Err(SendFailure::Gone);
        transport.registered = false;
        for _ in 0..1000 {
            assert_eq!(sink.deliver(&mut transport), Delivery::Dropped);
        }
        assert_eq!(transport.open, 0, "the stale handle was leaked");
        assert!(!sink.is_resolved());
    }

    #[test]
    fn a_sink_that_died_and_came_back_is_found_again() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        // The old endpoint is closed; the name now resolves to a new one that
        // takes events.
        transport.answer = Err(SendFailure::Gone);
        transport.registered = false;
        assert_eq!(sink.deliver(&mut transport), Delivery::Dropped);
        assert_eq!(transport.open, 0);
        transport.answer = Ok(());
        transport.registered = true;
        assert_eq!(sink.deliver(&mut transport), Delivery::Delivered);
        assert_eq!(transport.open, 1);
        assert!(sink.is_resolved());
    }

    #[test]
    fn a_resolved_but_unusable_endpoint_is_released() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        // The cached handle is stale and the name resolves to an endpoint that
        // is closed as well: nothing may be left open.
        transport.answer = Err(SendFailure::Gone);
        for _ in 0..100 {
            sink.deliver(&mut transport);
        }
        assert_eq!(transport.open, 0);
    }

    #[test]
    fn a_liveness_check_keeps_the_living_and_releases_the_dead() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        // A stalled sink is alive: the check does not touch its handle.
        transport.answer = Err(SendFailure::Backpressure);
        assert!(sink.check_alive(&mut transport));
        assert_eq!((transport.open, sink.is_resolved()), (1, true));
        transport.registered = false;
        assert!(!sink.check_alive(&mut transport));
        assert_eq!((transport.open, sink.is_resolved()), (0, false));
        assert!(
            !sink.check_alive(&mut transport),
            "nothing to release twice"
        );
    }

    #[test]
    fn leaving_releases_the_handle_once() {
        let mut transport = Counting::new();
        let mut sink = sink(&mut transport);
        sink.release(&mut transport);
        sink.release(&mut transport);
        assert_eq!(transport.open, 0);
    }
}
