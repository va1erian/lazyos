//! `beep` (`BEEP.ELF`): the smallest audio client, and the sound harness's
//! evidence program.
//!
//! It plays a sine tone through the system mixer (`audiod`, `os.lazy.audio`)
//! the way any application should: a `libs/audioclient` `PlaybackStream`
//! that it writes samples into and finally drains. It prints
//! `BEEP:PLAY:PASS` when the mixer reports the whole tone played; whether it
//! was *audible* is judged by the host from QEMU's recording
//! (`tools/sound/run.py`).
//!
//! Usage: `beep [freq_hz [ms [volume%]]]` from the desktop Terminal (default
//! 880 Hz for 800 ms at full volume); the `freq=<Hz>`, `ms=<milliseconds>` and
//! `volume=<percent>` spellings also work. Several `beep`s at once are mixed.
//!
//! Three more modes exercise the mixer the way the boot demo's evidence needs:
//! `probe=1` sends malformed and hostile requests (`BEEP:PROBE:PASS`),
//! `role=intruder stream=<id>` is its second task, refused on the owner's
//! stream (`BEEP:INTRUDER:PASS`), and `soak=<n>` runs `n` open/play/close
//! cycles of silence (`BEEP:SOAK:PASS`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use audioclient::{Params, PlaybackStream, UNITY_GAIN};
use pcm::tone::Tone;
use user::sys;

#[path = "beep/common.rs"]
mod common;
#[path = "beep/probe.rs"]
mod probe;
#[path = "beep/soak.rs"]
mod soak;

use common::{connect, fail};

const DEFAULT_FREQ_HZ: u32 = 880;
const DEFAULT_MS: u32 = 800;
const RATE_HZ: u32 = 48000;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: u32 = 8192;
/// Peak level: about half of full scale.
const AMPLITUDE_Q15: i32 = 16000;
/// Frames synthesized per write.
const CHUNK_FRAMES: usize = 1024;

/// What to do.
enum Mode {
    Tone,
    Probe,
    /// The probe's second task; `stream` is the owner's stream id.
    Intruder {
        stream: u32,
    },
    Soak(u32),
}

struct Args {
    freq_hz: u32,
    ms: u32,
    /// Stream volume in percent of unity (`SetVolume`), 100 by default.
    volume: u32,
    mode: Mode,
}

fn parse_args() -> Args {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut args = Args {
        freq_hz: DEFAULT_FREQ_HZ,
        ms: DEFAULT_MS,
        volume: 100,
        mode: Mode::Tone,
    };
    let mut intruder = false;
    let mut owner_stream = 0;
    // Bare numbers are the shell spelling: `beep 440 500`.
    let mut bare = 0;
    for part in text.split_whitespace() {
        if let Ok(value) = part.parse::<u32>() {
            match bare {
                0 => args.freq_hz = value.clamp(20, 20000),
                1 => args.ms = value.clamp(50, 10_000),
                2 => args.volume = value.min(400),
                _ => {}
            }
            bare += 1;
            continue;
        }
        if matches!(part, "-h" | "--help" | "help") {
            sys::write_str(
                "usage: beep [freq_hz [ms [volume%]]]   (20-20000 Hz, 50-10000 ms, 0-400 %)\n",
            );
            sys::exit(0);
        }
        match part.split_once('=') {
            Some(("freq", value)) => {
                args.freq_hz = value.parse().unwrap_or(DEFAULT_FREQ_HZ).clamp(20, 20000)
            }
            Some(("ms", value)) => args.ms = value.parse().unwrap_or(DEFAULT_MS).clamp(50, 10_000),
            Some(("volume", value)) => args.volume = value.parse().unwrap_or(100).min(400),
            Some(("probe", "1")) => args.mode = Mode::Probe,
            Some(("role", "intruder")) => intruder = true,
            Some(("stream", value)) => owner_stream = value.parse().unwrap_or(0),
            // Bounded so a hostile argument cannot pin the card for minutes.
            Some(("soak", value)) => {
                args.mode = Mode::Soak(value.parse().unwrap_or(0).clamp(1, 200))
            }
            _ => {}
        }
    }
    if intruder {
        args.mode = Mode::Intruder {
            stream: owner_stream,
        };
    }
    args
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let args = parse_args();
    let (marker, outcome) = match args.mode {
        Mode::Tone => ("BEEP:PLAY", play(&args)),
        Mode::Probe => (
            "BEEP:PROBE",
            probe::run()
                .map(|checks| sys::write_str(&format!("BEEP:PROBE:PASS checks={checks}\n"))),
        ),
        Mode::Intruder { stream } => (
            "BEEP:INTRUDER",
            probe::run_intruder(stream).map(|()| sys::write_str("BEEP:INTRUDER:PASS\n")),
        ),
        Mode::Soak(rounds) => (
            "BEEP:SOAK",
            soak::run(rounds)
                .map(|done| sys::write_str(&format!("BEEP:SOAK:PASS iterations={done}\n"))),
        ),
    };
    match outcome {
        Ok(()) => sys::exit(0),
        Err(reason) => {
            sys::write_str(&format!("{marker}:FAIL {reason}\n"));
            sys::exit(1)
        }
    }
}

fn play(args: &Args) -> Result<(), String> {
    let audio = connect()?;
    let params = Params::new(RATE_HZ, CHANNELS).period_bytes(PERIOD_BYTES);
    let mut out = PlaybackStream::open(&audio, params).map_err(fail("open"))?;
    if args.volume != 100 {
        out.set_volume(args.volume * UNITY_GAIN / 100)
            .map_err(fail("volume"))?;
    }
    let (rate, channels) = (out.rate(), out.channels() as usize);
    let total = u64::from(rate) * u64::from(args.ms) / 1000;
    let mut tone = Tone::new(args.freq_hz, rate, AMPLITUDE_Q15);
    let mut chunk = Vec::with_capacity(CHUNK_FRAMES * channels);
    let mut written = 0u64;
    while written < total {
        let frames = (total - written).min(CHUNK_FRAMES as u64) as usize;
        chunk.clear();
        for _ in 0..frames {
            let sample = tone.next_sample();
            chunk.extend(core::iter::repeat_n(sample, channels));
        }
        out.write(&chunk).map_err(fail("write"))?;
        written += frames as u64;
    }
    // Dropping the stream on an error path closes it; `finish` drains first.
    let played = out.finish().map_err(fail("drain"))?;
    if played != total {
        return Err(format!("mixer played {played} of {total} frames"));
    }
    sys::write_str(&format!(
        "BEEP:PLAY:PASS freq={} rate={rate} channels={channels} volume={} frames={played}\n",
        args.freq_hz, args.volume
    ));
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
