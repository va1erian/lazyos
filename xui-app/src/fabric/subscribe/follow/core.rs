//! The hub's logic over a [`Subscriber`], with no kernel in it so the host
//! tests drive it against the scripted broker.
//!
//! A thread's [`Core`] makes the subscriptions that are missing
//! ([`Core::connect`]) and, when a doorbell rang, reads what rang into each
//! feed's [`Queue`] ([`Core::service`]). Neither waits: a doorbell is a local
//! queue, and the broker is called only to subscribe and to drain what a bell
//! announced.

use std::rc::Rc;

use super::super::{Errno, Qos, SubId, Subscriber, Transport, MAX_DRAIN};
use super::{Queue, TopicEvent};

/// Ticks (100 Hz) before a subscription the broker lost, or could not answer
/// for, is made again.
const RETRY_TICKS: u64 = 200;

/// A refusal that asking again cannot change: not permitted, access denied,
/// invalid. (Anything else, a broker not yet registered or restarting, is
/// worth another try.)
fn permanent(code: Errno) -> bool {
    matches!(code, -1 | -13 | -22)
}

/// Looks a new subscription gets for its retained replay.
const REPLAY_LOOKS: u8 = 3;

/// One followed filter.
struct Entry {
    key: u64,
    filter: String,
    qos: Qos,
    depth: u32,
    queue: Rc<Queue>,
    sub: Option<SubId>,
    /// Made, or a ring that found nothing, since the last drain: read it
    /// without waiting for a ring. (The broker queued the retained replay
    /// before the doorbell existed; and a poll can miss an event a busy
    /// broker has queued, which leaves the bell rung with nothing to ring
    /// again.)
    pending: bool,
    /// Looks left for the replay a new subscription is owed: each finds the
    /// queue empty or not (a poll the broker does not answer in its turn is
    /// an empty one), so a few are made before giving the ring the job.
    replay_looks: u8,
    /// The last drain stopped at its batch: the bell is not re-armed.
    more: bool,
    /// The earliest tick to try subscribing again.
    retry_at: u64,
    /// The broker refused for good: never asked again.
    refused: bool,
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

    /// Follow `filter` into `queue`.
    pub(super) fn add(&mut self, key: u64, filter: &str, qos: Qos, depth: u32, queue: Rc<Queue>) {
        self.entries.push(Entry {
            key,
            filter: filter.to_owned(),
            qos,
            depth,
            queue,
            sub: None,
            pending: false,
            replay_looks: 0,
            more: false,
            retry_at: 0,
            refused: false,
        });
    }

    /// Stop following.
    pub(super) fn remove(&mut self, key: u64) {
        if let Some(at) = self.entries.iter().position(|entry| entry.key == key) {
            let entry = self.entries.remove(at);
            if let Some(id) = entry.sub {
                self.subscriber.forget(id);
            }
        }
    }

    /// Make the subscriptions that are missing, once their time has come.
    pub(super) fn connect(&mut self, now: u64) {
        for entry in &mut self.entries {
            if entry.sub.is_some() || entry.refused || now < entry.retry_at {
                continue;
            }
            let made = self
                .subscriber
                .subscribe(&entry.filter, entry.qos, entry.depth)
                .and_then(|id| match self.subscriber.attach_bell(id) {
                    Ok(_) => Ok(id),
                    Err(code) => {
                        self.subscriber.forget(id);
                        Err(code)
                    }
                });
            match made {
                Ok(id) => {
                    entry.sub = Some(id);
                    entry.pending = true;
                    entry.replay_looks = REPLAY_LOOKS;
                    entry.queue.set_subscribed(true);
                }
                Err(code) => {
                    entry.refused = permanent(code);
                    entry.retry_at = now.saturating_add(RETRY_TICKS);
                }
            }
        }
    }

    /// The doorbell of feed `key`, once it is subscribed.
    #[cfg(test)]
    pub(super) fn bell_of(&self, key: u64) -> Option<u64> {
        let id = self.entries.iter().find(|entry| entry.key == key)?.sub?;
        self.subscriber.bell_of(id)
    }

    /// The doorbells of every subscribed feed: what a loop parks on.
    pub(super) fn bells(&self) -> Vec<u64> {
        self.subscriber.doorbell_handles()
    }

    /// Whether the hub owes reads no doorbell will announce (a new
    /// subscription's replay, a poll that missed an event, a drain that
    /// stopped at its batch): a loop must look again before it parks.
    pub(super) fn owes(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.sub.is_some() && (entry.pending || entry.more))
    }

    /// Read what rang (and what is new, or left over) into the queues.
    pub(super) fn service(&mut self, now: u64) {
        let rung = self.subscriber.consume_ready();
        for entry in &mut self.entries {
            let Some(id) = entry.sub else {
                continue;
            };
            let lost = rung.lost.iter().any(|(lost, _)| *lost == id);
            let ready = rung.ready.contains(&id);
            if !lost && !ready && !entry.pending && !entry.more {
                continue;
            }
            let drained = if lost {
                Default::default()
            } else {
                self.subscriber.drain(id, MAX_DRAIN)
            };
            // A ring that found nothing: the poll may have missed an event.
            let empty = drained.events.is_empty() && drained.error.is_none();
            entry.pending = empty && (ready || entry.replay_looks > 1);
            if !drained.events.is_empty() {
                entry.replay_looks = 0;
            } else {
                entry.replay_looks = entry.replay_looks.saturating_sub(1);
            }
            entry.more = drained.more;
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
                // The old record goes whatever the broker says; one that
                // missed the request reaps the subscription by its dead bell.
                self.subscriber.forget(id);
                entry.sub = None;
                entry.more = false;
                entry.pending = false;
                entry.retry_at = now.saturating_add(RETRY_TICKS);
                entry.queue.set_subscribed(false);
            }
        }
    }

    /// The tick a missing subscription may be tried again; `None` when none
    /// is waiting for one.
    pub(super) fn retry_at(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter(|entry| entry.sub.is_none() && !entry.refused)
            .map(|entry| entry.retry_at)
            .min()
    }

    /// Whether a drain left events behind.
    #[cfg(test)]
    pub(super) fn busy(&self) -> bool {
        self.entries.iter().any(|entry| entry.more)
    }
}
