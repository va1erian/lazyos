//! One playback stream on the device: negotiated parameters, driver-owned DMA
//! slots, and the transmit bookkeeping.
//!
//! The ring a client fills is *not* device memory. Each period the driver
//! copies committed samples into one of these DMA slots and queues it, so a
//! client that keeps scribbling on its ring cannot change bytes the device is
//! reading (`docs/driver-plan.md` section 3.4: driver rings stay driver-owned).

use pcm::tone::Tone;
use user::sys;
use virtio_snd::params::{self, Grant, ParamError, Request};
use virtio_snd::wire::PcmInfo;

use super::card::{Card, StreamOp, MAX_SLOTS};
use super::dma::Region;
use super::error::Error;

/// Ticks without a completion before a stream is declared stuck.
const STALL_TICKS: u64 = 500;

pub(super) struct Stream {
    pub(super) index: u32,
    pub(super) grant: Grant,
    /// Driver-owned staging: `periods` slots of `period_bytes`.
    dma: Region,
    /// Slots the device currently owns.
    busy: [bool; MAX_SLOTS],
    /// Frames each busy slot carries (a short final period plays fewer).
    slot_frames: [u32; MAX_SLOTS],
    next_slot: usize,
    /// Frames the device has reported played.
    pub(super) frames_done: u64,
}

impl Stream {
    /// Negotiate `request` against `info`, program the device and prepare it.
    pub(super) fn open(
        card: &mut Card,
        index: u32,
        info: &PcmInfo,
        request: &Request,
    ) -> Result<Stream, Error> {
        let dma = card.take_staging()?;
        let max_ring = dma.len().min(u32::MAX as usize) as u32;
        let grant = match params::grant(info, request, max_ring) {
            Ok(grant) if grant.periods as usize <= MAX_SLOTS => grant,
            outcome => {
                card.give_back(dma);
                return Err(match outcome {
                    Err(ParamError::Invalid) => Error::Params,
                    _ => Error::Unsupported,
                });
            }
        };
        let mut stream = Stream {
            index,
            grant,
            dma,
            busy: [false; MAX_SLOTS],
            slot_frames: [0; MAX_SLOTS],
            next_slot: 0,
            frames_done: 0,
        };
        if let Err(error) = stream.program(card) {
            card.give_back(stream.into_region());
            return Err(error);
        }
        Ok(stream)
    }

    /// Hand the DMA staging back (to [`Card::give_back`]) once the stream is
    /// finished with it. The device must be stopped first.
    pub(super) fn into_region(self) -> Region {
        self.dma
    }

    /// `SET_PARAMS` + `PREPARE`: also how a halted stream is made startable
    /// again, since a stopped virtio stream must be re-prepared.
    pub(super) fn program(&mut self, card: &mut Card) -> Result<(), Error> {
        let grant = self.grant;
        card.set_params(
            self.index,
            grant.buffer_bytes(),
            grant.period_bytes,
            grant.channels as u8,
            grant.virtio_format,
            grant.virtio_rate,
        )?;
        card.stream_op(StreamOp::Prepare, self.index)?;
        self.next_slot = 0;
        self.frames_done = 0;
        Ok(())
    }

    pub(super) fn start(&mut self, card: &mut Card) -> Result<(), Error> {
        card.stream_op(StreamOp::Start, self.index)
    }

    /// `STOP`, wait for the device to hand back everything queued, then
    /// `RELEASE`. The stream must be [`program`](Self::program)med to run again.
    pub(super) fn halt(&mut self, card: &mut Card) -> Result<(), Error> {
        card.stream_op(StreamOp::Stop, self.index)?;
        self.drain(card)?;
        card.stream_op(StreamOp::Release, self.index)
    }

    /// `RELEASE` a stream that was prepared but never started.
    pub(super) fn release(&mut self, card: &mut Card) -> Result<(), Error> {
        card.stream_op(StreamOp::Release, self.index)
    }

    /// Whether the device still owns any slot.
    pub(super) fn has_busy(&self) -> bool {
        self.busy.iter().any(|&busy| busy)
    }

    /// Collect finished transmits, freeing their slots; whether any finished.
    pub(super) fn reap(&mut self, card: &mut Card) -> Result<bool, Error> {
        let mut any = false;
        while let Some((slot, ok)) = card.reap()? {
            if !self.busy[slot] {
                return Err(Error::Virtio(virtio::Error::DeviceError));
            }
            self.busy[slot] = false;
            if !ok {
                return Err(Error::Status(0));
            }
            self.frames_done += u64::from(self.slot_frames[slot]);
            any = true;
        }
        Ok(any)
    }

    /// Wait until every queued period has been played.
    pub(super) fn drain(&mut self, card: &mut Card) -> Result<(), Error> {
        let mut deadline = sys::clock() + STALL_TICKS;
        while self.has_busy() {
            if self.reap(card)? {
                deadline = sys::clock() + STALL_TICKS;
            } else if sys::clock() >= deadline {
                return Err(Error::Timeout);
            } else {
                card.idle();
            }
        }
        Ok(())
    }

    /// The next slot in ring order if the device has handed it back.
    pub(super) fn try_slot(&mut self) -> Option<usize> {
        let slot = self.next_slot;
        if self.busy[slot] {
            return None;
        }
        self.next_slot = (slot + 1) % self.grant.periods as usize;
        Some(slot)
    }

    /// [`try_slot`](Self::try_slot), waiting for the device if all are in flight.
    fn wait_slot(&mut self, card: &mut Card) -> Result<usize, Error> {
        let mut deadline = sys::clock() + STALL_TICKS;
        loop {
            if let Some(slot) = self.try_slot() {
                return Ok(slot);
            }
            if self.reap(card)? {
                deadline = sys::clock() + STALL_TICKS;
            } else if sys::clock() >= deadline {
                return Err(Error::Timeout);
            } else {
                card.idle();
            }
        }
    }

    /// The bytes of `slot`, to fill before [`submit_slot`](Self::submit_slot).
    pub(super) fn slot_bytes(&mut self, slot: usize) -> Result<&mut [u8], Error> {
        let period = self.grant.period_bytes as usize;
        self.dma.bytes(slot * period, period)
    }

    /// Queue `slot` carrying its first `frames` frames.
    pub(super) fn submit_slot(
        &mut self,
        card: &mut Card,
        slot: usize,
        frames: u32,
    ) -> Result<(), Error> {
        let len = frames as usize * self.grant.frame_bytes() as usize;
        self.busy[slot] = true;
        self.slot_frames[slot] = frames;
        let period = self.grant.period_bytes as usize;
        if let Err(error) = card.submit(self.index, &self.dma, slot, period, len) {
            self.busy[slot] = false;
            return Err(error);
        }
        Ok(())
    }

    /// Synthesize `frames` frames of `tone` straight into the slots and play
    /// them, then drain. Returns the frames the device reported played.
    pub(super) fn play_tone(
        &mut self,
        card: &mut Card,
        tone: &mut Tone,
        frames: u64,
    ) -> Result<u64, Error> {
        if self.grant.format != params::audio_format::S16_LE {
            return Err(Error::Params);
        }
        let channels = self.grant.channels as usize;
        let mut remaining = frames;
        while remaining > 0 {
            let slot = self.wait_slot(card)?;
            let written = tone.fill_s16le(self.slot_bytes(slot)?, channels) as u64;
            // A short final period plays only its real frames.
            let take = written.min(remaining);
            self.submit_slot(card, slot, take as u32)?;
            remaining -= take;
        }
        self.drain(card)?;
        Ok(self.frames_done)
    }
}
