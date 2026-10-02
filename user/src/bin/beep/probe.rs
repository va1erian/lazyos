//! `beep probe=1`: hostile and malformed requests against the audio driver.
//!
//! Every call here is one a correct client never makes; each must fail with
//! the documented errno, and the driver must survive all of them and still
//! serve the well-behaved tone afterwards. A second task (`role=intruder`)
//! tries to use the owner's stream and must be refused.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::BufferDesc;
use user::messenger::audio::{self as api, wire};
use user::messenger::errno;
use user::sys;

use super::common::{connect, fail, is_errno, now};

/// The intruder's command line (a second task, its own credentials slot).

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

/// The owner side: returns the number of checks that passed.
pub(super) fn run() -> Result<u32, String> {
    let client = connect()?;
    let mut checks = Checks { passed: 0 };

    // Malformed opens, all before any stream exists.
    let open = |dir, format, rate, channels, period| {
        client.open_stream(dir, format, rate, channels, period)
    };
    checks.expect(
        "capture is not supported",
        is_errno(&open(api::CAPTURE, 0, 48000, 2, 4096), errno::ENOTSUP),
    )?;
    checks.expect(
        "unknown direction",
        is_errno(&open(7, 0, 48000, 2, 4096), errno::EINVAL),
    )?;
    checks.expect(
        "unknown format",
        is_errno(&open(api::PLAYBACK, 9, 48000, 2, 4096), errno::EINVAL),
    )?;
    checks.expect(
        "zero channels",
        is_errno(&open(api::PLAYBACK, 0, 48000, 0, 4096), errno::EINVAL),
    )?;
    checks.expect(
        "absurd channels",
        is_errno(&open(api::PLAYBACK, 0, 48000, 999, 4096), errno::EINVAL),
    )?;
    checks.expect(
        "zero period",
        is_errno(&open(api::PLAYBACK, 0, 48000, 2, 0), errno::EINVAL),
    )?;

    // Calls on a stream that does not exist.
    checks.expect(
        "start with no stream",
        is_errno(&client.start(0), errno::EINVAL),
    )?;
    checks.expect(
        "commit with no stream",
        is_errno(&client.commit(0, 10), errno::EINVAL),
    )?;

    // An unsupported rate is snapped to the closest, not refused.
    let grant = open(api::PLAYBACK, api::S16_LE, 47000, 2, 4099).map_err(fail("open"))?;
    checks.expect("rate snapped to 48000", grant.rate == 48000)?;
    checks.expect(
        "period aligned to a frame",
        grant.period_bytes % (2 * grant.channels) == 0,
    )?;
    let stream = grant.stream;
    checks.expect(
        "second open is busy",
        is_errno(&open(api::PLAYBACK, 0, 48000, 2, 4096), errno::EBUSY),
    )?;

    // Before a ring is attached.
    checks.expect(
        "commit before attach",
        is_errno(&client.commit(stream, 0), errno::EINVAL),
    )?;
    checks.expect(
        "start before attach",
        is_errno(&client.start(stream), errno::EINVAL),
    )?;
    let no_buffer =
        wire::encode_attach_ring_args(&wire::AttachRingArgs { stream }).map_err(|_| "encode")?;
    checks.expect(
        "attach without a buffer",
        is_errno(
            &client.raw(wire::METHOD_ATTACHRING, no_buffer, Vec::new()),
            errno::EINVAL,
        ),
    )?;

    // A ring shorter than the grant is refused; a good one attaches once.
    let ring_bytes = u64::from(grant.period_bytes) * u64::from(grant.periods);
    let (short, _) = sys::display_create_buffer(4096).map_err(|c| format!("buffer errno {c}"))?;
    checks.expect(
        "short ring refused",
        is_errno(&client.attach_ring(stream, short, 4096), errno::EINVAL),
    )?;
    let _ = sys::display_close_buffer(short);
    let (ring, _) =
        sys::display_create_buffer(ring_bytes).map_err(|c| format!("buffer errno {c}"))?;
    client
        .attach_ring(stream, ring, ring_bytes)
        .map_err(fail("attach"))?;
    checks.expect(
        "second attach is busy",
        is_errno(&client.attach_ring(stream, ring, ring_bytes), errno::EBUSY),
    )?;

    // Commit counters: monotonic and never more than a ring ahead.
    let ring_frames = ring_bytes / u64::from(2 * grant.channels);
    checks.expect("commit within the ring", client.commit(stream, 100).is_ok())?;
    checks.expect(
        "commit backwards",
        is_errno(&client.commit(stream, 50), errno::EINVAL),
    )?;
    checks.expect(
        "commit beyond the ring",
        is_errno(&client.commit(stream, ring_frames + 1), errno::EINVAL),
    )?;
    checks.expect(
        "commit up to the ring",
        client.commit(stream, ring_frames).is_ok(),
    )?;
    checks.expect(
        "commit u64::MAX",
        is_errno(&client.commit(stream, u64::MAX), errno::EINVAL),
    )?;
    checks.expect("attach with a stray buffer on another method", {
        // A start carrying a buffer must not leak it or change behaviour.
        let body =
            wire::encode_position_args(&wire::PositionArgs { stream }).map_err(|_| "encode")?;
        let buffers = alloc::vec![BufferDesc {
            handle: ring,
            offset: 0,
            len: ring_bytes,
            flags: 0
        }];
        client.raw(wire::METHOD_POSITION, body, buffers).is_ok()
    })?;

    // Malformed requests that carry a buffer must not leak it into the driver's
    // handle table: hundreds of them would fill any table that leaked.
    let flood = (0..300).all(|_| {
        let buffers = alloc::vec![BufferDesc {
            handle: ring,
            offset: 0,
            len: ring_bytes,
            flags: 0
        }];
        client
            .raw(wire::METHOD_ATTACHRING, Vec::new(), buffers)
            .is_err()
    });
    checks.expect("malformed AttachRing flood is refused", flood)?;
    checks.expect(
        "driver still serves after the flood",
        client.position(stream).is_ok(),
    )?;

    // Someone else must not be able to drive this stream.
    checks.expect("intruder spawned", intruder()?)?;

    client.close_stream(stream).map_err(fail("close"))?;
    let _ = sys::display_close_buffer(ring);
    checks.expect(
        "commit after close",
        is_errno(&client.commit(stream, 200), errno::EINVAL),
    )?;
    checks.expect(
        "position after close",
        is_errno(&client.position(stream), errno::EINVAL),
    )?;
    // The driver is still healthy: it grants a fresh stream.
    let again = open(api::PLAYBACK, api::S16_LE, 48000, 2, 4096).map_err(fail("reopen"))?;
    client.close_stream(again.stream).map_err(fail("reclose"))?;
    checks.expect("driver survived", true)?;
    Ok(checks.passed)
}

/// Run the intruder task and report whether it was refused everywhere.
fn intruder() -> Result<bool, String> {
    let Some(pid) = sys::spawn(&user::cmdline::native(fhs::bin::BEEP, "role=intruder")) else {
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

/// The intruder side: every attempt on the owner's stream must fail with
/// `EACCES`, and a second open with `EBUSY`.
pub(super) fn run_intruder() -> Result<(), String> {
    let client = connect()?;
    let stream = 0;
    let refused = |name: &str, result: Result<(), user::messenger::Error>| {
        if is_errno(&result, errno::EACCES) {
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
    refused("close", client.close_stream(stream))?;
    let (ring, _) = sys::display_create_buffer(65536).map_err(|c| format!("buffer errno {c}"))?;
    refused("attach", client.attach_ring(stream, ring, 65536))?;
    let _ = sys::display_close_buffer(ring);
    if !is_errno(
        &client.open_stream(api::PLAYBACK, 0, 48000, 2, 4096),
        errno::EBUSY,
    ) {
        return Err(String::from("open while the card is busy was not EBUSY"));
    }
    Ok(())
}
