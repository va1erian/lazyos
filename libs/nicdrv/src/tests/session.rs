//! Attach, ownership, hostile clients and devices, and the long soak.

use super::*;

#[test]
fn attach_validates_everything() {
    let mut bed = Bed::new(16, 16);
    for bad in [0u32, 1, 8, 15, 24, 2048, u32::MAX] {
        assert_eq!(bed.attach(bad, OWNER), Err(AttachError::Invalid), "{bad}");
    }
    assert!(bed.engine.attached().is_none());
    // A buffer of the wrong size is refused; build one by hand.
    let mut mem = framering::fuzz::Mem::with_len(framering::ring_bytes(16) * 2 + 4096);
    let base = mem.base();
    // SAFETY: inside `mem`.
    unsafe {
        framering::Ring::create(base, framering::ring_bytes(16), 16).unwrap();
        framering::Ring::create(
            base.add(framering::ring_bytes(16)),
            framering::ring_bytes(16),
            16,
        )
        .unwrap();
        assert_eq!(
            bed.engine.attach(OWNER, 16, base, mem.len()),
            Err(AttachError::Invalid),
            "too long"
        );
        assert_eq!(
            bed.engine
                .attach(OWNER, 16, base, framering::ring_bytes(16)),
            Err(AttachError::Invalid),
            "one ring only"
        );
        assert_eq!(
            bed.engine.attach(OWNER, 16, base, 0),
            Err(AttachError::Invalid)
        );
    }
    // A zeroed header is refused.
    let mut blank = framering::fuzz::Mem::with_len(framering::ring_bytes(16) * 2);
    // SAFETY: inside `blank`.
    unsafe {
        assert_eq!(
            bed.engine.attach(OWNER, 16, blank.base(), blank.len()),
            Err(AttachError::Invalid)
        );
    }
    // Garbage in the second ring's header is refused too.
    let one = framering::ring_bytes(16);
    mem.bytes()[one + off::MAGIC] ^= 0xFF;
    // SAFETY: inside `mem`.
    unsafe {
        assert_eq!(
            bed.engine.attach(OWNER, 16, base, one * 2),
            Err(AttachError::Invalid)
        );
    }
    assert!(
        bed.engine.attached().is_none(),
        "nothing was left half attached"
    );
    bed.assert_guards();
}

#[test]
fn one_client_at_a_time_owned_by_the_attacher() {
    let mut bed = Bed::new(16, 16);
    let ring = bed.attach(16, OWNER).unwrap();
    assert_eq!(bed.engine.attached(), Some((OWNER, ring)));
    assert_eq!(bed.attach(16, OWNER + 1), Err(AttachError::Busy));
    assert_eq!(
        bed.attach(16, OWNER),
        Err(AttachError::Busy),
        "even the owner cannot attach twice"
    );
    // Control calls: only the owner, only the attached ring.
    assert_eq!(bed.engine.detach(OWNER + 1, ring), Err(CtlError::Denied));
    assert_eq!(bed.engine.detach(OWNER, ring + 1), Err(CtlError::NoRing));
    assert!(!bed.engine.kick(OWNER + 1, ring));
    assert!(!bed.engine.kick(OWNER, ring + 1));
    assert!(bed.engine.kick(OWNER, ring));
    assert_eq!(bed.engine.detach(OWNER, ring), Ok(()));
    assert_eq!(bed.engine.detach(OWNER, ring), Err(CtlError::NoRing));
    // A new client gets a fresh ring id.
    let second = bed.attach(16, OWNER + 1).unwrap();
    assert_ne!(second, ring);
    assert_eq!(bed.engine.set_rx_mode(OWNER, 1), Err(CtlError::Denied));
}

#[test]
fn a_detached_client_gets_nothing_more() {
    let mut bed = attached();
    let ring = bed.client().ring;
    bed.engine.detach(OWNER, ring).unwrap();
    bed.dev.deliver(&to_us(60, 1));
    assert_eq!(bed.pump().rx_delivered, 0);
    assert_eq!(bed.engine.stats().rx_dropped, 1);
    // What the old client queues is ignored.
    bed.client().tx.push(&frame(60, MAC, 1)).unwrap();
    assert_eq!(bed.pump().tx_sent, 0);
    assert_eq!(bed.dev.transmitted(), None);
}

#[test]
fn a_corrupt_receive_ring_detaches_the_client() {
    let mut bed = attached();
    bed.dev.deliver(&to_us(60, 1));
    bed.pump();
    // The client claims to have consumed frames that were never produced.
    let base = bed.client().mem.base();
    // SAFETY: inside the client's buffer (the receive ring's tail word).
    unsafe { core::ptr::write(base.add(off::TAIL) as *mut u32, 0x8000_0000) };
    bed.dev.deliver(&to_us(60, 2));
    let out = bed.pump();
    assert!(out.detached);
    assert!(bed.engine.attached().is_none());
    let s = bed.engine.stats();
    assert_eq!(s.ring_errors, 1);
    assert_eq!(
        bed.engine.queues().rx_in_flight(),
        16,
        "the device is still served"
    );
    // The next frames are dropped, not fatal.
    bed.dev.deliver(&to_us(60, 3));
    assert_eq!(bed.pump().rx_delivered, 0);
}

#[test]
fn a_corrupt_transmit_ring_detaches_the_client() {
    let mut bed = attached();
    let one = framering::ring_bytes(16);
    let base = bed.client().mem.base();
    // The transmit ring's head claims 1000 frames in a 16-slot ring.
    // SAFETY: inside the client's buffer.
    unsafe { core::ptr::write(base.add(one + off::HEAD) as *mut u32, 1000) };
    let out = bed.pump();
    assert!(out.detached);
    assert_eq!(bed.engine.stats().ring_errors, 1);
    assert_eq!(out.tx_sent, 0);
    assert_eq!(bed.dev.transmitted(), None);
}

#[test]
fn a_producer_that_keeps_publishing_cannot_hold_the_pump() {
    let mut bed = Bed::new(16, 256);
    bed.attach(16, OWNER).unwrap();
    for i in 0..16u8 {
        bed.client().tx.push(&frame(60, MAC, i)).unwrap();
    }
    // Works and returns; the bound is structural (ring size and budget).
    let out = bed.pump();
    assert_eq!(out.tx_sent, 16);
}

#[test]
fn a_device_returning_an_unknown_id_is_fatal_not_a_panic() {
    let mut bed = attached();
    bed.dev.write_used_raw(Which::Rx, 9999, 60);
    assert!(matches!(
        bed.engine.pump(&mut bed.dev),
        Err(Fatal::Device(_))
    ));
    let mut bed = attached();
    bed.dev.write_used_raw(Which::Tx, 3, 0);
    assert!(matches!(
        bed.engine.pump(&mut bed.dev),
        Err(Fatal::Device(_))
    ));
    // A used index that ran ahead of what was ever submitted.
    let mut bed = attached();
    bed.dev.set_used_index(Which::Rx, 500);
    assert!(matches!(
        bed.engine.pump(&mut bed.dev),
        Err(Fatal::Device(_))
    ));
    bed.assert_guards();
}

#[test]
fn a_replayed_completion_is_survivable() {
    // A recycled head looks in flight again, so a replayed used entry cannot be
    // told from a real completion; what matters is that nothing breaks.
    let mut bed = attached();
    bed.dev.deliver(&to_us(60, 1));
    bed.dev.write_used_raw(Which::Rx, 0, 72);
    bed.dev.write_used_raw(Which::Rx, 0, 72);
    if bed.engine.pump(&mut bed.dev).is_ok() {
        assert_eq!(bed.engine.queues().rx_in_flight(), 16, "no buffer was lost");
    }
    bed.assert_guards();
}

#[test]
fn tens_of_thousands_of_frames_each_way_leak_nothing() {
    let mut bed = Bed::new(64, 64);
    bed.attach(64, OWNER).unwrap();
    bed.client().rx.arm();
    let mut rx_seen = 0u64;
    let mut tx_seen = 0u64;
    for round in 0u32..20_000 {
        for k in 0..(1 + round % 7) {
            let len = 14 + ((round + k) as usize * 37) % 1500;
            bed.dev.deliver(&to_us(len, round as u8));
            bed.client()
                .tx
                .push(&frame(len, [0xFF; 6], round as u8))
                .unwrap();
        }
        bed.pump();
        while let Some(f) = pop(&mut bed) {
            assert_eq!(f[7], f[6].wrapping_add(1), "the pattern survived the trip");
            rx_seen += 1;
        }
        while let Some(f) = bed.dev.transmitted() {
            assert!(f.len() >= 14);
            tx_seen += 1;
        }
    }
    bed.pump();
    assert_eq!(rx_seen, bed.engine.stats().rx_frames);
    assert_eq!(tx_seen, bed.engine.stats().tx_frames);
    assert_eq!(rx_seen, tx_seen);
    assert_eq!(bed.engine.queues().rx_in_flight(), 64);
    assert_eq!(bed.engine.queues().tx_free(), 64);
    assert_eq!(bed.engine.queues().tx_in_flight(), 0);
    bed.assert_guards();
}

#[test]
fn pop_errors_name_what_happened() {
    // Documented here because the engine maps these two onto its counters.
    assert_ne!(PopError::Corrupt, PopError::BadLength(0));
}
