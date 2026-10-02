//! The mixer's hold on the card: the driver's one stream, opened through
//! `libs/audioclient` like any client would, and fed one mixed period at a
//! time.
//!
//! Pacing follows the card's own position: the mixer keeps [`LEAD_PERIODS`]
//! periods queued beyond what the card has played (about 64 ms at 48 kHz),
//! so a late wakeup costs latency, not a gap. The card starts once the first
//! periods are queued and stops after [`IDLE_STOP_TICKS`] with nothing to
//! play; the stream itself stays open for as long as `audiod` runs, so no
//! other task can take the card from the mixer.

use alloc::vec;
use alloc::vec::Vec;

use audioclient::{wire, Client, Error, Result, RingBuffer, Transport};
use audiomix::Mixer;
use user::audio::{Native, NativeRing};

use super::ring::MappedRing;

/// What the mixer asks the card for: 48 kHz stereo `S16Le` in periods of
/// 1024 frames (21 ms).
const RATE_HZ: u32 = 48000;
const PERIOD_FRAMES: u32 = 1024;
/// Bytes in one stereo `S16Le` frame.
const FRAME_BYTES: u32 = 4;
/// Periods queued ahead of the card's position.
const LEAD_PERIODS: u64 = 3;
/// Ticks with nothing to play before the card is stopped.
const IDLE_STOP_TICKS: u64 = 50;
/// The driver takes back a stream whose owner is silent for 10 s; a stopped
/// card is touched well before that.
const KEEPALIVE_TICKS: u64 = 300;

pub(super) struct Card {
    client: Client<Native>,
    stream: u32,
    ring: NativeRing,
    ring_frames: u64,
    period_frames: u64,
    rate: u32,
    /// Frames committed to / consumed by / played by the card since it
    /// last started.
    written: u64,
    consumed: u64,
    played: u64,
    running: bool,
    idle_since: Option<u64>,
    last_call: u64,
    period: Vec<i16>,
}

impl Card {
    /// Open the card's stream and attach the mixer's ring to it.
    pub(super) fn open(now: u64) -> Result<Card> {
        let client = Client::new(Native::connect(audioclient::CARD_NAME)?);
        let grant = client.open_stream(
            wire::DIRECTION_PLAYBACK,
            wire::FORMAT_S16_LE,
            RATE_HZ,
            2,
            PERIOD_FRAMES * FRAME_BYTES,
        )?;
        let close = |error: Error| {
            let _ = client.close_stream(grant.stream);
            error
        };
        // The engine mixes stereo `S16Le`; anything else is not this card.
        if grant.format != wire::FORMAT_S16_LE || grant.channels != 2 || grant.periods == 0 {
            return Err(close(Error::Unsupported));
        }
        let period_frames = u64::from(grant.period_bytes / FRAME_BYTES);
        let ring_bytes = (grant.period_bytes * grant.periods) as usize;
        let ring = client.transport().create_ring(ring_bytes).map_err(close)?;
        client
            .attach_ring(grant.stream, ring.share())
            .map_err(close)?;
        Ok(Card {
            stream: grant.stream,
            ring_frames: period_frames * u64::from(grant.periods),
            period_frames,
            rate: grant.rate,
            client,
            ring,
            written: 0,
            consumed: 0,
            played: 0,
            running: false,
            idle_since: None,
            last_call: now,
            period: vec![0; 2 * period_frames as usize],
        })
    }

    pub(super) fn rate(&self) -> u32 {
        self.rate
    }

    pub(super) fn period_frames(&self) -> usize {
        self.period_frames as usize
    }

    /// Whether the card is playing (the serve loop then wakes every tick).
    pub(super) fn running(&self) -> bool {
        self.running
    }

    /// One pacing step: learn how far the card got, resolve the streams'
    /// positions (`drained` hears of finished drains), queue mixed periods up
    /// to the lead, and start or stop the card.
    pub(super) fn pump(
        &mut self,
        mixer: &mut Mixer<MappedRing>,
        now: u64,
        drained: impl FnMut(u32),
    ) -> Result<()> {
        if self.running {
            self.consumed = self.client.commit(self.stream, self.written)?;
            self.played = self.client.position(self.stream)?;
            self.last_call = now;
            mixer.played(self.played, drained);
        }
        let lead = LEAD_PERIODS * self.period_frames;
        let mut fed = false;
        while mixer.wants_output()
            && self.written - self.played < lead
            && self.written + self.period_frames - self.consumed <= self.ring_frames
        {
            let end = self.written + self.period_frames;
            mixer.mix(&mut self.period, end);
            self.queue_period();
            self.written = end;
            fed = true;
        }
        if fed {
            self.consumed = self.client.commit(self.stream, self.written)?;
            if !self.running {
                self.client.start(self.stream)?;
                self.running = true;
            }
            self.idle_since = None;
            self.last_call = now;
        } else if self.running && !mixer.in_flight() && self.played >= self.written {
            let since = *self.idle_since.get_or_insert(now);
            if now.saturating_sub(since) > IDLE_STOP_TICKS {
                self.client.stop(self.stream)?;
                // The driver restarts its numbering at 0 after a stop.
                self.running = false;
                self.written = 0;
                self.consumed = 0;
                self.played = 0;
                self.idle_since = None;
            }
        }
        if !self.running && now.saturating_sub(self.last_call) > KEEPALIVE_TICKS {
            self.client.position(self.stream)?;
            self.last_call = now;
        }
        Ok(())
    }

    /// Copy the mixed period into the card's ring at `written`. Periods are
    /// whole and the ring is a whole number of them, so a period never wraps.
    fn queue_period(&mut self) {
        let offset = (self.written % self.ring_frames) as usize * FRAME_BYTES as usize;
        let mut bytes = [0u8; 1024];
        let mut at = offset;
        for piece in self.period.chunks(bytes.len() / 2) {
            for (pair, sample) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(piece) {
                *pair = sample.to_le_bytes();
            }
            let len = 2 * piece.len();
            self.ring.write(at, &bytes[..len]);
            at += len;
        }
    }

    /// Give the card back (best effort: the driver also reclaims it).
    pub(super) fn close(self) {
        let _ = self.client.close_stream(self.stream);
    }
}
