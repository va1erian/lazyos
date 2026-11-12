//! `beep starve=1`: starve a stream on purpose and check the mixer said so
//! on `system/audio/mixer/event` (issue #453).
//!
//! It subscribes first, then plays a short 880 Hz tone, lets the stream run
//! dry for a while (an underrun), plays a 660 Hz tone and drains. The events
//! of its own stream must be exactly one `Underrun`, exactly one `Drained`
//! and at least one `Period` (it is listening, so the mixer sends them).
//! The recording holds both tones with the gap between them, which the host
//! judges (`tools/sound/run.py --starve`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use audioclient::Params;
use libmessenger::client::Deadline;
use messenger_generated::os_lazy_audio_v1 as wire;
use pcm::tone::Tone;
use user::audio::PlaybackStream;
use user::central::Bus;
use user::messenger::topics_client;
use user::sys;

use super::common::{connect, fail};

/// Each tone, and the dry spell between them.
const TONE_MS: u64 = 300;
const GAP_NS: u64 = 700_000_000;
/// Events the subscription may hold between two looks.
const DEPTH: u32 = 64;
/// Ticks to keep listening after the drain for late events.
const SETTLE_TICKS: u64 = 50;

/// What the stream's events added up to.
#[derive(Default)]
struct Seen {
    underruns: u32,
    drained: u32,
    periods: u32,
}

/// Run the starve check; the `PASS` detail.
pub(super) fn run() -> Result<String, String> {
    let mut bus = Bus::connect().map_err(|error| format!("no broker: {error:?}"))?;
    let topic = wire::name_system_audio_event(user::audio_events::MIXER_CARD)
        .map_err(|_| String::from("topic name"))?;
    let sub = bus
        .subscribe_with_qos(&topic, topics_client::Qos::Buffered(DEPTH))
        .map_err(|error| format!("subscribe: {error:?}"))?;
    let audio = connect()?;
    let params = Params::new(48000, 2).period_bytes(4096);
    let mut out = user::audio::open(audio, params).map_err(fail("open"))?;
    let stream = out.grant().stream;
    let rate = out.rate();
    let mut seen = Seen::default();

    write_tone(&mut out, 880, rate)?;
    out.start().map_err(fail("start"))?;
    // Run dry: the mixer plays the tone out and finds nothing more.
    sys::sleep_ns(GAP_NS);
    collect(&sub, stream, &mut seen)?;
    write_tone(&mut out, 660, rate)?;
    out.finish().map_err(fail("drain"))?;
    let until = sys::clock() + SETTLE_TICKS;
    while sys::clock() < until {
        collect(&sub, stream, &mut seen)?;
        sys::nap();
    }
    let _ = sub.unsubscribe();
    let detail = format!(
        "stream={stream} underruns={} drained={} periods={}",
        seen.underruns, seen.drained, seen.periods
    );
    if seen.underruns == 1 && seen.drained == 1 && seen.periods > 0 {
        Ok(detail)
    } else {
        Err(format!("unexpected events: {detail}"))
    }
}

/// Queue `TONE_MS` of `freq` Hz.
fn write_tone(out: &mut PlaybackStream, freq: u32, rate: u32) -> Result<(), String> {
    let frames = u64::from(rate) * TONE_MS / 1000;
    let mut tone = Tone::new(freq, rate, 16000);
    let mut samples = Vec::with_capacity(2 * frames as usize);
    for _ in 0..frames {
        let sample = tone.next_sample();
        samples.extend([sample, sample]);
    }
    out.write(&samples).map_err(fail("write"))
}

/// Count every queued event of `stream`.
fn collect(sub: &user::central::Subscription, stream: u32, seen: &mut Seen) -> Result<(), String> {
    while let Some(event) = sub
        .recv(Deadline::Poll)
        .map_err(|error| format!("next event: {error:?}"))?
    {
        let Ok(event) = wire::decode_system_audio_event(&event.payload) else {
            continue;
        };
        if event.stream != stream {
            continue;
        }
        match event.kind {
            wire::EVENT_KIND_UNDERRUN => seen.underruns += 1,
            wire::EVENT_KIND_DRAINED => seen.drained += 1,
            wire::EVENT_KIND_PERIOD => seen.periods += 1,
            _ => {}
        }
    }
    Ok(())
}
