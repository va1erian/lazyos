//! Publish/subscribe topics client (issue #92). See the module doc on
//! [`crate::messenger::topics_client`] for the broker/delivery/QoS model.
//!
//! Split into [`wire`] (parcel encode/decode helpers) and [`client`]
//! ([`Client`]/[`Subscription`] and the module-level convenience functions);
//! both are re-exported here so callers keep using `topics_client::*`.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};

use super::{Error, Result};

mod client;
mod wire;

pub use client::*;
pub use wire::*;

/// Well-known broker name; `messengerd` registers it at startup.
pub const NAME: &str = "os.lazy.messenger.topics";

/// Topics interface id: `fnv1a64("os.lazy.messenger.topics.v1")`, the same
/// `tools/midlc` hash the kernel and broker use.
pub const INTERFACE: u64 = 0xc573_4f97_8fef_7231;

/// Broker method ids (`fnv1a32` of the method name, `tools/midlc` style).
pub mod method {
    /// Publish one payload under a topic.
    pub const PUBLISH: u32 = 1818372520;
    /// Create a subscription for a filter.
    pub const SUBSCRIBE: u32 = 6992035;
    /// Drop a subscription.
    pub const UNSUBSCRIBE: u32 = 2099666486;
    /// Wait for (or poll) the next event of a subscription.
    pub const NEXT_EVENT: u32 = 1278354512;
    /// Retire an event delivered by a `reliable` subscription.
    pub const ACK: u32 = 483717538;
    /// List topics the broker has seen.
    pub const LIST_TOPICS: u32 = 225427937;
    /// Per-subscription queue and drop counters.
    pub const STATS: u32 = 788260383;
    /// Round-trip probe used to detect a live broker.
    pub const PING: u32 = 2142761129;
}

/// TLV field ids of the broker protocol.
pub mod field {
    /// Publish topic / event topic.
    pub const TOPIC: u16 = 1;
    /// Subscription filter.
    pub const FILTER: u16 = 2;
    /// Encoded payload parcel bytes.
    pub const PAYLOAD: u16 = 3;
    /// Whether a publish is the retained value.
    pub const RETAINED: u16 = 4;
    /// QoS code.
    pub const QOS: u16 = 5;
    /// Buffered depth.
    pub const DEPTH: u16 = 6;
    /// Subscription id.
    pub const SUBSCRIPTION: u16 = 7;
    /// Event sequence / ack sequence.
    pub const SEQUENCE: u16 = 8;
    /// Publisher task slot.
    pub const PUBLISHER: u16 = 9;
    /// Nested event record.
    pub const EVENT: u16 = 10;
    /// Subscribers a publish matched.
    pub const MATCHED: u16 = 11;
    /// Dropped events (subscription stats).
    pub const DROPS: u16 = 12;
    /// Queued events (subscription stats).
    pub const QUEUED: u16 = 13;
    /// Delivered events (subscription stats).
    pub const DELIVERED: u16 = 14;
    /// Subscribers matching a listed topic.
    pub const SUBSCRIBERS: u16 = 15;
    /// Nested topic record.
    pub const ENTRY: u16 = 16;
    /// Structured error reply.
    pub const ERROR: u16 = 17;
}

/// TLV field ids of the kernel `authorize_topic` request; mirrors
/// `kernel/src/ipc/topics.rs`.
pub mod auth_field {
    pub const NAME: u16 = 1;
    pub const MODE: u16 = 2;
    pub const TXN: u16 = 3;
}

/// Kernel mode code for a publish ACL check.
pub const MODE_PUBLISH: u32 = 0;
/// Kernel mode code for a subscribe ACL check.
pub const MODE_SUBSCRIBE: u32 = 1;

/// Payload bytes accepted by the broker in one event. Sized well below the
/// 16 KiB call buffer so a `NextEvent` reply always fits.
pub const MAX_PAYLOAD: usize = 8 * 1024;

/// PIT ticks [`Client::connect`] waits for the broker name to appear.
/// `messengerd` registers [`NAME`] during its own boot and is spawned
/// before its clients, so a handful of ticks is ample; a boot without a
/// broker should still reach the prompt promptly. Userspace has no clock
/// syscall yet, so each retry sleeps one tick by parking on a private
/// channel pair with an expired deadline.
const CONNECT_ATTEMPTS: usize = 8;

/// Delivery contract chosen at subscribe time; enforced by the broker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Qos {
    /// Keep only the most recent event; a replacement overwrites.
    Latest,
    /// Keep up to `N` events; overflow drops the oldest.
    Buffered(u32),
    /// Coalesce to the latest event per publisher until consumed.
    Conflate,
    /// Keep events until the subscriber acks them; bounded retry.
    Reliable,
}

impl Qos {
    /// Largest accepted `Buffered` depth.
    pub const MAX_DEPTH: u32 = 64;
    /// Queue depth a `Reliable` subscription gets.
    pub const RELIABLE_DEPTH: u32 = 8;
    /// Distinct publishers a `Conflate` subscription coalesces across.
    pub const CONFLATE_WINDOW: u32 = 4;

    /// The wire code.
    pub const fn code(self) -> u32 {
        match self {
            Qos::Latest => 0,
            Qos::Buffered(_) => 1,
            Qos::Conflate => 2,
            Qos::Reliable => 3,
        }
    }

    /// The effective queue depth (clamped to at least one).
    pub const fn depth(self) -> u32 {
        match self {
            Qos::Buffered(depth) => {
                if depth == 0 {
                    1
                } else if depth > Self::MAX_DEPTH {
                    Self::MAX_DEPTH
                } else {
                    depth
                }
            }
            Qos::Reliable => Self::RELIABLE_DEPTH,
            Qos::Conflate => Self::CONFLATE_WINDOW,
            Qos::Latest => 1,
        }
    }

    /// Decode `(code, depth)` from the wire, or `None` for an unknown code.
    pub fn from_parts(code: u32, depth: u32) -> Option<Qos> {
        match code {
            0 => Some(Qos::Latest),
            1 => Some(Qos::Buffered(depth)),
            2 => Some(Qos::Conflate),
            3 => Some(Qos::Reliable),
            _ => None,
        }
    }
}

/// One delivered event: broker metadata plus the publisher's opaque
/// payload parcel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Event {
    /// Topic the event was published under.
    pub topic: String,
    /// Task slot of the publisher (kernel-stamped when the publish arrived).
    pub publisher: u64,
    /// Broker sequence number (monotonic per broker boot).
    pub sequence: u64,
    /// Whether this event is a retained value replay.
    pub retained: bool,
    /// The payload parcel, still encoded; decode with [`Event::parcel`].
    pub payload: Vec<u8>,
}

impl Event {
    /// Decode the stored payload into the parcel the publisher sent.
    pub fn parcel(&self) -> Result<Parcel> {
        Parcel::decode(&self.payload).map_err(Error::Parcel)
    }
}

/// Per-subscription delivery counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SubscriptionStats {
    /// QoS code the subscription was created with.
    pub qos: u32,
    /// Effective queue depth.
    pub depth: u32,
    /// Events currently queued (reliable: delivered but unacked included).
    pub queued: u64,
    /// Events handed to the subscriber.
    pub delivered: u64,
    /// Events the subscription's filter matched.
    pub matched: u64,
    /// Events dropped by the QoS policy or a full queue.
    pub drops: u64,
}

/// One row of [`Client::list`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TopicInfo {
    /// Topic name.
    pub topic: String,
    /// Live subscriptions whose filter matches it.
    pub subscribers: u64,
    /// Whether the broker holds a retained value for it.
    pub retained: bool,
}

/// A header for a broker parcel of `method`.
///
/// `ALLOW_NESTED` is required, not optional: every client shares the
/// daemon's bootstrap channel, so `next_event` may leave a transaction
/// open while another task publishes (see the module docs).
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: flags::SYNC | flags::ALLOW_NESTED,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}
