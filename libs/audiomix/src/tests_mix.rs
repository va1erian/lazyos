//! What comes out of the mixer: exact passthrough, summing, saturation,
//! gains, channel mapping, resampling, ring wrap and underruns.

use std::vec;
use std::vec::Vec;

use crate::gain::UNITY;
use crate::tests::{mixer, open_attached, VecRing, OWNER, PERIOD};
use crate::Mixer;

/// One period of output.
fn mix_once(mixer: &mut Mixer<VecRing>, card_end: u64) -> Vec<i16> {
    let mut out = vec![0i16; 2 * PERIOD];
    mixer.mix(&mut out, card_end);
    out
}

/// A stereo ramp, distinct per channel.
fn ramp(frames: usize, offset: i16) -> Vec<i16> {
    (0..frames)
        .flat_map(|n| [n as i16 + offset, -(n as i16) - offset])
        .collect()
}

/// Commit `samples` (stereo) and start the stream.
fn feed(mixer: &mut Mixer<VecRing>, id: u32, owner: u64, ring: &VecRing, samples: &[i16]) {
    ring.write(0, 2, samples);
    mixer
        .commit(id, owner, (samples.len() / 2) as u64, 0)
        .unwrap();
    mixer.start(id, owner, 0).unwrap();
}

#[test]
fn one_stream_at_the_mix_rate_passes_through_exactly() {
    let mut mixer = mixer();
    let (id, ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    let samples = ramp(2 * PERIOD, 1);
    feed(&mut mixer, id, OWNER, &ring, &samples);
    assert_eq!(mix_once(&mut mixer, 1024), samples[..2 * PERIOD]);
    assert_eq!(mix_once(&mut mixer, 2048), samples[2 * PERIOD..]);
}

#[test]
fn streams_sum_and_saturate_instead_of_wrapping() {
    let mut mixer = mixer();
    let (a, ring_a, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    let (b, ring_b, _) = open_attached(&mut mixer, 9, 48000, 2);
    feed(&mut mixer, a, OWNER, &ring_a, &vec![1000; 2 * PERIOD]);
    let mut loud = vec![i16::MAX; 2 * PERIOD];
    loud[1] = -300;
    feed(&mut mixer, b, 9, &ring_b, &loud);
    let out = mix_once(&mut mixer, 1024);
    assert_eq!(out[0], i16::MAX, "saturated, not wrapped");
    assert_eq!(out[1], 700);
}

#[test]
fn stream_gain_mute_and_master_apply() {
    let mut mixer = mixer();
    let (id, ring, frames) = open_attached(&mut mixer, OWNER, 48000, 2);
    feed(
        &mut mixer,
        id,
        OWNER,
        &ring,
        &vec![10000; frames as usize * 2],
    );

    mixer.set_volume(id, Some(OWNER), UNITY / 2, 0).unwrap();
    assert!(mix_once(&mut mixer, 1024).iter().all(|&s| s == 5000));

    mixer.set_master(UNITY / 2, false).unwrap();
    assert!(mix_once(&mut mixer, 2048).iter().all(|&s| s == 2500));

    mixer.set_mute(id, None, true, 0).unwrap();
    let before = mixer.commit(id, OWNER, frames, 0).unwrap();
    assert!(mix_once(&mut mixer, 3072).iter().all(|&s| s == 0));
    let after = mixer.commit(id, OWNER, frames, 0).unwrap();
    assert_eq!(
        after - before,
        PERIOD as u64,
        "a muted stream keeps consuming"
    );

    mixer.set_mute(id, None, false, 0).unwrap();
    mixer.set_master(4 * UNITY, true).unwrap();
    assert!(
        mix_once(&mut mixer, 4096).iter().all(|&s| s == 0),
        "master mute"
    );
}

#[test]
fn mono_is_copied_to_both_sides() {
    let mut mixer = mixer();
    let (id, ring, _) = open_attached(&mut mixer, OWNER, 48000, 1);
    let samples: Vec<i16> = (0..PERIOD as i16).collect();
    ring.write(0, 1, &samples);
    mixer.commit(id, OWNER, PERIOD as u64, 0).unwrap();
    mixer.start(id, OWNER, 0).unwrap();
    let out = mix_once(&mut mixer, 1024);
    for (n, frame) in out.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(*frame, [n as i16, n as i16]);
    }
}

#[test]
fn a_resampled_stream_keeps_its_pitch() {
    let mut mixer = mixer();
    let (id, ring, frames) = open_attached(&mut mixer, OWNER, 44100, 1);
    let sine = |n: u64| {
        let t = n as f64 / 44100.0;
        (12000.0 * (2.0 * core::f64::consts::PI * 1000.0 * t).sin()) as i16
    };
    let mut written = 0u64;
    let mut out = Vec::new();
    mixer.start(id, OWNER, 0).unwrap();
    for period in 1..=40u64 {
        // Keep the ring full, as a well-behaved client would.
        let consumed = mixer.commit(id, OWNER, written, 0).unwrap();
        let room = frames - (written - consumed);
        let chunk: Vec<i16> = (written..written + room).map(sine).collect();
        ring.write(written, 1, &chunk);
        written += room;
        mixer.commit(id, OWNER, written, 0).unwrap();
        out.extend(mix_once(&mut mixer, period * PERIOD as u64));
    }
    let left: Vec<i16> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
    let crossings: Vec<usize> = left
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0 && w[1] >= 0)
        .map(|(i, _)| i)
        .collect();
    let span = (crossings[crossings.len() - 1] - crossings[0]) as f64;
    let hz = (crossings.len() - 1) as f64 * 48000.0 / span;
    assert!((hz - 1000.0).abs() < 2.0, "measured {hz} Hz");
    assert_eq!(mixer.statuses().next().unwrap().underruns, 0);
}

#[test]
fn frames_across_the_ring_end_come_out_in_order() {
    let mut mixer = mixer();
    let (id, ring, frames) = open_attached(&mut mixer, OWNER, 48000, 2);
    mixer.start(id, OWNER, 0).unwrap();
    // Write and play until the write position has wrapped twice.
    let mut written = 0u64;
    let mut card = 0u64;
    let mut expected = Vec::new();
    let mut got = Vec::new();
    while written < 2 * frames + 777 {
        let consumed = mixer.commit(id, OWNER, written, 0).unwrap();
        let room = (frames - (written - consumed)).min(1500);
        let samples = ramp(room as usize, (written % 1000) as i16);
        ring.write(written, 2, &samples);
        expected.extend_from_slice(&samples);
        written += room;
        mixer.commit(id, OWNER, written, 0).unwrap();
        card += PERIOD as u64;
        got.extend(mix_once(&mut mixer, card));
    }
    assert_eq!(&got[..], &expected[..got.len()]);
}

#[test]
fn a_running_stream_waits_for_a_whole_period_unless_draining() {
    let mut mixer = mixer();
    let (id, ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    feed(&mut mixer, id, OWNER, &ring, &vec![100; 2 * (PERIOD - 1)]);
    assert!(!mixer.wants_output());
    assert!(mix_once(&mut mixer, 1024).iter().all(|&s| s == 0));
    assert_eq!(mixer.commit(id, OWNER, PERIOD as u64 - 1, 0), Ok(0));
    assert_eq!(mixer.drain(id, OWNER, 0), Ok(false));
    assert!(mixer.wants_output());
    let out = mix_once(&mut mixer, 2048);
    assert!(out[..2 * (PERIOD - 1)].iter().all(|&s| s == 100));
    assert_eq!(&out[2 * (PERIOD - 1)..], [0, 0], "the short tail is padded");
}

#[test]
fn underruns_count_once_per_dry_spell() {
    let mut mixer = mixer();
    let (id, ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    feed(&mut mixer, id, OWNER, &ring, &vec![1; 2 * PERIOD]);
    let mut card = 0;
    let mut tick = |m: &mut Mixer<VecRing>| {
        card += PERIOD as u64;
        mix_once(m, card);
    };
    tick(&mut mixer);
    tick(&mut mixer);
    tick(&mut mixer);
    assert_eq!(mixer.statuses().next().unwrap().underruns, 1);
    ring.write(PERIOD as u64, 2, &vec![1; 2 * PERIOD]);
    mixer.commit(id, OWNER, 2 * PERIOD as u64, 0).unwrap();
    tick(&mut mixer);
    tick(&mut mixer);
    assert_eq!(mixer.statuses().next().unwrap().underruns, 2);
}

#[test]
fn a_hostile_ring_rewrite_only_changes_its_own_samples() {
    let mut mixer = mixer();
    let (good, ring_good, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    let (bad, ring_bad, _) = open_attached(&mut mixer, 9, 48000, 2);
    feed(&mut mixer, good, OWNER, &ring_good, &vec![10; 2 * PERIOD]);
    feed(&mut mixer, bad, 9, &ring_bad, &vec![0; 2 * PERIOD]);
    // The client scribbles over its whole ring after committing.
    ring_bad.0.borrow_mut().fill(0x55);
    let out = mix_once(&mut mixer, 1024);
    let noise = i16::from_le_bytes([0x55, 0x55]);
    assert!(out.iter().all(|&s| s == 10 + noise));
}
