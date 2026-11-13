//! The delivery contract chosen at subscribe time.

use messenger_generated::os_lazy_messenger_topics_v1 as wire;

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
            Qos::Latest => wire::QOS_LATEST,
            Qos::Buffered(_) => wire::QOS_BUFFERED,
            Qos::Conflate => wire::QOS_CONFLATE,
            Qos::Reliable => wire::QOS_RELIABLE,
        }
    }

    /// A `Buffered` depth as the broker uses it: at least one, at most
    /// [`Qos::MAX_DEPTH`].
    pub const fn clamp_depth(depth: u32) -> u32 {
        if depth == 0 {
            1
        } else if depth > Self::MAX_DEPTH {
            Self::MAX_DEPTH
        } else {
            depth
        }
    }

    /// The effective queue depth (clamped to at least one).
    pub const fn depth(self) -> u32 {
        match self {
            Qos::Buffered(depth) => Self::clamp_depth(depth),
            Qos::Reliable => Self::RELIABLE_DEPTH,
            Qos::Conflate => Self::CONFLATE_WINDOW,
            Qos::Latest => 1,
        }
    }

    /// Decode `(code, depth)` from the wire, or `None` for an unknown code.
    /// The depth is any client's number, so it is clamped here
    /// ([`Qos::clamp_depth`]): the broker queues at most [`Qos::MAX_DEPTH`].
    pub fn from_parts(code: u32, depth: u32) -> Option<Qos> {
        match code {
            wire::QOS_LATEST => Some(Qos::Latest),
            wire::QOS_BUFFERED => Some(Qos::Buffered(Qos::clamp_depth(depth))),
            wire::QOS_CONFLATE => Some(Qos::Conflate),
            wire::QOS_RELIABLE => Some(Qos::Reliable),
            _ => None,
        }
    }
}
