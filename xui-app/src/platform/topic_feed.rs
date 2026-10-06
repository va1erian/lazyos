//! Following one topic filter on the central broker from a UI loop, without
//! ever parking it.
//!
//! The feed subscribes once the broker is there, then pulls with poll
//! `NextEvent` calls (an expired deadline: the kernel answers in the broker's
//! own service turn), at most [`PER_LOOK`] per look, each look at most every
//! `period` ticks. When the broker goes away the subscription is dropped and
//! made again on a later look; retained values are replayed by the broker on
//! every new subscription, so a state topic is never missed. Payloads come
//! out of the platform envelope (`libmessenger::envelope`), ready for the
//! topic's generated `decode_*`.

use libmessenger::envelope;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;

use super::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::{self, errno, EXPIRED_DEADLINE};

/// The broker's registered name.
const BROKER: &str = "os.lazy.messenger.topics";
/// Most events taken per look.
const PER_LOOK: usize = 8;
/// Ticks the subscribe call may take.
const SUBSCRIBE_TICKS: u64 = 50;
/// Ticks between subscription attempts while the broker is away.
const RETRY_TICKS: u64 = 200;

/// One event: its literal topic, whether it is a retained replay, and the
/// payload as the publisher's codec wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicEvent {
    pub topic: String,
    pub retained: bool,
    pub payload: Vec<u8>,
}

/// One followed filter.
pub struct TopicFeed {
    filter: String,
    qos: u32,
    depth: u32,
    period: u64,
    subscription: Option<u64>,
    next_look: u64,
}

impl TopicFeed {
    /// Follow `filter` with `qos` (a `QOS_*` value) and `depth`, looking at
    /// most every `period` ticks.
    pub fn new(filter: String, qos: u32, depth: u32, period: u64) -> TopicFeed {
        TopicFeed {
            filter,
            qos,
            depth,
            period,
            subscription: None,
            next_look: 0,
        }
    }

    /// The events that arrived since the last look (none when it is not due).
    pub fn poll(&mut self) -> Vec<TopicEvent> {
        let now = sys::clock_ticks();
        if now < self.next_look {
            return Vec::new();
        }
        self.next_look = now.saturating_add(self.period);
        let Some(subscription) = self.subscription.or_else(|| self.subscribe(now)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for _ in 0..PER_LOOK {
            match next_event(subscription) {
                Ok(Some(event)) => found.push(TopicEvent {
                    topic: event.topic,
                    retained: event.retained,
                    payload: envelope::unwrap(&event.payload),
                }),
                Ok(None) => break,
                // The broker restarted (or refused): subscribe again later.
                Err(_) => {
                    self.subscription = None;
                    break;
                }
            }
        }
        found
    }

    /// Look now, waiting up to `ticks` for the first event: right after
    /// subscribing, to read a retained value before acting on it (the broker
    /// replays it in its own turn, so an instant poll can miss it). Costs at
    /// most `ticks` when the topic has no value.
    pub fn poll_waiting(&mut self, ticks: u64) -> Vec<TopicEvent> {
        let now = sys::clock_ticks();
        let Some(subscription) = self.subscription.or_else(|| self.subscribe(now)) else {
            return Vec::new();
        };
        let deadline = now.saturating_add(ticks.max(1));
        let mut found = match next_event_until(subscription, deadline) {
            Ok(Some(event)) => vec![TopicEvent {
                topic: event.topic,
                retained: event.retained,
                payload: envelope::unwrap(&event.payload),
            }],
            Ok(None) => Vec::new(),
            Err(_) => {
                self.subscription = None;
                return Vec::new();
            }
        };
        self.next_look = 0;
        found.extend(self.poll());
        found
    }

    fn subscribe(&mut self, now: u64) -> Option<u64> {
        match subscribe(&self.filter, self.qos, self.depth) {
            Ok(id) => {
                self.subscription = Some(id);
                Some(id)
            }
            Err(_) => {
                self.next_look = now.saturating_add(RETRY_TICKS);
                None
            }
        }
    }
}

fn broker() -> Result<Service, i64> {
    Service::try_connect(BROKER).ok_or(-errno::ENOENT)
}

fn subscribe(filter: &str, qos: u32, depth: u32) -> Result<u64, i64> {
    let body = wire::encode_subscribe_args(&wire::SubscribeArgs {
        filter: filter.to_owned(),
        qos,
        depth,
    })
    .map_err(|_| -errno::EINVAL)?;
    let reply = broker()?.call_within(
        wire::INTERFACE_ID,
        wire::METHOD_SUBSCRIBE,
        ERROR_FIELD,
        body,
        SUBSCRIBE_TICKS,
    )?;
    wire::decode_subscribe_reply(&reply.body)
        .map(|reply| reply.subscription)
        .map_err(|_| -errno::EINVAL)
}

/// One poll `NextEvent`: `None` when nothing is queued.
fn next_event(subscription: u64) -> Result<Option<wire::Event>, i64> {
    next_event_until(subscription, EXPIRED_DEADLINE)
}

/// One `NextEvent` bounded by `deadline` (an absolute PIT tick, or
/// `EXPIRED_DEADLINE` for a poll): `None` when nothing came.
fn next_event_until(subscription: u64, deadline: u64) -> Result<Option<wire::Event>, i64> {
    let body = wire::encode_next_event_args(&wire::NextEventArgs { subscription })
        .map_err(|_| -errno::EINVAL)?;
    let reply = match broker()?.call_until(
        wire::INTERFACE_ID,
        wire::METHOD_NEXTEVENT,
        ERROR_FIELD,
        body,
        deadline,
    ) {
        Ok(reply) => reply,
        Err(code) if code == -errno::ETIMEDOUT => return Ok(None),
        Err(code) => return Err(code),
    };
    let event = wire::decode_next_event_reply(&reply.body)
        .map_err(|_| -errno::EINVAL)?
        .event;
    Ok((!event.topic.is_empty()).then_some(event))
}
