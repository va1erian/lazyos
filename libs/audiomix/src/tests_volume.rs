//! `sndd`'s `SetVolume` / `SetMute` path on the host (issue #452): requests
//! encoded and decoded with the generated `os.lazy.audio.v1` codec, applied
//! to a [`StreamVolume`], and judged on a staged period.

use std::vec::Vec;

use messenger_generated::os_lazy_audio_v1 as audio;

use crate::gain::{MAX_Q16, UNITY};
use crate::volume::{StreamVolume, VolumeError};

/// A period of a 16-bit triangle wave peaking at `peak`, as `S16Le` bytes.
fn triangle(peak: i16, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frames * 2);
    let period = 64i32;
    for frame in 0..frames as i32 {
        let phase = frame % period;
        let ramp = if phase < period / 2 {
            phase
        } else {
            period - phase
        };
        let sample = (i32::from(peak) * (4 * ramp - period) / period) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

fn rms(bytes: &[u8]) -> f64 {
    let values = samples(bytes);
    let sum: f64 = values.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / values.len() as f64).sqrt()
}

/// What `sndd`'s `METHOD_SETVOLUME` arm does with a request body.
fn set_volume(volume: &mut StreamVolume, gain_q16: u32, s16le: bool) -> Result<(), VolumeError> {
    let body = audio::encode_set_volume_args(&audio::SetVolumeArgs {
        stream: 3,
        gain_q16,
    })
    .unwrap();
    let args = audio::decode_set_volume_args(&body).unwrap();
    assert_eq!(args.stream, 3);
    volume.set_volume(args.gain_q16, s16le)
}

/// What `sndd`'s `METHOD_SETMUTE` arm does with a request body.
fn set_mute(volume: &mut StreamVolume, mute: bool) {
    let body = audio::encode_set_mute_args(&audio::SetMuteArgs { stream: 3, mute }).unwrap();
    volume.set_mute(audio::decode_set_mute_args(&body).unwrap().mute);
}

#[test]
fn a_new_stream_plays_unchanged() {
    let volume = StreamVolume::new();
    let tone = triangle(20000, 480);
    let mut staged = tone.clone();
    volume.stage(&mut staged, true);
    assert_eq!(staged, tone);
    assert!(volume.gain().is_unity() && !volume.muted());
}

#[test]
fn half_gain_from_the_wire_measures_minus_six_db() {
    let mut volume = StreamVolume::new();
    set_volume(&mut volume, UNITY / 2, true).unwrap();
    let tone = triangle(20000, 4800);
    let mut staged = tone.clone();
    volume.stage(&mut staged, true);
    let db = 20.0 * (rms(&staged) / rms(&tone)).log10();
    assert!((db + 6.02).abs() < 0.05, "{db} dB");
}

#[test]
fn zero_gain_is_silence_and_max_gain_saturates() {
    let mut volume = StreamVolume::new();
    set_volume(&mut volume, 0, true).unwrap();
    let mut staged = triangle(20000, 256);
    volume.stage(&mut staged, true);
    assert!(samples(&staged).iter().all(|&s| s == 0));

    set_volume(&mut volume, MAX_Q16, true).unwrap();
    let mut staged = triangle(20000, 256);
    volume.stage(&mut staged, true);
    let peak = samples(&staged).iter().map(|s| s.unsigned_abs()).max();
    assert_eq!(peak, Some(i16::MAX as u16 + 1), "clipped, never wrapped");
}

#[test]
fn refused_requests_leave_the_volume_alone() {
    let mut volume = StreamVolume::new();
    set_volume(&mut volume, UNITY / 4, true).unwrap();
    assert_eq!(
        set_volume(&mut volume, MAX_Q16 + 1, true),
        Err(VolumeError::OutOfRange)
    );
    assert_eq!(
        set_volume(&mut volume, u32::MAX, true),
        Err(VolumeError::OutOfRange)
    );
    assert_eq!(volume.gain().q16(), UNITY / 4);
}

#[test]
fn other_formats_accept_only_unity_and_stay_untouched() {
    let mut volume = StreamVolume::new();
    assert_eq!(
        set_volume(&mut volume, UNITY / 2, false),
        Err(VolumeError::Unsupported)
    );
    assert_eq!(set_volume(&mut volume, UNITY, false), Ok(()));
    let tone = triangle(1000, 64);
    let mut staged = tone.clone();
    volume.stage(&mut staged, false);
    assert_eq!(staged, tone);
}

#[test]
fn mute_stages_silence_and_unmute_restores_the_gain() {
    let mut volume = StreamVolume::new();
    set_volume(&mut volume, UNITY / 2, true).unwrap();
    set_mute(&mut volume, true);
    let tone = triangle(20000, 480);
    let mut staged = tone.clone();
    volume.stage(&mut staged, true);
    assert!(staged.iter().all(|&b| b == 0));
    // Silence even for a format the driver cannot scale.
    let mut other = tone.clone();
    volume.stage(&mut other, false);
    assert!(other.iter().all(|&b| b == 0));

    set_mute(&mut volume, false);
    let mut staged = tone.clone();
    volume.stage(&mut staged, true);
    let expected: Vec<i16> = samples(&tone).iter().map(|&s| s >> 1).collect();
    assert_eq!(samples(&staged), expected);
}
