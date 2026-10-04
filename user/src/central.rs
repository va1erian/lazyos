//! Central-broker topics client for the platform services (issue #169).
//!
//! `user/src/messenger/`'s `router` module is the interim per-service broker
//! the S2 services embedded: every service served its own `router::INTERFACE`
//! endpoint and kept its own subscription table, so `messengerd`'s central
//! broker reported zero topics and a fabric view could not tell a service's
//! publishers from its subscribers. This module is the move to the central
//! broker `docs/messenger.md` section 7 specifies:
//!
//! * publishes wrap the raw bytes the router carried in one small parcel and
//!   send it to `messengerd` through the [`topics_client`] wire protocol, so
//!   the kernel policy gate, retained values, fanout and drop accounting all
//!   live in one broker;
//! * subscriptions are broker-side ids; [`Subscription::recv_with`] polls or
//!   parks with a caller-owned buffer, so a service loop reuses one buffer and
//!   never grows its bump heap;
//! * events are handed back in the interim [`router::Event`] shape (topic,
//!   raw payload bytes, retained flag, sequence), so the services and their
//!   consumers that already speak that shape change one call site, not their
//!   payload codecs.
//!
//! `init` and `healthd` still serve their local brokers (`init`'s service
//! events and `healthd`'s retained health rows predate the move, and their
//! consumers connect to those names); every topic published through this
//! module is visible to `messengerctl topics` and the fabric viewers.

use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Kind, Parcel};
use messenger_generated::topics;

use crate::messenger::{
    create_pair, errno, router, topics_client, Endpoint, Error, Result, DEFAULT_BUFFER,
    EXPIRED_DEADLINE,
};

/// TLV field id of the wrapper's text payload.
const FIELD_TEXT: u16 = 1;
/// TLV field id of the wrapper's binary payload.
const FIELD_BYTES: u16 = 2;
/// The wrapper parcel's interface id: an opaque marker (the broker stores
/// payloads verbatim), never delivered to a subscriber.
const WRAPPER_INTERFACE: u64 = u64::from_le_bytes(*b"os.cntrl");

/// A connection to `messengerd`'s central broker.
pub struct Bus {
    endpoint: Endpoint,
    /// Reused call buffer: the user bump allocator never reclaims per-call
    /// buffers, so one publish/subscribe/list reply buffer lives as long as
    /// the bus.
    scratch: Vec<u8>,
}

impl Bus {
    /// Resolve [`topics_client::NAME`] and wrap the broker endpoint. Retries
    /// briefly while `messengerd` is still registering its name at boot.
    pub fn connect() -> Result<Bus> {
        let client = topics_client::Client::connect()?;
        Ok(Bus {
            endpoint: client.endpoint(),
            scratch: alloc::vec![0u8; DEFAULT_BUFFER],
        })
    }

    /// [`Bus::connect`] with an outer retry loop that parks one tick between
    /// attempts, for callers that start before the broker's name lands.
    pub fn connect_retry(attempts: usize) -> Result<Bus> {
        let mut last = Error::Errno(-errno::ENOENT);
        for _ in 0..attempts {
            match Bus::connect() {
                Ok(bus) => return Ok(bus),
                Err(error) => last = error,
            }
            park_tick();
        }
        Err(last)
    }

    /// Wrap an already-resolved broker endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Bus {
        Bus {
            endpoint,
            scratch: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// The underlying broker endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Publish raw payload bytes on `topic`; returns how many subscriptions
    /// matched. `retained` keeps the value for later subscribers.
    pub fn publish(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        let wrapped = wrap(payload)?;
        let request = topics_client::publish_request(topic, &wrapped, retained)?;
        let reply = self.endpoint.call_with(&request, &mut self.scratch, None)?;
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
        self.subscribe_with_deadline(filter, qos, None)
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
        })
    }

    /// List the topics the broker has seen, with live subscriber counts.
    pub fn list(&mut self) -> Result<Vec<topics_client::TopicInfo>> {
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
        let qos = topics_client::Qos::from_parts(qos, 1).unwrap_or(topics_client::Qos::Latest);
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
}

impl Subscription {
    /// The broker-side subscription id.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Wait for the next event into `buf`; `Ok(None)` means the deadline
    /// passed first (pass [`EXPIRED_DEADLINE`] for a non-blocking poll).
    pub fn recv_with(
        &self,
        buf: &mut [u8],
        deadline: Option<u64>,
    ) -> Result<Option<router::Event>> {
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
        let request = topics_client::bell_request(self.id, theirs.handle())?;
        let mut scratch = [0u8; 512];
        let installed = self
            .endpoint
            .call_with(&request, &mut scratch, None)
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

/// Wrap raw payload bytes in the one-field parcel the broker stores.
fn wrap(payload: &[u8]) -> Result<Vec<u8>> {
    let mut body = Encoder::new();
    match core::str::from_utf8(payload) {
        Ok(text) => body.string(FIELD_TEXT, text).map_err(Error::Parcel)?,
        Err(_) => body.bytes(FIELD_BYTES, payload).map_err(Error::Parcel)?,
    }
    let parcel = Parcel {
        header: libmessenger::Header {
            version: libmessenger::VERSION,
            flags: 0,
            interface_id: WRAPPER_INTERFACE,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}

/// Decode the wrapper and return the interim router shape. Only a parcel
/// stamped with [`WRAPPER_INTERFACE`] is treated as this module's wrapper; a
/// publisher outside this module (e.g. `messengerctl`'s self-test parcels)
/// may carry an unrelated field with the same id (`FIELD_TEXT` collides with
/// arbitrary interface field 1), so its payload is handed through unchanged
/// rather than misread as wrapper text.
fn unwrap_event(event: topics_client::Event) -> Result<router::Event> {
    let parcel = Parcel::decode(&event.payload).map_err(Error::Parcel)?;
    if parcel.header.interface_id == WRAPPER_INTERFACE {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            match (field.kind, field.id) {
                (Kind::String, FIELD_TEXT) => {
                    return Ok(router::Event {
                        topic: event.topic,
                        payload: field.as_str().map_err(Error::Parcel)?.as_bytes().to_vec(),
                        retained: event.retained,
                        seq: event.sequence,
                    });
                }
                (Kind::Bytes, FIELD_BYTES) => {
                    return Ok(router::Event {
                        topic: event.topic,
                        payload: field.as_bytes().to_vec(),
                        retained: event.retained,
                        seq: event.sequence,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(router::Event {
        topic: event.topic,
        payload: event.payload,
        retained: event.retained,
        seq: event.sequence,
    })
}

/// The positive errno of a structured broker error reply, if any.
fn error_code(parcel: &Parcel) -> Option<i64> {
    topics_client::error_field(parcel).ok().flatten()
}

/// Nap one PIT tick's worth between retries ([`crate::sys::nap`], a real sleep;
/// this used to park on a throwaway channel pair, since userspace had no
/// sleep call).
fn park_tick() {
    crate::sys::nap();
}
