//! Stream events for `system/audio/{card}/event` (issue #453): turning what
//! the mixer and the driver count into exactly-once notifications, so a
//! client can wait for them instead of polling `Position`.
//!
//! * [`Watch`] follows the mixer's streams (`audiod`): one [`Kind::Underrun`]
//!   per dry spell (the engine counts them, [`crate::Status::underruns`]),
//!   one [`Kind::Drained`] when a drain completes, and, while someone
//!   listens, a rate-limited [`Kind::Period`] as a running stream's position
//!   advances (the "a period was consumed, there is room again" signal).
//! * [`Starvation`] is the driver's edge detector (`sndd`): the device ran out
//!   of queued periods while the stream runs, reported once per spell.
//!
//! Kinds are the `EventKind` ordinals of `idl/audio.midl`.

use alloc::vec::Vec;

use messenger_generated::os_lazy_audio_v1 as wire;

use crate::Status;

/// Least ticks (100 Hz) between two `Period` events of one stream.
pub const PERIOD_TICKS: u64 = 5;

/// Why an event was published (`idl/audio.midl` `EventKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Underrun,
    Overrun,
    Drained,
    DeviceError,
    Period,
}

impl Kind {
    /// The wire ordinal (the generated `EVENT_KIND_*`).
    pub fn ordinal(self) -> u32 {
        match self {
            Kind::Underrun => wire::EVENT_KIND_UNDERRUN,
            Kind::Overrun => wire::EVENT_KIND_OVERRUN,
            Kind::Drained => wire::EVENT_KIND_DRAINED,
            Kind::DeviceError => wire::EVENT_KIND_DEVICE_ERROR,
            Kind::Period => wire::EVENT_KIND_PERIOD,
        }
    }

    /// The topic payload for an event of this kind.
    pub fn payload(self, stream: u32, frames: u64) -> wire::AudioEvent {
        wire::AudioEvent {
            stream,
            kind: self.ordinal(),
            frames,
        }
    }
}

/// One event: the stream, why, and its position (frames played) then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub stream: u32,
    pub kind: Kind,
    pub frames: u64,
}

/// What [`Watch`] last reported for one stream.
#[derive(Clone, Copy, Debug)]
struct Seen {
    id: u32,
    underruns: u32,
    /// Position of the last `Period` event, and when it went out.
    period_frames: u64,
    period_at: Option<u64>,
}

/// Follows the mixer's streams and yields each event once.
#[derive(Default)]
pub struct Watch {
    seen: Vec<Seen>,
}

impl Watch {
    pub fn new() -> Watch {
        Watch::default()
    }

    /// After one mixer step: `statuses` are the streams now, `drained` the
    /// drains that completed in the step, `now` the tick, and `periods`
    /// whether `Period` events are wanted (someone listens). Appends the new
    /// events to `out`; streams that closed are forgotten.
    pub fn step(
        &mut self,
        statuses: impl Iterator<Item = Status>,
        drained: &[u32],
        now: u64,
        periods: bool,
        out: &mut Vec<Event>,
    ) {
        let mut next = Vec::with_capacity(self.seen.len());
        for status in statuses {
            let mut seen = self
                .seen
                .iter()
                .find(|seen| seen.id == status.id)
                .copied()
                .unwrap_or(Seen {
                    id: status.id,
                    underruns: 0,
                    period_frames: status.played,
                    period_at: None,
                });
            for _ in seen.underruns..status.underruns {
                out.push(event(status.id, Kind::Underrun, status.played));
            }
            seen.underruns = status.underruns;
            if drained.contains(&status.id) {
                out.push(event(status.id, Kind::Drained, status.played));
            }
            let due = seen
                .period_at
                .is_none_or(|at| now.saturating_sub(at) >= PERIOD_TICKS);
            let running = status.state == crate::State::Running;
            if periods && running && due && status.played > seen.period_frames {
                out.push(event(status.id, Kind::Period, status.played));
                seen.period_frames = status.played;
                seen.period_at = Some(now);
            } else if !running {
                seen.period_frames = status.played;
            }
            next.push(seen);
        }
        // A drain that completed as its stream closed still reports.
        for &id in drained {
            if !next.iter().any(|seen| seen.id == id) {
                out.push(event(id, Kind::Drained, 0));
            }
        }
        self.seen = next;
    }
}

fn event(stream: u32, kind: Kind, frames: u64) -> Event {
    Event {
        stream,
        kind,
        frames,
    }
}

/// The driver's underrun detector: `true` once when a running stream's device
/// runs out of queued periods, again only after it was fed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Starvation {
    starved: bool,
    /// Underruns reported so far.
    pub count: u64,
}

impl Starvation {
    /// Look at the stream: `running`, and `starved` (nothing queued on the
    /// device and nothing committed left to queue). Whether this is a new
    /// underrun.
    pub fn observe(&mut self, running: bool, starved: bool) -> bool {
        let now = running && starved;
        let new = now && !self.starved;
        self.starved = now;
        if new {
            self.count += 1;
        }
        new
    }

    /// The stream stopped or restarted: the next starvation is a new one.
    pub fn reset(&mut self) {
        self.starved = false;
    }
}
