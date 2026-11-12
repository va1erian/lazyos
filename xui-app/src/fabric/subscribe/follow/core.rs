//! The watcher's logic over a [`Subscriber`], with no threads and no kernel in
//! it so the host tests drive it against the scripted broker.
//!
//! The watcher thread owns one [`Core`]. It takes [`Command`]s (a feed added
//! or dropped), makes the subscriptions that are missing ([`Core::connect`]),
//! parks on [`Core::handles`] (every doorbell) and, when one rang, reads what
//! rang ([`Core::service`]) into each feed's [`Queue`]. Nothing here waits:
//! the thread's park is the only place time passes.

use std::sync::Arc;

use super::super::{Qos, SubId, Subscriber, Transport, MAX_DRAIN};
use super::{Queue, TopicEvent, Wake};

/// Ticks (100 Hz) before a subscription the broker refused or lost is made
/// again: the watcher is the one place a retry timer is allowed (it is not the
/// UI thread, and it runs only while a feed is missing its subscription).
const RETRY_TICKS: u64 = 100;

/// What the UI side asks the watcher for.
pub(super) enum Command {
    /// Follow `filter` into `queue`, then call `wake`.
    Add {
        key: u64,
        filter: String,
        qos: Qos,
        depth: u32,
        queue: Arc<Queue>,
        wake: Wake,
    },
    /// Stop following.
    Remove(u64),
}

/// One followed filter.
struct Entry {
    key: u64,
    filter: String,
    qos: Qos,
    depth: u32,
    queue: Arc<Queue>,
    wake: Wake,
    sub: Option<SubId>,
    /// Made since the last drain: the broker queued the retained replay
    /// before the doorbell existed, so it is read without a ring.
    fresh: bool,
    /// The last drain stopped at its batch: the bell is not re-armed.
    more: bool,
    /// The earliest tick to try subscribing again.
    retry_at: u64,
}

/// The subscriber and the feeds it serves.
pub(super) struct Core<T: Transport> {
    pub(super) subscriber: Subscriber<T>,
    entries: Vec<Entry>,
}

impl<T: Transport> Core<T> {
    pub(super) fn new(subscriber: Subscriber<T>) -> Core<T> {
        Core {
            subscriber,
            entries: Vec::new(),
        }
    }

    /// Apply one command from the UI side.
    pub(super) fn apply(&mut self, command: Command) {
        match command {
            Command::Add {
                key,
                filter,
                qos,
                depth,
                queue,
                wake,
            } => self.entries.push(Entry {
                key,
                filter,
                qos,
                depth,
                queue,
                wake,
                sub: None,
                fresh: false,
                more: false,
                retry_at: 0,
            }),
            Command::Remove(key) => {
                if let Some(at) = self.entries.iter().position(|entry| entry.key == key) {
                    let entry = self.entries.remove(at);
                    if let Some(id) = entry.sub {
                        // A refusal leaves the record in the subscriber, whose
                        // drop makes the last attempt.
                        let _ = self.subscriber.unsubscribe(id);
                    }
                }
            }
        }
    }

    /// Make the subscriptions that are missing, once their time has come.
    pub(super) fn connect(&mut self, now: u64) {
        for entry in &mut self.entries {
            if entry.sub.is_some() || now < entry.retry_at {
                continue;
            }
            let made = self
                .subscriber
                .subscribe(&entry.filter, entry.qos, entry.depth)
                .and_then(|id| match self.subscriber.attach_bell(id) {
                    Ok(_) => Ok(id),
                    Err(code) => {
                        let _ = self.subscriber.unsubscribe(id);
                        Err(code)
                    }
                });
            match made {
                Ok(id) => {
                    entry.sub = Some(id);
                    entry.fresh = true;
                    entry.queue.set_subscribed(true);
                }
                Err(_) => entry.retry_at = now.saturating_add(RETRY_TICKS),
            }
        }
    }

    /// Our end of every doorbell, to park on.
    pub(super) fn handles(&self) -> Vec<u64> {
        self.subscriber.doorbell_handles()
    }

    /// The tick to wake at to make a subscription that is missing; `None` when
    /// none is.
    pub(super) fn retry_at(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter(|entry| entry.sub.is_none())
            .map(|entry| entry.retry_at)
            .min()
    }

    /// Whether a drain left events behind: park no longer than a poll.
    pub(super) fn busy(&self) -> bool {
        self.entries.iter().any(|entry| entry.more)
    }

    /// Read what rang (and what is new, or left over) into the queues. Returns
    /// the wakes of the feeds that got events, for the caller to ring outside
    /// any lock.
    pub(super) fn service(&mut self, now: u64) -> Vec<Wake> {
        let rung = self.subscriber.consume_ready();
        let mut wakes = Vec::new();
        for entry in &mut self.entries {
            let Some(id) = entry.sub else {
                continue;
            };
            let lost = rung.lost.iter().any(|(lost, _)| *lost == id);
            if !lost && !entry.fresh && !entry.more && !rung.ready.contains(&id) {
                continue;
            }
            let drained = if lost {
                Default::default()
            } else {
                self.subscriber.drain(id, MAX_DRAIN)
            };
            entry.fresh = false;
            entry.more = drained.more;
            let got = !drained.events.is_empty();
            entry.queue.push(
                drained
                    .events
                    .iter()
                    .map(|event| TopicEvent {
                        topic: event.topic.clone(),
                        retained: event.retained,
                        publisher: event.publisher,
                        payload: event.unwrapped_payload(),
                    })
                    .collect(),
            );
            if lost || drained.error.is_some() {
                // The broker went away (its restart replays the retained
                // values on the next subscription): subscribe again later.
                let _ = self.subscriber.unsubscribe(id);
                entry.sub = None;
                entry.more = false;
                entry.queue.set_subscribed(false);
                entry.retry_at = now.saturating_add(RETRY_TICKS);
            }
            if got {
                wakes.push(Arc::clone(&entry.wake));
            }
        }
        wakes
    }
}
