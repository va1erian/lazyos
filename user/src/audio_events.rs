//! Publishing `system/audio/{card}/event` (issue #453), shared by the mixer
//! (`audiod`, card `mixer`) and the driver (`sndd`, card `virtio-snd0`).
//!
//! Each event is one publish on the central broker with the generated
//! `os.lazy.audio.v1` topic codec, bounded by a short deadline so a slow
//! broker costs an event, never a period of audio. The broker answers how
//! many subscriptions matched; while nobody listens, the frequent `Period`
//! events are not sent at all (one probe every [`PROBE_TICKS`] finds out
//! whether someone subscribed since).
//!
//! Serial: `AUDIO:EVENT card=<card> stream=<id> kind=<name> frames=<n>
//! matched=<n>` for every event but `Period`, whose first per stream is
//! logged as `AUDIO:EVENT ... kind=period` and the rest counted.

use alloc::format;
use alloc::vec::Vec;

use audiomix::events::{Event, Kind};
use messenger_generated::os_lazy_audio_v1 as wire;

use crate::central;
use crate::messenger::{registry, topics_client};
use crate::sys;

/// The mixer's `{card}`: the streams applications open on `os.lazy.audio`.
pub const MIXER_CARD: &str = "mixer";
/// The virtio-sound driver's `{card}`: the card's one stream (the mixer's).
pub const VIRTIO_CARD: &str = "virtio-snd0";

/// Ticks one publish may take (100 Hz).
const PUBLISH_TICKS: u64 = 5;
/// Ticks between probes for a `Period` subscriber while there was none.
const PROBE_TICKS: u64 = 100;
/// Ticks between attempts to reach the broker while it is away.
const CONNECT_TICKS: u64 = 100;

/// One card's event topic.
pub struct EventPublisher {
    card: &'static str,
    bus: Option<central::Bus>,
    next_connect: u64,
    /// Whether the last publish reached a subscriber.
    listened: bool,
    next_probe: u64,
    /// Streams whose first `Period` was logged.
    logged_periods: Vec<u32>,
}

impl EventPublisher {
    pub const fn new(card: &'static str) -> EventPublisher {
        EventPublisher {
            card,
            bus: None,
            next_connect: 0,
            listened: false,
            next_probe: 0,
            logged_periods: Vec::new(),
        }
    }

    /// Whether `Period` events are worth computing now.
    pub fn wants_periods(&self, now: u64) -> bool {
        self.listened || now >= self.next_probe
    }

    /// Publish `events`, in order.
    pub fn publish(&mut self, events: &[Event], now: u64) {
        for event in events {
            let matched = self.publish_one(event, now);
            if event.kind == Kind::Period {
                if !matched.is_some_and(|count| count > 0) {
                    self.listened = false;
                    self.next_probe = now + PROBE_TICKS;
                } else {
                    self.listened = true;
                }
                if self.logged_periods.contains(&event.stream) {
                    continue;
                }
                self.logged_periods.push(event.stream);
            }
            sys::write_str(&format!(
                "AUDIO:EVENT card={} stream={} kind={} frames={} matched={}\n",
                self.card,
                event.stream,
                name(event.kind),
                event.frames,
                matched.map_or(-1, |count| count as i64)
            ));
        }
    }

    /// One publish; how many subscriptions it reached, `None` on failure.
    fn publish_one(&mut self, event: &Event, now: u64) -> Option<u64> {
        let topic = wire::name_system_audio_event(self.card).ok()?;
        let payload =
            wire::encode_system_audio_event(&event.kind.payload(event.stream, event.frames))
                .ok()?;
        if self.bus.is_none() {
            if now < self.next_connect {
                return None;
            }
            self.next_connect = now + CONNECT_TICKS;
            let endpoint = registry::resolve(topics_client::NAME).ok()?;
            self.bus = Some(central::Bus::from_endpoint(endpoint));
        }
        let bus = self.bus.as_mut()?;
        match bus.publish_by(
            &topic,
            &payload,
            wire::TOPIC_SYSTEM_AUDIO_EVENT_RETAINED,
            Some(sys::clock() + PUBLISH_TICKS),
        ) {
            Ok(matched) => Some(matched),
            Err(_) => {
                self.bus = None;
                None
            }
        }
    }
}

/// The kind's name in a log line.
fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::Underrun => "underrun",
        Kind::Overrun => "overrun",
        Kind::Drained => "drained",
        Kind::DeviceError => "device-error",
        Kind::Period => "period",
    }
}
