//! Unit tests: normal paths, boundaries and known-bad input. The randomized
//! model checks are in `fuzz.rs`.

use crate::fuzz::Mem;
use crate::*;

fn make(slots: u32) -> (Mem, Ring) {
    let mut mem = Mem::new(slots);
    // SAFETY: `mem` outlives every use of the ring in these tests.
    let ring = unsafe { Ring::create(mem.base(), mem.len(), slots) }.unwrap();
    (mem, ring)
}

fn frame(id: u8, len: usize) -> std::vec::Vec<u8> {
    (0..len).map(|j| id.wrapping_add(j as u8)).collect()
}

#[test]
fn geometry_helpers() {
    assert!(valid_slots(16) && valid_slots(256) && valid_slots(1024));
    for bad in [0, 1, 8, 15, 17, 24, 48, 100, 2048, u32::MAX] {
        assert!(!valid_slots(bad), "{bad}");
        assert_eq!(ring_bytes(bad), 0);
    }
    assert_eq!(ring_bytes(16), 4096 + 16 * 2048);
    assert_eq!(MAX_FRAME, 2046);
    const { assert!(MAX_FRAME >= 1500 + 14, "a full-MTU frame fits a slot") };
}

#[test]
fn empty_ring_pops_nothing() {
    let (_mem, ring) = make(16);
    let mut c = ring.consumer();
    let mut out = [0; MAX_FRAME];
    assert_eq!(c.pop(&mut out), Ok(None));
    assert_eq!(c.pending(), Ok(0));
}

#[test]
fn frames_come_out_in_order_and_intact() {
    let (_mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    let mut out = [0; MAX_FRAME];
    for len in [1usize, 14, 60, 1514, MAX_FRAME] {
        p.push(&frame(len as u8, len)).unwrap();
    }
    for len in [1usize, 14, 60, 1514, MAX_FRAME] {
        assert_eq!(c.pop(&mut out), Ok(Some(len)));
        assert_eq!(&out[..len], &frame(len as u8, len)[..]);
    }
    assert_eq!(c.pop(&mut out), Ok(None));
}

#[test]
fn full_ring_refuses_then_recovers() {
    let (_mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    let mut out = [0; MAX_FRAME];
    for i in 0..16 {
        p.push(&frame(i, 60)).unwrap();
    }
    assert_eq!(p.free(), Ok(0));
    assert_eq!(p.push(&frame(99, 60)), Err(PushError::Full));
    assert_eq!(c.pending(), Ok(16));
    assert_eq!(c.pop(&mut out), Ok(Some(60)));
    assert_eq!(&out[..60], &frame(0, 60)[..]);
    p.push(&frame(99, 60)).unwrap();
    assert_eq!(p.push(&frame(98, 60)), Err(PushError::Full));
}

#[test]
fn slot_boundary_frame_sizes() {
    let (_mem, ring) = make(16);
    let mut p = ring.producer();
    assert_eq!(p.push(&[]), Err(PushError::Empty));
    assert!(p.push(&frame(1, 1)).is_ok());
    assert!(p.push(&frame(2, MAX_FRAME)).is_ok());
    assert_eq!(p.push(&frame(3, MAX_FRAME + 1)), Err(PushError::TooLong));
    assert_eq!(p.push(&frame(4, 65_535)), Err(PushError::TooLong));
    assert_eq!(p.free(), Ok(14), "refused frames occupy nothing");
}

#[test]
fn wraps_around_many_times() {
    let (_mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    let mut out = [0; MAX_FRAME];
    for round in 0u32..1000 {
        let burst = 1 + round % 16;
        for i in 0..burst {
            p.push(&frame((round + i) as u8, 14 + (round + i) as usize % 1500))
                .unwrap();
        }
        for i in 0..burst {
            let want = 14 + (round + i) as usize % 1500;
            assert_eq!(c.pop(&mut out), Ok(Some(want)));
            assert_eq!(&out[..want], &frame((round + i) as u8, want)[..]);
        }
    }
    assert_eq!(p.published(), c.consumed());
}

#[test]
fn indices_cross_the_u32_wrap() {
    let (_mem, ring) = make(16);
    let (mut p, mut c) = ring.endpoints_at(u32::MAX - 5);
    let mut out = [0; MAX_FRAME];
    for i in 0..40u8 {
        p.push(&frame(i, 20)).unwrap();
        assert_eq!(c.pop(&mut out), Ok(Some(20)));
        assert_eq!(out[0], i);
    }
    // Fill across the wrap point, too.
    let (_mem2, ring2) = make(16);
    let (mut p, mut c) = ring2.endpoints_at(u32::MAX - 7);
    for i in 0..16 {
        p.push(&frame(i, 30)).unwrap();
    }
    assert_eq!(p.push(&frame(0, 30)), Err(PushError::Full));
    assert_eq!(c.pending(), Ok(16));
}

#[test]
fn arm_and_notify_coalesce_a_burst() {
    let (_mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    assert!(!p.take_notify(), "an unarmed ring never notifies");
    c.arm();
    for i in 0..5 {
        p.push(&frame(i, 60)).unwrap();
    }
    assert!(p.take_notify(), "the armed consumer is woken");
    assert!(!p.take_notify(), "once per arming");
    p.push(&frame(9, 60)).unwrap();
    assert!(!p.take_notify(), "not armed again yet");
    c.arm();
    assert!(p.take_notify());
}

#[test]
fn the_consumer_copy_is_immune_to_a_rewrite() {
    let (mut mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    p.push(&frame(7, 100)).unwrap();
    let mut out = [0; MAX_FRAME];
    assert_eq!(c.pop(&mut out), Ok(Some(100)));
    let snapshot = out;
    // The producer rewrites the slot after the pop; the copy does not change.
    mem.bytes()[HEADER_BYTES + 2..HEADER_BYTES + 102].fill(0xFF);
    assert_eq!(out, snapshot);
}

#[test]
fn a_zero_length_slot_is_delivered_as_a_runt() {
    let (mut mem, ring) = make(16);
    let mut c = ring.consumer();
    // A hostile producer publishes head = 1 over a zeroed slot.
    mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&1u32.to_le_bytes());
    let mut out = [0; MAX_FRAME];
    assert_eq!(c.pop(&mut out), Ok(Some(0)));
}

#[test]
fn an_oversized_length_is_skipped_not_truncated() {
    let (mut mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    p.push(&frame(1, 60)).unwrap();
    p.push(&frame(2, 60)).unwrap();
    // Claim 60000 bytes in the first slot.
    mem.bytes()[HEADER_BYTES..HEADER_BYTES + 2].copy_from_slice(&60_000u16.to_le_bytes());
    let mut out = [0; MAX_FRAME];
    assert_eq!(c.pop(&mut out), Err(PopError::BadLength(60_000)));
    assert_eq!(
        c.pop(&mut out),
        Ok(Some(60)),
        "the next frame is unaffected"
    );
    assert_eq!(out[0], 2);
    assert!(!c.is_poisoned());
    // MAX_FRAME + 1 is oversized, MAX_FRAME is fine.
    p.push(&frame(3, 60)).unwrap();
    let slot = HEADER_BYTES + 2 * SLOT_BYTES;
    mem.bytes()[slot..slot + 2].copy_from_slice(&(MAX_FRAME as u16 + 1).to_le_bytes());
    assert_eq!(
        c.pop(&mut out),
        Err(PopError::BadLength(MAX_FRAME as u16 + 1))
    );
}

#[test]
fn an_impossible_head_poisons_the_consumer() {
    let (mut mem, ring) = make(16);
    let mut c = ring.consumer();
    // 17 frames outstanding in a 16-slot ring is a lie, and so is "behind".
    for lie in [17u32, 1000, u32::MAX, 0x8000_0000] {
        let (mut mem, ring) = make(16);
        mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&lie.to_le_bytes());
        let mut c = ring.consumer();
        let mut out = [0; MAX_FRAME];
        assert_eq!(c.pop(&mut out), Err(PopError::Corrupt), "head {lie:#x}");
        assert_eq!(c.pending(), Err(Error::Corrupt));
        assert!(c.is_poisoned());
    }
    // A plausible head that a later honest value contradicts is caught as well.
    mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&5u32.to_le_bytes());
    let mut out = [0; MAX_FRAME];
    for _ in 0..5 {
        assert!(matches!(c.pop(&mut out), Ok(Some(0))));
    }
    mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        c.pop(&mut out),
        Err(PopError::Corrupt),
        "head went backwards"
    );
    mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(
        c.pop(&mut out),
        Err(PopError::Corrupt),
        "and it stays poisoned"
    );
}

#[test]
fn an_impossible_tail_poisons_the_producer() {
    for lie in [1u32, 100, 0x8000_0000, 0xFFFF_FF00] {
        let (mut mem, ring) = make(16);
        let mut p = ring.producer();
        p.push(&frame(1, 60)).unwrap();
        // The consumer claims to have consumed more than was ever produced.
        mem.bytes()[off::TAIL..off::TAIL + 4]
            .copy_from_slice(&(1u32.wrapping_add(lie)).to_le_bytes());
        assert_eq!(
            p.push(&frame(2, 60)),
            Err(PushError::Corrupt),
            "tail lie {lie:#x}"
        );
        assert!(p.is_poisoned());
        assert_eq!(p.free(), Err(Error::Corrupt));
        // Sticky even after the tail is repaired.
        mem.bytes()[off::TAIL..off::TAIL + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(p.push(&frame(2, 60)), Err(PushError::Corrupt));
        // Invalid frames are still reported as such, not as corruption.
        assert_eq!(p.push(&[]), Err(PushError::Empty));
    }
}

#[test]
fn a_lagging_tail_only_shrinks_the_ring() {
    let (mut mem, ring) = make(16);
    let mut p = ring.producer();
    for i in 0..4 {
        p.push(&frame(i, 60)).unwrap();
    }
    // The consumer claims it consumed nothing: legal, just no progress.
    mem.bytes()[off::TAIL..off::TAIL + 4].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(p.free(), Ok(12));
    assert!(!p.is_poisoned());
}

#[test]
fn scribbling_on_our_own_index_changes_nothing_for_us() {
    let (mut mem, ring) = make(16);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    let mut out = [0; MAX_FRAME];
    for i in 0..10 {
        // Vandalize the producer's header word between pushes: the producer
        // works from its private index and re-publishes it.
        mem.bytes()[off::HEAD..off::HEAD + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        p.push(&frame(i, 60)).unwrap();
        assert_eq!(c.pop(&mut out), Ok(Some(60)));
        assert_eq!(out[0], i);
    }
}

#[test]
fn create_and_attach_validate_geometry() {
    let mut mem = Mem::new(16);
    let (base, len) = (mem.base(), mem.len());
    // SAFETY (all calls): `mem` is live and `len` bytes long.
    unsafe {
        assert_eq!(Ring::create(base, len, 24).err(), Some(InitError::BadSlots));
        assert_eq!(
            Ring::create(base, len - 1, 16).err(),
            Some(InitError::BadLength)
        );
        assert_eq!(
            Ring::create(base, len + 4096, 16).err(),
            Some(InitError::BadLength)
        );
        assert_eq!(
            Ring::create(base.add(1), len, 16).err(),
            Some(InitError::Misaligned)
        );
        // Nothing to attach to yet: an all-zero header.
        assert_eq!(
            Ring::attach(base, len, 16).err(),
            Some(InitError::BadHeader)
        );
        Ring::create(base, len, 16).unwrap();
        assert!(Ring::attach(base, len, 16).is_ok());
        assert_eq!(
            Ring::attach(base, len, 32).err(),
            Some(InitError::BadLength)
        );
    }
}

#[test]
fn attach_refuses_every_bad_header_field() {
    for (offset, value) in [
        (off::MAGIC, 0u32),
        (off::VERSION, 2),
        (off::SLOTS, 32),
        (off::SLOT_BYTES, 1024),
        (off::HEAD, 1),
        (off::TAIL, 1),
        (off::ARMED, 2),
    ] {
        let mut mem = Mem::new(16);
        let (base, len) = (mem.base(), mem.len());
        // SAFETY: `mem` is live and `len` bytes long.
        unsafe { Ring::create(base, len, 16) }.unwrap();
        mem.bytes()[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        // SAFETY: as above.
        let attached = unsafe { Ring::attach(base, len, 16) };
        assert_eq!(
            attached.err(),
            Some(InitError::BadHeader),
            "field at {offset:#x} = {value}"
        );
    }
}

#[test]
fn two_threads_move_many_frames_in_order() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mem, ring) = make(64);
    let (mut p, mut c) = (ring.producer(), ring.consumer());
    let done = AtomicBool::new(false);
    const N: u32 = 300_000;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut sent = 0u32;
            while sent < N {
                let len = 14 + (sent as usize * 7) % 1500;
                let mut f = frame(sent as u8, len);
                f[..4].copy_from_slice(&sent.to_le_bytes());
                match p.push(&f) {
                    Ok(()) => sent += 1,
                    Err(PushError::Full) => std::thread::yield_now(),
                    Err(e) => panic!("{e:?}"),
                }
            }
            done.store(true, Ordering::SeqCst);
        });
        let mut want = 0u32;
        let mut out = [0; MAX_FRAME];
        while want < N {
            match c.pop(&mut out) {
                Ok(Some(n)) => {
                    assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), want);
                    assert_eq!(n, 14 + (want as usize * 7) % 1500);
                    assert_eq!(out[4], (want as u8).wrapping_add(4));
                    want += 1;
                }
                Ok(None) => std::thread::yield_now(),
                Err(e) => panic!("{e:?}"),
            }
        }
        assert!(done.load(Ordering::SeqCst) || want == N);
    });
    mem.assert_guards();
}
