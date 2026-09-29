//! The userspace topics broker: subscription tables, QoS queues, retained
//! values, fanout and the drop accounting behind `docs/messenger.md` section
//! 7.2's QoS table.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::{self, errno, topics_client};

use super::filter::{is_system_topic, Filter};

/// Largest number of live subscriptions the broker keeps.
pub(super) const MAX_SUBSCRIPTIONS: usize = 64;
/// Largest number of distinct topics tracked for [`Broker::list`].
const MAX_TOPICS: usize = 64;
/// Largest number of parked `NextEvent` transactions.
pub(super) const MAX_PENDING: usize = 64;

/// What one broker request produced: an optional immediate reply plus any
/// parked pulls that the request satisfied.
#[derive(Default)]
pub struct Outcome {
    pub(super) reply: Option<libmessenger::Parcel>,
    pub(super) wakes: Vec<Wake>,
    /// An event handed out in the request's own `NextEvent` reply; it is
    /// committed only when that reply reaches the caller.
    pub(super) delivery: Option<Delivery>,
}

/// A parked pull the broker can answer.
pub struct Wake {
    pub(super) txn: u64,
    pub(super) parcel: libmessenger::Parcel,
    pub(super) subscription: u64,
    pub(super) sequence: u64,
}

/// One event handed to a subscriber; popped from its queue when the reply
/// carrying it succeeds (a failed reply means the poll was abandoned, and the
/// event must stay for the next one).
#[derive(Clone, Copy)]
pub(super) struct Delivery {
    pub(super) subscription: u64,
    pub(super) sequence: u64,
}

/// One live subscription with its QoS queue and counters.
pub(super) struct Subscription {
    pub(super) id: u64,
    /// Task slot the subscribing call came from (kernel-stamped).
    pub(super) owner: u64,
    pub(super) filter: Filter,
    pub(super) qos: topics_client::Qos,
    /// Events ready for delivery (latest / buffered / reliable).
    pub(super) queue: VecDeque<topics_client::Event>,
    /// Coalesced per-publisher slots (conflate only).
    pub(super) conflated: Vec<(u64, topics_client::Event)>,
    pub(super) drops: u64,
    pub(super) delivered: u64,
    pub(super) matched: u64,
}

impl Subscription {
    /// Pending events across whichever queue the QoS uses.
    pub(super) fn queued(&self) -> u64 {
        (self.queue.len() + self.conflated.len()) as u64
    }
}

/// One parked `NextEvent` transaction waiting for a matching publish.
#[derive(Clone, Copy)]
pub(super) struct Pending {
    pub(super) subscription: u64,
    pub(super) owner: u64,
    pub(super) txn: u64,
}

/// A topic the broker has seen at least one publish for.
struct TopicRow {
    topic: String,
    retained: bool,
}

/// The userspace topics broker: subscriptions, retained values, fanout and
/// the drop accounting behind `docs/messenger.md` section 7.2's QoS table.
pub struct Broker {
    pub(super) subscriptions: Vec<Subscription>,
    /// Retained value per topic, most recent last.
    retained: Vec<(String, topics_client::Event)>,
    topics: Vec<TopicRow>,
    pub(super) pending: Vec<Pending>,
    pub(super) next_subscription: u64,
    next_sequence: u64,
}

impl Default for Broker {
    /// An empty broker; ids start at 1 so 0 is never a valid handle.
    fn default() -> Broker {
        Broker {
            subscriptions: Vec::new(),
            retained: Vec::new(),
            topics: Vec::new(),
            pending: Vec::new(),
            next_subscription: 1,
            next_sequence: 1,
        }
    }
}

impl Broker {
    pub fn new() -> Broker {
        Self::default()
    }

    /// Live `(topics, subscriptions)` counts under the platform's `system/`
    /// root, for the soak evidence markers. Counting the whole table would
    /// let the `messengerctl` self-test's own `topics/`, `selftest/` traffic
    /// satisfy the soak's threshold without `sysmond`, `clipboardd` or
    /// `mimed` ever publishing centrally.
    pub fn counts(&self) -> (usize, usize) {
        let topics = self
            .topics
            .iter()
            .filter(|row| is_system_topic(&row.topic))
            .count();
        let subs = self
            .subscriptions
            .iter()
            .filter(|sub| sub.filter.segments.first().is_some_and(|s| s == "system"))
            .count();
        (topics, subs)
    }

    /// Fan one publish out to every matching subscription, applying each
    /// subscription's QoS policy and counting drops. Returns the match count,
    /// which the publish reply reports.
    pub(super) fn publish(
        &mut self,
        topic: &str,
        publisher: u64,
        payload: Vec<u8>,
        retained: bool,
    ) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let event = topics_client::Event {
            topic: String::from(topic),
            publisher,
            sequence,
            retained,
            payload,
        };
        if retained {
            self.set_retained(&event);
        }
        let mut matched = 0;
        for index in 0..self.subscriptions.len() {
            if self.subscriptions[index].filter.matches(topic) {
                matched += 1;
                self.subscriptions[index].matched += 1;
                enqueue(&mut self.subscriptions[index], event.clone());
            }
        }
        self.touch_topic(topic, retained);
        matched
    }

    /// Remember (or replace) the retained value for a topic.
    fn set_retained(&mut self, event: &topics_client::Event) {
        if let Some(slot) = self
            .retained
            .iter_mut()
            .find(|(topic, _)| topic == &event.topic)
        {
            slot.1 = event.clone();
        } else if self.retained.len() < MAX_TOPICS {
            self.retained.push((event.topic.clone(), event.clone()));
        }
    }

    /// Hand the freshly created subscription any retained value its filter
    /// matches (`docs/messenger.md` 7.2: "new subscribers get it immediately").
    pub(super) fn replay_retained(&mut self, index: usize, id: u64) {
        let matches: Vec<topics_client::Event> = self
            .retained
            .iter()
            .filter(|(topic, _)| self.subscriptions[index].filter.matches(topic))
            .map(|(_, event)| event.clone())
            .collect();
        let sub = &mut self.subscriptions[index];
        for mut event in matches {
            event.retained = true;
            enqueue(sub, event);
        }
        debug_assert_eq!(sub.id, id);
    }

    /// Answer every parked pull that now has a deliverable event. Events are
    /// peeked, not popped: the caller commits them once a reply reaches the
    /// subscriber (see [`Broker::commit`]).
    pub(super) fn satisfy(&mut self) -> Vec<Wake> {
        let mut wakes = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let pending = self.pending[index];
            let Some(sub_index) = self
                .subscriptions
                .iter()
                .position(|sub| sub.id == pending.subscription && sub.owner == pending.owner)
            else {
                // The subscription is gone; drop the parked pull.
                self.pending.remove(index);
                continue;
            };
            match peek(&self.subscriptions[sub_index]) {
                Some(event) => {
                    let sequence = event.sequence;
                    let encoded = topics_client::reply_event(event);
                    self.pending.remove(index);
                    match encoded {
                        Ok(parcel) => {
                            wakes.push(Wake {
                                txn: pending.txn,
                                parcel,
                                subscription: pending.subscription,
                                sequence,
                            });
                        }
                        Err(_) => {
                            // The event can't be encoded into a reply (e.g.
                            // too large for the buffer): drop it so this
                            // subscription doesn't stall on the same
                            // unencodable head forever. This parked pull
                            // gets no reply from this round; the caller's
                            // own deadline (or its next poll) covers it.
                            self.drop_undeliverable(pending.subscription, sequence);
                        }
                    }
                }
                None => index += 1,
            }
        }
        wakes
    }

    /// Drop the head event of `id`'s queue unconditionally, including for
    /// `Reliable` (which [`Broker::commit`] otherwise never pops without an
    /// explicit `ack`), because it could not be encoded into a reply and
    /// would otherwise stall the subscription on the same event forever.
    /// Counts as a QoS drop.
    pub(super) fn drop_undeliverable(&mut self, id: u64, sequence: u64) {
        let Some(sub) = self.subscriptions.iter_mut().find(|sub| sub.id == id) else {
            return;
        };
        sub.drops += 1;
        match sub.qos {
            topics_client::Qos::Conflate => {
                if sub
                    .conflated
                    .first()
                    .is_some_and(|(_, event)| event.sequence == sequence)
                {
                    sub.conflated.remove(0);
                }
            }
            topics_client::Qos::Reliable
            | topics_client::Qos::Latest
            | topics_client::Qos::Buffered(_) => {
                if sub
                    .queue
                    .front()
                    .is_some_and(|event| event.sequence == sequence)
                {
                    sub.queue.pop_front();
                }
            }
        }
    }

    /// Retire the event a successful reply carried. The head is checked by
    /// sequence, so a late commit cannot pop a newer event (`reliable`
    /// subscriptions do not pop at all; their events retire on [`ACK`]).
    /// `delivered` is counted here rather than where the reply is built,
    /// since only a reply that actually reached the subscriber (a `commit`)
    /// is a real delivery; counting earlier risked a double count when the
    /// reply failed and the same still-queued event was delivered again.
    pub(super) fn commit(&mut self, id: u64, sequence: u64) {
        let Some(sub) = self.subscriptions.iter_mut().find(|sub| sub.id == id) else {
            return;
        };
        sub.delivered += 1;
        match sub.qos {
            topics_client::Qos::Reliable => {}
            topics_client::Qos::Conflate => {
                if sub
                    .conflated
                    .first()
                    .is_some_and(|(_, event)| event.sequence == sequence)
                {
                    sub.conflated.remove(0);
                }
            }
            topics_client::Qos::Latest | topics_client::Qos::Buffered(_) => {
                if sub
                    .queue
                    .front()
                    .is_some_and(|event| event.sequence == sequence)
                {
                    sub.queue.pop_front();
                }
            }
        }
    }

    /// Drop a subscription, scoped to its owner.
    pub(super) fn remove(&mut self, id: u64, owner: u64) -> Result<(), messenger::Error> {
        let index = self
            .subscriptions
            .iter()
            .position(|sub| sub.id == id)
            .ok_or(messenger::Error::Topics(errno::ENOENT))?;
        if self.subscriptions[index].owner != owner {
            return Err(messenger::Error::Topics(errno::EPERM));
        }
        self.subscriptions.remove(index);
        // Parked pulls for it can never be satisfied; the clients discover
        // that on their own deadline, so just drop the bookkeeping.
        self.pending
            .retain(|pending| !(pending.subscription == id && pending.owner == owner));
        Ok(())
    }

    /// Track a topic for [`Broker::list`].
    fn touch_topic(&mut self, topic: &str, retained: bool) {
        if let Some(row) = self.topics.iter_mut().find(|row| row.topic == topic) {
            row.retained = row.retained || retained;
        } else if self.topics.len() < MAX_TOPICS {
            self.topics.push(TopicRow {
                topic: String::from(topic),
                retained,
            });
        }
    }

    /// Snapshot the topic table with live subscriber counts.
    pub(super) fn list(&self) -> Vec<topics_client::TopicInfo> {
        self.topics
            .iter()
            .map(|row| topics_client::TopicInfo {
                topic: row.topic.clone(),
                subscribers: self
                    .subscriptions
                    .iter()
                    .filter(|sub| sub.filter.matches(&row.topic))
                    .count() as u64,
                retained: row.retained,
            })
            .collect()
    }
}

/// Queue one event under a subscription's QoS policy.
///
/// * `latest`: one slot; a new event replaces the old one (a drop).
/// * `buffered`: `depth` slots; overflow drops the oldest.
/// * `conflate`: the latest event per publisher; replacing a publisher's
///   pending event drops the coalesced one. The window is "until consumed":
///   without a clock the broker cannot time-window, and the subscriber's
///   `NextEvent` is the natural boundary.
/// * `reliable`: like buffered, but delivery does not pop; [`take`] hands out
///   the head until the subscriber acks it.
fn enqueue(sub: &mut Subscription, event: topics_client::Event) {
    match sub.qos {
        topics_client::Qos::Latest => {
            if !sub.queue.is_empty() {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Buffered(depth) => {
            let depth = depth.max(1) as usize;
            while sub.queue.len() >= depth {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Reliable => {
            let depth = topics_client::Qos::RELIABLE_DEPTH as usize;
            while sub.queue.len() >= depth {
                sub.queue.pop_front();
                sub.drops += 1;
            }
            sub.queue.push_back(event);
        }
        topics_client::Qos::Conflate => {
            if let Some(slot) = sub
                .conflated
                .iter_mut()
                .find(|(publisher, _)| *publisher == event.publisher)
            {
                *slot = (event.publisher, event);
                sub.drops += 1;
            } else {
                let window = topics_client::Qos::CONFLATE_WINDOW as usize;
                while sub.conflated.len() >= window {
                    sub.conflated.remove(0);
                    sub.drops += 1;
                }
                sub.conflated.push((event.publisher, event));
            }
        }
    }
}

/// The next event for a subscription, without consuming it: the pop happens
/// in [`Broker::commit`] once the reply carrying the event has reached the
/// subscriber. Borrowed rather than cloned: `reply_event` only needs to read
/// it, and cloning a payload-sized event on every poll is an avoidable copy.
pub(super) fn peek(sub: &Subscription) -> Option<&topics_client::Event> {
    match sub.qos {
        topics_client::Qos::Conflate => sub.conflated.first().map(|(_, event)| event),
        topics_client::Qos::Reliable
        | topics_client::Qos::Latest
        | topics_client::Qos::Buffered(_) => sub.queue.front(),
    }
}
