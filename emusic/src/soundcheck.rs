//! `emusic.elf --sound-check <file>`: plays the sample track through the
//! audio backend in a fixed sequence, for `tools/emusic/run.py` to record and
//! judge (docs/media-plan.md P4). No window: the sound path alone.
//!
//! Each phase plays the track (A4 for 2 s, then C5 for 2 s) to its end, with
//! a second of silence between phases, so the judge can tell them apart:
//!
//! 1. `whole`: from the start;
//! 2. `seek`: from 1.5 s, so the first tone sounds for 0.5 s only;
//! 3. `pause`: paused at 1 s for 1 s, then resumed: the first tone loses and
//!    repeats nothing;
//! 4. `volume`: at gain 0.5, 6 dB below phase 1.
//!
//! Lines: `EMUSIC:CHECK:<phase>:START`, `...:DONE`, then `EMUSIC:CHECK:DONE`
//! or `EMUSIC:CHECK:FAIL:<reason>`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use emusic_player::backend::{AudioBackend, BackendChannel};

use crate::markers::Sink;

/// The silence between phases.
const GAP: Duration = Duration::from_secs(1);
/// Where the pause phase pauses, and for how long.
const PAUSE_AT: Duration = Duration::from_secs(1);
const PAUSE_FOR: Duration = Duration::from_secs(1);
/// Where the seek phase starts.
const SEEK_TO: Duration = Duration::from_millis(1500);
/// The volume phase's gain.
const HALF: f32 = 0.5;
/// The longest a phase may take (the track is 4 s).
const PHASE_LIMIT: Duration = Duration::from_secs(30);

/// Runs the sequence on `track` through `backend`; `scale` divides every wait
/// (1.0 on LazyOS; tests play faster than real time).
pub fn run(
    backend: &dyn AudioBackend,
    track: &Path,
    sink: &Sink,
    scale: f64,
) -> Result<(), String> {
    let wait = |duration: Duration| std::thread::sleep(duration.div_f64(scale));
    for phase in ["whole", "seek", "pause", "volume"] {
        sink(&format!("EMUSIC:CHECK:{phase}:START"));
        let channel = backend
            .open(track)
            .map_err(|error| format!("{phase}: {error}"))?;
        let ended = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ended);
        let _guard = channel
            .on_end(Box::new(move || flag.store(true, Ordering::SeqCst)))
            .map_err(|error| error.to_string())?;
        match phase {
            "seek" => channel.seek(SEEK_TO).map_err(|e| e.to_string())?,
            "volume" => channel.set_volume(HALF).map_err(|e| e.to_string())?,
            _ => {}
        }
        channel.play(false).map_err(|e| e.to_string())?;
        if phase == "pause" {
            until(&*channel, &ended, scale, |at| at >= PAUSE_AT)?;
            channel.pause().map_err(|e| e.to_string())?;
            wait(PAUSE_FOR);
            channel.play(false).map_err(|e| e.to_string())?;
        }
        until(&*channel, &ended, scale, |_| false)?;
        sink(&format!("EMUSIC:CHECK:{phase}:DONE"));
        wait(GAP);
    }
    Ok(())
}

/// Polls `channel` until it ends or `done` holds for its position.
fn until(
    channel: &dyn BackendChannel,
    ended: &AtomicBool,
    scale: f64,
    done: impl Fn(Duration) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let position = channel.position().map_err(|e| e.to_string())?;
        if ended.load(Ordering::SeqCst) || done(position) {
            return Ok(());
        }
        if start.elapsed() > PHASE_LIMIT.div_f64(scale) {
            return Err(format!(
                "a phase did not end (at {} ms, active {})",
                position.as_millis(),
                channel.is_active()
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use emusic_lazyaudio::LazyBackend;
    use emusic_lazyaudio::output::MemorySink;

    use super::*;

    #[test]
    fn the_sequence_plays_what_the_judge_expects() {
        let memory = MemorySink::new(8192, 2048, 10.0);
        let backend = LazyBackend::new(Arc::new(memory.clone()));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&lines);
        let sink: Sink = Arc::new(move |line| log.lock().unwrap().push(line.to_string()));
        let track = Path::new(env!("CARGO_MANIFEST_DIR")).join("package/resources/tones.mp3");
        run(&backend, &track, &sink, 10.0).expect("the sequence runs");
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[7], "EMUSIC:CHECK:volume:DONE");
        let recording = memory.recording();
        // Four streams for four phases, plus one more for the resume.
        assert_eq!(recording.opened.len(), 5);
        assert!(recording.gains.iter().any(|&(_, gain)| gain == HALF));
        // 4 s + 2.5 s + 4 s + 4 s of track at 44.1 kHz stereo, plus padding.
        let frames = recording.samples.len() / 2;
        assert!(
            (14 * 44_100 + 22_050..16 * 44_100).contains(&frames),
            "{frames}"
        );
    }
}
