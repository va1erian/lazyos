//! The supervisors' publisher (split out of `central.rs`).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::client::Deadline;
use messenger_generated::topics;

use super::Bus;
use crate::messenger::{errno, Error, Result};

/// Ticks (100 Hz) one [`Publisher`] publish may take.
const PUBLISH_TICKS: u64 = 50;
/// Ticks a [`Publisher`] leaves the broker alone after a publish timed out.
const COOLDOWN_TICKS: u64 = 20;
/// Most retained values a [`Publisher`] keeps for a broker that is not there
/// yet (one per topic, newest wins).
const BACKLOG: usize = 64;

/// A publisher for a supervisor (`init`, `healthd`) that must never wait on
/// the broker.
///
/// `messengerd` is a child of `init`, so the first state events are published
/// before it serves: a retained value that cannot be sent yet is kept (one
/// per topic) and sent, in order, by the next publish or [`Publisher::flush`]
/// that finds the broker. Every publish carries a deadline, and a timeout
/// leaves the broker alone for [`COOLDOWN_TICKS`], so a wedged broker costs
/// events, never the supervisor. Nothing here retries on a timer: the caller
/// flushes when something else woke it.
pub struct Publisher {
    bus: Option<Bus>,
    backlog: Vec<(String, Vec<u8>)>,
    /// No attempt before this tick (set by a timeout).
    quiet_until: u64,
}

impl Publisher {
    /// A publisher that connects on its first publish.
    pub const fn new() -> Publisher {
        Publisher {
            bus: None,
            backlog: Vec::new(),
            quiet_until: 0,
        }
    }

    /// Send what an absent broker held back; cheap when nothing is waiting.
    pub fn flush(&mut self) {
        while let Some((topic, payload)) = self.backlog.first().cloned() {
            if self.send(&topic, &payload, true).is_err() {
                return;
            }
            self.backlog.remove(0);
        }
    }

    /// Drop the value held back for `topic`: for an announcement whose
    /// caller acts on the failure (refuses and keeps its state), so a late
    /// replay cannot contradict the answer it gave.
    pub fn forget(&mut self, topic: &str) {
        self.backlog.retain(|(held, _)| held != topic);
    }

    /// One publish with a deadline, reconnecting once to a restarted broker.
    fn send(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        if crate::sys::clock() < self.quiet_until {
            return Err(Error::Errno(-errno::ETIMEDOUT));
        }
        let mut outcome = self.try_send(topic, payload, retained);
        if outcome.is_err() {
            self.bus = None;
            outcome = self.try_send(topic, payload, retained);
        }
        if matches!(outcome, Err(Error::Errno(code)) if code == -errno::ETIMEDOUT) {
            self.quiet_until = crate::sys::clock() + COOLDOWN_TICKS;
        }
        outcome
    }

    fn try_send(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        if self.bus.is_none() {
            // One registry lookup: no retry loop that could hold the
            // supervisor while `messengerd` is away.
            self.bus = Some(Bus::connect()?);
        }
        let Some(bus) = self.bus.as_mut() else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        bus.publish_within(topic, payload, retained, Deadline::ticks(PUBLISH_TICKS))
    }

    /// Keep a retained value for later, replacing the topic's older one.
    fn hold(&mut self, topic: &str, payload: &[u8]) {
        if let Some(slot) = self.backlog.iter_mut().find(|(held, _)| held == topic) {
            slot.1 = payload.to_vec();
        } else if self.backlog.len() < BACKLOG {
            self.backlog.push((String::from(topic), payload.to_vec()));
        }
    }
}

impl Default for Publisher {
    fn default() -> Self {
        Publisher::new()
    }
}

impl topics::Publish for Publisher {
    type Error = Error;

    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.flush();
        // Older values go first: a newer one must not overtake them.
        if !self.backlog.is_empty() {
            if retained {
                self.hold(topic, payload);
            }
            return Err(Error::Errno(-errno::EAGAIN));
        }
        let outcome = self.send(topic, payload, retained);
        if outcome.is_err() && retained {
            self.hold(topic, payload);
        }
        outcome
    }
}
