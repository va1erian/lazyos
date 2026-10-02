//! `mixer` (`MIXER.ELF`): the system mixer's control panel from the shell
//! (`os.lazy.audio.mixer.v1`, docs/audio-plan.md stage A5).
//!
//! ```text
//! mixer                         master volume and every stream
//! mixer master <percent>        set the master volume (0-400, 100 is unity)
//! mixer mute | unmute           mute or unmute everything
//! mixer stream <id> <percent> [mute]   set one stream's volume
//! mixer probe                   control-panel checks (boot evidence)
//! ```
//!
//! `mixer probe` opens a stream of its own, drives the panel against it
//! (listing, per-stream and master volume, refusals) and prints
//! `MIXER:PROBE:PASS checks=<n>`, or `MIXER:PROBE:FAIL <reason>`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use audioclient::{control_wire as control, wire, Client, Error, MixerControl, UNITY_GAIN};
use user::audio::{self, Native};
use user::sys;

/// Ticks (100 Hz) to wait for the mixer to register.
const CONNECT_TICKS: u64 = 500;
const EINVAL: i64 = 22;
const ENOENT: i64 = 2;

const USAGE: &str =
    "usage: mixer [master <0-400> | mute | unmute | stream <id> <0-400> [mute] | probe]\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let words: Vec<&str> = text.split_whitespace().collect();
    let outcome = match words.as_slice() {
        ["probe"] => probe().map(|checks| {
            sys::write_str(&format!("MIXER:PROBE:PASS checks={checks}\n"));
        }),
        _ => command(&words),
    };
    match outcome {
        Ok(()) => sys::exit(0),
        Err(reason) => {
            let marker = if words.first() == Some(&"probe") {
                "MIXER:PROBE:FAIL "
            } else {
                "mixer: "
            };
            sys::write_str(&format!("{marker}{reason}\n"));
            sys::exit(1)
        }
    }
}

fn connect() -> Result<Native, String> {
    audio::connect_wait(audio::NAME, CONNECT_TICKS)
        .map_err(|error| format!("no audio service: {error}"))
}

fn fail(what: &str) -> impl Fn(Error) -> String + '_ {
    move |error| format!("{what}: {error}")
}

/// A volume percentage (0-400) as a 16.16 gain.
fn gain(word: &str) -> Result<u32, String> {
    match word.trim_end_matches('%').parse::<u32>() {
        Ok(percent) if percent <= 400 => Ok(percent * UNITY_GAIN / 100),
        _ => Err(format!("not a volume (0-400): {word}\n{USAGE}")),
    }
}

/// A 16.16 gain as the nearest percentage.
fn percent(gain_q16: u32) -> u64 {
    (u64::from(gain_q16) * 100 + u64::from(UNITY_GAIN) / 2) / u64::from(UNITY_GAIN)
}

fn command(words: &[&str]) -> Result<(), String> {
    let native = connect()?;
    let panel = MixerControl::new(&native);
    match words {
        [] => show(&panel),
        ["master", value] => {
            let master = panel.master().map_err(fail("master"))?;
            panel
                .set_master(gain(value)?, master.mute)
                .map_err(fail("master"))?;
            show(&panel)
        }
        [word @ ("mute" | "unmute")] => {
            let master = panel.master().map_err(fail("master"))?;
            panel
                .set_master(master.gain_q16, *word == "mute")
                .map_err(fail("master"))?;
            show(&panel)
        }
        ["stream", id, value, rest @ ..] => {
            let id = id
                .parse::<u32>()
                .map_err(|_| format!("not a stream id: {id}"))?;
            let mute = matches!(rest, ["mute"]);
            panel
                .set_stream_volume(id, gain(value)?, mute)
                .map_err(fail("stream"))?;
            show(&panel)
        }
        ["-h" | "--help" | "help"] => {
            sys::write_str(USAGE);
            Ok(())
        }
        _ => Err(String::from(USAGE.trim_end())),
    }
}

fn show(panel: &MixerControl<&Native>) -> Result<(), String> {
    let master = panel.master().map_err(fail("master"))?;
    if !master.card {
        sys::write_str("master: no sound card attached\n");
    } else {
        sys::write_str(&format!(
            "master {}%{}  ({} Hz, {} ch, {}/{} streams)\n",
            percent(master.gain_q16),
            if master.mute { " muted" } else { "" },
            master.rate,
            master.channels,
            master.streams,
            master.max_streams
        ));
    }
    for stream in panel.streams().map_err(fail("list"))? {
        sys::write_str(&format!(
            "stream {:<3} task {:<4} {:<8} {} Hz {} ch  volume {}%{}  played {}  underruns {}\n",
            stream.stream,
            stream.owner,
            state_name(stream.state),
            stream.rate,
            stream.channels,
            percent(stream.gain_q16),
            if stream.mute { " muted" } else { "" },
            stream.frames,
            stream.underruns
        ));
    }
    Ok(())
}

fn state_name(state: u32) -> &'static str {
    match state {
        control::STREAM_STATE_IDLE => "idle",
        control::STREAM_STATE_RUNNING => "running",
        control::STREAM_STATE_STOPPED => "stopped",
        control::STREAM_STATE_DRAINING => "draining",
        control::STREAM_STATE_DRAINED => "drained",
        _ => "?",
    }
}

struct Checks(u32);

impl Checks {
    fn expect(&mut self, label: &str, ok: bool) -> Result<(), String> {
        if !ok {
            return Err(format!("check failed: {label}"));
        }
        self.0 += 1;
        Ok(())
    }
}

fn refused<T>(result: Result<T, Error>, errno: i64) -> bool {
    matches!(result, Err(Error::Errno(code)) if code == errno)
}

/// Drive the control panel against a stream of our own.
fn probe() -> Result<u32, String> {
    let native = connect()?;
    let panel = MixerControl::new(&native);
    let client = Client::new(&native);
    let mut checks = Checks(0);

    let master = panel.master().map_err(fail("master"))?;
    checks.expect("a card is attached", master.card && master.rate > 0)?;
    checks.expect(
        "master starts at unity",
        master.gain_q16 == UNITY_GAIN && !master.mute,
    )?;

    let grant = client
        .open_stream(
            wire::DIRECTION_PLAYBACK,
            wire::FORMAT_S16_LE,
            44100,
            1,
            4096,
        )
        .map_err(fail("open"))?;
    let id = grant.stream;
    let find = |panel: &MixerControl<&Native>| -> Result<Option<control::StreamStatus>, String> {
        Ok(panel
            .streams()
            .map_err(fail("list"))?
            .into_iter()
            .find(|s| s.stream == id))
    };
    let listed = find(&panel)?;
    checks.expect(
        "the stream is listed as opened",
        listed.as_ref().is_some_and(|s| {
            s.state == control::STREAM_STATE_IDLE
                && (s.rate, s.channels) == (44100, 1)
                && s.gain_q16 == UNITY_GAIN
                && !s.mute
        }),
    )?;

    checks.expect(
        "set another task's stream volume",
        panel.set_stream_volume(id, UNITY_GAIN / 2, true).is_ok(),
    )?;
    checks.expect(
        "the listing shows it",
        find(&panel)?.is_some_and(|s| s.gain_q16 == UNITY_GAIN / 2 && s.mute),
    )?;
    checks.expect(
        "a gain out of range is refused",
        refused(panel.set_stream_volume(id, u32::MAX, false), EINVAL),
    )?;
    checks.expect(
        "and changes nothing",
        find(&panel)?.is_some_and(|s| s.gain_q16 == UNITY_GAIN / 2 && s.mute),
    )?;
    checks.expect(
        "an unknown stream is ENOENT",
        refused(panel.set_stream_volume(u32::MAX, UNITY_GAIN, false), ENOENT),
    )?;

    checks.expect(
        "a master gain out of range is refused",
        refused(panel.set_master(4 * UNITY_GAIN + 1, false), EINVAL),
    )?;
    panel
        .set_master(3 * UNITY_GAIN / 4, false)
        .map_err(fail("set master"))?;
    checks.expect(
        "the master gain is reported back",
        panel.master().map_err(fail("master"))?.gain_q16 == 3 * UNITY_GAIN / 4,
    )?;
    panel
        .set_master(UNITY_GAIN, false)
        .map_err(fail("restore master"))?;

    client.close_stream(id).map_err(fail("close"))?;
    checks.expect(
        "a closed stream is no longer listed",
        find(&panel)?.is_none(),
    )?;
    Ok(checks.0)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
