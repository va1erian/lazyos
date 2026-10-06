//! One client stream inside the mixer: its grant, its ring, the
//! commit/consume/play counters and its share of each mixed period.
//!
//! The counters follow `os.lazy.audio.v1` exactly. `committed` is the client's
//! total written frames; `consumed` is how far the mixer has read the ring
//! (frames before it are safe to overwrite); `played` is how far the card has
//! played what the mixer made from them. Every mixed period leaves a *mark*,
//! `(card frame at the period's end, consumed after it)`, and the card's
//! position resolves marks into `played`, so `played <= consumed <= committed`
//! always holds and a client pacing on `Position` is always safe.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use virtio_snd::params::Grant;

use crate::gain::Gain;
use crate::mixer::MixError;
use crate::resample::Resampler;
use crate::Ring;

/// Marks kept per stream. The card holds a few periods at most, so this is
/// never reached in practice; past it the oldest mark is dropped and the
/// position simply jumps a little later.
const MAX_MARKS: usize = 32;

/// A stream's lifecycle. Ordinals match `os.lazy.audio.mixer.v1`'s
/// `StreamState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Opened; not started yet.
    Idle,
    Running,
    /// After `Stop`: restartable, numbering restarts at 0.
    Stopped,
    /// `Drain` was called: play what is committed, then stop.
    Draining,
    /// Drained: only `CloseStream` is left.
    Drained,
}

impl State {
    pub fn ordinal(self) -> u32 {
        match self {
            State::Idle => 0,
            State::Running => 1,
            State::Stopped => 2,
            State::Draining => 3,
            State::Drained => 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Mark {
    card_end: u64,
    consumed: u64,
}

/// Scratch space shared by every stream's mixing pass, kept by the mixer so
/// the steady state allocates nothing.
#[derive(Default)]
pub(crate) struct Scratch {
    bytes: Vec<u8>,
    samples: Vec<i16>,
    pub(crate) frames: Vec<[i16; 2]>,
}

pub(crate) struct Stream<R> {
    pub(crate) id: u32,
    pub(crate) owner: u64,
    pub(crate) grant: Grant,
    ring: Option<R>,
    ring_frames: u64,
    frame_bytes: usize,
    channels: usize,
    committed: u64,
    consumed: u64,
    pub(crate) played: u64,
    pub(crate) state: State,
    pub(crate) gain: Gain,
    pub(crate) muted: bool,
    resampler: Resampler,
    marks: VecDeque<Mark>,
    pub(crate) underruns: u32,
    starved: bool,
    last_active: u64,
}

type Result<T> = core::result::Result<T, MixError>;

impl<R: Ring> Stream<R> {
    pub(crate) fn new(id: u32, owner: u64, grant: Grant, mix_rate: u32, now: u64) -> Self {
        let frame_bytes = grant.frame_bytes().max(1) as usize;
        Stream {
            id,
            owner,
            grant,
            ring: None,
            ring_frames: u64::from(grant.buffer_bytes()) / frame_bytes as u64,
            frame_bytes,
            channels: grant.channels as usize,
            committed: 0,
            consumed: 0,
            played: 0,
            state: State::Idle,
            gain: Gain::UNITY,
            muted: false,
            resampler: Resampler::new(grant.rate_hz, mix_rate),
            marks: VecDeque::with_capacity(MAX_MARKS + 1),
            underruns: 0,
            starved: false,
            last_active: now,
        }
    }

    /// Note owner activity (resets the reclaim timer).
    pub(crate) fn touch(&mut self, now: u64) {
        self.last_active = now;
    }

    /// Take the client's ring; it must cover the whole grant.
    pub(crate) fn attach(&mut self, ring: R) -> Result<()> {
        if self.ring.is_some() {
            return Err(MixError::Busy);
        }
        if (ring.len() as u64) < u64::from(self.grant.buffer_bytes()) {
            return Err(MixError::Invalid);
        }
        self.ring = Some(ring);
        Ok(())
    }

    /// Accept the client's write position; returns frames consumed so far.
    pub(crate) fn commit(&mut self, written: u64) -> Result<u64> {
        if self.ring.is_none() || self.state == State::Drained {
            return Err(MixError::Invalid);
        }
        // Monotonic, and never more than a ring ahead of what was read.
        if written < self.committed || written - self.consumed > self.ring_frames {
            return Err(MixError::Invalid);
        }
        self.committed = written;
        Ok(self.consumed)
    }

    pub(crate) fn start(&mut self) -> Result<()> {
        if self.ring.is_none() {
            return Err(MixError::Invalid);
        }
        match self.state {
            State::Running | State::Draining => {}
            State::Drained => return Err(MixError::Busy),
            State::Idle | State::Stopped => {
                self.state = State::Running;
                self.starved = false;
            }
        }
        Ok(())
    }

    /// Stop now, discarding what was committed but not yet mixed.
    pub(crate) fn stop(&mut self) {
        if matches!(self.state, State::Running | State::Draining) {
            self.committed = 0;
            self.consumed = 0;
            self.played = 0;
            self.marks.clear();
            self.resampler.reset();
            self.state = State::Stopped;
        }
    }

    /// Begin draining; `Ok(true)` when there is nothing left to play.
    pub(crate) fn drain(&mut self) -> Result<bool> {
        match self.state {
            State::Drained => return Ok(true),
            State::Running => self.state = State::Draining,
            State::Draining => {}
            State::Idle | State::Stopped => return Err(MixError::Invalid),
        }
        Ok(self.finish_drain())
    }

    /// Move a draining stream that has played everything to `Drained`.
    fn finish_drain(&mut self) -> bool {
        if self.state == State::Draining && self.consumed == self.committed && self.marks.is_empty()
        {
            self.state = State::Drained;
            return true;
        }
        false
    }

    /// Whether mixed audio of this stream is still on its way to the speaker.
    pub(crate) fn in_flight(&self) -> bool {
        !self.marks.is_empty()
    }

    /// Whether this stream can fill (part of) an output period right now.
    pub(crate) fn wants_output(&self, outputs: usize) -> bool {
        let available = self.committed - self.consumed;
        match self.state {
            State::Running => available >= self.threshold(outputs),
            State::Draining => available > 0,
            _ => false,
        }
    }

    /// Frames a running stream must have committed before it plays: one
    /// output period's worth, or the whole ring when the ring is smaller.
    fn threshold(&self, outputs: usize) -> u64 {
        self.resampler
            .input_needed(outputs)
            .min(self.ring_frames)
            .max(1)
    }

    /// Add this stream's share of one output period of `outputs` frames to
    /// `acc` (interleaved stereo) and leave a mark at `card_end`.
    pub(crate) fn mix_into(
        &mut self,
        acc: &mut [i64],
        outputs: usize,
        card_end: u64,
        scratch: &mut Scratch,
    ) {
        if !matches!(self.state, State::Running | State::Draining) {
            return;
        }
        let draining = self.state == State::Draining;
        let available = self.committed - self.consumed;
        if available == 0 || (!draining && available < self.threshold(outputs)) {
            // Ran dry while playing: one underrun per dry spell.
            if self.state == State::Running && self.consumed > 0 && !self.starved {
                self.underruns = self.underruns.saturating_add(1);
                self.starved = true;
            }
            return;
        }
        self.starved = false;
        let need = self.resampler.input_needed(outputs);
        let take = available.min(need);
        self.read_frames(take as usize, scratch);
        let frames = &mut scratch.frames[..outputs];
        let (used, produced) = self
            .resampler
            .process(&scratch.samples, self.channels, frames);
        // A short take is the stream's tail (or a ring smaller than a
        // period): consume all of it, dropping the fraction of a frame the
        // converter still holds, so a drain always completes.
        let used = if take < need { take } else { used as u64 };
        self.consumed += used;
        if !self.muted && !self.gain.is_silent() {
            for (frame, out) in frames[..produced].iter().zip(acc.as_chunks_mut::<2>().0) {
                out[0] += self.gain.scale(i32::from(frame[0]));
                out[1] += self.gain.scale(i32::from(frame[1]));
            }
        }
        if self.marks.len() == MAX_MARKS {
            self.marks.pop_front();
        }
        self.marks.push_back(Mark {
            card_end,
            consumed: self.consumed,
        });
    }

    /// Copy `count` frames starting at `consumed` out of the ring into
    /// `scratch.samples`, splitting the copy where the ring wraps.
    fn read_frames(&self, count: usize, scratch: &mut Scratch) {
        let bytes = count * self.frame_bytes;
        scratch.bytes.clear();
        scratch.bytes.resize(bytes, 0);
        if let Some(ring) = &self.ring {
            let start = (self.consumed % self.ring_frames) as usize;
            let first = count.min(self.ring_frames as usize - start);
            let split = first * self.frame_bytes;
            let (head, tail) = scratch.bytes.split_at_mut(split);
            ring.read(start * self.frame_bytes, head);
            ring.read(0, tail);
        }
        scratch.samples.clear();
        scratch.samples.extend(
            scratch
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair)),
        );
    }

    /// The card has played up to `card_played`: advance `played`. Returns
    /// whether that completed a drain. A running stream that has now played
    /// everything it was mixed and has less than an output period of
    /// `outputs` frames waiting ran dry: one underrun per dry spell, even when
    /// no other stream keeps the mixer mixing (issue #453).
    pub(crate) fn resolve(&mut self, card_played: u64, outputs: usize) -> bool {
        while let Some(mark) = self.marks.front() {
            if mark.card_end > card_played {
                break;
            }
            self.played = mark.consumed;
            self.marks.pop_front();
        }
        let dry = self.committed - self.consumed < self.threshold(outputs);
        if self.state == State::Running
            && self.consumed > 0
            && self.marks.is_empty()
            && dry
            && !self.starved
        {
            self.underruns = self.underruns.saturating_add(1);
            self.starved = true;
        }
        self.finish_drain()
    }

    /// The card is gone: treat everything mixed as played.
    pub(crate) fn forget_card(&mut self) -> bool {
        self.played = self.consumed;
        self.marks.clear();
        self.finish_drain()
    }

    /// Whether the mixer should take this stream back: its owner has been
    /// silent for `ticks` and nothing of it is left to play. A stream never
    /// started has nothing playing even with frames committed, so those must
    /// not keep it alive.
    pub(crate) fn abandoned(&self, now: u64, ticks: u64) -> bool {
        let idle = match self.state {
            State::Draining => false,
            State::Idle => true,
            _ => self.committed == self.consumed && self.marks.is_empty(),
        };
        idle && now.saturating_sub(self.last_active) > ticks
    }
}
