//! The stream table and the mixing pass.
//!
//! Every stream call names a stream and the caller; the caller must be the
//! stream's owner (the kernel-stamped sender of `OpenStream`, which `audiod`
//! passes in), or the call fails with [`MixError::Access`]. The control panel
//! calls (`os.lazy.audio.mixer.v1`) pass no owner: they may change a stream's
//! volume, never feed, stop or close it.

use alloc::vec::Vec;

use virtio_snd::params::{Grant, ParamError, Request};

use crate::gain::{saturate, Gain};
use crate::grant::{self, MAX_RING_BYTES};
use crate::resample::Resampler;
use crate::stream::{Scratch, State, Stream};
use crate::{Ring, MIX_CHANNELS};

/// Why a call was refused; `audiod` maps each onto an errno.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixError {
    /// Malformed request or a call out of order (`EINVAL`).
    Invalid,
    /// No such stream (`EINVAL` on the stream interface, `ENOENT` on the
    /// control interface).
    NotFound,
    /// Someone else's stream (`EACCES`).
    Access,
    /// Stream table full, or a second ring (`EBUSY`).
    Busy,
    /// Nothing close to the request can be provided (`ENOTSUP`).
    Unsupported,
}

/// The mixer's fixed parameters.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The mix (and card) rate in Hz.
    pub rate: u32,
    /// Frames in one output period.
    pub period_frames: usize,
    /// Streams open at once, across every client.
    pub max_streams: usize,
    /// Streams one owner may hold at once.
    pub max_per_owner: usize,
    /// Owner silence after which an idle stream is taken back.
    pub reclaim_ticks: u64,
}

impl Config {
    /// The defaults `audiod` runs with, at the card's `rate`.
    pub fn new(rate: u32, period_frames: usize) -> Config {
        Config {
            rate,
            period_frames,
            max_streams: 16,
            max_per_owner: 4,
            // 10 s at the 100 Hz PIT, like the driver.
            reclaim_ticks: 1000,
        }
    }
}

/// One stream as the control panel lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub id: u32,
    pub owner: u64,
    pub state: State,
    pub rate: u32,
    pub channels: u32,
    pub gain: Gain,
    pub muted: bool,
    pub played: u64,
    pub underruns: u32,
}

pub struct Mixer<R> {
    config: Config,
    streams: Vec<Stream<R>>,
    next_id: u32,
    master: Gain,
    master_muted: bool,
    acc: Vec<i64>,
    scratch: Scratch,
}

type Result<T> = core::result::Result<T, MixError>;

impl<R: Ring> Mixer<R> {
    pub fn new(config: Config) -> Self {
        Mixer {
            config,
            streams: Vec::with_capacity(config.max_streams),
            next_id: 1,
            master: Gain::UNITY,
            master_muted: false,
            acc: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Streams open now.
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Open a stream for `owner`; returns its id and grant. The grant follows the driver's policy, with
    /// the period raised when needed so the ring holds at least two output
    /// periods of input (a high-rate client with a tiny ring would otherwise
    /// play in bursts).
    pub fn open(&mut self, owner: u64, request: &Request, now: u64) -> Result<(u32, Grant)> {
        let mut grant = grant::grant(request).map_err(|error| match error {
            ParamError::Invalid => MixError::Invalid,
            ParamError::Unsupported => MixError::Unsupported,
        })?;
        if self.streams.len() >= self.config.max_streams
            || self.owned_by(owner) >= self.config.max_per_owner
        {
            return Err(MixError::Busy);
        }
        self.widen_period(&mut grant);
        let id = self.allocate_id();
        let stream = Stream::new(id, owner, grant, self.config.rate, now);
        self.streams.push(stream);
        Ok((id, grant))
    }

    fn owned_by(&self, owner: u64) -> usize {
        self.streams.iter().filter(|s| s.owner == owner).count()
    }

    fn widen_period(&self, grant: &mut Grant) {
        let frame = grant.frame_bytes().max(1);
        let resampler = Resampler::new(grant.rate_hz, self.config.rate);
        // A converting stream may pop one frame more than the estimate from
        // its current fraction.
        let need = resampler.input_needed(self.config.period_frames)
            + u64::from(!resampler.is_passthrough());
        let wanted = (2 * need).div_ceil(u64::from(grant.periods.max(1)));
        let ceiling = u64::from(MAX_RING_BYTES / grant.periods.max(1) / frame);
        let frames = wanted.min(ceiling) as u32;
        grant.period_bytes = grant.period_bytes.max(frames * frame);
    }

    /// The next free id: never 0, never one in use.
    fn allocate_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.streams.iter().any(|s| s.id == id) {
                return id;
            }
        }
    }

    /// The stream `id`, checked against `owner` when one is given; an owner's
    /// call also resets its reclaim timer.
    fn stream(&mut self, id: u32, owner: Option<u64>, now: u64) -> Result<&mut Stream<R>> {
        let stream = self
            .streams
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(MixError::NotFound)?;
        if let Some(owner) = owner {
            if stream.owner != owner {
                return Err(MixError::Access);
            }
            stream.touch(now);
        }
        Ok(stream)
    }

    pub fn attach(&mut self, id: u32, owner: u64, ring: R, now: u64) -> Result<()> {
        self.stream(id, Some(owner), now)?.attach(ring)
    }

    pub fn commit(&mut self, id: u32, owner: u64, written: u64, now: u64) -> Result<u64> {
        self.stream(id, Some(owner), now)?.commit(written)
    }

    pub fn start(&mut self, id: u32, owner: u64, now: u64) -> Result<()> {
        self.stream(id, Some(owner), now)?.start()
    }

    pub fn stop(&mut self, id: u32, owner: u64, now: u64) -> Result<()> {
        self.stream(id, Some(owner), now)?.stop();
        Ok(())
    }

    /// Begin draining; `Ok(true)` when it is already complete, otherwise the
    /// completion is reported by [`Mixer::played`].
    pub fn drain(&mut self, id: u32, owner: u64, now: u64) -> Result<bool> {
        self.stream(id, Some(owner), now)?.drain()
    }

    pub fn position(&mut self, id: u32, owner: u64, now: u64) -> Result<u64> {
        Ok(self.stream(id, Some(owner), now)?.played)
    }

    /// Close a stream; its ring is dropped (and so unmapped by `audiod`).
    pub fn close(&mut self, id: u32, owner: u64, now: u64) -> Result<()> {
        self.stream(id, Some(owner), now)?;
        self.streams.retain(|s| s.id != id);
        Ok(())
    }

    /// Set a stream's gain; `owner` is `None` for the control panel.
    pub fn set_volume(
        &mut self,
        id: u32,
        owner: Option<u64>,
        gain_q16: u32,
        now: u64,
    ) -> Result<()> {
        let gain = Gain::new(gain_q16).ok_or(MixError::Invalid)?;
        self.stream(id, owner, now)?.gain = gain;
        Ok(())
    }

    pub fn set_mute(&mut self, id: u32, owner: Option<u64>, mute: bool, now: u64) -> Result<()> {
        self.stream(id, owner, now)?.muted = mute;
        Ok(())
    }

    pub fn master(&self) -> (Gain, bool) {
        (self.master, self.master_muted)
    }

    pub fn set_master(&mut self, gain_q16: u32, mute: bool) -> Result<()> {
        self.master = Gain::new(gain_q16).ok_or(MixError::Invalid)?;
        self.master_muted = mute;
        Ok(())
    }

    /// Every open stream, in opening order.
    pub fn statuses(&self) -> impl Iterator<Item = Status> + '_ {
        self.streams.iter().map(|s| Status {
            id: s.id,
            owner: s.owner,
            state: s.state,
            rate: s.grant.rate_hz,
            channels: s.grant.channels,
            gain: s.gain,
            muted: s.muted,
            played: s.played,
            underruns: s.underruns,
        })
    }

    /// Whether some stream can fill an output period now: the card should be
    /// fed (and started, if it is not running).
    pub fn wants_output(&self) -> bool {
        let outputs = self.config.period_frames;
        self.streams.iter().any(|s| s.wants_output(outputs))
    }

    /// Whether mixed audio is still on its way to the speaker: the card must
    /// keep running until [`Mixer::played`] has resolved it.
    pub fn in_flight(&self) -> bool {
        self.streams.iter().any(Stream::in_flight)
    }

    /// Mix one output period into `out` (interleaved stereo `S16Le` samples;
    /// a trailing odd sample is zeroed). `card_end` is the card's frame count
    /// once this period has played.
    pub fn mix(&mut self, out: &mut [i16], card_end: u64) {
        let outputs = out.len() / MIX_CHANNELS;
        self.acc.clear();
        self.acc.resize(outputs * MIX_CHANNELS, 0);
        if self.scratch.frames.len() < outputs {
            self.scratch.frames.resize(outputs, [0; 2]);
        }
        for stream in &mut self.streams {
            stream.mix_into(&mut self.acc, outputs, card_end, &mut self.scratch);
        }
        let master = if self.master_muted {
            Gain::SILENT
        } else {
            self.master
        };
        for (sample, &mixed) in out.iter_mut().zip(self.acc.iter()) {
            *sample = saturate(master.scale_wide(mixed));
        }
        for sample in out.iter_mut().skip(self.acc.len()) {
            *sample = 0;
        }
    }

    /// The card has played `card_played` frames since it started: advance
    /// every stream's position and report each drain that completed.
    pub fn played(&mut self, card_played: u64, mut drained: impl FnMut(u32)) {
        let outputs = self.config.period_frames;
        for stream in &mut self.streams {
            if stream.resolve(card_played, outputs) {
                drained(stream.id);
            }
        }
    }

    /// The card stopped or vanished: everything mixed counts as played.
    pub fn forget_card(&mut self, mut drained: impl FnMut(u32)) {
        for stream in &mut self.streams {
            if stream.forget_card() {
                drained(stream.id);
            }
        }
    }

    /// Close every stream whose owner has gone silent with nothing left to
    /// play, reporting each.
    pub fn reclaim(&mut self, now: u64, mut reclaimed: impl FnMut(u32)) {
        let ticks = self.config.reclaim_ticks;
        self.streams.retain(|s| {
            let keep = !s.abandoned(now, ticks);
            if !keep {
                reclaimed(s.id);
            }
            keep
        });
    }
}
