//! A queue of results the UI takes at its own pace, with a doorbell.
//!
//! Most workers answer with one message per result ([`spawn`](crate::spawn),
//! a [`Sink`] over the proxy). A burst of small results (thumbnails, laid-out
//! rows) is better as one queue the UI drains in a batch, under its own time
//! budget: the workers push, and a single doorbell message says "there is
//! something".

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use xui_core::app::Proxy;

use crate::sink::Sink;

struct Shared<T> {
    queue: Mutex<VecDeque<T>>,
    /// Whether the bell rang since the UI last took results; one ring per
    /// burst, so a thousand results do not post a thousand messages.
    rung: AtomicBool,
}

/// The UI's end of a result queue: [`inbox`] makes it with the workers' sink.
pub struct Inbox<T> {
    shared: Arc<Shared<T>>,
}

/// A queue whose pushes ring `bell` through `proxy` (once per burst), and the
/// sink workers push results with. The window answers `bell` by calling
/// [`Inbox::drain`] or [`Inbox::take`].
pub fn inbox<M, T>(proxy: &Proxy<M>, bell: M) -> (Sink<T>, Inbox<T>)
where
    M: Clone + Send + 'static,
    T: Send + 'static,
{
    let ring = Sink::bell(proxy, bell);
    inbox_with(move || ring.ring())
}

/// [`inbox`] over any doorbell function (host tests count rings with one).
pub fn inbox_with<T: Send + 'static>(
    ring: impl Fn() -> bool + Send + Sync + 'static,
) -> (Sink<T>, Inbox<T>) {
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        rung: AtomicBool::new(false),
    });
    let pusher = Arc::clone(&shared);
    let sink = Sink::from_fn(move |value| {
        pusher
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(value);
        if pusher.rung.swap(true, Ordering::AcqRel) {
            return true;
        }
        ring()
    });
    (sink, Inbox { shared })
}

impl<T> Inbox<T> {
    /// Takes up to `most` results, oldest first, and re-arms the bell: a
    /// result pushed after this call rings again. Results left behind stay
    /// queued; the caller comes back for them (a timer it already runs, or
    /// its own next message).
    pub fn take(&self, most: usize) -> Vec<T> {
        self.shared.rung.store(false, Ordering::Release);
        let mut queue = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        let count = most.min(queue.len());
        queue.drain(..count).collect()
    }

    /// Takes every waiting result.
    pub fn drain(&self) -> Vec<T> {
        self.take(usize::MAX)
    }

    /// Whether no result is waiting.
    pub fn is_empty(&self) -> bool {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn a_burst_rings_once_and_a_take_rearms() {
        let rings = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&rings);
        let (sink, inbox) = inbox_with(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            true
        });
        for n in 0..100 {
            assert!(sink.send(n));
        }
        assert_eq!(rings.load(Ordering::SeqCst), 1);
        assert_eq!(inbox.take(3), vec![0, 1, 2]);
        assert!(!inbox.is_empty());
        sink.send(100);
        assert_eq!(rings.load(Ordering::SeqCst), 2);
        assert_eq!(inbox.drain().len(), 98);
        assert!(inbox.is_empty());
    }
}
