//! Transmit path, wake-ups and link events.

use super::*;

#[test]
fn transmit_delivers_valid_frames_exactly_and_drops_the_rest() {
    let mut bed = attached();
    let good = [14usize, 60, 1514];
    let bad = [0usize, 1, 13, 1515, 2046];
    let mut expected = Vec::new();
    for (i, len) in good.iter().chain(bad.iter()).enumerate() {
        let f = frame(*len, [0xFF; 6], i as u8);
        let pushed = bed.client().tx.push(&f);
        if *len == 0 {
            assert_eq!(
                pushed,
                Err(PushError::Empty),
                "the ring itself refuses an empty frame"
            );
        } else {
            assert_eq!(pushed, Ok(()));
        }
        if good.contains(len) {
            expected.push(f);
        }
    }
    let out = bed.pump();
    assert_eq!(out.tx_sent, 3);
    assert_eq!(bed.dev.tx_kicks, 1);
    for f in expected {
        assert_eq!(bed.dev.transmitted(), Some(f));
    }
    assert_eq!(
        bed.dev.transmitted(),
        None,
        "nothing else reached the device"
    );
    let s = bed.engine.stats();
    assert_eq!(
        (s.tx_frames, s.tx_dropped, s.runts, s.oversize),
        (3, 4, 2, 2)
    );
    assert_eq!(s.tx_bytes, 14 + 60 + 1514);
    bed.assert_guards();
}

#[test]
fn a_hostile_slot_length_on_transmit_is_an_oversize_drop() {
    let mut bed = attached();
    bed.client().tx.push(&frame(60, MAC, 1)).unwrap();
    bed.client().tx.push(&frame(60, MAC, 2)).unwrap();
    // The client rewrites the first slot's length to 60000.
    let base = bed.client().mem.base();
    let slot = framering::ring_bytes(16) + HEADER_BYTES; // the transmit ring's first slot
                                                         // SAFETY: inside the client's buffer.
    unsafe { core::ptr::write(base.add(slot) as *mut u16, 60_000) };
    let out = bed.pump();
    assert_eq!(out.tx_sent, 1);
    assert_eq!(bed.engine.stats().oversize, 1);
    assert!(!out.detached, "a bad length is a drop, not a poisoning");
    assert_eq!(
        bed.dev.transmitted().map(|f| f[6]),
        Some(2 + 6),
        "the second frame, pattern offset 6"
    );
}

#[test]
fn transmit_backpressure_keeps_frames_in_the_client_ring() {
    let mut bed = Bed::new(16, 4);
    bed.attach(16, OWNER).unwrap();
    for i in 0..10u8 {
        bed.client().tx.push(&frame(60, MAC, i)).unwrap();
    }
    let out = bed.pump();
    assert_eq!(out.tx_sent, 4, "only four transmit slots");
    assert_eq!(bed.engine.queues().tx_free(), 0);
    // Nothing moves until the device returns slots.
    assert_eq!(bed.pump().tx_sent, 0);
    for i in 0..4u8 {
        assert_eq!(bed.dev.transmitted().map(|f| f[6]), Some(i + 6));
    }
    let out = bed.pump();
    assert_eq!(out.tx_sent, 4);
    for _ in 0..4 {
        bed.dev.transmitted().unwrap();
    }
    assert_eq!(bed.pump().tx_sent, 2);
    for i in 8..10u8 {
        assert_eq!(
            bed.dev.transmitted().map(|f| f[6]),
            Some(i + 6),
            "order is preserved"
        );
    }
    bed.pump();
    assert_eq!(bed.engine.queues().tx_free(), 4, "every slot came back");
    assert_eq!(bed.engine.stats().tx_frames, 10);
}

#[test]
fn a_full_transmit_ring_is_reported_when_it_drains() {
    let mut bed = Bed::new(16, 16);
    bed.attach(16, OWNER).unwrap();
    for i in 0..16u8 {
        bed.client().tx.push(&frame(60, MAC, i)).unwrap();
    }
    assert_eq!(
        bed.client().tx.push(&frame(60, MAC, 99)),
        Err(PushError::Full)
    );
    let out = bed.pump();
    assert_eq!(out.events & EV_TX_SPACE, EV_TX_SPACE);
    assert_eq!(out.tx_sent, 16);
    assert_eq!(bed.pump().events & EV_TX_SPACE, 0, "only once");
}

#[test]
fn wake_ups_are_coalesced_and_follow_arming() {
    let mut bed = attached();
    // An unarmed client is never notified.
    bed.dev.deliver(&to_us(60, 1));
    assert_eq!(bed.pump().events & EV_RX_READY, 0);
    pop(&mut bed).unwrap();
    // Armed: a burst yields one notice.
    bed.client().rx.arm();
    for i in 0..5 {
        bed.dev.deliver(&to_us(60, i));
    }
    let out = bed.pump();
    assert_eq!(out.rx_delivered, 5);
    assert_eq!(out.events & EV_RX_READY, EV_RX_READY);
    bed.dev.deliver(&to_us(60, 9));
    assert_eq!(bed.pump().events & EV_RX_READY, 0, "not armed again");
}

#[test]
fn the_driver_asks_to_be_kicked_when_the_transmit_ring_is_empty() {
    let mut bed = attached();
    // `attach` armed the ring; the first push then finds it armed.
    bed.client().tx.push(&frame(60, MAC, 1)).unwrap();
    assert!(bed.client().tx.take_notify(), "armed at attach");
    assert_eq!(bed.pump().tx_sent, 1);
    // The pump drained and re-armed, so the next push wants a kick again.
    bed.client().tx.push(&frame(60, MAC, 2)).unwrap();
    assert!(bed.client().tx.take_notify());
}

#[test]
fn link_changes_are_reported_to_an_attached_client() {
    let mut bed = attached();
    assert!(bed.engine.link());
    assert!(!bed.engine.set_link(true));
    assert!(bed.engine.set_link(false));
    assert_eq!(bed.pump().events & EV_LINK_CHANGE, EV_LINK_CHANGE);
    assert_eq!(bed.pump().events & EV_LINK_CHANGE, 0);
    assert_eq!(bed.engine.stats().link_changes, 1);
    // With nobody attached the event is simply forgotten.
    bed.engine.release();
    assert!(bed.engine.set_link(true));
    assert_eq!(bed.pump().events, 0);
}
