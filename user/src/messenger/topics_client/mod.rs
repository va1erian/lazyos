//! Publish/subscribe topics client (issue #92). See the module doc on
//! [`crate::messenger::topics_client`] for the broker/delivery/QoS model.
//!
//! Split into [`wire`] (parcel encode/decode helpers) and [`client`]
//! ([`Client`]/[`Subscription`] and the module-level convenience functions);
//! both are re-exported here so callers keep using `topics_client::*`.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_topics_publish_v1 as publish_scope;
use messenger_generated::os_lazy_messenger_topics_v1 as generated;

use super::{Error, Result};

mod bell;
mod client;
mod wire;

pub use bell::*;
pub use client::*;
pub use wire::*;

/// Well-known broker name; `messengerd` registers it at startup.
pub const NAME: &str = "os.lazy.messenger.topics";

/// Topics interface id (`os.lazy.messenger.topics.v1`), generated from
/// `idl/topics.midl`.
pub const INTERFACE: u64 = generated::INTERFACE_ID;

/// Broker method ids, generated from `idl/topics.midl`.
pub mod method {
    use super::generated;

    /// Publish one payload under a topic.
    pub const PUBLISH: u32 = generated::METHOD_PUBLISH;
    /// Create a subscription for a filter.
    pub const SUBSCRIBE: u32 = generated::METHOD_SUBSCRIBE;
    /// Drop a subscription.
    pub const UNSUBSCRIBE: u32 = generated::METHOD_UNSUBSCRIBE;
    /// Wait for (or poll) the next event of a subscription.
    pub const NEXT_EVENT: u32 = generated::METHOD_NEXTEVENT;
    /// Retire an event delivered by a `reliable` subscription.
    pub const ACK: u32 = generated::METHOD_ACK;
    /// List topics the broker has seen.
    pub const LIST_TOPICS: u32 = generated::METHOD_LISTTOPICS;
    /// Per-subscription queue and drop counters.
    pub const STATS: u32 = generated::METHOD_STATS;
    /// Round-trip probe used to detect a live broker.
    pub const PING: u32 = generated::METHOD_PING;
    /// Give a subscription a doorbell (P7.2).
    pub const BELL: u32 = generated::METHOD_BELL;
}

/// Kernel mode code for a publish ACL check (`Mode::Publish`).
pub const MODE_PUBLISH: u32 = publish_scope::MODE_PUBLISH;
/// Kernel mode code for a subscribe ACL check (`Mode::Subscribe`).
pub const MODE_SUBSCRIBE: u32 = publish_scope::MODE_SUBSCRIBE;

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
            Qos::Latest => generated::QOS_LATEST,
            Qos::Buffered(_) => generated::QOS_BUFFERED,
            Qos::Conflate => generated::QOS_CONFLATE,
            Qos::Reliable => generated::QOS_RELIABLE,
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
            generated::QOS_LATEST => Some(Qos::Latest),
            generated::QOS_BUFFERED => Some(Qos::Buffered(depth)),
            generated::QOS_CONFLATE => Some(Qos::Conflate),
            generated::QOS_RELIABLE => Some(Qos::Reliable),
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
