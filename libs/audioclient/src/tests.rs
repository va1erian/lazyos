//! The client against the real mixer: `libs/audiomix`'s engine and wire
//! layer behind a fake transport that paces a virtual card the way `audiod`
//! does (a period at a time, a few periods ahead, 480 frames per tick).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use audiomix::service::{self, Outcome, Request};
use audiomix::{Config, Mixer};

use crate::{
    Error, MixerControl, Params, PlaybackStream, Result, RingBuffer, RingRef, Transfers, Transport,
    UNITY_GAIN,
};

const PERIOD: usize = 1024;
const LEAD: u64 = 3 * PERIOD as u64;
const FRAMES_PER_TICK: u64 = 480;
const ETIMEDOUT: i64 = 110;

/// A ring the client writes and the mixer reads.
#[derive(Clone)]
pub(crate) struct SharedRing {
    handle: u64,
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl RingBuffer for SharedRing {
    fn share(&self) -> RingRef {
        RingRef {
            handle: self.handle,
            len: self.bytes.borrow().len() as u64,
        }
    }

    fn len(&self) -> usize {
        self.bytes.borrow().len()
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

impl audiomix::Ring for SharedRing {
    fn len(&self) -> usize {
        self.bytes.borrow().len()
    }

    fn read(&self, offset: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self.bytes.borrow()[offset..offset + dst.len()]);
    }
}

pub(crate) struct State {
    pub(crate) mixer: Mixer<SharedRing>,
    rings: BTreeMap<u64, SharedRing>,
    next_handle: u64,
    pub(crate) clock: u64,
    card: u64,
    played: u64,
    drained: Vec<u32>,
    /// Everything the card played, interleaved stereo.
    pub(crate) out: Vec<i16>,
    /// A frozen card plays nothing (stall tests).
    pub(crate) frozen: bool,
    pub(crate) sender: u64,
}

pub(crate) struct Fake(pub(crate) RefCell<State>);

impl Fake {
    pub(crate) fn new() -> Fake {
        Fake(RefCell::new(State {
            mixer: Mixer::new(Config::new(48000, PERIOD)),
            rings: BTreeMap::new(),
            next_handle: 1,
            clock: 0,
            card: 0,
            played: 0,
            drained: Vec::new(),
            out: Vec::new(),
            frozen: false,
            sender: 1,
        }))
    }

    /// One tick of the virtual card: keep it a few periods ahead, play.
    fn tick(&self) {
        let mut state = self.0.borrow_mut();
        state.clock += 1;
        if state.frozen {
            return;
        }
        let mut period = vec![0i16; 2 * PERIOD];
        while state.card - state.played < LEAD && state.mixer.wants_output() {
            let end = state.card + PERIOD as u64;
            state.mixer.mix(&mut period, end);
            state.out.extend_from_slice(&period);
            state.card = end;
        }
        state.played = (state.played + FRAMES_PER_TICK).min(state.card);
        let played = state.played;
        let mut done = Vec::new();
        state.mixer.played(played, |id| done.push(id));
        state.drained.extend(done);
    }
}

impl Transport for Fake {
    type Ring = SharedRing;

    fn call(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        transfers: Transfers,
        deadline: Option<u64>,
    ) -> Result<Vec<u8>> {
        let outcome = {
            let mut state = self.0.borrow_mut();
            let ring = transfers.buffers.first().and_then(|r| {
                let ring = state.rings.get(&r.handle)?.clone();
                let fits = r.len <= ring.bytes.borrow().len() as u64;
                fits.then_some(ring)
            });
            let request = Request {
                interface,
                method,
                body: &body,
                sender: state.sender,
                ring,
                now: state.clock,
            };
            service::handle(Some(&mut state.mixer), request)
        };
        match outcome {
            Outcome::Reply(body) => Ok(body),
            Outcome::Refuse(errno) => Err(Error::Errno(errno)),
            Outcome::DrainPending(stream) => loop {
                self.tick();
                let mut state = self.0.borrow_mut();
                if let Some(at) = state.drained.iter().position(|&id| id == stream) {
                    state.drained.remove(at);
                    return Ok(service::drained_reply());
                }
                if deadline.is_some_and(|d| state.clock > d) {
                    return Err(Error::Errno(ETIMEDOUT));
                }
            },
        }
    }

    fn create_ring(&self, bytes: usize) -> Result<SharedRing> {
        let mut state = self.0.borrow_mut();
        let handle = state.next_handle;
        state.next_handle += 1;
        let ring = SharedRing {
            handle,
            bytes: Rc::new(RefCell::new(vec![0; bytes])),
        };
        state.rings.insert(handle, ring.clone());
        Ok(ring)
    }

    fn now(&self) -> u64 {
        self.0.borrow().clock
    }

    fn sleep(&self) {
        self.tick();
    }
}

fn pattern(frames: usize, seed: i16) -> Vec<i16> {
    (0..frames)
        .flat_map(|n| {
            let v = (n as i16).wrapping_mul(7).wrapping_add(seed);
            [v, v.wrapping_neg()]
        })
        .collect()
}

#[test]
fn every_sample_written_comes_out_in_order() {
    let fake = Fake::new();
    let mut stream = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    let samples = pattern(48000, 3);
    // Odd chunk sizes, so writes straddle the ring end everywhere.
    let mut at = 0;
    for size in [1usize, 777, 4096, 13, 8192, 3333].iter().cycle() {
        if at >= samples.len() {
            break;
        }
        let end = (at + size * 2).min(samples.len());
        stream.write(&samples[at..end]).unwrap();
        at = end;
    }
    assert_eq!(stream.finish().unwrap(), 48000);
    let out = &fake.0.borrow().out;
    assert_eq!(&out[..samples.len()], &samples[..]);
    assert!(
        out[samples.len()..].iter().all(|&s| s == 0),
        "padding is silence"
    );
    assert!(fake.0.borrow().mixer.is_empty(), "finish closed the stream");
}

#[test]
fn a_sound_shorter_than_the_ring_starts_at_drain() {
    let fake = Fake::new();
    let mut stream = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    stream.write(&pattern(100, 0)).unwrap();
    assert_eq!(stream.finish().unwrap(), 100);
}

#[test]
fn volume_mute_and_master_reach_the_output() {
    let fake = Fake::new();
    let mut stream = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    stream.set_volume(UNITY_GAIN / 2).unwrap();
    stream.write(&vec![8000i16; 2 * 4096]).unwrap();
    stream.finish().unwrap();
    assert!(fake.0.borrow().out[..2 * 4096].iter().all(|&s| s == 4000));

    let control = MixerControl::new(&fake);
    control.set_master(UNITY_GAIN / 4, false).unwrap();
    let mut muted = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    muted.set_mute(true).unwrap();
    muted.write(&vec![8000i16; 2 * 4096]).unwrap();
    assert_eq!(
        muted.finish().unwrap(),
        4096,
        "muted streams still play out"
    );
    let master = control.master().unwrap();
    assert_eq!((master.gain_q16, master.mute), (UNITY_GAIN / 4, false));
}

#[test]
fn two_streams_are_mixed() {
    let fake = Fake::new();
    let mut a = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    let mut b = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    assert_ne!(a.grant().stream, b.grant().stream);
    let listed = MixerControl::new(&fake).streams().unwrap();
    assert_eq!(listed.len(), 2);
    // Fill both rings, then let them play together.
    let frames = a.free_frames().unwrap() as usize;
    a.write(&vec![1000i16; 2 * frames]).unwrap();
    b.write(&vec![234i16; 2 * frames]).unwrap();
    a.drain().unwrap();
    b.drain().unwrap();
    let out = &fake.0.borrow().out;
    assert!(out.iter().take(2 * frames).all(|&s| s == 1234), "summed");
}

#[test]
fn a_mono_stream_at_another_rate_plays_out() {
    let fake = Fake::new();
    let mut stream = PlaybackStream::open(&fake, Params::new(22050, 1)).unwrap();
    assert_eq!((stream.rate(), stream.channels()), (22050, 1));
    stream.write(&vec![500i16; 22050]).unwrap();
    assert_eq!(stream.finish().unwrap(), 22050);
    let out = &fake.0.borrow().out;
    let loud = out.iter().filter(|&&s| s == 500).count() / 2;
    assert!(loud.abs_diff(48000) < 100, "{loud} frames at 48 kHz");
}

#[test]
fn bad_writes_and_limits_are_reported() {
    let fake = Fake::new();
    let mut stream = PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap();
    assert_eq!(stream.write(&[1, 2, 3]), Err(Error::Errno(22)));
    let more: Vec<_> = (0..3)
        .map(|_| PlaybackStream::open(&fake, Params::new(48000, 2)).unwrap())
        .collect();
    // Four per client.
    assert!(matches!(
        PlaybackStream::open(&fake, Params::new(48000, 2)),
        Err(Error::Errno(16))
    ));
    drop(more);
    drop(stream);
    assert!(fake.0.borrow().mixer.is_empty(), "drop gives streams back");
}

#[test]
fn a_frozen_card_stalls_writes_and_drains_instead_of_hanging() {
    let fake = Fake::new();
    fake.0.borrow_mut().frozen = true;
    let mut stream = PlaybackStream::open(&fake, Params::new(48000, 2).stall_ticks(50)).unwrap();
    let frames = stream.free_frames().unwrap() as usize;
    assert_eq!(
        stream.write(&vec![1i16; 2 * (frames + 1)]),
        Err(Error::Stalled)
    );
    assert_eq!(stream.drain(), Err(Error::Errno(ETIMEDOUT)));
}

#[test]
fn seeded_write_patterns_play_exactly_what_was_written() {
    fuzzkit::for_seeds("audioclient::writes", |_seed, rng| {
        let fake = Fake::new();
        let channels = 1 + rng.below(2) as u32;
        let period = 64 * (1 + rng.below(64) as u32);
        let params = Params::new(48000, channels).period_bytes(period);
        let mut stream = PlaybackStream::open(&fake, params).unwrap();
        let mut expected = Vec::new();
        let total = rng.range(1, 20000) as usize;
        let mut sent = 0;
        while sent < total {
            let frames = (rng.range(1, 3000) as usize).min(total - sent);
            let chunk: Vec<i16> = (0..frames * channels as usize)
                .map(|_| rng.next_u32() as i16)
                .collect();
            if rng.one_in(3) {
                // Non-blocking: write what fits, keep the rest for later.
                let done = stream.try_write(&chunk).unwrap();
                expected.extend_from_slice(&chunk[..done * channels as usize]);
                sent += done;
                fake.sleep();
            } else {
                stream.write(&chunk).unwrap();
                expected.extend_from_slice(&chunk);
                sent += frames;
            }
        }
        assert_eq!(stream.finish().unwrap(), total as u64);
        let out = fake.0.borrow().out.clone();
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        let first: Vec<i16> = expected
            .iter()
            .step_by(channels as usize)
            .copied()
            .collect();
        assert_eq!(&left[..first.len()], &first[..]);
    });
}
