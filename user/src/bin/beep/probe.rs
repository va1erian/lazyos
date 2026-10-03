//! `beep probe=1`: hostile and malformed requests against the mixer.
//!
//! Every call here is one a correct client never makes; each must fail with
//! the documented errno, and the mixer must survive all of them and still
//! serve the well-behaved tone afterwards. A second task (`role=intruder`)
//! tries to use the owner's stream and must be refused, while still getting
//! a stream of its own: that is what a mixer is for.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use audioclient::{
    wire, Client, Error, Grant, RingBuffer, RingRef, Transfers, Transport, UNITY_GAIN,
};
use user::audio::Native;
use user::sys;

use super::common::{connect, fail, is_errno, now};

/// Positive errno values the probe expects (Linux numbering).
const EACCES: i64 = 13;
const EBUSY: i64 = 16;
const EINVAL: i64 = 22;
const ENOTSUP: i64 = 95;

/// Streams one client may hold at once (`audiomix::Config::new`).
const STREAMS_PER_CLIENT: usize = 4;

struct Checks {
    passed: u32,
}

impl Checks {
    fn expect(&mut self, label: &str, ok: bool) -> Result<(), String> {
        if !ok {
            return Err(format!("check failed: {label}"));
        }
        self.passed += 1;
        Ok(())
    }
}

type Probe<'a> = Client<&'a Native>;

fn open(
    client: &Probe,
    dir: u32,
    format: u32,
    rate: u32,
    channels: u32,
    period: u32,
) -> Result<Grant, Error> {
    client.open_stream(dir, format, rate, channels, period)
}

fn playback(client: &Probe) -> Result<Grant, Error> {
    open(
        client,
        wire::DIRECTION_PLAYBACK,
        wire::FORMAT_S16_LE,
        48000,
        2,
        4096,
    )
}

/// The owner side: returns the number of checks that passed.
pub(super) fn run() -> Result<u32, String> {
    let audio = connect()?;
    let client = Client::new(&audio);
    let mut checks = Checks { passed: 0 };
    malformed_opens(&client, &mut checks)?;

    // An unsupported rate is snapped to the closest, not refused.
    let grant = open(
        &client,
        wire::DIRECTION_PLAYBACK,
        wire::FORMAT_S16_LE,
        47000,
        2,
        4099,
    )
    .map_err(fail("open"))?;
    checks.expect("rate snapped to 48000", grant.rate == 48000)?;
    checks.expect(
        "period aligned to a frame",
        grant.period_bytes % (2 * grant.channels) == 0,
    )?;
    let stream = grant.stream;
    stream_limits(&client, stream, &mut checks)?;

    // Before a ring is attached.
    checks.expect(
        "commit before attach",
        is_errno(&client.commit(stream, 0), EINVAL),
    )?;
    checks.expect(
        "start before attach",
        is_errno(&client.start(stream), EINVAL),
    )?;
    let no_buffer =
        wire::encode_attach_ring_args(&wire::AttachRingArgs { stream }).map_err(|_| "encode")?;
    checks.expect(
        "attach without a buffer",
        is_errno(
            &client.raw(wire::METHOD_ATTACHRING, no_buffer, Transfers::NONE),
            EINVAL,
        ),
    )?;

    // A ring shorter than the grant is refused; a good one attaches once.
    let ring_bytes = grant.period_bytes as usize * grant.periods as usize;
    let short = audio.create_ring(4096).map_err(fail("ring"))?;
    checks.expect(
        "short ring refused",
        is_errno(&client.attach_ring(stream, short.share()), EINVAL),
    )?;
    drop(short);
    let ring = audio.create_ring(ring_bytes).map_err(fail("ring"))?;
    client
        .attach_ring(stream, ring.share())
        .map_err(fail("attach"))?;
    checks.expect(
        "second attach is busy",
        is_errno(&client.attach_ring(stream, ring.share()), EBUSY),
    )?;

    commit_rules(&client, &grant, &mut checks)?;
    volume_rules(&client, stream, &mut checks)?;
    stray_transfers(&client, stream, ring.share(), &mut checks)?;

    // Someone else must not be able to drive this stream.
    checks.expect("intruder refused", intruder(stream)?)?;

    client.close_stream(stream).map_err(fail("close"))?;
    drop(ring);
    checks.expect(
        "commit after close",
        is_errno(&client.commit(stream, 200), EINVAL),
    )?;
    checks.expect(
        "position after close",
        is_errno(&client.position(stream), EINVAL),
    )?;
    // The mixer is still healthy: it grants a fresh stream.
    let again = playback(&client).map_err(fail("reopen"))?;
    client.close_stream(again.stream).map_err(fail("reclose"))?;
    checks.expect("mixer survived", true)?;
    Ok(checks.passed)
}

/// Opens that make no sense, and calls on streams that do not exist.
fn malformed_opens(client: &Probe, checks: &mut Checks) -> Result<(), String> {
    let pb = wire::DIRECTION_PLAYBACK;
    checks.expect(
        "capture is not supported",
        is_errno(
            &open(client, wire::DIRECTION_CAPTURE, 0, 48000, 2, 4096),
            ENOTSUP,
        ),
    )?;
    checks.expect(
        "unknown direction",
        is_errno(&open(client, 7, 0, 48000, 2, 4096), EINVAL),
    )?;
    checks.expect(
        "unknown format",
        is_errno(&open(client, pb, 9, 48000, 2, 4096), EINVAL),
    )?;
    checks.expect(
        "zero channels",
        is_errno(&open(client, pb, 0, 48000, 0, 4096), EINVAL),
    )?;
    checks.expect(
        "absurd channels",
        is_errno(&open(client, pb, 0, 48000, 999, 4096), EINVAL),
    )?;
    checks.expect(
        "zero period",
        is_errno(&open(client, pb, 0, 48000, 2, 0), EINVAL),
    )?;
    checks.expect("start with no stream", is_errno(&client.start(0), EINVAL))?;
    checks.expect(
        "commit with no stream",
        is_errno(&client.commit(0, 10), EINVAL),
    )?;
    Ok(())
}

/// A client may hold several streams (each its own id), but only so many.
fn stream_limits(client: &Probe, first: u32, checks: &mut Checks) -> Result<(), String> {
    let mut extra = Vec::new();
    while extra.len() + 1 < STREAMS_PER_CLIENT {
        let grant = playback(client).map_err(fail("extra open"))?;
        extra.push(grant.stream);
    }
    checks.expect(
        "every stream has its own id",
        !extra.contains(&first) && extra.iter().all(|&s| s != 0),
    )?;
    checks.expect(
        "one stream too many is busy",
        is_errno(&playback(client), EBUSY),
    )?;
    for stream in extra {
        client.close_stream(stream).map_err(fail("extra close"))?;
    }
    Ok(())
}

/// Commit counters: monotonic and never more than a ring ahead.
fn commit_rules(client: &Probe, grant: &Grant, checks: &mut Checks) -> Result<(), String> {
    let stream = grant.stream;
    let ring_frames = u64::from(grant.period_bytes * grant.periods / (2 * grant.channels));
    checks.expect("commit within the ring", client.commit(stream, 100).is_ok())?;
    checks.expect(
        "commit backwards",
        is_errno(&client.commit(stream, 50), EINVAL),
    )?;
    checks.expect(
        "commit beyond the ring",
        is_errno(&client.commit(stream, ring_frames + 1), EINVAL),
    )?;
    checks.expect(
        "commit up to the ring",
        client.commit(stream, ring_frames).is_ok(),
    )?;
    checks.expect(
        "commit u64::MAX",
        is_errno(&client.commit(stream, u64::MAX), EINVAL),
    )
}

/// `SetVolume` / `SetMute`: in range only, on a stream that exists.
fn volume_rules(client: &Probe, stream: u32, checks: &mut Checks) -> Result<(), String> {
    checks.expect(
        "half volume",
        client.set_volume(stream, UNITY_GAIN / 2).is_ok(),
    )?;
    checks.expect(
        "volume above four times unity",
        is_errno(&client.set_volume(stream, 4 * UNITY_GAIN + 1), EINVAL),
    )?;
    checks.expect(
        "volume u32::MAX",
        is_errno(&client.set_volume(stream, u32::MAX), EINVAL),
    )?;
    checks.expect("mute", client.set_mute(stream, true).is_ok())?;
    checks.expect("unmute", client.set_mute(stream, false).is_ok())?;
    checks.expect(
        "volume of no stream",
        is_errno(&client.set_volume(0, UNITY_GAIN), EINVAL),
    )
}

/// A buffer sent with a method that declares none is refused and closed, not
/// kept: hundreds of them would fill any handle table that leaked.
fn stray_transfers(
    client: &Probe,
    stream: u32,
    ring: RingRef,
    checks: &mut Checks,
) -> Result<(), String> {
    let stray = || Transfers {
        handles: Vec::new(),
        buffers: alloc::vec![ring.desc()],
    };
    let position =
        wire::encode_position_args(&wire::PositionArgs { stream }).map_err(|_| "encode")?;
    checks.expect(
        "a stray buffer on another method is refused",
        is_errno(
            &client.raw(wire::METHOD_POSITION, position, stray()),
            EINVAL,
        ),
    )?;
    let flood = (0..300).all(|_| {
        client
            .raw(wire::METHOD_ATTACHRING, Vec::new(), stray())
            .is_err()
    });
    checks.expect("malformed AttachRing flood is refused", flood)?;
    checks.expect(
        "mixer still serves after the flood",
        client.position(stream).is_ok(),
    )
}

/// Run the intruder task and report whether it was refused everywhere.
fn intruder(stream: u32) -> Result<bool, String> {
    let stream_arg = format!("stream={stream}");
    let Some(pid) = sys::spawn_native(fhs::bin::BEEP, &["role=intruder", &stream_arg]) else {
        return Err(String::from("could not spawn the intruder"));
    };
    let deadline = now() + 1000;
    loop {
        match sys::wait(deadline) {
            Some((child, status)) if child == pid => return Ok(status == 0),
            Some(_) => continue,
            None => return Err(String::from("intruder never finished")),
        }
    }
}

/// The intruder side: every attempt on the owner's `stream` must fail with
/// `EACCES`, and its own open must succeed with a different stream.
pub(super) fn run_intruder(stream: u32) -> Result<(), String> {
    let audio = connect()?;
    let client = Client::new(&audio);
    let refused = |name: &str, result: Result<(), Error>| {
        if is_errno(&result, EACCES) {
            Ok(())
        } else {
            Err(format!("{name} was not refused with EACCES"))
        }
    };
    refused("start", client.start(stream))?;
    refused("stop", client.stop(stream))?;
    refused("commit", client.commit(stream, 1).map(|_| ()))?;
    refused("position", client.position(stream).map(|_| ()))?;
    refused("drain", client.drain(stream, Some(now() + 50)))?;
    refused("volume", client.set_volume(stream, 0))?;
    refused("mute", client.set_mute(stream, true))?;
    refused("close", client.close_stream(stream))?;
    let ring = audio.create_ring(65536).map_err(fail("ring"))?;
    refused("attach", client.attach_ring(stream, ring.share()))?;
    let own = playback(&client).map_err(fail("own stream"))?;
    if own.stream == stream {
        return Err(String::from("the intruder was handed the owner's stream"));
    }
    client.close_stream(own.stream).map_err(fail("own close"))?;
    Ok(())
}
