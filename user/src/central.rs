//! The platform services' topics client: the one way a native program
//! publishes and subscribes (issue #169, `docs/client-api-plan.md` 3.5).
//!
//! Everything goes through `messengerd`'s central broker, so the kernel
//! policy gate, retained values, fanout and drop accounting live in one
//! place and `messengerctl topics` and the fabric viewers see every topic:
//!
//! * publishes wrap the raw bytes in one small parcel and send it through the
//!   generated [`topics_client`] client ([`Bus`] for a connection of one's own,
//!   [`Publisher`] for a supervisor that must never wait on the broker);
//! * subscriptions are broker-side ids; [`Subscription::recv`] polls or parks,
//!   and [`Subscription::bell`] is the doorbell to park on instead of a timer;
//! * a subscription implements the generated `topics::Events`, so
//!   `subscribe_<topic>` returns a typed stream whose `next` decodes the
//!   declared payload (no consumer decodes one by hand).

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

mod feed;
mod publisher;

pub use feed::Feed;
pub use publisher::Publisher;

use libmessenger::client::Deadline;
use libmessenger::envelope;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;
use messenger_generated::topics;

use crate::messenger::topics_client::{self, lift};
use crate::messenger::{create_pair, errno, registry, Endpoint, Error, Result, EXPIRED_DEADLINE};

/// One delivered event: its topic, the publisher's payload with the bus
/// envelope taken off, whether it replays a retained value, the broker's
/// sequence number and the publisher's kernel-stamped task slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Topic the message was published on.
    pub topic: String,
    /// The publisher's own payload bytes.
    pub payload: Vec<u8>,
    /// Whether the event is a retained replay.
    pub retained: bool,
    /// Broker publish sequence.
    pub seq: u64,
    /// The publisher's task slot (kernel-stamped; slots are reused, so act
    /// on it while it is fresh).
    pub publisher: u64,
}

/// Most doorbell messages one [`Subscription::rung`] consumes.
const MAX_RINGS: usize = 16;

/// What a bell message needs of a buffer: one small one-way parcel.
const BELL_BUFFER: usize = 512;

/// A connection to `messengerd`'s central broker.
pub struct Bus {
    /// Shared with the subscriptions it makes, so a subscription outlives
    /// the bus and the broker handle is released with the last of them.
    client: Rc<topics_client::Client>,
    /// What a subscribe (and the bell of what it subscribes) may take; none
    /// uses the IDL's deadline.
    bound: Option<Deadline>,
}

impl Bus {
    /// Resolve the broker, waiting a moment (80 ms) while `messengerd`
    /// registers its name at boot; a daemon loop tries again on its next pass.
    pub fn connect() -> Result<Bus> {
        Ok(Bus::new(topics_client::connect()?))
    }

    /// [`Bus::connect`] for a one-shot program that needs the broker and
    /// never tries again: waits the runtime's connect bound (10 s).
    pub fn connect_waiting() -> Result<Bus> {
        Ok(Bus::new(topics_client::connect_waiting()?))
    }

    /// One registry lookup of the broker, never a wait: for a caller that
    /// tries again later (a service that may start before `messengerd`).
    pub fn lookup() -> Result<Bus> {
        Ok(Bus::from_endpoint(registry::resolve(topics_client::NAME)?))
    }

    /// Wrap an already-resolved broker endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Bus {
        Bus::new(topics_client::client_of(endpoint))
    }

    fn new(client: topics_client::Client) -> Bus {
        Bus {
            client: Rc::new(client),
            bound: None,
        }
    }

    /// This bus with every subscribe (and the bell of what it subscribes)
    /// bounded to `ticks`, for a supervisor that must not wait on a broker
    /// that stopped answering.
    pub fn bounded(mut self, ticks: u64) -> Bus {
        self.bound = Some(Deadline::ticks(ticks));
        self
    }

    /// The generated client, for a caller that speaks the raw interface
    /// (`messengerctl`'s hostile-input checks).
    pub fn client(&self) -> &topics_client::Client {
        &self.client
    }

    /// Publish raw payload bytes on `topic`; returns how many subscriptions
    /// matched. `retained` keeps the value for later subscribers.
    pub fn publish(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.publish_within(topic, payload, retained, wire::METHOD_PUBLISH_DEADLINE)
    }

    /// [`Bus::publish`] that gives up after `within` with `-ETIMEDOUT`, for a
    /// publisher such as `init` that must never stall on a broker that
    /// stopped answering.
    pub fn publish_within(
        &mut self,
        topic: &str,
        payload: &[u8],
        retained: bool,
        within: Deadline,
    ) -> Result<u64> {
        let wrapped = envelope::wrap(payload).map_err(Error::Parcel)?;
        self.client
            .publish_within(topic, &wrapped, retained, within)
            .map_err(lift)
    }

    /// Subscribe to `filter` with `latest` QoS; retained matching values are
    /// replayed by the broker.
    pub fn subscribe(&mut self, filter: &str) -> Result<Subscription> {
        self.subscribe_with_qos(filter, topics_client::Qos::Latest)
    }

    /// Subscribe to `filter` with an explicit QoS. Callers that cannot afford
    /// to lose events between polls (e.g. an audit feed) should use
    /// [`topics_client::Qos::Buffered`] or [`topics_client::Qos::Reliable`]
    /// instead of the default `latest` (one slot, overwritten on overflow).
    pub fn subscribe_with_qos(
        &mut self,
        filter: &str,
        qos: topics_client::Qos,
    ) -> Result<Subscription> {
        let within = self.bound.unwrap_or(wire::METHOD_SUBSCRIBE_DEADLINE);
        self.subscribe_within(filter, qos, within)
    }

    /// [`Bus::subscribe_with_qos`] that gives up after `within` with
    /// `-ETIMEDOUT`, for a caller such as a compositor that must not stall on
    /// a broker that stopped answering.
    pub fn subscribe_within(
        &mut self,
        filter: &str,
        qos: topics_client::Qos,
        within: Deadline,
    ) -> Result<Subscription> {
        let id = self
            .client
            .subscribe_within(filter, qos.code(), qos.depth(), within)
            .map_err(lift)?;
        Ok(Subscription {
            client: Rc::clone(&self.client),
            id,
            bell: None,
            bound: self.bound,
            wait_ticks: 0,
        })
    }

    /// List the topics the broker has seen, with live subscriber counts.
    pub fn list(&mut self) -> Result<Vec<topics_client::TopicInfo>> {
        self.client.list_topics().map_err(lift)
    }
}

impl topics::Publish for Bus {
    type Error = Error;

    /// Publish through the broker; this is exactly [`Bus::publish`], exposed
    /// as the transport the generated `publish_*` helpers call (issue #307).
    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.publish(topic, payload, retained)
    }
}

impl topics::Subscribe for Bus {
    type Error = Error;
    type Subscription = Subscription;

    /// Subscribe with the `qos` code from a generated topic declaration; an
    /// unknown code falls back to `latest`.
    fn subscribe_topic(&mut self, filter: &str, qos: u32) -> Result<Subscription> {
        let qos = topics_client::Qos::from_parts(qos, topics_client::Qos::MAX_DEPTH)
            .unwrap_or(topics_client::Qos::Latest);
        self.subscribe_with_qos(filter, qos)
    }
}

/// A live central-broker subscription.
pub struct Subscription {
    client: Rc<topics_client::Client>,
    id: u64,
    /// Our end of the doorbell, once [`Subscription::bell`] asked for one.
    bell: Option<Endpoint>,
    /// What the bell request may take (the bus's bound).
    bound: Option<Deadline>,
    /// Ticks an [`topics::Events::next_event`] pull waits for an event; `0`
    /// is the poll (an expired deadline: the broker answers only if it is
    /// parked at that instant, so a busy broker can make a poll miss a queued
    /// event).
    wait_ticks: u64,
}

/// The typed stream's pull: one non-waiting `NextEvent`, which also re-arms
/// the doorbell when it finds the queue empty.
impl topics::Events for Subscription {
    type Error = Error;

    fn next_event(&mut self) -> Result<Option<topics::RawEvent>> {
        let within = match self.wait_ticks {
            0 => Deadline::Poll,
            ticks => Deadline::ticks(ticks),
        };
        Ok(self.recv(within)?.map(|event| topics::RawEvent {
            topic: event.topic,
            retained: event.retained,
            publisher: event.publisher,
            payload: event.payload,
        }))
    }
}

impl Subscription {
    /// The broker-side subscription id.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Wait for the next event; `Ok(None)` means `within` passed first
    /// ([`Deadline::Poll`] is a non-blocking poll).
    ///
    /// With `reliable` QoS the broker redelivers the head until it is
    /// acked, so the same event can come back more than once; call
    /// [`Subscription::ack`] once the payload is safely processed.
    pub fn recv(&self, within: Deadline) -> Result<Option<Event>> {
        Ok(topics_client::next(&self.client, self.id, within)?.map(unwrap_event))
    }

    /// The subscription's doorbell (docs/performance-plan.md P7.2): the
    /// broker sends one message on the returned endpoint when events are
    /// waiting. Park on it beside your own endpoints (`wait::wait_any`); on
    /// each wake call [`Subscription::take_ring`], then drain
    /// [`Subscription::recv`] with [`Deadline::Poll`] until it returns
    /// `None`, which re-arms the bell. Asked once; later calls return the
    /// same end.
    pub fn bell(&mut self) -> Result<Endpoint> {
        if let Some(bell) = self.bell {
            return Ok(bell);
        }
        let (mine, theirs) = create_pair()?;
        let within = self.bound.unwrap_or(wire::METHOD_BELL_DEADLINE);
        if let Err(error) = self.client.bell_within(self.id, theirs.handle(), within) {
            // The transfer may not have happened: drop both ends (closing a
            // handle that already moved is a harmless `EBADF`).
            let _ = theirs.close();
            let _ = mine.close();
            return Err(lift(error));
        }
        self.bell = Some(mine);
        Ok(mine)
    }

    /// Let each pull of [`topics::Events::next_event`] wait up to `ticks` for
    /// an event (`0`: poll). See [`Feed::drain_waiting`].
    pub fn set_wait_ticks(&mut self, ticks: u64) {
        self.wait_ticks = ticks;
    }

    /// Park on the doorbell until an event is queued, or `deadline` (an
    /// absolute tick, `None` = forever): `true` when it rang, `false` at the
    /// deadline. For a one-shot program that wants the next typed event: read
    /// with `next` until it says nothing is queued (which arms the bell), then
    /// park, then read again.
    pub fn park(&mut self, deadline: Option<u64>) -> Result<bool> {
        let bell = self.bell()?;
        match crate::messenger::wait::wait_any(&[bell], 0, deadline) {
            Ok(_) => {
                self.take_ring();
                Ok(true)
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Whether the doorbell holds a message, consumed. A local queue, never a
    /// call to the broker: cheap at any rate. `false` before [`Subscription::bell`]
    /// asked for one. A failed bell counts as rung, so the next read finds out.
    pub fn rung(&self) -> bool {
        let Some(bell) = self.bell else {
            return false;
        };
        // The counters first: an expired-deadline `recv` on an empty queue
        // parks until the next tick.
        match lazyos_sys::msg::queued(bell.handle()) {
            Ok(0) => return false,
            Ok(_) => {}
            // A dead peer: the next read finds out.
            Err(_) => return true,
        }
        for _ in 0..MAX_RINGS {
            if !matches!(lazyos_sys::msg::queued(bell.handle()), Ok(queued) if queued > 0) {
                break;
            }
            self.take_ring();
        }
        true
    }

    /// Consume the `Ready` the doorbell holds (the wait said one is there).
    pub fn take_ring(&self) {
        if let Some(bell) = self.bell {
            let mut buf = [0u8; BELL_BUFFER];
            let _ = bell.recv_with(&mut buf, Some(EXPIRED_DEADLINE));
        }
    }

    /// Retire every event up to `sequence` (drives `reliable` queues).
    pub fn ack(&self, sequence: u64) -> Result<()> {
        self.client.ack(self.id, sequence).map_err(lift)
    }

    /// Per-subscription delivery counters, including QoS overflow drops.
    pub fn stats(&self) -> Result<topics_client::SubscriptionStats> {
        self.client.stats(self.id).map_err(lift)
    }

    /// Drop this subscription; later publishes stop matching it.
    pub fn unsubscribe(self) -> Result<()> {
        if let Some(bell) = self.bell {
            let _ = bell.close();
        }
        self.client.unsubscribe(self.id).map_err(lift)
    }
}

/// A broker event as [`Event`], its payload taken out of the envelope. A
/// publisher outside the envelope (e.g. `messengerctl`'s self-test bodies) is
/// handed through unchanged ([`envelope::unwrap`]).
fn unwrap_event(event: topics_client::Event) -> Event {
    Event {
        payload: envelope::unwrap(&event.payload),
        topic: event.topic,
        retained: event.retained,
        seq: event.sequence,
        publisher: event.publisher,
    }
}
