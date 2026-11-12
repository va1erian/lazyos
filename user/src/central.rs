//! The platform services' topics client: the one way a native program
//! publishes and subscribes (issue #169, `docs/client-api-plan.md` 3.5).
//!
//! Everything goes through `messengerd`'s central broker, so the kernel
//! policy gate, retained values, fanout and drop accounting live in one
//! place and `messengerctl topics` and the fabric viewers see every topic:
//!
//! * publishes wrap the raw bytes in one small parcel and send it through the
//!   [`topics_client`] wire protocol ([`Bus`] for a connection of one's own,
//!   [`Publisher`] for a supervisor that must never wait on the broker);
//! * subscriptions are broker-side ids; [`Subscription::recv_with`] polls or
//!   parks with a caller-owned buffer, so a service loop reuses one buffer and
//!   never grows its bump heap, and [`Subscription::bell`] is the doorbell to
//!   park on instead of a timer;
//! * a subscription implements the generated `topics::Events`, so
//!   `subscribe_<topic>` returns a typed stream whose `next` decodes the
//!   declared payload (no consumer decodes one by hand).

use alloc::string::String;
use alloc::vec::Vec;

mod feed;
mod publisher;

pub use feed::Feed;
pub use publisher::Publisher;

use libmessenger::{envelope, Parcel};
use messenger_generated::topics;

use crate::messenger::{
    create_pair, errno, registry, topics_client, Endpoint, Error, Result, DEFAULT_BUFFER,
    EXPIRED_DEADLINE,
};

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

/// A connection to `messengerd`'s central broker.
pub struct Bus {
    endpoint: Endpoint,
    /// Reused call buffer: the user bump allocator never reclaims per-call
    /// buffers, so one publish/subscribe/list reply buffer lives as long as
    /// the bus.
    scratch: Vec<u8>,
    /// Ticks a subscribe may take; no bound waits as long as the broker does.
    bound: Option<u64>,
}

impl Bus {
    /// Resolve [`topics_client::NAME`] and wrap the broker endpoint, waiting the
    /// runtime's connect bound while `messengerd` registers its name at boot.
    pub fn connect() -> Result<Bus> {
        let client = topics_client::Client::connect()?;
        Ok(Bus {
            endpoint: client.endpoint(),
            scratch: Vec::new(),
            bound: None,
        })
    }

    /// One registry lookup of the broker, never a wait: for a caller that
    /// tries again later (a service that may start before `messengerd`).
    pub fn lookup() -> Result<Bus> {
        Ok(Bus::from_endpoint(registry::resolve(topics_client::NAME)?))
    }

    /// Wrap an already-resolved broker endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Bus {
        Bus {
            endpoint,
            scratch: Vec::new(),
            bound: None,
        }
    }

    /// The reply buffer, made on the first call that needs it so that a bus
    /// that only ever fails to connect costs nothing.
    fn reserve(&mut self) {
        if self.scratch.is_empty() {
            self.scratch = alloc::vec![0u8; DEFAULT_BUFFER];
        }
    }

    /// Point this bus at a (restarted) broker, keeping its buffer.
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        self.endpoint = endpoint;
    }

    /// This bus with every subscribe (and the bell of what it subscribes)
    /// bounded to `ticks`, for a supervisor that must not wait on a broker
    /// that stopped answering.
    pub fn bounded(mut self, ticks: u64) -> Bus {
        self.bound = Some(ticks);
        self
    }

    /// The underlying broker endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Publish raw payload bytes on `topic`; returns how many subscriptions
    /// matched. `retained` keeps the value for later subscribers.
    pub fn publish(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.publish_by(topic, payload, retained, None)
    }

    /// [`Bus::publish`] that gives up at `deadline` (an absolute tick, `None`
    /// = wait forever) with `-ETIMEDOUT`, for a publisher such as `init` that
    /// must never stall on a broker that stopped answering.
    pub fn publish_by(
        &mut self,
        topic: &str,
        payload: &[u8],
        retained: bool,
        deadline: Option<u64>,
    ) -> Result<u64> {
        let wrapped = wrap(payload)?;
        let request = topics_client::publish_request(topic, &wrapped, retained)?;
        self.reserve();
        let reply = self
            .endpoint
            .call_with(&request, &mut self.scratch, deadline)?;
        if let Some(code) = error_code(&reply) {
            return Err(Error::Topics(code));
        }
        topics_client::decode_matched(&reply)
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
        let deadline = self.bound.map(|ticks| crate::sys::clock() + ticks);
        self.subscribe_with_deadline(filter, qos, deadline)
    }

    /// [`Bus::subscribe_with_qos`] that gives up at `deadline` (an absolute
    /// tick, `None` = wait forever) with `-ETIMEDOUT`, for a caller such as a
    /// compositor that must not stall on a broker that stopped answering.
    pub fn subscribe_with_deadline(
        &mut self,
        filter: &str,
        qos: topics_client::Qos,
        deadline: Option<u64>,
    ) -> Result<Subscription> {
        let request = topics_client::subscribe_request(filter, qos)?;
        self.reserve();
        let reply = self
            .endpoint
            .call_with(&request, &mut self.scratch, deadline)?;
        if let Some(code) = error_code(&reply) {
            return Err(Error::Topics(code));
        }
        let id = topics_client::decode_subscription(&reply)?;
        // Encoded once here and reused on every poll: `Endpoint::call_with`
        // would otherwise re-encode these fixed requests on every single
        // `recv_with`/`stats_with` call.
        let request = encode_request(topics_client::subscription_request(
            topics_client::method::NEXT_EVENT,
            id,
        )?)?;
        let stats_request = encode_request(topics_client::subscription_request(
            topics_client::method::STATS,
            id,
        )?)?;
        Ok(Subscription {
            endpoint: self.endpoint,
            id,
            request,
            stats_request,
            bell: None,
            scratch: Vec::new(),
            bound: self.bound,
        })
    }

    /// List the topics the broker has seen, with live subscriber counts.
    pub fn list(&mut self) -> Result<Vec<topics_client::TopicInfo>> {
        self.reserve();
        let request = topics_client::list_request();
        let reply = self.endpoint.call_with(&request, &mut self.scratch, None)?;
        if let Some(code) = error_code(&reply) {
            return Err(Error::Topics(code));
        }
        topics_client::decode_topics(&reply)
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
    endpoint: Endpoint,
    id: u64,
    /// Pre-encoded `NextEvent` request, reused on every poll.
    request: Vec<u8>,
    /// Pre-encoded `Stats` request, reused on every [`Subscription::stats_with`] call.
    stats_request: Vec<u8>,
    /// Our end of the doorbell, once [`Subscription::bell`] asked for one.
    bell: Option<Endpoint>,
    /// The reply buffer of [`topics::Events::next_event`], made on first use
    /// and kept (the user bump allocator never reclaims per-call buffers).
    scratch: Vec<u8>,
    /// Ticks the bell request may take (the bus's bound).
    bound: Option<u64>,
}

/// The typed stream's pull: one non-waiting `NextEvent`, which also re-arms
/// the doorbell when it finds the queue empty.
impl topics::Events for Subscription {
    type Error = Error;

    fn next_event(&mut self) -> Result<Option<topics::RawEvent>> {
        if self.scratch.is_empty() {
            self.scratch = alloc::vec![0u8; DEFAULT_BUFFER];
        }
        let mut buf = core::mem::take(&mut self.scratch);
        let event = self.recv_with(&mut buf, Some(EXPIRED_DEADLINE));
        self.scratch = buf;
        Ok(event?.map(|event| topics::RawEvent {
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

    /// Wait for the next event into `buf`; `Ok(None)` means the deadline
    /// passed first (pass [`EXPIRED_DEADLINE`] for a non-blocking poll).
    pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Option<Event>> {
        match self.endpoint.call_bytes_with(&self.request, buf, deadline) {
            Ok(reply) => {
                if let Some(code) = error_code(&reply) {
                    return Err(Error::Topics(code));
                }
                match topics_client::decode_event(&reply)? {
                    Some(event) => Ok(Some(unwrap_event(event)?)),
                    None => Ok(None),
                }
            }
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The subscription's doorbell (docs/performance-plan.md P7.2): the
    /// broker sends one message on the returned endpoint when events are
    /// waiting. Park on it beside your own endpoints (`wait::wait_any`); on
    /// each wake call [`Subscription::take_ring`], then drain
    /// [`Subscription::recv_with`] with [`EXPIRED_DEADLINE`] until it returns
    /// `None`, which re-arms the bell. Asked once; later calls return the
    /// same end.
    pub fn bell(&mut self) -> Result<Endpoint> {
        if let Some(bell) = self.bell {
            return Ok(bell);
        }
        let (mine, theirs) = create_pair()?;
        let deadline = self.bound.map(|ticks| crate::sys::clock() + ticks);
        let request = topics_client::bell_request(self.id, theirs.handle())?;
        let mut scratch = [0u8; 512];
        let installed = self
            .endpoint
            .call_with(&request, &mut scratch, deadline)
            .and_then(|reply| match error_code(&reply) {
                Some(code) => Err(Error::Topics(code)),
                None => Ok(()),
            });
        if let Err(error) = installed {
            // The transfer may not have happened: drop both ends (closing a
            // handle that already moved is a harmless `EBADF`).
            let _ = theirs.close();
            let _ = mine.close();
            return Err(error);
        }
        self.bell = Some(mine);
        Ok(mine)
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
                self.take_ring(&mut [0u8; 512]);
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
        let mut buf = [0u8; 512];
        !matches!(
            bell.recv_with(&mut buf, Some(EXPIRED_DEADLINE)),
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT
        )
    }

    /// Consume the `Ready` the doorbell holds (the wait said one is there).
    pub fn take_ring(&self, buf: &mut [u8]) {
        if let Some(bell) = self.bell {
            let _ = bell.recv_with(buf, Some(EXPIRED_DEADLINE));
        }
    }

    /// Per-subscription delivery counters, including QoS overflow drops.
    /// Takes a caller-owned reply buffer: a long-lived poll loop must reuse
    /// one here too, or the user bump allocator grows to fit both this and
    /// the (also fixed) request, which is why that is pre-encoded and reused
    /// as well.
    pub fn stats_with(&self, buf: &mut [u8]) -> Result<topics_client::SubscriptionStats> {
        let reply = self
            .endpoint
            .call_bytes_with(&self.stats_request, buf, None)?;
        if let Some(code) = error_code(&reply) {
            return Err(Error::Topics(code));
        }
        topics_client::decode_stats(&reply)
    }

    /// Drop this subscription; later publishes stop matching it.
    pub fn unsubscribe(self) -> Result<()> {
        if let Some(bell) = self.bell {
            let _ = bell.close();
        }
        let request =
            topics_client::subscription_request(topics_client::method::UNSUBSCRIBE, self.id)?;
        let mut scratch = alloc::vec![0u8; DEFAULT_BUFFER];
        let reply = self.endpoint.call_with(&request, &mut scratch, None)?;
        if let Some(code) = error_code(&reply) {
            return Err(Error::Topics(code));
        }
        Ok(())
    }
}

/// Build and encode a broker request parcel once, for a caller that will
/// reuse the bytes on every subsequent call instead of re-encoding a fixed
/// request each time.
fn encode_request(request: Parcel) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    request.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}

/// Wrap raw payload bytes in the broker's envelope (`libmessenger::envelope`).
fn wrap(payload: &[u8]) -> Result<Vec<u8>> {
    envelope::wrap(payload).map_err(Error::Parcel)
}

/// A broker event as [`Event`], its payload taken out of the
/// envelope. A publisher outside the envelope (e.g. `messengerctl`'s
/// self-test parcels) is handed through unchanged ([`envelope::unwrap`]).
fn unwrap_event(event: topics_client::Event) -> Result<Event> {
    Ok(Event {
        payload: envelope::unwrap(&event.payload),
        topic: event.topic,
        retained: event.retained,
        seq: event.sequence,
        publisher: event.publisher,
    })
}

/// The positive errno of a structured broker error reply, if any.
fn error_code(parcel: &Parcel) -> Option<i64> {
    topics_client::error_field(parcel).ok().flatten()
}
