//! Correctness tests: parsing, pitch, volume, timing and song flow.

use std::vec;
use std::vec::Vec;

use crate::synth::{effect, note, square, ModBuilder};
use crate::tables::{finetuned, semitones_up};
use crate::{ModError, Module, Note, Options, Player};

const RATE: u32 = 48_000;
/// Frames in one row at speed 6, tempo 125, 48 kHz: 6 ticks of 960.
const ROW: usize = 6 * 960;

fn hard() -> Options {
    Options {
        separation: 100,
        interpolate: false,
        ..Options::default()
    }
}

/// Render the whole song (bounded) and return the interleaved samples.
fn render_all(bytes: &[u8], options: Options) -> Vec<i16> {
    let module = Module::parse(bytes).unwrap();
    let mut player = Player::new(&module, RATE, options);
    let mut out = Vec::new();
    let mut buf = vec![0i16; 4096];
    loop {
        let n = player.render(&mut buf);
        out.extend_from_slice(&buf[..n * 2]);
        if n == 0 || out.len() > 2 * RATE as usize * 600 {
            return out;
        }
    }
}

fn frames(bytes: &[u8]) -> usize {
    render_all(bytes, hard()).len() / 2
}

fn tone() -> ModBuilder {
    ModBuilder::new().sample(1, square(32), 64, true)
}

fn left(samples: &[i16]) -> Vec<i16> {
    samples.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
}

fn crossings(channel: &[i16]) -> usize {
    channel
        .windows(2)
        .filter(|w| (w[0] < 0) != (w[1] < 0))
        .count()
}

#[test]
fn rejects_malformed_files() {
    let good = tone().build();
    assert_eq!(Module::parse(&[]).unwrap_err(), ModError::TooShort);
    assert_eq!(
        Module::parse(&good[..1083]).unwrap_err(),
        ModError::TooShort
    );

    let mut bad = good.clone();
    bad[1080..1084].copy_from_slice(b"XXXX");
    assert_eq!(Module::parse(&bad).unwrap_err(), ModError::BadSignature);

    for tag in [b"6CHN", b"8CHN", b"16CN", b"32CH", b"FLT8", b"OCTA"] {
        let mut other = good.clone();
        other[1080..1084].copy_from_slice(tag);
        assert_eq!(
            Module::parse(&other).unwrap_err(),
            ModError::UnsupportedChannels
        );
    }

    for length in [0u8, 129, 255] {
        let mut b = good.clone();
        b[950] = length;
        assert_eq!(Module::parse(&b).unwrap_err(), ModError::BadSongLength);
    }

    let mut order = good.clone();
    order[952] = 128;
    assert_eq!(Module::parse(&order).unwrap_err(), ModError::BadOrder);

    // Order table points at pattern 5 but only one pattern is stored.
    let mut missing = good.clone();
    missing[953] = 5;
    assert_eq!(
        Module::parse(&missing).unwrap_err(),
        ModError::TruncatedPatterns
    );
    assert_eq!(
        Module::parse(&good[..1084 + 512]).unwrap_err(),
        ModError::TruncatedPatterns
    );
}

#[test]
fn clips_samples_to_the_bytes_present() {
    let mut bytes = ModBuilder::new().sample(1, square(64), 64, true).build();
    bytes.truncate(bytes.len() - 40); // chop most of the sample
    let module = Module::parse(&bytes).unwrap();
    let sample = &module.samples[0];
    assert_eq!(sample.data.len(), 24);
    let (start, end) = sample.loop_range.unwrap();
    assert!(start < end && end <= sample.data.len());
}

#[test]
fn loop_beyond_the_sample_is_dropped() {
    let mut bytes = tone().build();
    // Sample 1 header: loop start = 100 words, far past its 16-word data.
    let at = 20 + 22;
    bytes[at + 4..at + 6].copy_from_slice(&100u16.to_be_bytes());
    let module = Module::parse(&bytes).unwrap();
    assert_eq!(module.samples[0].loop_range, None);
}

#[test]
fn parses_a_cell() {
    let bytes = tone()
        .note(
            0,
            3,
            2,
            Note {
                period: 0x1AB,
                sample: 0x1F,
                effect: 0xE,
                param: 0x91,
            },
        )
        .build();
    let module = Module::parse(&bytes).unwrap();
    assert_eq!(
        module.note(0, 3, 2),
        Note {
            period: 0x1AB,
            sample: 0x1F,
            effect: 0xE,
            param: 0x91
        }
    );
    assert_eq!(module.note(99, 0, 0), Note::default());
    assert_eq!(module.note(0, 64, 0), Note::default());
}

#[test]
fn period_tables() {
    assert_eq!(finetuned(428, 0), 428);
    assert!(finetuned(428, 7) < 428, "positive finetune raises pitch");
    assert!(finetuned(428, -8) > 428);
    assert_eq!(semitones_up(428, 12), 214);
    assert_eq!(semitones_up(428, 0), 428);
    assert_eq!(semitones_up(428, 99), semitones_up(428, 15));
}

#[test]
fn plays_the_right_pitch() {
    // A 32-byte cycle at period 126: PAL / (2 * 126 * 32) = 439.8 Hz.
    let bytes = tone().note(0, 0, 0, note(126, 1)).build();
    let out = render_all(&bytes, hard());
    let l = left(&out);
    let second = &l[..RATE as usize];
    let hz = crossings(second) as f64 / 2.0;
    assert!((hz - 439.8).abs() < 440.0 * 0.02, "measured {hz} Hz");
}

#[test]
fn octave_effects_double_the_frequency() {
    // E5 finetune is small; an arpeggio of +12 on every third tick is audible
    // as extra crossings versus the plain note.
    let plain = tone().note(0, 0, 0, note(200, 1)).build();
    let arp = tone()
        .note(
            0,
            0,
            0,
            Note {
                period: 200,
                sample: 1,
                effect: 0,
                param: 0x0C,
            },
        )
        .build();
    let a = crossings(&left(&render_all(&plain, hard()))[..ROW]);
    let b = crossings(&left(&render_all(&arp, hard()))[..ROW]);
    assert!(b > a + a / 4, "arpeggio {b} vs plain {a}");
}

#[test]
fn volume_and_hard_pan() {
    let full = tone().note(0, 0, 0, note(200, 1)).build();
    let half = tone()
        .note(
            0,
            0,
            0,
            Note {
                period: 200,
                sample: 1,
                effect: 0xC,
                param: 32,
            },
        )
        .build();
    let peak = |bytes: &[u8], channel: usize| {
        render_all(bytes, hard())
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| i32::from(f[channel]).abs())
            .max()
            .unwrap()
    };
    assert_eq!(peak(&full, 1), 0, "channel 0 is hard left");
    let (p_full, p_half) = (peak(&full, 0), peak(&half, 0));
    assert!(p_full > 16_000 && p_full <= 16_384, "full scale {p_full}");
    assert!(
        (p_half * 2 - p_full).abs() <= 2,
        "half volume {p_half} vs {p_full}"
    );
}

#[test]
fn mono_separation_mixes_both_sides() {
    let bytes = tone().note(0, 0, 0, note(200, 1)).build();
    let out = render_all(
        &bytes,
        Options {
            separation: 0,
            interpolate: false,
            ..Options::default()
        },
    );
    assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]));
    assert!(out.iter().any(|&s| s != 0));
}

#[test]
fn four_full_scale_voices_do_not_clip() {
    let mut b = tone();
    for channel in 0..4 {
        b = b.note(0, 0, channel, note(200, 1));
    }
    let out = render_all(&b.build(), hard());
    let peak = out.iter().map(|&s| i32::from(s).abs()).max().unwrap();
    assert!(peak <= 32_768 && peak > 30_000, "peak {peak}");
}

#[test]
fn a_silent_song_is_one_pattern_long() {
    assert_eq!(frames(&tone().build()), 64 * ROW);
}

#[test]
fn speed_and_tempo_change_row_length() {
    let speed = tone().note(0, 0, 0, effect(0xF, 3)).build();
    assert_eq!(frames(&speed), 64 * 3 * 960);
    // Tempo 250 halves the tick: 960 -> 480 frames.
    let tempo = tone().note(0, 0, 0, effect(0xF, 250)).build();
    assert_eq!(frames(&tempo), 64 * 6 * 480);
}

#[test]
fn pattern_break_skips_to_the_next_order() {
    let bytes = tone().orders(&[0, 1]).note(0, 0, 0, effect(0xD, 0)).build();
    assert_eq!(frames(&bytes), (1 + 64) * ROW);
    // D12 is decimal row 12 of the next pattern.
    let to_row = tone()
        .orders(&[0, 1])
        .note(0, 0, 0, effect(0xD, 0x12))
        .build();
    assert_eq!(frames(&to_row), (1 + 64 - 12) * ROW);
}

#[test]
fn jump_to_a_visited_row_ends_the_play() {
    let bytes = tone().note(0, 3, 0, effect(0xB, 0)).build();
    assert_eq!(frames(&bytes), 4 * ROW);
    let twice = render_all(
        &bytes,
        Options {
            loops: Some(3),
            ..hard()
        },
    );
    assert_eq!(twice.len() / 2, 3 * 4 * ROW);
}

#[test]
fn pattern_loop_repeats_rows() {
    let bytes = tone()
        .note(0, 0, 0, effect(0xE, 0x60))
        .note(0, 1, 0, effect(0xE, 0x62))
        .build();
    assert_eq!(frames(&bytes), (64 + 4) * ROW);
}

#[test]
fn pattern_delay_repeats_a_row() {
    let bytes = tone().note(0, 5, 0, effect(0xE, 0xE1)).build();
    assert_eq!(frames(&bytes), 65 * ROW);
}

#[test]
fn note_delay_holds_the_note_back() {
    // ED3: the note starts at tick 3, so the first three ticks are silent.
    let bytes = tone()
        .note(
            0,
            0,
            0,
            Note {
                period: 200,
                sample: 1,
                effect: 0xE,
                param: 0xD3,
            },
        )
        .build();
    let out = render_all(&bytes, hard());
    assert!(out[..2 * 3 * 960].iter().all(|&s| s == 0));
    assert!(out[2 * 3 * 960..2 * 6 * 960].iter().any(|&s| s != 0));
}

#[test]
fn note_cut_silences_the_channel() {
    let bytes = tone()
        .note(
            0,
            0,
            0,
            Note {
                period: 200,
                sample: 1,
                effect: 0xE,
                param: 0xC2,
            },
        )
        .build();
    let out = render_all(&bytes, hard());
    assert!(out[..2 * 960].iter().any(|&s| s != 0));
    assert!(out[2 * 3 * 960..2 * ROW].iter().all(|&s| s == 0));
}

#[test]
fn one_shot_samples_end_and_fall_silent() {
    let b = ModBuilder::new()
        .sample(1, square(32), 64, false)
        .note(0, 0, 0, note(100, 1))
        .build();
    let out = render_all(&b, hard());
    // 32 bytes at ~17.7 kB/s is under 2 ms; everything after must be silent.
    assert!(out[2 * 200..].iter().all(|&s| s == 0));
}

#[test]
fn counts_unsupported_effects() {
    let bytes = tone()
        .note(0, 0, 0, effect(0x8, 0x40))
        .note(0, 1, 1, effect(0x7, 0x11))
        .build();
    let module = Module::parse(&bytes).unwrap();
    let mut player = Player::new(&module, RATE, Options::default());
    let mut buf = vec![0i16; 2 * 4 * ROW];
    player.render(&mut buf);
    assert_eq!(player.unsupported_effects(), 2);
}

#[test]
fn slides_move_the_pitch() {
    let slide_up = Note {
        period: 400,
        sample: 1,
        effect: 1,
        param: 0x20,
    };
    let bytes = tone().note(0, 0, 0, slide_up).build();
    let l = left(&render_all(&bytes, hard()));
    let first = crossings(&l[..960]);
    let last = crossings(&l[5 * 960..ROW]);
    assert!(
        last > first,
        "slide up should raise pitch: {first} -> {last}"
    );
}

#[test]
fn render_is_chunking_independent() {
    let mut b = tone().orders(&[0, 1]);
    for row in (0..64).step_by(8) {
        b = b.note(
            0,
            row,
            0,
            Note {
                period: 150 + row as u16,
                sample: 1,
                effect: 4,
                param: 0x47,
            },
        );
        b = b.note(
            1,
            row,
            1,
            Note {
                period: 300,
                sample: 1,
                effect: 0xA,
                param: 0x04,
            },
        );
    }
    let bytes = b.build();
    let whole = render_all(&bytes, Options::default());
    let module = Module::parse(&bytes).unwrap();
    let mut player = Player::new(&module, RATE, Options::default());
    let mut chunked = Vec::new();
    let mut buf = [0i16; 2 * 97]; // an awkward size
    loop {
        let n = player.render(&mut buf);
        chunked.extend_from_slice(&buf[..n * 2]);
        if n == 0 {
            break;
        }
    }
    assert_eq!(whole, chunked);
    assert!(player.is_finished());
}

#[test]
fn endless_song_keeps_going_and_stays_bounded() {
    // A one-row loop forever: soak a few minutes of audio, checking each chunk.
    let bytes = tone()
        .note(0, 0, 0, note(200, 1))
        .note(0, 1, 0, effect(0xB, 0))
        .build();
    let module = Module::parse(&bytes).unwrap();
    let mut player = Player::new(
        &module,
        RATE,
        Options {
            loops: None,
            ..Options::default()
        },
    );
    let mut buf = vec![0i16; 2 * 4800];
    for _ in 0..(60 * 10) {
        assert_eq!(player.render(&mut buf), 4800);
    }
    assert!(!player.is_finished());
}

#[test]
fn rate_is_clamped() {
    let module = Module::parse(&tone().build()).unwrap();
    let mut player = Player::new(&module, 0, Options::default());
    let mut buf = [0i16; 64];
    assert_eq!(player.render(&mut buf), 32);
}
