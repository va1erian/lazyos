//! A blocking sink for interleaved stereo `i16` over `os.lazy.audio.v1`.
//!
//! This is the small seam between the player and the audio driver: `write`
//! hands over samples and blocks while the ring is full. It is the part
//! `libs/audioclient` (#451) will replace; keeping it behind this one type
//! makes that a one-file change.

use alloc::format;
use alloc::string::String;
use core::ptr;

use user::messenger::audio::{self as api, Client, Grant};
use user::sys;

const RATE_HZ: u32 = 48000;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: u32 = 8192;
/// Bytes per stereo S16 frame.
const FRAME_BYTES: usize = 4;
/// Ticks (100 Hz) a write may wait for ring space before giving up.
const STALL_TICKS: u64 = 500;
/// Ticks to wait for the last audio to play after the song ends.
const DRAIN_SLACK_TICKS: u64 = 500;

pub struct Sink {
    client: Client,
    grant: Grant,
    ring: *mut u8,
    ring_frames: u64,
    written: u64,
    consumed: u64,
    started: bool,
    closed: bool,
}

fn fail(what: &str) -> impl Fn(user::messenger::Error) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

impl Sink {
    /// Connect (retrying while the driver starts) and open a 48 kHz stereo
    /// S16 playback stream. The driver snaps parameters it cannot do; this
    /// fails if it would not grant stereo S16.
    pub fn open() -> Result<Sink, String> {
        let deadline = sys::clock() + 500;
        let client = loop {
            match Client::connect() {
                Ok(client) => break client,
                Err(error) if sys::clock() >= deadline => {
                    return Err(format!("no audio service: {}", error.message()))
                }
                Err(_) => nap(),
            }
        };
        client.info().map_err(fail("info"))?;
        let grant = client
            .open_stream(api::PLAYBACK, api::S16_LE, RATE_HZ, CHANNELS, PERIOD_BYTES)
            .map_err(fail("open"))?;
        if grant.format != api::S16_LE || grant.channels != CHANNELS {
            let _ = client.close_stream(grant.stream);
            return Err(String::from("driver did not grant stereo S16"));
        }
        let ring_bytes = grant.period_bytes as usize * grant.periods as usize;
        let (handle, va) = match sys::display_create_buffer(ring_bytes as u64) {
            Ok(buffer) => buffer,
            Err(code) => {
                let _ = client.close_stream(grant.stream);
                return Err(format!("ring allocation failed (errno {code})"));
            }
        };
        if let Err(error) = client.attach_ring(grant.stream, handle, ring_bytes as u64) {
            let _ = client.close_stream(grant.stream);
            return Err(fail("attach")(error));
        }
        Ok(Sink {
            client,
            ring: va as *mut u8,
            ring_frames: (ring_bytes / FRAME_BYTES) as u64,
            grant,
            written: 0,
            consumed: 0,
            started: false,
            closed: false,
        })
    }

    /// The sample rate the driver granted.
    pub fn rate(&self) -> u32 {
        self.grant.rate
    }

    /// Frames handed to the driver so far.
    pub fn frames(&self) -> u64 {
        self.written
    }

    /// Queue `samples` (interleaved stereo), blocking while the ring is full.
    pub fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let mut rest = samples;
        let deadline = sys::clock() + STALL_TICKS;
        while !rest.is_empty() {
            let free = self.ring_frames - (self.written - self.consumed);
            if free == 0 {
                // Let the device play some, then look again.
                nap();
                self.consumed = self
                    .client
                    .commit(self.grant.stream, self.written)
                    .map_err(fail("commit"))?;
                if sys::clock() > deadline {
                    return Err(String::from("timed out waiting for ring space"));
                }
                continue;
            }
            let frames = (rest.len() / 2).min(free as usize);
            self.copy_in(&rest[..frames * 2]);
            rest = &rest[frames * 2..];
            self.written += frames as u64;
            self.consumed = self
                .client
                .commit(self.grant.stream, self.written)
                .map_err(fail("commit"))?;
            // Start once a full ring is queued, so the device never starts dry.
            if !self.started && self.written >= self.ring_frames {
                self.start()?;
            }
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        self.client
            .start(self.grant.stream)
            .map_err(fail("start"))?;
        self.started = true;
        Ok(())
    }

    /// Copy `samples` into the ring at the current write position, splitting
    /// the copy where it wraps.
    fn copy_in(&mut self, samples: &[i16]) {
        let start = (self.written % self.ring_frames) as usize;
        let first = (self.ring_frames as usize - start).min(samples.len() / 2);
        self.copy_frames(start, &samples[..first * 2]);
        self.copy_frames(0, &samples[first * 2..]);
    }

    fn copy_frames(&mut self, frame: usize, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let bytes = samples.len() * 2;
        // SAFETY: `frame + samples.len() / 2 <= ring_frames` by the callers
        // (the copy was split at the ring end), so the destination range lies
        // inside the mapping `display_create_buffer` returned; `i16` has no
        // padding and the ring is not otherwise accessed by this task.
        unsafe {
            ptr::copy_nonoverlapping(
                samples.as_ptr() as *const u8,
                self.ring.add(frame * FRAME_BYTES),
                bytes,
            )
        };
    }

    /// Let everything queued play out, then give the stream back. Returns the
    /// frames the driver reports played.
    pub fn finish(mut self) -> Result<u64, String> {
        if !self.started {
            self.start()?;
        }
        let nominal_ticks = self.written * 100 / u64::from(self.grant.rate);
        let deadline = sys::clock() + nominal_ticks + DRAIN_SLACK_TICKS;
        let drained = self
            .client
            .drain(self.grant.stream, Some(deadline))
            .map_err(fail("drain"));
        let played = self
            .client
            .position(self.grant.stream)
            .map_err(fail("position"));
        // Always give the stream back, even after a failure.
        let _ = self.client.close_stream(self.grant.stream);
        self.closed = true;
        drained?;
        played
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        // A failed write drops the sink without `finish`: give the stream back.
        if !self.closed {
            let _ = self.client.close_stream(self.grant.stream);
        }
    }
}

fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}
