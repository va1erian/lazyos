//! The topics broker's tables: subscriptions, QoS queues, retained values,
//! fanout and the drop accounting behind `docs/messenger.md` section 7.2's
//! QoS table, under per-tenant budgets.
//!
//! # Tenants
//!
//! Limits are charged to a [`Tenant`], not to a task slot. A slot names one
//! thread and is reused after the task exits, so keying a budget on it lets an
//! app with many threads multiply its allowance and lets an unrelated task
//! inherit a dead one's charges. A tenant is the kernel-stamped
//! `(label, uid, session)` of an app (all its threads and processes share
//! one budget), or one platform task slot (no label, no login session, the
//! rule the kernel uses for the `WaitFor` reserve). Who may pull, ack or
//! remove a subscription is a different question and stays with the creating
//! [`Client`] (slot and tenant).
//!
//! # The budgets
//!
//! * **Bytes.** Retained values are charged to the tenant that published them
//!   and queued events to the tenant that subscribed, so an app can exhaust
//!   only its own allowance ([`TENANT_BYTES`], [`SYSTEM_TENANT_BYTES`] for
//!   platform tasks). Behind the allowances sits the global [`MAX_BYTES`];
//!   the last [`SYSTEM_RESERVE`] of it is kept for platform tasks.
//! * **Subscriptions** ([`TENANT_SUBSCRIPTIONS`] per app) and **retained
//!   values** ([`TENANT_RETAINED`] per app): the last
//!   [`SYSTEM_SUBSCRIPTION_RESERVE`] subscription rows and
//!   [`SYSTEM_TOPIC_RESERVE`] topic rows are kept for platform tasks too.
//!
//! So apps together can never starve the services, and one app, however many
//! threads or processes it runs, never holds more than its tenant's share.
//! Apps of different labels, uids or sessions are different tenants and can
//! still fill the shared app part between them (a pool of tenants, not a
//! per-user guarantee). A queue that has no room sheds its own oldest events
//! instead of refusing the publisher; only a retained publish is refused
//! (`NoRoom`), and only the one that would exceed its own tenant's budget.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use messenger_generated::os_lazy_messenger_topics_v1::{Event, TopicInfo};

use crate::filter::{is_system_topic, Filter};
use crate::ledger::Account;
use crate::limits::{
    MAX_PENDING, MAX_SUBSCRIPTIONS, MAX_TOPICS, SYSTEM_SUBSCRIPTION_RESERVE,
    SYSTEM_TENANT_SUBSCRIPTIONS, SYSTEM_TOPIC_RESERVE, TENANT_SUBSCRIPTIONS,
};
use crate::qos::Qos;
use crate::queue::Subscription;
use crate::retained::Retained;
use crate::tenant::{Client, Tenant};

/// Why the broker refused a request; `messengerd` maps each to an errno-style
/// code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A table or budget is full (`ENOMEM`).
    NoRoom,
    /// The caller holds too many of something (`EAGAIN`).
    Busy,
    /// No such subscription (`ENOENT`).
    NotFound,
    /// The subscription is someone else's (`EPERM`).
    NotOwner,
    /// The request itself is malformed (`EINVAL`).
    Invalid,
}

/// A parked pull the broker can answer.
pub struct Wake<T> {
    pub txn: u64,
    pub parcel: T,
    pub subscription: u64,
    pub sequence: u64,
}

/// One parked `NextEvent` transaction waiting for a matching publish.
#[derive(Clone, Copy)]
pub struct Pending {
    pub subscription: u64,
    pub owner: u64,
    pub txn: u64,
}

/// What [`Broker::pull`] found.
#[derive(PartialEq, Eq, Debug)]
pub enum Pulled {
    /// The subscription (at this index) has a head event to answer with.
    Head(usize),
    /// Nothing queued and the caller did not wait (a poll): answer nothing.
    Empty,
    /// Nothing queued; the transaction is parked for a matching publish.
    Parked,
}

/// A topic the broker has seen at least one publish for.
struct TopicRow {
    topic: String,
    retained: bool,
}

/// The tables. `B` is the doorbell handle a subscription may carry.
pub struct Broker<B> {
    pub subscriptions: Vec<Subscription<B>>,
    /// Retained value per topic, most recent last, with the tenant it is
    /// charged to.
    pub(crate) retained: Vec<Retained>,
    topics: Vec<TopicRow>,
    pub pending: Vec<Pending>,
    next_subscription: u64,
    next_sequence: u64,
    /// Per-tenant ledger of what [`Broker::used`] is made of.
    pub(crate) accounts: BTreeMap<Tenant, Account>,
    /// Bytes held across every account.
    pub(crate) used: usize,
}

impl<B> Default for Broker<B> {
    /// An empty broker; ids start at 1 so 0 is never a valid handle.
    fn default() -> Self {
        Broker {
            subscriptions: Vec::new(),
            retained: Vec::new(),
            topics: Vec::new(),
            pending: Vec::new(),
            next_subscription: 1,
            next_sequence: 1,
            accounts: BTreeMap::new(),
            used: 0,
        }
    }
}

impl<B: Copy> Broker<B> {
    pub fn new() -> Self {
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
    pub fn bytes_used(&self) -> usize {
        self.used
    }

    /// Bytes charged to `tenant` (retained values it published plus the
    /// queues it subscribed).
    pub fn bytes_of(&self, tenant: &Tenant) -> usize {
        self.accounts.get(tenant).map_or(0, |account| account.bytes)
    }

    /// How many retained values `tenant` holds.
    pub fn retained_by(&self, tenant: &Tenant) -> usize {
        self.accounts
            .get(tenant)
            .map_or(0, |account| account.retained)
    }

    /// How many subscriptions `tenant` holds.
    pub fn owned_by(&self, tenant: &Tenant) -> usize {
        self.subscriptions
            .iter()
            .filter(|sub| sub.tenant == *tenant)
            .count()
    }

    /// Create a subscription for `client` and hand it any retained value its
    /// filter matches (`docs/messenger.md` 7.2: "new subscribers get it
    /// immediately"). Returns its id. Refused when the table is full (an
    /// app's part stops short of [`SYSTEM_SUBSCRIPTION_RESERVE`]) or its
    /// tenant already holds its share ([`TENANT_SUBSCRIPTIONS`]); the byte
    /// budget never refuses a subscribe (queues shed instead).
    pub fn subscribe(&mut self, client: Client, filter: Filter, qos: Qos) -> Result<u64, Refusal> {
        let platform = client.tenant.is_platform();
        let (rows, share) = if platform {
            (MAX_SUBSCRIPTIONS, SYSTEM_TENANT_SUBSCRIPTIONS)
        } else {
            (
                MAX_SUBSCRIPTIONS - SYSTEM_SUBSCRIPTION_RESERVE,
                TENANT_SUBSCRIPTIONS,
            )
        };
        if self.subscriptions.len() >= rows {
            return Err(Refusal::NoRoom);
        }
        // One tenant cannot take the table.
        if self.owned_by(&client.tenant) >= share {
            return Err(Refusal::Busy);
        }
        let id = self.next_subscription;
        self.next_subscription += 1;
        self.subscriptions
            .push(Subscription::new(id, client, filter, qos));
        let index = self.subscriptions.len() - 1;
        let replay: Vec<Event> = self
            .retained
            .iter()
            .filter(|held| self.subscriptions[index].filter.matches(&held.event.topic))
            .map(|held| held.event.clone())
            .collect();
        for mut event in replay {
            event.retained = true;
            self.deliver(index, event);
        }
        Ok(id)
    }

    /// Undo [`Broker::subscribe`] when its reply never reached `owner` (the
    /// caller timed out or is gone, or the request was one way): nobody holds
    /// the id, so the entry would leak. Returns the doorbell to close.
    pub fn settle_created(
        &mut self,
        created: Option<u64>,
        client: Client,
        learned: bool,
    ) -> Option<B> {
        let id = created.filter(|_| !learned)?;
        self.remove(id, client).ok().flatten()
    }

    /// Fan one publish out to every matching subscription, applying each
    /// subscription's QoS policy and counting drops. Returns the match count,
    /// which the publish reply reports. A retained publish that does not fit
    /// its tenant's budget is refused before anything changes.
    pub fn publish(
        &mut self,
        topic: &str,
        publisher: Client,
        payload: Vec<u8>,
        retained: bool,
    ) -> Result<u64, Refusal> {
        let mut event = Event {
            topic: String::from(topic),
            publisher: publisher.slot,
            sequence: 0,
            retained,
            payload,
        };
        if retained {
            self.check_retained(&event, publisher.tenant)?;
        }
        event.sequence = self.next_sequence;
        self.next_sequence += 1;
        if retained {
            self.store_retained(&event, publisher.tenant);
        }
        let mut matched = 0;
        for index in 0..self.subscriptions.len() {
            if self.subscriptions[index].filter.matches(topic) {
                matched += 1;
                self.subscriptions[index].matched += 1;
                self.deliver(index, event.clone());
            }
        }
        self.touch_topic(topic, retained, publisher.tenant.is_platform());
        Ok(matched)
    }

    /// Answer every parked pull that now has a deliverable event. Events are
    /// peeked, not popped: the caller commits them once a reply reaches the
    /// subscriber (see [`Broker::commit`]). `encode` turns the head event
    /// into the reply; `None` means it cannot be encoded.
    pub fn satisfy<T>(&mut self, encode: impl Fn(&Event) -> Option<T>) -> Vec<Wake<T>> {
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
            let Some(event) = self.subscriptions[sub_index].peek() else {
                index += 1;
                continue;
            };
            let sequence = event.sequence;
            let encoded = encode(event);
            self.pending.remove(index);
            match encoded {
                Some(parcel) => wakes.push(Wake {
                    txn: pending.txn,
                    parcel,
                    subscription: pending.subscription,
                    sequence,
                }),
                // The event can't be encoded into a reply (e.g. too large for
                // the buffer): drop it so this subscription doesn't stall on
                // the same unencodable head forever. This parked pull gets no
                // reply from this round; the caller's own deadline (or its
                // next poll) covers it.
                None => self.drop_undeliverable(pending.subscription, sequence),
            }
        }
        wakes
    }

    /// Where `client`'s subscription `id` is, or why it is not theirs: the
    /// slot and the tenant must both be the creator's, so a task that reuses
    /// an exited one's slot as another tenant cannot touch its subscriptions.
    pub fn index_of(&self, id: u64, client: Client) -> Result<usize, Refusal> {
        let index = self
            .subscriptions
            .iter()
            .position(|sub| sub.id == id)
            .ok_or(Refusal::NotFound)?;
        if self.subscriptions[index].client() != client {
            return Err(Refusal::NotOwner);
        }
        Ok(index)
    }

    /// `NextEvent` bookkeeping: report the head event of `client`'s
    /// subscription `id`, or (nothing queued) note the owner drained it so the
    /// next event rings its bell, and park transaction `txn` unless the call
    /// is a `poll` (its deadline already passed: the kernel ends it when the
    /// turn returns, so parking would leave a dead transaction in the table).
    /// One parked pull per (subscription, owner slot): a retry replaces its
    /// own stale entry.
    pub fn pull(
        &mut self,
        id: u64,
        client: Client,
        poll: bool,
        txn: Option<u64>,
    ) -> Result<Pulled, Refusal> {
        let owner = client.slot;
        let index = self.index_of(id, client)?;
        if self.subscriptions[index].peek().is_some() {
            return Ok(Pulled::Head(index));
        }
        self.subscriptions[index].rung = false;
        if poll {
            return Ok(Pulled::Empty);
        }
        let txn = txn.ok_or(Refusal::Invalid)?;
        self.pending
            .retain(|p| !(p.subscription == id && p.owner == owner));
        if self.pending.len() >= MAX_PENDING {
            return Err(Refusal::Busy);
        }
        self.pending.push(Pending {
            subscription: id,
            owner,
            txn,
        });
        Ok(Pulled::Parked)
    }

    /// Drop the head event of `id`'s queue unconditionally, including for
    /// `Reliable` (which [`Broker::commit`] otherwise never pops without an
    /// explicit `ack`), because it could not be encoded into a reply and
    /// would otherwise stall the subscription on the same event forever.
    /// Counts as a QoS drop.
    pub fn drop_undeliverable(&mut self, id: u64, sequence: u64) {
        let Some(index) = self.subscriptions.iter().position(|sub| sub.id == id) else {
            return;
        };
        self.change(index, |sub| {
            sub.drops += 1;
            sub.retire(sequence);
        });
    }

    /// Retire the event a successful reply carried. The head is checked by
    /// sequence, so a late commit cannot pop a newer event (`reliable`
    /// subscriptions do not pop at all; their events retire on [`Broker::ack`]).
    /// `delivered` is counted here rather than where the reply is built,
    /// since only a reply that actually reached the subscriber (a `commit`)
    /// is a real delivery; counting earlier risked a double count when the
    /// reply failed and the same still-queued event was delivered again.
    pub fn commit(&mut self, id: u64, sequence: u64) {
        let Some(index) = self.subscriptions.iter().position(|sub| sub.id == id) else {
            return;
        };
        self.change(index, |sub| {
            sub.delivered += 1;
            if sub.qos != Qos::Reliable {
                sub.retire(sequence);
            }
        });
    }

    /// A `reliable` subscriber acknowledged everything up to `sequence`.
    pub fn ack(&mut self, id: u64, client: Client, sequence: u64) -> Result<(), Refusal> {
        let index = self.index_of(id, client)?;
        self.change(index, |sub| sub.ack(sequence));
        Ok(())
    }

    /// Drop a subscription, scoped to its owner. Returns its doorbell, which
    /// the caller closes.
    pub fn remove(&mut self, id: u64, client: Client) -> Result<Option<B>, Refusal> {
        let index = self.index_of(id, client)?;
        let sub = self.subscriptions.remove(index);
        self.rebook(sub.tenant, sub.bytes, 0);
        // Parked pulls for it can never be satisfied; the clients discover
        // that on their own deadline, so just drop the bookkeeping.
        self.pending
            .retain(|pending| !(pending.subscription == id && pending.owner == client.slot));
        Ok(sub.bell)
    }

    /// Remove subscription `id` whose owner's doorbell end is gone: the
    /// owner dropped it (or died), nobody can pull or unsubscribe it again,
    /// and a leaked entry would count against its owner's limits. Returns the
    /// broker's doorbell end to close.
    pub fn reap(&mut self, id: u64) -> Option<B> {
        let client = self.subscriptions.iter().find(|sub| sub.id == id)?.client();
        self.remove(id, client).ok().flatten()
    }

    /// Install `bell` as subscription `id`'s doorbell (owner only) and return
    /// the one it replaces, for the caller to close. The bell is unrung, so
    /// queued events ring it.
    pub fn set_bell(&mut self, id: u64, client: Client, bell: B) -> Result<Option<B>, Refusal> {
        let index = self.index_of(id, client)?;
        let sub = &mut self.subscriptions[index];
        sub.rung = false;
        Ok(sub.bell.replace(bell))
    }

    /// The bells to ring now: every unrung bell whose subscription has an
    /// event queued (parked pulls were answered first, so nobody is pulling
    /// it). Each is marked rung until its owner drains the queue.
    pub fn bells_due(&mut self) -> Vec<(u64, B)> {
        let mut due = Vec::new();
        for sub in self.subscriptions.iter_mut() {
            if let Some(bell) = sub.bell.filter(|_| !sub.rung && sub.queued() > 0) {
                sub.rung = true;
                due.push((sub.id, bell));
            }
        }
        due
    }

    /// Forget subscription `id`'s bell (its owner's end closed); the handle
    /// for the caller to close.
    pub fn drop_bell(&mut self, id: u64) -> Option<B> {
        self.subscriptions
            .iter_mut()
            .find(|sub| sub.id == id)
            .and_then(|sub| sub.bell.take())
    }

    /// Track a topic for [`Broker::list`]; an app's new topic may not take
    /// the rows kept for platform tasks.
    fn touch_topic(&mut self, topic: &str, retained: bool, platform: bool) {
        let rows = if platform {
            MAX_TOPICS
        } else {
            MAX_TOPICS - SYSTEM_TOPIC_RESERVE
        };
        if let Some(row) = self.topics.iter_mut().find(|row| row.topic == topic) {
            row.retained = row.retained || retained;
        } else if self.topics.len() < rows {
            self.topics.push(TopicRow {
                topic: String::from(topic),
                retained,
            });
        }
    }

    /// Snapshot the topic table with live subscriber counts.
    pub fn list(&self) -> Vec<TopicInfo> {
        self.topics
            .iter()
            .map(|row| TopicInfo {
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
