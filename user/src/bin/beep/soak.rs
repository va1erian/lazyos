//! `beep soak=<n>`: open, fill, play and close a stream `n` times in a row.
//!
//! A leak of anything per stream (a handle, a mapping, a DMA charge, a queue
//! slot) shows up as a failure long before `n` iterations are done, because the
//! driver's handle table, the client's buffer quota and the device's queues
//! are all small. The audio is silence, so the recording the host analyses is
//! unaffected.

use alloc::format;
use alloc::string::String;

use user::messenger::audio as api;
use user::sys;

use super::common::{connect, fail, now};

/// Small periods keep each iteration short (a 2 KiB period is ~10 ms).
const PERIOD_BYTES: u32 = 2048;

/// Run `iterations` stream lifecycles; returns how many completed.
pub(super) fn run(iterations: u32) -> Result<u32, String> {
    let client = connect()?;
    for round in 0..iterations {
        let what = |step: &str| format!("iteration {round}: {step}");
        let grant = client
            .open_stream(api::PLAYBACK, api::S16_LE, 48000, 2, PERIOD_BYTES)
            .map_err(fail(&what("open")))?;
        let stream = grant.stream;
        let ring_bytes = u64::from(grant.period_bytes) * u64::from(grant.periods);
        let frames = ring_bytes / u64::from(2 * grant.channels);

        let (buffer, _va) = sys::display_create_buffer(ring_bytes)
            .map_err(|code| what(&format!("ring allocation errno {code}")))?;
        // A fresh buffer is zero-filled: a ring of silence.
        client
            .attach_ring(stream, buffer, ring_bytes)
            .map_err(fail(&what("attach")))?;
        client
            .commit(stream, frames)
            .map_err(fail(&what("commit")))?;
        client.start(stream).map_err(fail(&what("start")))?;
        client
            .drain(stream, Some(now() + 500))
            .map_err(fail(&what("drain")))?;
        let played = client.position(stream).map_err(fail(&what("position")))?;
        if played != frames {
            return Err(what(&format!("played {played} of {frames} frames")));
        }
        client.close_stream(stream).map_err(fail(&what("close")))?;
        sys::display_close_buffer(buffer)
            .map_err(|code| what(&format!("closing the ring failed (errno {code})")))?;
    }
    Ok(iterations)
}
