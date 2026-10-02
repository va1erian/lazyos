//! `beep soak=<n>`: open, fill, play and close a stream `n` times in a row.
//!
//! A leak of anything per stream (a handle, a mapping, a ring, a stream slot)
//! shows up as a failure long before `n` iterations are done, because the
//! mixer's stream table, handle table and the client's buffer quota are all
//! small. The audio is silence, so the recording the host analyses is
//! unaffected.

use alloc::format;
use alloc::string::String;

use audioclient::{wire, Client, RingBuffer, Transport};

use super::common::{connect, fail, now};

/// Small periods keep each iteration short (a 2 KiB period is ~10 ms).
const PERIOD_BYTES: u32 = 2048;

/// Run `iterations` stream lifecycles; returns how many completed.
pub(super) fn run(iterations: u32) -> Result<u32, String> {
    let audio = connect()?;
    let client = Client::new(&audio);
    for round in 0..iterations {
        let what = |step: &str| format!("iteration {round}: {step}");
        let grant = client
            .open_stream(
                wire::DIRECTION_PLAYBACK,
                wire::FORMAT_S16_LE,
                48000,
                2,
                PERIOD_BYTES,
            )
            .map_err(fail(&what("open")))?;
        let stream = grant.stream;
        let ring_bytes = grant.period_bytes as usize * grant.periods as usize;
        let frames = (ring_bytes / (2 * grant.channels as usize)) as u64;

        // A fresh ring is zero-filled: a ring of silence. Dropping it at the
        // end of the iteration closes the buffer.
        let ring = audio
            .create_ring(ring_bytes)
            .map_err(fail(&what("ring allocation")))?;
        client
            .attach_ring(stream, ring.share())
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
    }
    Ok(iterations)
}
