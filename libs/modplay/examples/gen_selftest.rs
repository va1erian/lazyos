//! Write the guest self-test song: `cargo run -p modplay --features fuzz
//! --example gen_selftest -- user/src/bin/modplay/selftest.mod`.
//!
//! One voice on channel 0 plays seven notes of 320 ms each, so the sound
//! harness can check the recording's pitch sequence (`tools/sound/run.py
//! --modplay`). Adjacent notes differ by at least 30%, which is what the
//! detector needs to see them as separate tones. The pitches are
//! `PAL_CLOCK / (2 * period * 16)`; keep `tools/sound/run.py` in step.

use modplay::synth::{effect, note, square, ModBuilder};

/// Periods of the melody, one per two rows.
const MELODY: [u16; 7] = [428, 285, 214, 285, 428, 214, 285];

fn main() -> std::io::Result<()> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: gen_selftest <out.mod>");
        std::process::exit(2);
    };
    let mut song = ModBuilder::new()
        .sample(1, square(16), 64, true)
        // Speed 8 at tempo 125: 160 ms per row.
        .note(0, 0, 1, effect(0xF, 8));
    for (i, &period) in MELODY.iter().enumerate() {
        song = song.note(0, i * 2, 0, note(period, 1));
    }
    // Break out after row 13 so the song is exactly 14 rows long.
    song = song.note(0, 13, 1, effect(0xD, 0));
    std::fs::write(path, song.build())
}
