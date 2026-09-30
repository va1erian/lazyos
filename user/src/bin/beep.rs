//! `beep` (`BEEP.ELF`): the smallest audio client, and the sound harness's
//! evidence program.
//!
//! It plays a sine tone through the audio driver's `os.lazy.audio.v1`
//! interface, the way any application would: open a stream, share a ring,
//! write samples ahead of the driver, commit, start, drain, close. It prints
//! `BEEP:PLAY:PASS` when the driver reports the whole tone played; whether it
//! was *audible* is judged by the host from QEMU's recording
//! (`tools/sound/run.py`).
//!
//! Usage: `beep [freq_hz [ms]]` from the desktop Terminal (default 880 Hz for
//! 800 ms); the `freq=<Hz>` and `ms=<milliseconds>` spellings also work.
//!
//! Three more modes exercise the driver the way the boot demo's evidence needs:
//! `probe=1` sends malformed and hostile requests (`BEEP:PROBE:PASS`),
//! `role=intruder` is its second task, refused on the owner's stream
//! (`BEEP:INTRUDER:PASS`), and `soak=<n>` runs `n` open/play/close cycles of
//! silence (`BEEP:SOAK:PASS`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::panic::PanicInfo;
use core::ptr;

use pcm::tone::Tone;
use user::messenger::audio::{self as api, Grant};
use user::sys;

#[path = "beep/common.rs"]
mod common;
#[path = "beep/probe.rs"]
mod probe;
#[path = "beep/soak.rs"]
mod soak;

use common::{connect, fail, nap, now};

const DEFAULT_FREQ_HZ: u32 = 880;
const DEFAULT_MS: u32 = 800;
const RATE_HZ: u32 = 48000;
const CHANNELS: u32 = 2;
const PERIOD_BYTES: u32 = 8192;
/// Peak level: about half of full scale.
const AMPLITUDE_Q15: i32 = 16000;
/// Ticks to wait for playback to finish beyond its nominal length.
const DRAIN_SLACK_TICKS: u64 = 500;

/// What to do.
enum Mode {
    Tone,
    Probe,
    Intruder,
    Soak(u32),
}

struct Args {
    freq_hz: u32,
    ms: u32,
    mode: Mode,
}

fn parse_args() -> Args {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut args = Args {
        freq_hz: DEFAULT_FREQ_HZ,
        ms: DEFAULT_MS,
        mode: Mode::Tone,
    };
    // Bare numbers are the shell spelling: `beep 440 500`.
    let mut bare = 0;
    for part in text.split_whitespace() {
        if let Ok(value) = part.parse::<u32>() {
            match bare {
                0 => args.freq_hz = value.clamp(20, 20000),
                1 => args.ms = value.clamp(50, 10_000),
                _ => {}
            }
            bare += 1;
            continue;
        }
        if matches!(part, "-h" | "--help" | "help") {
            sys::write_str("usage: beep [freq_hz [ms]]   (20-20000 Hz, 50-10000 ms)\n");
            sys::exit(0);
        }
        match part.split_once('=') {
            Some(("freq", value)) => {
                args.freq_hz = value.parse().unwrap_or(DEFAULT_FREQ_HZ).clamp(20, 20000)
            }
            Some(("ms", value)) => args.ms = value.parse().unwrap_or(DEFAULT_MS).clamp(50, 10_000),
            Some(("probe", "1")) => args.mode = Mode::Probe,
            Some(("role", "intruder")) => args.mode = Mode::Intruder,
            // Bounded so a hostile argument cannot pin the card for minutes.
            Some(("soak", value)) => {
                args.mode = Mode::Soak(value.parse().unwrap_or(0).clamp(1, 200))
            }
            _ => {}
        }
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
        Mode::Intruder => (
            "BEEP:INTRUDER",
            probe::run_intruder().map(|()| sys::write_str("BEEP:INTRUDER:PASS\n")),
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
    let client = connect()?;
    client.info().map_err(fail("info"))?;

    let grant = client
        .open_stream(api::PLAYBACK, api::S16_LE, RATE_HZ, CHANNELS, PERIOD_BYTES)
        .map_err(fail("open"))?;
    if grant.format != api::S16_LE {
        return Err(String::from(
            "driver granted a format beep cannot synthesize",
        ));
    }
    let stream = grant.stream;
    let result = stream_tone(&client, &grant, args);
    // Always give the stream back, even after a failure.
    let _ = client.close_stream(stream);
    let played = result?;
    sys::write_str(&format!(
        "BEEP:PLAY:PASS freq={} rate={} channels={} frames={played}\n",
        args.freq_hz, grant.rate, grant.channels
    ));
    Ok(())
}

/// Write the tone into a shared ring ahead of the driver; returns the frames
/// the driver reports played.
fn stream_tone(client: &api::Client, grant: &Grant, args: &Args) -> Result<u64, String> {
    let frame_bytes = 2 * grant.channels as usize;
    let period_bytes = grant.period_bytes as usize;
    let period_frames = (period_bytes / frame_bytes) as u64;
    let ring_bytes = period_bytes * grant.periods as usize;
    let ring_frames = (ring_bytes / frame_bytes) as u64;
    let total = u64::from(grant.rate) * u64::from(args.ms) / 1000;

    let (handle, va) = sys::display_create_buffer(ring_bytes as u64)
        .map_err(|code| format!("ring allocation failed (errno {code})"))?;
    let ring = va as *mut u8;
    client
        .attach_ring(grant.stream, handle, ring_bytes as u64)
        .map_err(fail("attach"))?;

    let mut tone = Tone::new(args.freq_hz, grant.rate, AMPLITUDE_Q15);
    let mut chunk = vec![0u8; period_bytes];
    let (mut written, mut consumed, mut started) = (0u64, 0u64, false);
    let deadline = now() + u64::from(args.ms) / 10 + DRAIN_SLACK_TICKS;
    while written < total {
        let frames = period_frames.min(total - written);
        if ring_frames - (written - consumed) < frames {
            // The ring is full: let the device play some, then look again.
            nap();
            consumed = client
                .commit(grant.stream, written)
                .map_err(fail("commit"))?;
            if now() > deadline {
                return Err(String::from("timed out waiting for ring space"));
            }
            continue;
        }
        let bytes = frames as usize * frame_bytes;
        tone.fill_s16le(&mut chunk[..bytes], grant.channels as usize);
        // Frame `n` lives at `(n mod ring_frames) * frame_bytes`; chunks are
        // whole periods (bar the last), so a chunk never wraps the ring.
        let offset = (written % ring_frames) as usize * frame_bytes;
        // SAFETY: `offset + bytes <= ring_bytes`: `written % ring_frames` is
        // below the ring and a chunk is at most one period, which divides the
        // ring; `ring` is the mapping `display_create_buffer` returned.
        unsafe { ptr::copy_nonoverlapping(chunk.as_ptr(), ring.add(offset), bytes) };
        written += frames;
        consumed = client
            .commit(grant.stream, written)
            .map_err(fail("commit"))?;
        // Start once a full ring is queued, so the device never starts dry.
        if !started && written >= ring_frames.min(total) {
            client.start(grant.stream).map_err(fail("start"))?;
            started = true;
        }
    }
    if !started {
        client.start(grant.stream).map_err(fail("start"))?;
    }
    client
        .drain(grant.stream, Some(deadline))
        .map_err(fail("drain"))?;
    let played = client.position(grant.stream).map_err(fail("position"))?;
    if played != total {
        return Err(format!("driver played {played} of {total} frames"));
    }
    Ok(played)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
