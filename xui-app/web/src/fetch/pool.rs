//! [`HttpFetcher`]: runs each fetch on a thread of its own, at most
//! [`Options::max_concurrent`] at a time.
//!
//! A thread per fetch rather than a fixed pool: on LazyOS a thread's sockets
//! are its own, so nothing is shared between fetches anyway, and a fetch
//! that hangs until its timeout holds one slot, not a worker other pages
//! queue behind forever.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use ureq::Agent;

use super::{transfer, Options, Request, Sink};

/// Stack for a fetch thread: a TLS handshake and an inflater, nothing deep.
const STACK_BYTES: usize = 512 * 1024;

/// Fetches pages; cheap to clone, and every clone shares the slot limit.
#[derive(Clone)]
pub struct HttpFetcher {
    shared: Arc<Shared>,
}

struct Shared {
    agent: Agent,
    options: Options,
    slots: Slots,
}

impl HttpFetcher {
    pub fn new(options: Options) -> HttpFetcher {
        HttpFetcher {
            shared: Arc::new(Shared {
                agent: transfer::agent(&options),
                slots: Slots::new(options.max_concurrent.max(1)),
                options,
            }),
        }
    }

    /// Starts fetching `request` on a new thread and returns at once.
    pub fn start<S: Sink>(&self, request: Request, sink: S) {
        // The sink waits here until the thread takes it, so it can still be
        // failed if the thread never starts.
        let parked = Arc::new(Mutex::new(Some(sink)));
        let taken = Arc::clone(&parked);
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name("lazyweb-fetch".into())
            .stack_size(STACK_BYTES)
            .spawn(move || {
                if let Some(sink) = lock(&taken).take() {
                    shared.fetch(request, sink);
                }
            });
        if let Err(e) = spawned {
            if let Some(sink) = lock(&parked).take() {
                sink.fail(&format!("cannot start a fetch: {e}"));
            }
        }
    }

    /// Fetches `request` on the calling thread (holding a slot like any
    /// other fetch).
    pub fn fetch_here<S: Sink>(&self, request: Request, sink: S) {
        self.shared.fetch(request, sink);
    }
}

impl Shared {
    fn fetch<S: Sink>(&self, request: Request, sink: S) {
        let _slot = self.slots.acquire();
        transfer::run(&self.agent, &self.options, request, sink);
    }
}

/// A counting semaphore.
struct Slots {
    busy: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

impl Slots {
    fn new(max: usize) -> Slots {
        Slots {
            busy: Mutex::new(0),
            freed: Condvar::new(),
            max,
        }
    }

    fn acquire(&self) -> Slot<'_> {
        let mut busy = lock(&self.busy);
        while *busy >= self.max {
            busy = self.freed.wait(busy).unwrap_or_else(|p| p.into_inner());
        }
        *busy += 1;
        Slot(self)
    }
}

/// A held slot, given back on drop (also when a fetch panics).
struct Slot<'a>(&'a Slots);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) -= 1;
        self.0.freed.notify_one();
    }
}

/// Locks `mutex`, ignoring poisoning: the data is a counter or an `Option`
/// that a panicking holder leaves consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn slots_cap_concurrency() {
        let slots = Arc::new(Slots::new(2));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (slots, running, peak) = (slots.clone(), running.clone(), peak.clone());
                thread::spawn(move || {
                    let _slot = slots.acquire();
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
}
