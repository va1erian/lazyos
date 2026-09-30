//! The interim userspace topic router (issue #93). See the module doc on
//! [`crate::messenger::router`] for why this exists ahead of the kernel/
//! `messengerd` broker.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use super::{create_pair, errno, registry, Endpoint, Error, Message, Result};

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
        handles: Vec::new(),
        buffers: Vec::new(),
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

/// One attached subscriber.
struct Subscription {
    filter: String,
    sink: String,
    /// Cached endpoint; `None` until the sink resolves, or after a delivery
    /// failed (the next publish retries the registry).
    endpoint: Option<Endpoint>,
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
                self.attach(filter, sink)?;
                Ok(parcel(method::SUBSCRIBE, Encoder::new()))
            }
            method::UNSUBSCRIBE => {
                let sink = string_field(&message.parcel, field::SINK)?;
                self.subscribers
                    .retain(|subscriber| subscriber.sink != sink);
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
    fn attach(&mut self, filter: String, sink: String) -> Result<()> {
        if self
            .subscribers
            .iter()
            .any(|subscriber| subscriber.sink == sink)
        {
            return Ok(());
        }
        let endpoint = registry::resolve(&sink)?;
        let index = self.subscribers.len();
        self.subscribers.push(Subscription {
            filter,
            sink,
            endpoint: Some(endpoint),
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

    /// Push one event to subscriber `index`, falling back to one registry
    /// re-resolution when the cached endpoint is stale.
    fn deliver(&mut self, index: usize, event: &Parcel) {
        if let Some(endpoint) = self.subscribers[index].endpoint {
            if endpoint.send(event).is_ok() {
                self.delivered += 1;
                return;
            }
        }
        let resolved = registry::resolve(&self.subscribers[index].sink)
            .and_then(|endpoint| endpoint.send(event).map(|()| endpoint));
        match resolved {
            Ok(endpoint) => {
                self.subscribers[index].endpoint = Some(endpoint);
                self.delivered += 1;
            }
            Err(_) => {
                self.subscribers[index].endpoint = None;
                self.dropped += 1;
            }
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

/// The client half: connect to a service's broker, subscribe, publish.
pub struct Bus {
    endpoint: Endpoint,
}

impl Bus {
    /// Resolve `name` (a broker service, e.g. `os.lazy.healthd`).
    pub fn connect(name: &str) -> Result<Bus> {
        Ok(Bus {
            endpoint: registry::resolve(name)?,
        })
    }

    /// The underlying broker endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Subscribe to `filter`; returns a receiver already attached to the
    /// broker. Retained matching values are queued by the broker.
    pub fn subscribe(&self, filter: &str) -> Result<Subscriber> {
        // One round trip reserves a unique sink name; the subscriber then
        // registers its own endpoint under it, so no handle transfer is
        // needed across processes.
        let reply = self
            .endpoint
            .call(&parcel(method::RESERVE, Encoder::new()), None)?;
        let sink = string_field(&reply, field::SINK)?;
        let (published, received) = create_pair()?;
        registry::register(&sink, &published, &[], 0)?;

        let mut body = Encoder::new();
        body.string(field::SINK, &sink).map_err(Error::Parcel)?;
        body.string(field::FILTER, filter).map_err(Error::Parcel)?;
        self.endpoint.call(&parcel(method::SUBSCRIBE, body), None)?;
        Ok(Subscriber {
            endpoint: received,
            filter: String::from(filter),
        })
    }

    /// Publish `payload` on `topic` through the broker.
    pub fn publish(&self, topic: &str, payload: &[u8], retained: bool) -> Result<()> {
        let mut body = Encoder::new();
        body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
        body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
        body.u64(field::RETAINED, retained as u64)
            .map_err(Error::Parcel)?;
        self.endpoint.call(&parcel(method::PUBLISH, body), None)?;
        Ok(())
    }
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

/// The transport the generated `topic` helpers publish through when the
/// publisher is a client of someone else's broker (e.g. `logind` publishing on
/// `init`'s). The interim `Bus::publish` discards the broker's fanout count.
impl messenger_generated::topics::Publish for Bus {
    type Error = Error;

    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.publish(topic, payload, retained).map(|()| 0)
    }
}

/// The transport the generated `topic` helpers subscribe through. The interim
/// router has no QoS queues, so the declared `qos` is accepted and ignored.
impl messenger_generated::topics::Subscribe for Bus {
    type Error = Error;
    type Subscription = Subscriber;

    fn subscribe_topic(&mut self, filter: &str, _qos: u32) -> Result<Subscriber> {
        self.subscribe(filter)
    }
}

/// A subscriber's receiving end.
pub struct Subscriber {
    endpoint: Endpoint,
    /// The filter this subscriber attached with.
    pub filter: String,
}

impl Subscriber {
    /// Receive the next event, or `None` when `deadline` passes first.
    ///
    /// Allocates the receive buffer per call; a polling loop should use
    /// [`Subscriber::recv_with`] and reuse one buffer.
    pub fn recv(&self, deadline: Option<u64>) -> Result<Option<Event>> {
        let mut buf = alloc::vec![0u8; super::DEFAULT_BUFFER];
        self.recv_with(&mut buf, deadline)
    }

    /// [`Subscriber::recv`] with a caller-owned buffer.
    pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Option<Event>> {
        match self.endpoint.recv_with(buf, deadline) {
            Ok(message) => Ok(Some(Event::from_message(&message)?)),
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
            Err(error) => Err(error),
        }
    }
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
