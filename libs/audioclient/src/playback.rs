//! A blocking playback stream: open, write, drain, close, with the shared
//! ring and the commit arithmetic hidden.
//!
//! Frame *n* lives at ring byte `(n mod ring_frames) * frame_bytes`; the
//! stream never writes more than `ring_frames` ahead of what the service has
//! consumed (the `consumed` every `Commit` reports), so it never overwrites a
//! frame the service has not read. Writes split where the ring wraps. The
//! stream starts itself once a full ring is queued, so playback never starts
//! dry, or at [`PlaybackStream::drain`] for sounds shorter than the ring.

use crate::wire::{DIRECTION_PLAYBACK, FORMAT_S16_LE};
use crate::{Client, Error, Grant, Result, RingBuffer, Transport, TICK_HZ};

/// `EINVAL`: a write that is not a whole number of frames.
const EINVAL: i64 = 22;

/// Samples converted per piece when copying into the ring.
const PIECE_SAMPLES: usize = 512;

/// What to ask for. The service snaps to what it can do; the granted values
/// are in [`PlaybackStream::grant`].
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub rate: u32,
    pub channels: u32,
    /// Bytes per period; the ring is a few periods.
    pub period_bytes: u32,
    /// Ticks a write or drain may go without progress before it fails with
    /// [`Error::Stalled`].
    pub stall_ticks: u64,
}

impl Params {
    /// Interleaved `S16Le` at `rate` Hz with `channels` channels, 8 KiB
    /// periods, a 5 s stall limit.
    pub fn new(rate: u32, channels: u32) -> Params {
        Params {
            rate,
            channels,
            period_bytes: 8192,
            stall_ticks: 5 * TICK_HZ,
        }
    }

    pub fn period_bytes(mut self, bytes: u32) -> Params {
        self.period_bytes = bytes;
        self
    }

    pub fn stall_ticks(mut self, ticks: u64) -> Params {
        self.stall_ticks = ticks;
        self
    }
}

pub struct PlaybackStream<T: Transport> {
    client: Client<T>,
    grant: Grant,
    ring: T::Ring,
    ring_frames: u64,
    channels: usize,
    written: u64,
    consumed: u64,
    started: bool,
    open: bool,
    stall_ticks: u64,
}

impl<T: Transport> PlaybackStream<T> {
    /// Open an `S16Le` playback stream and attach a fresh ring to it. Fails
    /// with [`Error::Unsupported`] when the service would not grant `S16Le`.
    pub fn open(transport: T, params: Params) -> Result<PlaybackStream<T>> {
        let client = Client::new(transport);
        let grant = client.open_stream(
            DIRECTION_PLAYBACK,
            FORMAT_S16_LE,
            params.rate,
            params.channels,
            params.period_bytes,
        )?;
        // From here on the stream exists: give it back on any failure.
        let close = |error: Error| {
            let _ = client.close_stream(grant.stream);
            error
        };
        let channels = grant.channels as usize;
        let ring_bytes = grant.period_bytes.checked_mul(grant.periods).unwrap_or(0) as usize;
        let frame_bytes = 2 * channels;
        if grant.format != FORMAT_S16_LE
            || channels == 0
            || ring_bytes < frame_bytes
            || grant.rate == 0
        {
            return Err(close(Error::Unsupported));
        }
        let ring = client.transport().create_ring(ring_bytes).map_err(close)?;
        client
            .attach_ring(grant.stream, ring.share())
            .map_err(close)?;
        Ok(PlaybackStream {
            ring_frames: (ring_bytes / frame_bytes) as u64,
            client,
            grant,
            ring,
            channels,
            written: 0,
            consumed: 0,
            started: false,
            open: true,
            stall_ticks: params.stall_ticks,
        })
    }

    /// The parameters the service granted.
    pub fn grant(&self) -> &Grant {
        &self.grant
    }

    pub fn rate(&self) -> u32 {
        self.grant.rate
    }

    pub fn channels(&self) -> u32 {
        self.grant.channels
    }

    /// Frames handed to the service so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// The underlying typed client, for calls this type does not wrap.
    pub fn client(&self) -> &Client<T> {
        &self.client
    }

    /// Ask the service how far it has read (a `Commit` of the current total).
    fn refresh(&mut self) -> Result<()> {
        self.consumed = self.client.commit(self.grant.stream, self.written)?;
        Ok(())
    }

    /// Frames that can be written without blocking.
    pub fn free_frames(&mut self) -> Result<u64> {
        self.refresh()?;
        Ok(self.ring_frames - (self.written - self.consumed))
    }

    /// Write as many whole frames of `samples` (interleaved) as fit now;
    /// returns the frames written.
    pub fn try_write(&mut self, samples: &[i16]) -> Result<usize> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(Error::Errno(EINVAL));
        }
        let free = (self.ring_frames - (self.written - self.consumed)) as usize;
        let frames = free.min(samples.len() / self.channels);
        if frames == 0 {
            return Ok(0);
        }
        self.copy_in(&samples[..frames * self.channels]);
        self.written += frames as u64;
        self.refresh()?;
        if !self.started && self.written >= self.ring_frames {
            self.start()?;
        }
        Ok(frames)
    }

    /// Write all of `samples` (interleaved, whole frames), blocking while the
    /// ring is full.
    pub fn write(&mut self, samples: &[i16]) -> Result<()> {
        let mut rest = samples;
        let mut progress_at = self.client.transport().now();
        while !rest.is_empty() {
            let frames = self.try_write(rest)?;
            if frames > 0 {
                rest = &rest[frames * self.channels..];
                progress_at = self.client.transport().now();
                continue;
            }
            // The ring is full. A stream that has not started yet never
            // empties it, so start it now.
            if !self.started {
                self.start()?;
            }
            self.client.transport().sleep();
            let before = self.consumed;
            self.refresh()?;
            let now = self.client.transport().now();
            if self.consumed > before {
                progress_at = now;
            } else if now.saturating_sub(progress_at) > self.stall_ticks {
                return Err(Error::Stalled);
            }
        }
        Ok(())
    }

    /// Copy whole frames to the ring at the write position, splitting where
    /// the ring wraps.
    fn copy_in(&mut self, samples: &[i16]) {
        let frame_bytes = 2 * self.channels;
        let mut frame = self.written % self.ring_frames;
        for piece in samples.chunks(PIECE_SAMPLES / self.channels * self.channels) {
            let mut bytes = [0u8; 2 * PIECE_SAMPLES];
            for (pair, sample) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(piece) {
                *pair = sample.to_le_bytes();
            }
            let bytes = &bytes[..2 * piece.len()];
            let until_end = ((self.ring_frames - frame) as usize) * frame_bytes;
            let (head, tail) = bytes.split_at(bytes.len().min(until_end));
            self.ring.write(frame as usize * frame_bytes, head);
            self.ring.write(0, tail);
            frame = (frame + (piece.len() / self.channels) as u64) % self.ring_frames;
        }
    }

    /// Start playback now (it otherwise starts once a full ring is queued).
    pub fn start(&mut self) -> Result<()> {
        self.client.start(self.grant.stream)?;
        self.started = true;
        Ok(())
    }

    /// Scale this stream: 16.16 fixed point, 65536 is unity.
    pub fn set_volume(&mut self, gain_q16: u32) -> Result<()> {
        self.client.set_volume(self.grant.stream, gain_q16)
    }

    pub fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.client.set_mute(self.grant.stream, mute)
    }

    /// Frames actually played.
    pub fn position(&self) -> Result<u64> {
        self.client.position(self.grant.stream)
    }

    /// Play everything written, then return the frames played. The stream
    /// cannot be written to afterwards.
    pub fn drain(&mut self) -> Result<u64> {
        if !self.started {
            self.start()?;
        }
        let left = self.written.saturating_sub(self.position()?);
        let nominal = left * TICK_HZ / u64::from(self.grant.rate).max(1);
        let deadline = self.client.transport().now() + nominal + self.stall_ticks;
        self.drain_until(deadline)
    }

    /// Like [`drain`](Self::drain) with an absolute `deadline` (a transport
    /// tick) of the caller's choosing; fails with `ETIMEDOUT` at it.
    pub fn drain_until(&mut self, deadline: u64) -> Result<u64> {
        if !self.started {
            self.start()?;
        }
        self.client.drain(self.grant.stream, Some(deadline))?;
        self.position()
    }

    /// Drain, then close; returns the frames played.
    pub fn finish(mut self) -> Result<u64> {
        let played = self.drain();
        let closed = self.close_now();
        let played = played?;
        closed.map(|()| played)
    }

    /// Close at once, discarding whatever has not played.
    pub fn close(mut self) -> Result<()> {
        self.close_now()
    }

    fn close_now(&mut self) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        self.client.close_stream(self.grant.stream)
    }
}

impl<T: Transport> Drop for PlaybackStream<T> {
    fn drop(&mut self) {
        // A stream dropped after a failure must still be given back.
        let _ = self.close_now();
    }
}
