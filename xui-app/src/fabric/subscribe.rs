//! A native subscription client for the topics broker (`messengerd`'s
//! `os.lazy.messenger.topics`), built so a GUI can listen without polling
//! (`docs/messenger-playground-plan.md` D5).
//!
//! # The contract the caller must keep
//!
//! `fabricmon` found that `messengerd`'s bump heap never frees a receive
//! buffer: every request the broker receives costs it memory for good, and a
//! once-a-second poll exhausts it after about a hundred calls. So this client
//! **must never be driven from a timer**. The only supported loop is:
//!
//! 1. [`Subscriber::subscribe`], then [`Subscriber::attach_bell`] for it;
//! 2. learn when a bell rang: park on [`Subscriber::doorbell_handles`] beside
//!    the window's other endpoints (`sys::msg_wait_any`, or the event loop's
//!    endpoint set), or, in a window whose event loop cannot park on them
//!    (xui's), look at the bells from a short timer. That is allowed only
//!    because [`Subscriber::consume_ready`] reads the local bell channels and
//!    never calls the broker: the Playground's topic monitor does it every
//!    `BELL_MS`;
//! 3. on a wake (or a bell found rung), call [`Subscriber::consume_ready`] and
//!    [`Subscriber::drain`] each subscription it names;
//! 4. if [`Drained::more`] is set (the per-call cap was hit) drain again on
//!    the next frame **without** waiting for a bell: the broker rings once
//!    per batch and only re-arms the bell when a `NextEvent` finds the queue
//!    empty (`idl/topics.midl`, `Bell`).
//!
//! Other calls ([`stats`](Subscriber::stats), [`ack`](Subscriber::ack),
//! subscribe/unsubscribe) are driven by an explicit user action or by a
//! drain, never by a timer. A drain costs one broker receive per event plus
//! one for the empty poll that re-arms the bell; the caller's own budget
//! (events per second, [`MAX_DRAIN`] per call) is what keeps a firehose from
//! starving the window.
//!
//! # Reliable subscriptions
//!
//! A `Reliable` broker queue redelivers its head until acked, so a drain
//! would never reach the end of it. [`Subscriber::drain`] therefore acks each
//! event of a `Reliable` subscription as it takes it (the events are in the
//! caller's hands from then on). **Delivery is at-least-once**: the ack
//! follows the push, so if it fails the drain stops with the event already in
//! [`Drained::events`] *and* the error set, the broker still holds the event,
//! and the next drain delivers it a second time (same `sequence`). A caller
//! that must not show an event twice drops a `sequence` it has seen.
//!
//! # Shape
//!
//! The logic is generic over [`Transport`] (bytes in, bytes out); [`SysTransport`]
//! is the real one, a test double stands in for the broker on the host. Errors
//! are negative errno values ([`Errno`]), as everywhere in [`super`]; a broker
//! error field `code` is reported as `-code`.

mod bell;
mod transport;
mod wire;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod tests;

pub use bell::Rung;
pub use transport::{SysTransport, Transport, Wait};

use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::error::{EINVAL, ENOENT};
pub(super) use lazyos_sys::errno::ETIMEDOUT;

/// A negative errno (the module's error currency).
pub type Errno = i64;

/// Most events one [`Subscriber::drain`] returns, whatever `max` asks for.
pub const MAX_DRAIN: usize = 256;
/// Bell messages read from one doorbell per [`Subscriber::consume_ready`],
/// so a misbehaving peer cannot hold the UI thread.
pub const MAX_BELL_MESSAGES: usize = 64;
/// Longest filter accepted locally (the broker's own limits are tighter).
const MAX_FILTER_BYTES: usize = 512;
/// Reply buffer: an event carries at most 8 KiB of payload plus its topic.
const REPLY_BYTES: usize = 16 * 1024;

/// A broker-side subscription id (never zero).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SubId(pub u64);

/// The delivery contract chosen at subscribe time (`idl/topics.midl` `Qos`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Qos {
    /// Only the newest event is kept.
    Latest,
    /// `depth` events are kept; overflow drops the oldest.
    Buffered,
    /// The newest event per publisher is kept until consumed.
    Conflate,
    /// Events are kept until acked.
    Reliable,
}

impl Qos {
    /// Largest `Buffered` depth the broker accepts.
    pub const MAX_DEPTH: u32 = 64;

    /// The wire value.
    pub const fn code(self) -> u32 {
        match self {
            Qos::Latest => topics::QOS_LATEST,
            Qos::Buffered => topics::QOS_BUFFERED,
            Qos::Conflate => topics::QOS_CONFLATE,
            Qos::Reliable => topics::QOS_RELIABLE,
        }
    }
}

/// One delivered event: broker metadata plus the publisher's payload parcel,
/// still encoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RawEvent {
    /// The topic it was published under.
    pub topic: String,
    /// The publisher's task slot (slots are reused: resolve it now or show `?`).
    pub publisher: u64,
    /// The broker's sequence number.
    pub sequence: u64,
    /// Whether this is a replay of the retained value.
    pub retained: bool,
    /// The payload exactly as the broker holds it.
    pub payload: Vec<u8>,
}

impl RawEvent {
    /// The payload with the bus envelope taken off (`libmessenger::envelope`);
    /// a publisher's own parcel comes back unchanged.
    pub fn unwrapped_payload(&self) -> Vec<u8> {
        libmessenger::envelope::unwrap(&self.payload)
    }
}

/// Delivery counters of one subscription.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SubStats {
    /// The `Qos` code it was created with.
    pub qos: u32,
    /// The effective queue depth.
    pub depth: u32,
    /// Events queued now (a reliable queue counts unacked ones).
    pub queued: u64,
    /// Events handed to the subscriber.
    pub delivered: u64,
    /// Events the filter matched.
    pub matched: u64,
    /// Events dropped by the policy or a full queue.
    pub drops: u64,
}

/// The result of one [`Subscriber::drain`]. Events already taken are never
/// lost to an error: they are here even when `error` is set.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Drained {
    /// The events taken, oldest first.
    pub events: Vec<RawEvent>,
    /// The per-call cap was reached before the broker said "nothing": drain
    /// again soon, the bell has not been re-armed.
    pub more: bool,
    /// The call that ended the drain early, if any.
    pub error: Option<Errno>,
}

/// One local subscription record.
struct Sub {
    id: SubId,
    qos: Qos,
    /// Our end of the doorbell channel, once attached.
    bell: Option<u64>,
}

/// A topics-broker subscriber (see the module docs for how to drive it).
pub struct Subscriber<T: Transport = SysTransport> {
    transport: T,
    subs: Vec<Sub>,
    /// One reply buffer for the client's lifetime.
    reply: Vec<u8>,
}

impl Default for Subscriber<SysTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl Subscriber<SysTransport> {
    /// A subscriber on the real broker; the name is resolved on the first
    /// call and the endpoint is cached (and released, never closed).
    pub fn new() -> Self {
        Self::with_transport(SysTransport::new())
    }
}

impl<T: Transport> Subscriber<T> {
    /// A subscriber over `transport`.
    pub fn with_transport(transport: T) -> Self {
        Subscriber {
            transport,
            subs: Vec::new(),
            reply: vec![0u8; REPLY_BYTES],
        }
    }

    /// Whether `id` is a subscription this client holds.
    pub fn holds(&self, id: SubId) -> bool {
        self.sub(id).is_some()
    }

    /// The subscriptions this client holds, oldest first.
    pub fn ids(&self) -> Vec<SubId> {
        self.subs.iter().map(|sub| sub.id).collect()
    }

    /// Create a subscription for `filter` (`+` one segment, trailing `#` the
    /// tail). `depth` matters for `Buffered` only and is clamped to `1..=64`.
    /// The broker replays matching retained values into the queue.
    pub fn subscribe(&mut self, filter: &str, qos: Qos, depth: u32) -> Result<SubId, Errno> {
        if filter.is_empty() || filter.len() > MAX_FILTER_BYTES {
            return Err(-EINVAL);
        }
        let request = wire::subscribe_request(filter, qos, depth)?;
        let reply = self.exchange(&request, Wait::Bounded)?;
        let id = wire::decode_subscription(&reply)?;
        if self.holds(id) {
            // A broker handing out a live id again has lost track of us.
            return Err(-EINVAL);
        }
        self.subs.push(Sub {
            id,
            qos,
            bell: None,
        });
        Ok(id)
    }

    /// Drop a subscription. When the broker call fails the local record and
    /// its doorbell **stay** (forgetting them would leak the broker's queue
    /// for good) and the error is returned, so the caller can retry; the
    /// client's `Drop` also makes one last attempt. A broker that no longer
    /// knows the id (it restarted) has nothing left to drop: that is success.
    pub fn unsubscribe(&mut self, id: SubId) -> Result<(), Errno> {
        let index = self.index(id)?;
        let request = wire::by_id_request(topics::METHOD_UNSUBSCRIBE, id)?;
        match self.exchange(&request, Wait::Bounded) {
            Ok(_) => {}
            Err(code) if code == -ENOENT => {}
            Err(code) => return Err(code),
        }
        let sub = self.subs.remove(index);
        if let Some(bell) = sub.bell {
            self.transport.close(bell);
        }
        Ok(())
    }

    /// Retire every event up to `sequence` on a `Reliable` subscription.
    pub fn ack(&mut self, id: SubId, sequence: u64) -> Result<(), Errno> {
        self.index(id)?;
        let request = wire::ack_request(id, sequence)?;
        self.exchange(&request, Wait::Bounded).map(drop)
    }

    /// The broker's counters for `id`. User-driven only, never on a timer.
    pub fn stats(&mut self, id: SubId) -> Result<SubStats, Errno> {
        self.index(id)?;
        let request = wire::by_id_request(topics::METHOD_STATS, id)?;
        let reply = self.exchange(&request, Wait::Bounded)?;
        wire::decode_stats(&reply)
    }

    /// Take up to `max` queued events (at most [`MAX_DRAIN`], at least one)
    /// with expired-deadline `NextEvent` calls, until the broker has nothing.
    /// The final empty poll is what re-arms the doorbell, so a drain that ends
    /// with [`Drained::more`] set has not re-armed it.
    pub fn drain(&mut self, id: SubId, max: usize) -> Drained {
        let mut out = Drained::default();
        let qos = match self.sub(id) {
            Some(sub) => sub.qos,
            None => {
                out.error = Some(-ENOENT);
                return out;
            }
        };
        let poll = match wire::by_id_request(topics::METHOD_NEXTEVENT, id)
            .and_then(|request| wire::encode(&request))
        {
            Ok(bytes) => bytes,
            Err(code) => {
                out.error = Some(code);
                return out;
            }
        };
        let max = max.clamp(1, MAX_DRAIN);
        while out.events.len() < max {
            let event = match self.poll_once(&poll) {
                Ok(Some(event)) => event,
                Ok(None) => return out,
                Err(code) => {
                    out.error = Some(code);
                    return out;
                }
            };
            let sequence = event.sequence;
            out.events.push(event);
            if qos == Qos::Reliable {
                if let Err(code) = self.ack(id, sequence) {
                    out.error = Some(code);
                    return out;
                }
            }
        }
        out.more = true;
        out
    }

    /// Forget every subscription without asking the broker (it was restarted
    /// and no longer knows them), closing the doorbells.
    pub fn reset(&mut self) {
        for sub in std::mem::take(&mut self.subs) {
            if let Some(bell) = sub.bell {
                self.transport.close(bell);
            }
        }
    }

    fn sub(&self, id: SubId) -> Option<&Sub> {
        self.subs.iter().find(|sub| sub.id == id)
    }

    fn index(&self, id: SubId) -> Result<usize, Errno> {
        self.subs.iter().position(|sub| sub.id == id).ok_or(-ENOENT)
    }

    /// One poll: `Ok(None)` is "nothing queued" (the kernel's timeout of an
    /// expired-deadline call, or an empty reply).
    fn poll_once(&mut self, encoded: &[u8]) -> Result<Option<RawEvent>, Errno> {
        let length = match self.transport.call(encoded, &mut self.reply, Wait::Poll) {
            Ok(length) => length,
            Err(code) if code == -ETIMEDOUT => return Ok(None),
            Err(code) => return Err(code),
        };
        let bytes = self.reply.get(..length).ok_or(-EINVAL)?;
        wire::decode_event(&wire::parse_reply(bytes)?)
    }

    /// Encode `request`, call, and decode the reply (a broker error field is
    /// an `Err`).
    fn exchange(
        &mut self,
        request: &libmessenger::Parcel,
        wait: Wait,
    ) -> Result<libmessenger::Parcel, Errno> {
        let encoded = wire::encode(request)?;
        let length = self.transport.call(&encoded, &mut self.reply, wait)?;
        let bytes = self.reply.get(..length).ok_or(-EINVAL)?;
        wire::parse_reply(bytes)
    }
}

impl<T: Transport> Drop for Subscriber<T> {
    /// Best effort and never blocking: each doorbell is closed and each
    /// subscription unsubscribed with a poll call, so a window closing does
    /// not leave the broker queueing for a dead owner.
    fn drop(&mut self) {
        for sub in std::mem::take(&mut self.subs) {
            if let Some(bell) = sub.bell {
                self.transport.close(bell);
            }
            if let Ok(request) = wire::by_id_request(topics::METHOD_UNSUBSCRIBE, sub.id) {
                if let Ok(encoded) = wire::encode(&request) {
                    let _ = self.transport.call(&encoded, &mut self.reply, Wait::Poll);
                }
            }
        }
    }
}
