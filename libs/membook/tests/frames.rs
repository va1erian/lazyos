//! Frame refcount rules, the free chain and the counters, over a model of
//! physical memory, plus a seeded soak of allocate/share/release.

use std::collections::HashMap;

use membook::frames::{
    dropped, shared, FrameMemory, Ledger, Refused, Release, FRAME_SIZE, RESERVED,
};

/// Physical memory as maps: refcounts and the link word of each frame.
#[derive(Default)]
struct Memory {
    counts: HashMap<u64, u32>,
    links: HashMap<u64, u64>,
}

impl FrameMemory for Memory {
    fn refcount(&self, phys: u64) -> u32 {
        self.counts.get(&phys).copied().unwrap_or(0)
    }
    fn set_refcount(&mut self, phys: u64, value: u32) {
        self.counts.insert(phys, value);
    }
    fn link(&self, phys: u64) -> u64 {
        self.links[&phys]
    }
    fn set_link(&mut self, phys: u64, next: u64) {
        self.links.insert(phys, next);
    }
}

const BASE: u64 = 0x10_0000;

fn frame(n: u64) -> u64 {
    BASE + n * FRAME_SIZE
}

#[test]
fn count_transitions() {
    assert_eq!(shared(1), Ok(2));
    assert_eq!(shared(0), Err(Refused::NotLive(0)));
    assert_eq!(shared(RESERVED), Err(Refused::NotLive(RESERVED)));
    assert_eq!(shared(RESERVED - 1), Err(Refused::NotLive(RESERVED - 1)));
    assert_eq!(shared(RESERVED - 2), Ok(RESERVED - 1));
    assert_eq!(dropped(1), Ok(0));
    assert_eq!(dropped(7), Ok(6));
    assert_eq!(dropped(0), Err(Refused::DoubleFree));
    assert_eq!(dropped(RESERVED), Err(Refused::Reserved));
}

#[test]
fn the_chain_is_last_in_first_out() {
    let mut mem = Memory::default();
    let mut ledger = Ledger::new();
    assert!(!ledger.has_free());
    assert_eq!(ledger.pop_free(&mem), None);
    for n in 0..3 {
        ledger.push_free(&mut mem, frame(n));
    }
    assert_eq!(ledger.pop_free(&mem), Some(frame(2)));
    assert_eq!(ledger.pop_free(&mem), Some(frame(1)));
    assert_eq!(ledger.pop_free(&mem), Some(frame(0)));
    assert_eq!(ledger.pop_free(&mem), None);
}

#[test]
fn share_then_release_frees_on_the_last_reference() {
    let mut mem = Memory::default();
    let mut ledger = Ledger::new();
    let phys = frame(4);
    ledger.note_alloc(&mut mem, phys);
    assert_eq!(ledger.share(&mut mem, phys, true), Ok(()));
    assert_eq!(mem.refcount(phys), 2);
    assert_eq!(
        ledger.release(&mut mem, phys, true, false),
        Release::Shared(1)
    );
    assert!(!ledger.has_free());
    assert_eq!(ledger.release(&mut mem, phys, true, false), Release::Freed);
    assert_eq!(mem.refcount(phys), 0);
    assert_eq!(ledger.pop_free(&mem), Some(phys));
    assert_eq!((ledger.allocated, ledger.freed, ledger.live()), (1, 1, 0));
}

#[test]
fn bad_releases_are_counted_and_change_nothing() {
    let mut mem = Memory::default();
    let mut ledger = Ledger::new();
    let phys = frame(1);
    // A free frame: a double free.
    assert_eq!(
        ledger.release(&mut mem, phys, true, false),
        Release::Invalid(Refused::DoubleFree)
    );
    // Unaligned, or outside the usable regions.
    assert_eq!(
        ledger.release(&mut mem, phys + 8, true, false),
        Release::Invalid(Refused::Unusable)
    );
    assert_eq!(
        ledger.release(&mut mem, phys, false, false),
        Release::Invalid(Refused::Unusable)
    );
    // The allocator's own frames.
    mem.set_refcount(frame(2), RESERVED);
    assert_eq!(
        ledger.release(&mut mem, frame(2), true, false),
        Release::Invalid(Refused::Reserved)
    );
    assert_eq!(mem.refcount(frame(2)), RESERVED);
    assert_eq!((ledger.double_frees, ledger.invalid_frees), (1, 3));
    assert!(!ledger.has_free(), "a refused release reached the chain");
    // Sharing a dead or reserved frame is refused too.
    assert_eq!(ledger.share(&mut mem, phys, true), Err(Refused::NotLive(0)));
    assert_eq!(
        ledger.share(&mut mem, frame(2), true),
        Err(Refused::NotLive(RESERVED))
    );
    assert_eq!(
        ledger.share(&mut mem, phys + 1, true),
        Err(Refused::Unusable)
    );
}

#[test]
fn pool_frames_go_back_reserved_and_stay_out_of_the_counters() {
    let mut mem = Memory::default();
    let mut ledger = Ledger::new();
    let phys = frame(9);
    mem.set_refcount(phys, 1); // handed out by the pool, not `note_alloc`
    assert_eq!(ledger.share(&mut mem, phys, true), Ok(()));
    assert_eq!(
        ledger.release(&mut mem, phys, true, true),
        Release::Shared(1)
    );
    assert_eq!(
        ledger.release(&mut mem, phys, true, true),
        Release::PoolFreed
    );
    assert_eq!(mem.refcount(phys), RESERVED);
    assert!(!ledger.has_free());
    assert_eq!((ledger.allocated, ledger.freed), (0, 0));
    // Released again: it is reserved now, so refused.
    assert_eq!(
        ledger.release(&mut mem, phys, true, true),
        Release::Invalid(Refused::Reserved)
    );
}

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound
    }
}

/// Soak: a small machine of frames allocated from the chain (or fresh),
/// shared, released, double freed on purpose, checked against a model of
/// the reference counts after every step. At the end every frame is free,
/// the chain holds each exactly once, and the counters add up.
#[test]
fn soak_allocate_share_release_matches_a_model() {
    const FRAMES: u64 = 256;
    let steps = if cfg!(miri) { 600 } else { 500_000 };
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut mem = Memory::default();
    let mut ledger = Ledger::new();
    let mut model: HashMap<u64, u32> = HashMap::new();
    let mut fresh = 0u64;
    let mut bugs = (0usize, 0usize);
    for step in 0..steps {
        match rng.below(10) {
            0..=3 => {
                let phys = match ledger.pop_free(&mem) {
                    Some(phys) => phys,
                    None if fresh < FRAMES => {
                        fresh += 1;
                        frame(fresh - 1)
                    }
                    None => continue,
                };
                assert_eq!(
                    model.get(&phys).copied().unwrap_or(0),
                    0,
                    "step {step}: live frame reused"
                );
                ledger.note_alloc(&mut mem, phys);
                model.insert(phys, 1);
            }
            4..=5 => {
                let phys = frame(rng.below(fresh.max(1)));
                let count = model.get(&phys).copied().unwrap_or(0);
                let outcome = ledger.share(&mut mem, phys, fresh > 0);
                if count > 0 && fresh > 0 {
                    assert_eq!(outcome, Ok(()), "step {step}");
                    model.insert(phys, count + 1);
                } else {
                    assert!(outcome.is_err(), "step {step}: shared a free frame");
                }
            }
            _ => {
                let phys = frame(rng.below(fresh.max(1)));
                let count = model.get(&phys).copied().unwrap_or(0);
                match ledger.release(&mut mem, phys, true, false) {
                    Release::Freed => assert_eq!(count, 1, "step {step}"),
                    Release::Shared(left) => assert_eq!(left + 1, count, "step {step}"),
                    Release::Invalid(Refused::DoubleFree) => {
                        assert_eq!(count, 0, "step {step}");
                        bugs.0 += 1;
                    }
                    other => panic!("step {step}: {other:?}"),
                }
                model.insert(phys, count.saturating_sub(1));
            }
        }
        let live = model.values().filter(|&&count| count > 0).count();
        assert_eq!(ledger.live(), live, "step {step}: leak report disagrees");
    }
    // Release everything still held.
    for (phys, count) in model.clone() {
        for _ in 0..count {
            assert_ne!(
                ledger.release(&mut mem, phys, true, false),
                Release::Invalid(Refused::DoubleFree)
            );
        }
    }
    assert_eq!(ledger.live(), 0);
    assert_eq!(ledger.double_frees, bugs.0);
    assert_eq!(ledger.invalid_frees, bugs.1);
    let mut chained = std::collections::HashSet::new();
    while let Some(phys) = ledger.pop_free(&mem) {
        assert!(chained.insert(phys), "frame {phys:#x} chained twice");
        assert_eq!(mem.refcount(phys), 0);
    }
    assert_eq!(chained.len() as u64, fresh, "frames lost from the chain");
}
