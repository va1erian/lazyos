//! Seeded fuzz: random bytes and random fields through the decoder and the
//! validator (docs/notify-plan.md, "Acceptance"), and random operation
//! sequences through the center with its invariants checked after every
//! step. `FUZZ_CASES` / `FUZZ_SEED` as in `fuzzkit`.

use std::collections::HashMap;

use fuzzkit::{for_seeds, Rng};
use lazymotion::Motion;
use messenger_generated::os_lazy_notify_v1 as wire;

use super::center::*;
use super::note::{CloseReason, Request};
use super::policy::Policy;

fn text(rng: &mut Rng, max: u64) -> String {
    let len = rng.below(max);
    (0..len)
        .map(|_| char::from(rng.range(0, 0x7f) as u8))
        .collect()
}

fn image(rng: &mut Rng) -> wire::Image {
    let (w, h) = (rng.below(70) as u32, rng.below(70) as u32);
    let mut len = (w * h * 4) as usize;
    if rng.one_in(4) {
        len = rng.below(64) as usize;
    }
    wire::Image {
        width: w,
        height: h,
        data: rng.bytes(len),
    }
}

fn args(rng: &mut Rng) -> wire::NotifyArgs {
    wire::NotifyArgs {
        level: rng.below(6) as u32,
        title: text(rng, 90),
        body: text(rng, 300),
        icon: rng.one_in(2).then(|| wire::Icon {
            lucide: rng.one_in(2).then(|| text(rng, 12)),
            image: rng.one_in(2).then(|| image(rng)),
        }),
        actions: (0..rng.below(6))
            .map(|_| wire::Action {
                id: rng.below(5) as u32,
                label: text(rng, 30),
            })
            .collect(),
        progress: rng.one_in(2).then(|| rng.below(130) as u32),
        replaces_id: rng.one_in(3).then(|| rng.below(40)),
        timeout: rng.one_in(2).then(|| rng.below(100_000) as u32),
    }
}

/// A request that passes validation, for the queue fuzz.
fn request(rng: &mut Rng) -> Request {
    loop {
        let mut a = args(rng);
        a.level = rng.below(4) as u32;
        if a.title.trim().is_empty() {
            a.title = String::from("t");
        }
        if let Ok(request) = Request::from_wire(a) {
            return request;
        }
    }
}

#[test]
fn the_decoder_and_validator_never_panic_on_garbage() {
    for_seeds("notify_decoder_garbage", |_, rng| {
        let len = rng.below(300) as usize;
        let junk = rng.bytes(len);
        if let Ok(decoded) = wire::decode_notify_args(&junk) {
            let _ = Request::from_wire(decoded);
        }
        // A valid encoding with a cut or flipped bytes.
        let Ok(mut body) = wire::encode_notify_args(&args(rng)) else {
            return;
        };
        if rng.one_in(2) && !body.is_empty() {
            body.truncate(rng.below(body.len() as u64) as usize);
        } else {
            let flips = rng.range(1, 6) as usize;
            rng.flip_bits(&mut body, flips);
        }
        if let Ok(decoded) = wire::decode_notify_args(&body) {
            let _ = Request::from_wire(decoded);
        }
    });
}

#[test]
fn validated_requests_stay_within_their_limits() {
    for_seeds("notify_validated_limits", |_, rng| {
        let Ok(request) = Request::from_wire(args(rng)) else {
            return;
        };
        let note = request.note;
        assert!(note.title.chars().count() <= super::note::TITLE_MAX);
        assert!(note.body.chars().count() <= super::note::BODY_MAX);
        assert!(note.actions.len() <= super::note::ACTIONS_MAX);
        assert!(note.progress.is_none_or(|p| p <= 100));
        assert!(!note.title.chars().any(char::is_control));
        let mut ids: Vec<u32> = note.actions.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), note.actions.len());
        assert!(ids.iter().all(|id| *id != 0));
        if let Some(ms) = note.lifetime_ms() {
            assert!((1000..=60_000).contains(&ms) || ms == 5000);
        }
    });
}

/// What the sequence checker follows per id.
#[derive(Default)]
struct Ledger {
    posted: Vec<u64>,
    closed: HashMap<u64, u32>,
}

fn drain(center: &mut Center, ledger: &mut Ledger) {
    for event in center.take_events() {
        if let EventKind::Closed(_) = event.kind {
            *ledger.closed.entry(event.id).or_default() += 1;
        }
    }
}

fn check(center: &Center, ledger: &Ledger, now: u64) {
    assert!(center.live().len() <= VISIBLE_MAX);
    assert!(center.waiting() <= WAITING_MAX);
    let mut owners: HashMap<&str, usize> = HashMap::new();
    for toast in center.live() {
        *owners.entry(toast.owner.as_str()).or_default() += 1;
    }
    assert!(owners.values().all(|n| *n <= OWNER_VISIBLE_MAX));
    // Every posted id is alive or was closed exactly once; never both.
    for id in &ledger.posted {
        let alive = center.get(*id).is_some();
        let closed = ledger.closed.get(id).copied().unwrap_or(0);
        assert!(closed <= 1, "id {id} closed {closed} times");
        assert!(
            alive != (closed == 1),
            "id {id} alive={alive} closed={closed}"
        );
    }
    // Frames are consistent with the live set.
    let frames = center.frames(1280, now);
    for toast in center.live() {
        assert!(frames.iter().filter(|f| f.id == toast.id).count() <= 1);
    }
}

#[test]
fn random_sequences_keep_the_center_consistent() {
    for_seeds("notify_center_sequences", |_, rng| {
        let mut center = Center::new();
        let mut ledger = Ledger::default();
        center.set_motion(*rng.pick(&[Motion::Full, Motion::Reduced, Motion::None]));
        let mut now = 0u64;
        let owners = ["a", "b", "c", "d", "e"];
        for _ in 0..rng.range(1, 250) {
            now += rng.below(80);
            let owner = *rng.pick(&owners);
            match rng.below(10) {
                0..=3 => {
                    let mut req = request(rng);
                    // Replace something that may or may not be alive.
                    if rng.one_in(3) && !ledger.posted.is_empty() {
                        req.replaces = Some(*rng.pick(&ledger.posted));
                    }
                    if let Ok(id) = center.post(owner, req, now) {
                        if !ledger.posted.contains(&id) {
                            ledger.posted.push(id);
                        }
                    }
                }
                4 => {
                    if !ledger.posted.is_empty() {
                        let id = *rng.pick(&ledger.posted);
                        center.close(owner, id, now);
                    }
                }
                5 => {
                    if !ledger.posted.is_empty() {
                        let id = *rng.pick(&ledger.posted);
                        match rng.below(3) {
                            0 => center.dismiss(id, now),
                            1 => center.activate(id, now),
                            _ => center.invoke(id, rng.below(4) as u32, now),
                        }
                    }
                }
                6 => {
                    if !ledger.posted.is_empty() {
                        let id = *rng.pick(&ledger.posted);
                        center.hover(id, rng.one_in(2), now);
                    }
                }
                7 => {
                    let ids: Vec<(u64, u32)> =
                        center.live().iter().map(|t| (t.id, t.rev)).collect();
                    for (id, rev) in ids {
                        center.set_height(id, rev, rng.range(30, 160) as i32);
                    }
                    center.layout(692, now);
                }
                8 => center.set_policy(Policy {
                    dnd: rng.one_in(3),
                    muted: if rng.one_in(4) {
                        vec![(*rng.pick(&owners)).to_owned()]
                    } else {
                        Vec::new()
                    },
                }),
                _ => center.tick(now),
            }
            drain(&mut center, &mut ledger);
            check(&center, &ledger, now);
        }
        // Time passing closes everything that can expire, and a restart
        // closes the rest: nothing is left alive, and every id closed once.
        center.tick(now + 1_000_000);
        center.restart(now + 1_000_001);
        drain(&mut center, &mut ledger);
        check(&center, &ledger, now + 1_000_001);
        assert!(center.live().is_empty() && center.waiting() == 0);
        for id in &ledger.posted {
            assert_eq!(ledger.closed.get(id), Some(&1), "id {id}");
        }
        let _ = CloseReason::User;
    });
}
