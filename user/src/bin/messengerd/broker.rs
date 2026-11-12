//! The userspace topics broker: subscription tables, QoS queues, retained
//! values, fanout and the drop accounting behind `docs/messenger.md` section
//! 7.2's QoS table.

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::{self, errno, topics_client};

use super::filter::is_system_topic;
pub(super) use super::queue::Subscription;

/// Largest number of live subscriptions the broker keeps: every service and
/// app follows its topics through it (the per-service routers are gone).
pub(super) const MAX_SUBSCRIPTIONS: usize = 256;
/// Largest number of distinct topics tracked for [`Broker::list`], and of
/// retained values kept: a retained value dropped for want of a slot is a
/// late subscriber's replay that never comes (the supervisors' per-service
/// rows alone are a hundred).
const MAX_TOPICS: usize = 1024;
/// Largest number of live subscriptions one task may hold.
pub(super) const MAX_PER_OWNER: usize = 64;
/// Largest number of parked `NextEvent` transactions: one per subscription
/// at most (a new pull replaces its own stale entry), so a parked pull is
/// never refused for want of a slot.
pub(super) const MAX_PENDING: usize = MAX_SUBSCRIPTIONS;
/// Bytes the broker holds for retained values and queued events together.
/// A publish that would pass it is refused (`ENOMEM`) instead of growing
/// the daemon without bound; 256 subscriptions of 64 events of 8 KiB could
/// otherwise reach 128 MiB.
pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;

/// What one broker request produced: an optional immediate reply plus any
/// parked pulls that the request satisfied.
#[derive(Default)]
pub struct Outcome {
    pub(super) reply: Option<libmessenger::Parcel>,
    pub(super) wakes: Vec<Wake>,
    /// An event handed out in the request's own `NextEvent` reply; it is
    /// committed only when that reply reaches the caller.
    pub(super) delivery: Option<Delivery>,
    /// A subscription this request created: rolled back when its reply
    /// cannot be delivered (the caller timed out or is gone), or the
    /// caller never learns the id and the entry leaks.
    pub(super) created: Option<u64>,
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
    /// Bytes the retained values hold.
    retained_bytes: usize,
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
            retained_bytes: 0,
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

    /// Bytes held for retained values and queued events.
    pub(super) fn bytes_used(&self) -> usize {
        self.retained_bytes
            + self
                .subscriptions
                .iter()
                .map(|sub| sub.bytes)
                .sum::<usize>()
    }

    /// Whether `payload_len` bytes published on `topic` (every matching
    /// subscription queues a copy, and a retained publish keeps one) fit the
    /// memory budget.
    pub(super) fn has_room(&self, topic: &str, payload_len: usize, retained: bool) -> bool {
        let copies = self
            .subscriptions
            .iter()
            .filter(|sub| sub.filter.matches(topic))
            .count()
            + usize::from(retained);
        let each = payload_len + topic.len() + 64;
        self.bytes_used()
            .saturating_add(each.saturating_mul(copies))
            <= MAX_BYTES
    }

    /// How many subscriptions `owner` holds.
    pub(super) fn owned_by(&self, owner: u64) -> usize {
        self.subscriptions
            .iter()
            .filter(|sub| sub.owner == owner)
            .count()
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
                self.subscriptions[index].push(event.clone());
            }
        }
        self.touch_topic(topic, retained);
        matched
    }

    /// Remember (or replace) the retained value for a topic.
    fn set_retained(&mut self, event: &topics_client::Event) {
        let cost = super::queue::event_cost(event);
        if let Some(slot) = self
            .retained
            .iter_mut()
            .find(|(topic, _)| topic == &event.topic)
        {
            let old = super::queue::event_cost(&slot.1);
            slot.1 = event.clone();
            self.retained_bytes = self.retained_bytes.saturating_sub(old) + cost;
        } else if self.retained.len() < MAX_TOPICS {
            self.retained.push((event.topic.clone(), event.clone()));
            self.retained_bytes += cost;
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
            sub.push(event);
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
            match self.subscriptions[sub_index].peek() {
                Some(event) => {
                    let sequence = event.sequence;
                    let encoded = super::wire::reply_event(event);
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
        sub.retire(sequence);
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
        if sub.qos != topics_client::Qos::Reliable {
            sub.retire(sequence);
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
        if let Some(bell) = self.subscriptions.remove(index).bell {
            let _ = bell.close();
        }
        // Parked pulls for it can never be satisfied; the clients discover
        // that on their own deadline, so just drop the bookkeeping.
        self.pending
            .retain(|pending| !(pending.subscription == id && pending.owner == owner));
        Ok(())
    }

    /// Install `bell` as subscription `id`'s doorbell (owner only), closing
    /// the one it replaces. The bell is unrung, so queued events ring it.
    pub(super) fn set_bell(
        &mut self,
        id: u64,
        owner: u64,
        bell: messenger::Endpoint,
    ) -> Result<(), messenger::Error> {
        let sub = self
            .subscriptions
            .iter_mut()
            .find(|sub| sub.id == id)
            .ok_or(messenger::Error::Topics(errno::ENOENT))?;
        if sub.owner != owner {
            return Err(messenger::Error::Topics(errno::EPERM));
        }
        if let Some(old) = sub.bell.replace(bell) {
            let _ = old.close();
        }
        sub.rung = false;
        Ok(())
    }

    /// The bells to ring now: every unrung bell whose subscription has an
    /// event queued (parked pulls were answered first, so nobody is pulling
    /// it). Each is marked rung until its owner drains the queue.
    pub(super) fn bells_due(&mut self) -> Vec<(u64, messenger::Endpoint)> {
        let mut due = Vec::new();
        for sub in self.subscriptions.iter_mut() {
            if let Some(bell) = sub.bell.filter(|_| !sub.rung && sub.queued() > 0) {
                sub.rung = true;
                due.push((sub.id, bell));
            }
        }
        due
    }

    /// Forget subscription `id`'s bell (its owner's end closed).
    pub(super) fn drop_bell(&mut self, id: u64) {
        if let Some(sub) = self.subscriptions.iter_mut().find(|sub| sub.id == id) {
            if let Some(bell) = sub.bell.take() {
                let _ = bell.close();
            }
        }
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
