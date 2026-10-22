//! The pacer: exact nominal pacing, feedback in every format real devices
//! send, and refusal of the ones no shift explains.

use crate::packet::{esit_microframes, Pacer};

#[test]
fn service_intervals() {
    assert_eq!(esit_microframes(false, 1), 8);
    assert_eq!(esit_microframes(false, 4), 64);
    assert_eq!(esit_microframes(true, 1), 1);
    assert_eq!(esit_microframes(true, 4), 8);
    assert_eq!(esit_microframes(true, 0), 1, "bInterval 0 reads as 1");
}

#[test]
fn nominal_44100_full_speed_is_exact() {
    let mut pacer = Pacer::new(44100, 8, 49, false);
    let ten: [u32; 10] = core::array::from_fn(|_| pacer.next_frames());
    assert_eq!(ten.iter().sum::<u32>(), 441);
    assert!(ten.iter().all(|&f| f == 44 || f == 45));
    // A long run never drifts by a frame.
    let total: u64 = (0..100_000).map(|_| u64::from(pacer.next_frames())).sum();
    assert_eq!(total, 4_410_000);
    // 45 + an eighth + 1, capped at the payload limit.
    assert_eq!(pacer.slot_frames(), 49);
    assert_eq!(Pacer::new(44100, 8, 64, false).slot_frames(), 51);
}

#[test]
fn nominal_48000_high_speed() {
    let mut pacer = Pacer::new(48000, 1, 25, true);
    assert!((0..800).all(|_| pacer.next_frames() == 6));
}

#[test]
fn full_speed_feedback_10_14() {
    let mut pacer = Pacer::new(48000, 8, 49, false);
    // 48.25 frames per frame, a fast device clock.
    let value: u32 = (48 << 14) + (1 << 12);
    assert!(pacer.feedback(&value.to_le_bytes()[..3]));
    let total: u32 = (0..4).map(|_| pacer.next_frames()).sum();
    assert_eq!(total, 193);
    assert_eq!(pacer.rate_hz(), 48250);
}

#[test]
fn high_speed_feedback_16_16_and_shifted() {
    let mut pacer = Pacer::new(48000, 1, 25, true);
    assert!(pacer.feedback(&(6u32 << 16).to_le_bytes()));
    assert_eq!(pacer.rate_hz(), 48000);
    // A device sending 10.14 at high speed: two bits short, found by shift.
    let mut shifted = Pacer::new(48000, 1, 25, true);
    assert!(shifted.feedback(&(6u32 << 14).to_le_bytes()));
    assert_eq!(shifted.rate_hz(), 48000);
}

#[test]
fn hostile_feedback_is_ignored() {
    let mut pacer = Pacer::new(48000, 8, 49, false);
    assert!(!pacer.feedback(&[0xFF, 0xFF, 0xFF]));
    assert!(!pacer.feedback(&[0, 0, 0, 0]));
    assert!(!pacer.feedback(&[1]));
    // Twice the nominal rate is no clock error.
    assert!(!pacer.feedback(&(96u32 << 14).to_le_bytes()[..3]) || pacer.rate_hz() == 48000);
    assert_eq!(pacer.rate_hz(), 48000);
    // Never more than the payload allows, whatever the pace.
    let mut capped = Pacer::new(48000, 8, 40, false);
    assert!((0..100).all(|_| capped.next_frames() <= 40));
}
