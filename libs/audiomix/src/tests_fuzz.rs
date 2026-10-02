//! Seeded scripts of hostile and well-behaved calls from several clients,
//! interleaved with mixing and card progress (`FUZZ_SEED`, `FUZZ_CASES`).
//!
//! Whatever the script, the mixer must not panic, must keep
//! `position <= consumed <= committed` for every stream, must keep each
//! stream's position monotonic between stops, must finish every drain once
//! the card has played everything, and must never hold more streams than its
//! limits.

use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use fuzzkit::{for_seeds, Rng};
use virtio_snd::params::{audio_direction, Request};

use crate::tests::{mixer, VecRing, PERIOD};
use crate::{MixError, State};

/// What the script believes about one stream.
#[derive(Default)]
struct Model {
    owner: u64,
    committed: u64,
    position: u64,
    ring_frames: u64,
}

const OWNERS: [u64; 3] = [1, 2, 3];
const RATES: [u32; 6] = [8000, 22050, 44100, 48000, 96000, 47123];

fn random_request(rng: &mut Rng) -> Request {
    Request {
        direction: if rng.one_in(10) {
            audio_direction::CAPTURE
        } else {
            audio_direction::PLAYBACK
        },
        format: rng.below(5) as u32,
        rate_hz: *rng.pick(&RATES),
        channels: rng.range(0, 3) as u32,
        period_bytes: rng.range(0, 20000) as u32,
    }
}

#[test]
fn seeded_scripts_keep_the_stream_invariants() {
    for_seeds("audiomix::scripts", |_seed, rng| run_script(rng));
}

fn run_script(rng: &mut Rng) {
    let mut mixer = mixer();
    let mut models: BTreeMap<u32, Model> = BTreeMap::new();
    let mut card = 0u64;
    let mut card_played = 0u64;
    let mut now = 0u64;
    let mut out = vec![0i16; 2 * PERIOD];
    for _ in 0..400 {
        now += rng.below(30);
        let ids: Vec<u32> = models.keys().copied().collect();
        // Mostly real ids, sometimes made-up ones.
        let id = if ids.is_empty() || rng.one_in(8) {
            rng.below(40) as u32
        } else {
            *rng.pick(&ids)
        };
        // Mostly the right owner, sometimes an impostor.
        let owner = match models.get(&id) {
            Some(model) if !rng.one_in(6) => model.owner,
            _ => *rng.pick(&OWNERS),
        };
        match rng.below(12) {
            0 => {
                if let Ok((id, grant)) = mixer.open(owner, &random_request(rng), now) {
                    let ring_frames = u64::from(grant.buffer_bytes() / grant.frame_bytes());
                    models.insert(
                        id,
                        Model {
                            owner,
                            ring_frames,
                            ..Model::default()
                        },
                    );
                }
            }
            1 => {
                let bytes = rng.range(0, 70000) as usize;
                let ring = VecRing::new(bytes);
                rng.fill(&mut ring.0.borrow_mut());
                let _ = mixer.attach(id, owner, ring, now);
            }
            2 | 3 => {
                let model_committed = models.get(&id).map_or(0, |m| m.committed);
                let written = match rng.below(4) {
                    0 => rng.next_u64(),
                    1 => model_committed.saturating_sub(rng.below(10)),
                    _ => model_committed + rng.below(5000),
                };
                if let Ok(consumed) = mixer.commit(id, owner, written, now) {
                    let model = models.get_mut(&id).expect("committed to an unknown stream");
                    assert_eq!(model.owner, owner);
                    assert!(written >= model.committed, "commit went backwards");
                    assert!(written - consumed <= model.ring_frames);
                    assert!(consumed <= written);
                    model.committed = written;
                }
            }
            4 => {
                let _ = mixer.start(id, owner, now);
            }
            5 => {
                let was_playing = mixer
                    .statuses()
                    .find(|s| s.id == id)
                    .is_some_and(|s| matches!(s.state, State::Running | State::Draining));
                if mixer.stop(id, owner, now).is_ok() && was_playing {
                    // Numbering restarts at 0 after a stop.
                    let model = models.get_mut(&id).unwrap();
                    model.committed = 0;
                    model.position = 0;
                }
            }
            6 => {
                let _ = mixer.drain(id, owner, now);
            }
            7 => {
                if let Ok(position) = mixer.position(id, owner, now) {
                    let model = models.get_mut(&id).unwrap();
                    assert!(position <= model.committed, "played more than committed");
                    assert!(position >= model.position, "position went backwards");
                    model.position = position;
                }
            }
            8 => {
                if mixer.close(id, owner, now).is_ok() {
                    models.remove(&id);
                }
            }
            9 => {
                let gain = if rng.one_in(4) {
                    rng.next_u32()
                } else {
                    rng.below(1 << 18) as u32
                };
                let by = if rng.one_in(2) { None } else { Some(owner) };
                let result = mixer.set_volume(id, by, gain, now);
                if gain > crate::gain::MAX_Q16 {
                    assert!(result.is_err());
                }
                let _ = mixer.set_mute(id, by, rng.one_in(3), now);
                let _ = mixer.set_master(rng.below(1 << 18) as u32, rng.one_in(10));
            }
            _ => {
                // The card asks for a period and plays some of what it holds.
                if mixer.wants_output() || rng.one_in(3) {
                    card += PERIOD as u64;
                    mixer.mix(&mut out, card);
                }
                card_played = (card_played + rng.below(3 * PERIOD as u64)).min(card);
                mixer.played(card_played, |_| {});
            }
        }
        if rng.one_in(20) {
            mixer.reclaim(now, |id| {
                models.remove(&id);
            });
        }
        assert!(mixer.len() <= mixer.config().max_streams);
        assert_eq!(mixer.len(), models.len());
        for owner in OWNERS {
            let held = mixer.statuses().filter(|s| s.owner == owner).count();
            assert!(held <= mixer.config().max_per_owner);
        }
    }
    // Wind down: play everything; every draining stream must finish.
    let ids: Vec<u32> = models.keys().copied().collect();
    for &id in &ids {
        let owner = models[&id].owner;
        let _ = mixer.drain(id, owner, now);
    }
    for _ in 0..200 {
        if !(mixer.wants_output() || mixer.in_flight()) {
            break;
        }
        card += PERIOD as u64;
        mixer.mix(&mut out, card);
        mixer.played(card, |_| {});
    }
    for status in mixer.statuses() {
        assert_ne!(status.state, State::Draining, "a drain never completed");
    }
    assert_eq!(mixer.drain(u32::MAX, 1, now), Err(MixError::NotFound));
}
