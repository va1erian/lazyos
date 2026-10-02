//! A blocking sink for interleaved stereo `i16`: a `libs/audioclient`
//! `PlaybackStream` on the system mixer (#451), so the player shares the
//! speakers with every other program.

use alloc::format;
use alloc::string::String;

use audioclient::{Error, Params, PlaybackStream};
use user::audio::{self, Native};

const RATE_HZ: u32 = 48000;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: u32 = 8192;
/// Ticks (100 Hz) to wait for the mixer to register.
const CONNECT_TICKS: u64 = 500;

pub struct Sink {
    stream: PlaybackStream<Native>,
}

fn fail(what: &str) -> impl Fn(Error) -> String + '_ {
    move |error| format!("{what}: {error}")
}

impl Sink {
    /// Connect (retrying while the mixer starts) and open a 48 kHz stereo
    /// stream. The mixer snaps parameters it cannot do; this fails unless it
    /// grants stereo.
    pub fn open() -> Result<Sink, String> {
        let native = audio::connect_wait(audio::NAME, CONNECT_TICKS)
            .map_err(|error| format!("no audio service: {error}"))?;
        let params = Params::new(RATE_HZ, CHANNELS).period_bytes(PERIOD_BYTES);
        let stream = PlaybackStream::open(native, params).map_err(fail("open"))?;
        if stream.channels() != CHANNELS {
            return Err(String::from("mixer did not grant stereo"));
        }
        Ok(Sink { stream })
    }

    /// The sample rate the mixer granted.
    pub fn rate(&self) -> u32 {
        self.stream.rate()
    }

    /// Frames handed to the mixer so far.
    pub fn frames(&self) -> u64 {
        self.stream.written()
    }

    /// Queue `samples` (interleaved stereo), blocking while the ring is full.
    pub fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        self.stream.write(samples).map_err(fail("write"))
    }

    /// Let everything queued play out, then give the stream back. Returns the
    /// frames the mixer reports played.
    pub fn finish(self) -> Result<u64, String> {
        self.stream.finish().map_err(fail("drain"))
    }
}
