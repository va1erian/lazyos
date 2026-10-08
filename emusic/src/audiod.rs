//! emusic's [`Output`] over the system mixer: one `audiod` playback stream
//! (`os.lazy.audio.v1`, docs/audio-plan.md) per output, through
//! `audioclient::PlaybackStream` on the musl transport.

use audioclient::UNITY_GAIN;
use emusic_lazyaudio::output::{Error, Output, OutputFactory, Result};
use xui_app::platform::audio::{Audio, Params, PlaybackStream};

/// The loudest gain `audiod` takes: four times unity.
const MAX_GAIN_Q16: u32 = 4 * UNITY_GAIN;

/// Opens `audiod` streams.
pub struct Audiod;

impl OutputFactory for Audiod {
    fn open(&self, rate: u32, channels: u16) -> Result<Box<dyn Output>> {
        tracing::debug!(rate, channels, "opening an audiod stream");
        let transport = Audio::try_connect().ok_or_else(|| Error("no sound card".into()))?;
        tracing::debug!("audiod resolved");
        let stream = PlaybackStream::open(transport, Params::new(rate, u32::from(channels)))
            .map_err(failed)?;
        let grant = stream.grant();
        tracing::debug!(?grant, "audiod stream open");
        let frame_bytes = 2 * grant.channels.max(1);
        let period_frames = u64::from(grant.period_bytes / frame_bytes).max(1);
        let queue_frames = period_frames * u64::from(grant.periods);
        Ok(Box::new(AudiodOutput {
            stream,
            period_frames,
            queue_frames,
            started: false,
        }))
    }
}

/// One `audiod` stream.
struct AudiodOutput {
    stream: PlaybackStream<Audio>,
    period_frames: u64,
    /// Frames the stream's ring holds: it starts itself once that many are
    /// written.
    queue_frames: u64,
    started: bool,
}

impl Output for AudiodOutput {
    fn rate(&self) -> u32 {
        self.stream.rate()
    }

    fn channels(&self) -> u16 {
        self.stream.channels() as u16
    }

    fn period_frames(&self) -> u64 {
        self.period_frames
    }

    fn try_write(&mut self, samples: &[i16]) -> Result<usize> {
        let written = self.stream.try_write(samples).map_err(failed)?;
        if written > 0 {
            return Ok(written);
        }
        // `try_write` sizes the free space from the consumed count of the
        // last commit; a full ring stays full until something asks `audiod`
        // again, which `free_frames` does.
        if self.stream.free_frames().map_err(failed)? == 0 {
            return Ok(0);
        }
        self.stream.try_write(samples).map_err(failed)
    }

    fn start(&mut self) -> Result<()> {
        // A second `Start` would restart the stream's position count, so
        // start only a stream that has not started itself on a full ring.
        if self.started || self.stream.written() >= self.queue_frames {
            return Ok(());
        }
        self.stream.start().map_err(failed)?;
        self.started = true;
        Ok(())
    }

    fn played(&mut self) -> Result<u64> {
        let played = self.stream.position().map_err(failed);
        tracing::trace!(?played, written = self.stream.written(), "audiod position");
        played
    }

    fn set_volume(&mut self, gain: f32) -> Result<()> {
        let q16 = (gain.max(0.0) * UNITY_GAIN as f32).round() as u32;
        self.stream
            .set_volume(q16.min(MAX_GAIN_Q16))
            .map_err(failed)
    }
}

fn failed(error: audioclient::Error) -> Error {
    tracing::warn!(?error, "audiod call failed");
    Error(format!("{error:?}"))
}
