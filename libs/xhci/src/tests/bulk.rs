//! Bulk transfers (mass storage): the Normal TRB, the bulk endpoint context
//! with its SuperSpeed burst, and abandoning a ring after a stall or a
//! timeout, lap after lap.

use super::{Consumer, VecMem, RING_PHYS};
use crate::context::{EndpointContext, EndpointType, InputContext};
use crate::ring::ProducerRing;
use crate::trb::{self, kind, Trb, IOC, ISP};
use crate::Error;
use std::vec;

#[test]
fn bulk_trb_layout() {
    let normal = trb::bulk(0x1_0000, 31).unwrap();
    assert_eq!(
        (normal.kind(), normal.parameter, normal.status),
        (kind::NORMAL, 0x1_0000, 31)
    );
    assert_eq!(normal.control & (IOC | ISP), IOC | ISP);
    assert_eq!(
        trb::bulk(0, trb::MAX_TRB_TRANSFER).unwrap().status,
        64 * 1024
    );
    assert_eq!(trb::bulk(0, trb::MAX_TRB_TRANSFER + 1), None);
}

#[test]
fn bulk_endpoint_contexts_carry_the_burst() {
    let mut buffer = vec![0u32; 33 * 16];
    let mut input = InputContext::new(&mut buffer, true).unwrap();
    let out = EndpointContext::bulk(EndpointType::BulkOut, 1024, 0, RING_PHYS | 1);
    let r#in = EndpointContext::bulk(EndpointType::BulkIn, 1024, 15, RING_PHYS | 1);
    input.endpoint(2, &out).unwrap();
    input.endpoint(3, &r#in).unwrap();
    let dwords = input.dwords();
    let ep = |dci: usize| &dwords[(dci + 1) * 16..(dci + 2) * 16];
    // Bulk OUT is type 2, bulk IN type 6; packet size in 31:16, burst 15:8.
    assert_eq!((ep(2)[1] >> 3) & 7, 2);
    assert_eq!((ep(3)[1] >> 3) & 7, 6);
    assert_eq!(ep(3)[1] >> 16, 1024);
    assert_eq!((ep(3)[1] >> 8) & 0xFF, 15, "max burst");
    assert_eq!((ep(2)[1] >> 8) & 0xFF, 0);
    assert_eq!(input.added(), 1 << 2 | 1 << 3);
}

/// A transfer that never completes is abandoned: the dequeue pointer moves
/// to the enqueue position, the ring forgets it, and the next transfer is
/// the next TRB the controller sees, across many laps.
#[test]
fn abandoned_transfers_are_skipped() {
    let mut ring = ProducerRing::new(VecMem::new(8, RING_PHYS)).unwrap();
    let mut controller = Consumer::new();
    for round in 0..50u64 {
        // One transfer the controller consumes but never completes.
        let lost = Trb {
            parameter: round,
            ..trb::bulk(0x1000, 512).unwrap()
        };
        ring.enqueue(&[lost], false).unwrap();
        assert!(controller.next(ring.mem()).is_some());
        // Stop Endpoint + Set TR Dequeue: the controller now starts at the
        // producer's position with its cycle state.
        let pointer = ring.abandon();
        assert_eq!(ring.in_flight(), 0);
        let index = ((pointer & !0xF) - RING_PHYS) / 16;
        controller.index = index as usize;
        controller.cycle = pointer & 1 != 0;
        // Retiring the lost TRB now is refused: it is not in flight.
        assert_eq!(
            ring.retire(RING_PHYS + ((index + 6) % 7) * 16),
            Err(Error::BadPointer)
        );
        let next = Trb {
            parameter: 1000 + round,
            ..trb::bulk(0x2000, 31).unwrap()
        };
        let at = ring.enqueue(&[next], false).unwrap();
        let (seen_at, seen) = controller.next(ring.mem()).expect("the next transfer");
        assert_eq!(
            (seen_at, seen.parameter),
            (at, 1000 + round),
            "round {round}"
        );
        ring.retire(at).unwrap();
        assert_eq!(ring.free(), 6);
    }
}
