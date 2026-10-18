//! The interim userspace topic router (issue #93). See the module doc on
//! [`crate::messenger::router`] for why this exists ahead of the kernel/
//! `messengerd` broker.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use svcpolicy::{Delivery, SendFailure, Sink, Transport};

use super::{create_pair, errno, registry, Endpoint, Error, Message, Result};

mod client;
pub use client::{Bus, Subscriber};

/// Topic router interface id. The human interface is
/// `os.lazy.local.topics.v1`; this is its interim eight-byte ABI id (a
/// `midlc` hash replaces it when the idl compiler owns the surface).
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.topic");

/// Router methods.
pub mod method {
    /// Allocate a unique sink name for a would-be subscriber.
    pub const RESERVE: u32 = 1;
    /// Attach a registered sink endpoint with a topic filter.
    pub const SUBSCRIBE: u32 = 2;
    /// Detach a sink endpoint.
    pub const UNSUBSCRIBE: u32 = 3;
    /// Publish a message on a topic (optionally retained).
    pub const PUBLISH: u32 = 4;
    /// Broker -> subscriber event delivery (one-way).
    pub const EVENT: u32 = 5;
}

/// Router TLV field ids.
pub mod field {
    pub const FILTER: u16 = 1;
    pub const SINK: u16 = 2;
    pub const TOPIC: u16 = 3;
    pub const PAYLOAD: u16 = 4;
    pub const RETAINED: u16 = 5;
    pub const SEQ: u16 = 6;
}

/// Most recent retained messages a broker keeps (oldest dropped first).
pub const RETAIN_LIMIT: usize = 64;

/// A header for a topic-router parcel of `method`.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: 0,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// Wrap an encoded body in a topic-router parcel.
pub fn parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        objects: Vec::new(),
    }
}

/// Whether `topic` matches subscription `filter`: `+` matches exactly one
/// segment, `#` matches zero or more trailing segments.
pub fn matches(filter: &str, topic: &str) -> bool {
    let mut filter_segments = filter.split('/');
    let mut topic_segments = topic.split('/');
    loop {
        match (filter_segments.next(), topic_segments.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(expected), Some(actual)) if expected == actual => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// A message a broker retains and replays to new subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retained {
    /// Topic the message was published on.
    pub topic: String,
    /// Opaque publisher payload.
    pub payload: Vec<u8>,
    /// Publish sequence assigned by the broker.
    pub seq: u64,
}

/// One event delivered to a subscriber.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Topic the message was published on.
    pub topic: String,
    /// Opaque publisher payload.
    pub payload: Vec<u8>,
    /// Whether the event was retained by the broker.
    pub retained: bool,
    /// Broker publish sequence.
    pub seq: u64,
}

impl Event {
    /// Decode an `EVENT` parcel; other methods/interfaces are `EINVAL`.
    pub fn from_message(message: &Message) -> Result<Event> {
        if message.interface_id() != INTERFACE || message.method() != method::EVENT {
            return Err(Error::Errno(-errno::EINVAL));
        }
        Ok(Event {
            topic: string_field(&message.parcel, field::TOPIC)?,
            payload: bytes_field(&message.parcel, field::PAYLOAD),
            retained: u64_field(&message.parcel, field::RETAINED).unwrap_or(0) != 0,
            seq: u64_field(&message.parcel, field::SEQ).unwrap_or(0),
        })
    }
}

/// Most subscribers a broker serves: each holds a handle in the broker's table
/// (`init`'s has 256 slots, shared with everything else it does) and a name in
/// the kernel's 64-entry registry, so a flood of subscriptions must not be
/// able to take them all.
pub const MAX_SUBSCRIBERS: usize = 64;
/// Most subscribers one unprivileged caller (no `CAP_SETUID`) may hold: a
/// session can reach the broker, and 64 resolvable names would take every
/// slot. System services subscribe a handful each, within the global bound.
pub const MAX_PER_USER: usize = 8;

/// One attached subscriber: its filter and its sink (the registered name and
/// the one handle cached for it, `svcpolicy::Sink`).
struct Subscription {
    filter: String,
    sink: Sink<Endpoint>,
    /// The uid that subscribed (kernel-stamped), for [`MAX_PER_USER`], and
    /// whether it was a system service, exempt from it.
    owner: u32,
    system: bool,
}

/// The real handles behind one delivery: `svcpolicy::Sink` decides what to do
/// with them, this opens, uses and releases them (audit H12).
struct Wire<'a> {
    event: Option<&'a Parcel>,
}

impl Transport for Wire<'_> {
    type Handle = Endpoint;

    fn resolve(&mut self, name: &str) -> Option<Endpoint> {
        registry::resolve(name).ok()
    }

    fn send(&mut self, handle: Endpoint) -> core::result::Result<(), SendFailure> {
        let Some(event) = self.event else {
            return Err(SendFailure::Gone);
        };
        handle.send(event).map_err(|error| {
            if error.errno() == Some(-errno::EAGAIN) {
                SendFailure::Backpressure
            } else {
                SendFailure::Gone
            }
        })
    }

    fn release(&mut self, handle: Endpoint) {
        // The handle is this broker's alias of the sink's endpoint: releasing
        // closes the alias, never the side the subscriber serves on.
        let _ = handle.release();
    }
}

/// The broker half of the router: embed one in a service endpoint and call
/// [`TopicBroker::handle`] for every message on the router interface.
pub struct TopicBroker {
    prefix: &'static str,
    subscribers: Vec<Subscription>,
    retained: Vec<Retained>,
    next_sink: u64,
    next_seq: u64,
    /// Messages a publisher handed to the broker.
    pub published: u64,
    /// Events successfully pushed to subscribers.
    pub delivered: u64,
    /// Events refused because a subscriber was gone or its queue full.
    pub dropped: u64,
}

impl TopicBroker {
    /// A broker whose sink names are `<prefix>.<n>` (`prefix` must be a
    /// valid registry name segment; services use their own name).
    pub fn new(prefix: &'static str) -> TopicBroker {
        TopicBroker {
            prefix,
            subscribers: Vec::new(),
            retained: Vec::new(),
            next_sink: 0,
            next_seq: 0,
            published: 0,
            delivered: 0,
            dropped: 0,
        }
    }

    /// Serve one router request; the caller replies with the returned
    /// parcel when the message carried a transaction.
    pub fn handle(&mut self, message: &Message) -> Result<Parcel> {
        if message.interface_id() != INTERFACE {
            return Err(Error::Errno(-errno::EINVAL));
        }
        match message.method() {
            method::RESERVE => {
                self.next_sink += 1;
                let sink = format!("{}.{}", self.prefix, self.next_sink);
                let mut body = Encoder::new();
                body.string(field::SINK, &sink).map_err(Error::Parcel)?;
                Ok(parcel(method::RESERVE, body))
            }
            method::SUBSCRIBE => {
                let sink = string_field(&message.parcel, field::SINK)?;
                let filter = string_field(&message.parcel, field::FILTER)?;
                let caller = message.caller();
                let system = caller.caps & crate::sys::CAP_SETUID != 0;
                self.attach(filter, sink, caller.uid, system)?;
                Ok(parcel(method::SUBSCRIBE, Encoder::new()))
            }
            method::UNSUBSCRIBE => {
                let sink = string_field(&message.parcel, field::SINK)?;
                self.detach(&sink);
                Ok(parcel(method::UNSUBSCRIBE, Encoder::new()))
            }
            method::PUBLISH => {
                let topic = string_field(&message.parcel, field::TOPIC)?;
                let payload = bytes_field(&message.parcel, field::PAYLOAD);
                let retained = u64_field(&message.parcel, field::RETAINED).unwrap_or(0) != 0;
                self.publish(&topic, &payload, retained);
                Ok(parcel(method::PUBLISH, Encoder::new()))
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// Publish `payload` on `topic`, fanning out to matching subscribers;
    /// `retained` also keeps it for subscribers that arrive later.
    /// Returns the broker sequence number.
    pub fn publish(&mut self, topic: &str, payload: &[u8], retained: bool) -> u64 {
        self.next_seq = self.next_seq.wrapping_add(1);
        let seq = self.next_seq;
        self.published += 1;
        if retained {
            let entry = Retained {
                topic: topic.to_string(),
                payload: payload.to_vec(),
                seq,
            };
            match self.retained.iter_mut().find(|entry| entry.topic == topic) {
                Some(existing) => *existing = entry,
                None => {
                    if self.retained.len() >= RETAIN_LIMIT {
                        self.retained.remove(0);
                    }
                    self.retained.push(entry);
                }
            }
        }
        for index in 0..self.subscribers.len() {
            if !matches(&self.subscribers[index].filter, topic) {
                continue;
            }
            let Ok(event) = event_parcel(topic, payload, retained, seq) else {
                self.dropped += 1;
                continue;
            };
            self.deliver(index, &event);
        }
        self.prune_dead();
        seq
    }

    /// The retained values, oldest first (introspection and tests).
    pub fn retained(&self) -> &[Retained] {
        &self.retained
    }

    /// Attached subscribers (introspection and tests).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Resolve `sink`, record the subscription, and replay the retained
    /// values `filter` already matches.
    fn attach(&mut self, filter: String, sink: String, owner: u32, system: bool) -> Result<()> {
        if self
            .subscribers
            .iter()
            .any(|subscriber| subscriber.sink.name() == sink)
        {
            return Ok(());
        }
        // A subscriber whose sink died is dead weight for the cap: its name is
        // gone, and every restart of a service subscribes under a new one.
        self.prune_dead();
        let held = self
            .subscribers
            .iter()
            .filter(|subscriber| !subscriber.system && subscriber.owner == owner)
            .count();
        if self.subscribers.len() >= MAX_SUBSCRIBERS || (!system && held >= MAX_PER_USER) {
            return Err(Error::Errno(-errno::ENOSPC));
        }
        let endpoint = registry::resolve(&sink)?;
        let index = self.subscribers.len();
        self.subscribers.push(Subscription {
            filter,
            sink: Sink::new(sink, endpoint),
            owner,
            system,
        });
        let retained: Vec<(String, Vec<u8>, u64)> = self
            .retained
            .iter()
            .filter(|entry| matches(&self.subscribers[index].filter, &entry.topic))
            .map(|entry| (entry.topic.clone(), entry.payload.clone(), entry.seq))
            .collect();
        for (topic, payload, seq) in retained {
            if let Ok(event) = event_parcel(&topic, &payload, true, seq) {
                self.deliver(index, &event);
            }
        }
        Ok(())
    }

    /// Forget the subscribers whose sink no longer resolves (`Sink::deliver`
    /// released its handle and the name is gone): nothing will ever unsubscribe
    /// them, and they would count against the caps for good.
    fn prune_dead(&mut self) {
        self.subscribers
            .retain(|subscriber| subscriber.sink.is_resolved());
    }

    /// Drop the subscriber attached with `sink`, giving its handle back.
    fn detach(&mut self, sink: &str) {
        let mut index = 0;
        while index < self.subscribers.len() {
            if self.subscribers[index].sink.name() == sink {
                let mut gone = self.subscribers.remove(index);
                gone.sink.release(&mut Wire { event: None });
            } else {
                index += 1;
            }
        }
    }

    /// Push one event to subscriber `index`. A stalled subscriber costs the
    /// event, never a handle; a dead one gives its handle back and is looked
    /// up again once (`svcpolicy::Sink`).
    fn deliver(&mut self, index: usize, event: &Parcel) {
        let mut wire = Wire { event: Some(event) };
        match self.subscribers[index].sink.deliver(&mut wire) {
            Delivery::Delivered => self.delivered += 1,
            Delivery::Dropped => self.dropped += 1,
        }
    }
}

/// Build an `EVENT` parcel for delivery to subscribers.
fn event_parcel(topic: &str, payload: &[u8], retained: bool, seq: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
    body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
    body.u64(field::RETAINED, retained as u64)
        .map_err(Error::Parcel)?;
    body.u64(field::SEQ, seq).map_err(Error::Parcel)?;
    Ok(parcel(method::EVENT, body))
}

/// The transport the generated `topic` helpers publish through: the interim
/// broker's publish is infallible and returns the fanout sequence, so the
/// generated result is always `Ok`.
impl messenger_generated::topics::Publish for TopicBroker {
    type Error = Error;

    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        Ok(self.publish(topic, payload, retained))
    }
}

/// The `(topic, payload)` of a `PUBLISH` request, for a broker host that acts
/// on what one trusted publisher says (`init` learns session owners from
/// `logind`'s login events); `None` for any other message.
pub fn published(message: &Message) -> Option<(String, Vec<u8>)> {
    if message.interface_id() != INTERFACE || message.method() != method::PUBLISH {
        return None;
    }
    let topic = string_field(&message.parcel, field::TOPIC).ok()?;
    Some((topic, bytes_field(&message.parcel, field::PAYLOAD)))
}

/// The first string field with `id`.
fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// The first `u64` field with `id`, if any.
fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// The first `bytes` field with `id` (`Vec::new` when absent).
fn bytes_field(parcel: &Parcel, id: u16) -> Vec<u8> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Bytes && field.id == id {
            return field.as_bytes().to_vec();
        }
    }
    Vec::new()
}
