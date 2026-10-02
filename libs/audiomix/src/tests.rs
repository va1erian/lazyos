//! The `os.lazy.audio.v1` stream contract, as the mixer enforces it.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use virtio_snd::params::{audio_direction, audio_format, Request};

use crate::{Config, MixError, Mixer, Ring, State};

/// A ring both the test (as the client) and the mixer can see.
#[derive(Clone)]
pub(crate) struct VecRing(pub(crate) Rc<RefCell<Vec<u8>>>);

impl VecRing {
    pub(crate) fn new(bytes: usize) -> VecRing {
        VecRing(Rc::new(RefCell::new(vec![0; bytes])))
    }

    /// Write `samples` as frame `frame` onward (the client side), wrapping.
    pub(crate) fn write(&self, frame: u64, channels: usize, samples: &[i16]) {
        let mut ring = self.0.borrow_mut();
        let frame_bytes = 2 * channels;
        let frames = ring.len() / frame_bytes;
        for (index, chunk) in samples.chunks_exact(channels).enumerate() {
            let at = ((frame as usize + index) % frames) * frame_bytes;
            for (channel, sample) in chunk.iter().enumerate() {
                ring[at + 2 * channel..at + 2 * channel + 2].copy_from_slice(&sample.to_le_bytes());
            }
        }
    }
}

impl Ring for VecRing {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }

    fn read(&self, offset: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self.0.borrow()[offset..offset + dst.len()]);
    }
}

pub(crate) const RATE: u32 = 48000;
pub(crate) const PERIOD: usize = 1024;
pub(crate) const OWNER: u64 = 7;
const OTHER: u64 = 8;

pub(crate) fn mixer() -> Mixer<VecRing> {
    Mixer::new(Config::new(RATE, PERIOD))
}

pub(crate) fn request(rate_hz: u32, channels: u32, period_bytes: u32) -> Request {
    Request {
        direction: audio_direction::PLAYBACK,
        format: audio_format::S16_LE,
        rate_hz,
        channels,
        period_bytes,
    }
}

/// Open and attach a stream; returns its id, ring and ring frames.
pub(crate) fn open_attached(
    mixer: &mut Mixer<VecRing>,
    owner: u64,
    rate: u32,
    channels: u32,
) -> (u32, VecRing, u64) {
    let (id, grant) = mixer
        .open(owner, &request(rate, channels, 8192), 0)
        .unwrap();
    let ring = VecRing::new(grant.buffer_bytes() as usize);
    mixer.attach(id, owner, ring.clone(), 0).unwrap();
    let frames = u64::from(grant.buffer_bytes() / grant.frame_bytes());
    (id, ring, frames)
}

#[test]
fn ids_are_never_zero_and_never_reused_while_open() {
    let mut mixer = mixer();
    let mut ids = Vec::new();
    for _ in 0..4 {
        let (id, _) = mixer.open(OWNER, &request(48000, 2, 4096), 0).unwrap();
        assert_eq!(id, mixer.statuses().last().unwrap().id);
        ids.push(id);
    }
    assert!(!ids.contains(&0));
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
}

#[test]
fn stream_limits_per_owner_and_overall() {
    let mut mixer = mixer();
    for _ in 0..4 {
        mixer.open(OWNER, &request(48000, 2, 4096), 0).unwrap();
    }
    assert_eq!(
        mixer.open(OWNER, &request(48000, 2, 4096), 0),
        Err(MixError::Busy)
    );
    for owner in 100..112 {
        mixer.open(owner, &request(48000, 2, 4096), 0).unwrap();
    }
    assert_eq!(mixer.len(), 16);
    assert_eq!(
        mixer.open(200, &request(48000, 2, 4096), 0),
        Err(MixError::Busy)
    );
    // Closing one frees a slot.
    let id = mixer.statuses().next().unwrap().id;
    mixer.close(id, OWNER, 0).unwrap();
    assert!(mixer.open(200, &request(48000, 2, 4096), 0).is_ok());
}

#[test]
fn malformed_opens_fail_and_odd_ones_snap() {
    let mut mixer = mixer();
    assert_eq!(
        mixer.open(OWNER, &request(48000, 0, 4096), 0),
        Err(MixError::Invalid)
    );
    let capture = Request {
        direction: audio_direction::CAPTURE,
        ..request(48000, 2, 4096)
    };
    assert_eq!(mixer.open(OWNER, &capture, 0), Err(MixError::Unsupported));
    let (_, grant) = mixer.open(OWNER, &request(47000, 2, 4099), 0).unwrap();
    assert_eq!(grant.rate_hz, 48000);
    assert_eq!(grant.period_bytes % 4, 0);
}

#[test]
fn small_periods_are_widened_to_cover_two_output_periods() {
    let mut mixer = mixer();
    for (rate, channels) in [(48000, 2), (96000, 2), (192000, 1), (8000, 1)] {
        let (id, grant) = mixer.open(OWNER, &request(rate, channels, 64), 0).unwrap();
        let ring_frames = u64::from(grant.buffer_bytes() / grant.frame_bytes());
        let need = u64::from(rate) * PERIOD as u64 / u64::from(RATE);
        assert!(ring_frames >= 2 * need, "{rate} Hz: ring {ring_frames}");
        mixer.close(id, OWNER, 0).unwrap();
    }
}

#[test]
fn only_the_owner_drives_a_stream() {
    let mut mixer = mixer();
    let (id, _ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    assert_eq!(mixer.start(id, OTHER, 0), Err(MixError::Access));
    assert_eq!(mixer.stop(id, OTHER, 0), Err(MixError::Access));
    assert_eq!(mixer.commit(id, OTHER, 1, 0), Err(MixError::Access));
    assert_eq!(mixer.drain(id, OTHER, 0), Err(MixError::Access));
    assert_eq!(mixer.position(id, OTHER, 0), Err(MixError::Access));
    assert_eq!(mixer.close(id, OTHER, 0), Err(MixError::Access));
    assert_eq!(
        mixer.set_volume(id, Some(OTHER), 1, 0),
        Err(MixError::Access)
    );
    assert_eq!(
        mixer.attach(id, OTHER, VecRing::new(1 << 16), 0),
        Err(MixError::Access)
    );
    // The control panel (no owner) may change its volume, nothing else.
    assert!(mixer.set_volume(id, None, 1, 0).is_ok());
    assert!(mixer.set_mute(id, None, true, 0).is_ok());
    assert_eq!(mixer.close(id, OWNER, 0), Ok(()));
    assert_eq!(mixer.start(id, OWNER, 0), Err(MixError::NotFound));
}

#[test]
fn attach_rules() {
    let mut mixer = mixer();
    let (id, grant) = mixer.open(OWNER, &request(48000, 2, 4096), 0).unwrap();
    assert_eq!(mixer.commit(id, OWNER, 0, 0), Err(MixError::Invalid));
    assert_eq!(mixer.start(id, OWNER, 0), Err(MixError::Invalid));
    let short = VecRing::new(grant.buffer_bytes() as usize - 1);
    assert_eq!(mixer.attach(id, OWNER, short, 0), Err(MixError::Invalid));
    let ring = VecRing::new(grant.buffer_bytes() as usize);
    assert_eq!(mixer.attach(id, OWNER, ring.clone(), 0), Ok(()));
    assert_eq!(mixer.attach(id, OWNER, ring, 0), Err(MixError::Busy));
}

#[test]
fn commit_counters_are_monotonic_and_bounded_by_the_ring() {
    let mut mixer = mixer();
    let (id, _ring, frames) = open_attached(&mut mixer, OWNER, 48000, 2);
    assert_eq!(mixer.commit(id, OWNER, 100, 0), Ok(0));
    assert_eq!(mixer.commit(id, OWNER, 50, 0), Err(MixError::Invalid));
    assert_eq!(
        mixer.commit(id, OWNER, frames + 1, 0),
        Err(MixError::Invalid)
    );
    assert_eq!(mixer.commit(id, OWNER, frames, 0), Ok(0));
    assert_eq!(mixer.commit(id, OWNER, u64::MAX, 0), Err(MixError::Invalid));
    // Once some is consumed, the window slides.
    mixer.start(id, OWNER, 0).unwrap();
    let mut out = vec![0i16; 2 * PERIOD];
    mixer.mix(&mut out, PERIOD as u64);
    assert_eq!(
        mixer.commit(id, OWNER, frames + PERIOD as u64, 0),
        Ok(PERIOD as u64)
    );
    assert_eq!(
        mixer.commit(id, OWNER, 2 * frames, 0),
        Err(MixError::Invalid)
    );
}

#[test]
fn lifecycle_states_follow_the_driver() {
    let mut mixer = mixer();
    let (id, _ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    let state = |m: &Mixer<VecRing>| m.statuses().next().unwrap().state;
    assert_eq!(state(&mixer), State::Idle);
    assert_eq!(mixer.drain(id, OWNER, 0), Err(MixError::Invalid));
    mixer.start(id, OWNER, 0).unwrap();
    mixer.start(id, OWNER, 0).unwrap();
    assert_eq!(state(&mixer), State::Running);
    mixer.commit(id, OWNER, 4096, 0).unwrap();
    mixer.stop(id, OWNER, 0).unwrap();
    assert_eq!(state(&mixer), State::Stopped);
    // Numbering restarts at 0 after a stop.
    assert_eq!(mixer.commit(id, OWNER, 10, 0), Ok(0));
    assert_eq!(mixer.position(id, OWNER, 0), Ok(0));
    mixer.start(id, OWNER, 0).unwrap();
    // Nothing committed beyond 10 frames: draining completes once those play.
    assert_eq!(mixer.drain(id, OWNER, 0), Ok(false));
    let mut out = vec![0i16; 2 * PERIOD];
    mixer.mix(&mut out, PERIOD as u64);
    let mut drained = Vec::new();
    mixer.played(PERIOD as u64 - 1, |id| drained.push(id));
    assert!(drained.is_empty());
    mixer.played(PERIOD as u64, |id| drained.push(id));
    assert_eq!(drained, [id]);
    assert_eq!(state(&mixer), State::Drained);
    assert_eq!(mixer.position(id, OWNER, 0), Ok(10));
    assert_eq!(mixer.commit(id, OWNER, 20, 0), Err(MixError::Invalid));
    assert_eq!(mixer.start(id, OWNER, 0), Err(MixError::Busy));
    assert_eq!(mixer.drain(id, OWNER, 0), Ok(true));
}

#[test]
fn draining_an_empty_stream_completes_at_once() {
    let mut mixer = mixer();
    let (id, _ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    mixer.start(id, OWNER, 0).unwrap();
    assert_eq!(mixer.drain(id, OWNER, 0), Ok(true));
}

#[test]
fn position_never_passes_consumed_and_follows_the_card() {
    let mut mixer = mixer();
    let (id, _ring, frames) = open_attached(&mut mixer, OWNER, 48000, 2);
    mixer.commit(id, OWNER, frames, 0).unwrap();
    mixer.start(id, OWNER, 0).unwrap();
    let mut out = vec![0i16; 2 * PERIOD];
    let mut card = 0;
    for _ in 0..3 {
        card += PERIOD as u64;
        mixer.mix(&mut out, card);
    }
    assert_eq!(mixer.position(id, OWNER, 0), Ok(0));
    mixer.played(PERIOD as u64, |_| {});
    assert_eq!(mixer.position(id, OWNER, 0), Ok(PERIOD as u64));
    mixer.played(card, |_| {});
    assert_eq!(mixer.position(id, OWNER, 0), Ok(3 * PERIOD as u64));
    assert!(!mixer.in_flight());
}

#[test]
fn abandoned_streams_are_reclaimed_but_draining_ones_are_not() {
    let mut mixer = mixer();
    let (idle, _a, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    let (busy, _b, frames) = open_attached(&mut mixer, OTHER, 48000, 2);
    mixer.commit(busy, OTHER, frames, 0).unwrap();
    mixer.start(busy, OTHER, 0).unwrap();
    // Committed but never started also counts as abandoned.
    let (never, _c, _) = open_attached(&mut mixer, 9, 48000, 2);
    mixer.commit(never, 9, 100, 0).unwrap();

    let mut gone = Vec::new();
    mixer.reclaim(1000, |id| gone.push(id));
    assert!(gone.is_empty(), "not silent long enough yet");
    mixer.reclaim(1001, |id| gone.push(id));
    gone.sort();
    let mut expected = vec![idle, never];
    expected.sort();
    assert_eq!(gone, expected);
    // The running stream with frames left keeps playing.
    assert_eq!(mixer.len(), 1);
    mixer.drain(busy, OTHER, 1001).unwrap();
    mixer.reclaim(1_000_000, |id| gone.push(id));
    assert_eq!(mixer.len(), 1);
}

#[test]
fn gains_are_validated() {
    let mut mixer = mixer();
    let (id, _ring, _) = open_attached(&mut mixer, OWNER, 48000, 2);
    assert_eq!(
        mixer.set_volume(id, Some(OWNER), crate::gain::MAX_Q16 + 1, 0),
        Err(MixError::Invalid)
    );
    assert_eq!(
        mixer.set_master(crate::gain::MAX_Q16 + 1, false),
        Err(MixError::Invalid)
    );
    assert_eq!(mixer.set_volume(99, None, 1, 0), Err(MixError::NotFound));
    mixer.set_master(32768, true).unwrap();
    assert_eq!(mixer.master().0.q16(), 32768);
    assert!(mixer.master().1);
}
