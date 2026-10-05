//! Unit tests for the driver core: normal paths, boundaries and hostile
//! devices and clients. The randomized versions are in `fuzz.rs`.

#[allow(unused_imports)]
use std::vec::Vec;

#[allow(unused_imports)]
use framering::{off, PopError, PushError, HEADER_BYTES, MAX_FRAME, SLOT_BYTES};
#[allow(unused_imports)]
use virtio_net::hdr::{NetHdr, HDR_LEN};

#[allow(unused_imports)]
use crate::engine::{AttachError, CtlError, RxMode, EV_LINK_CHANGE, EV_RX_READY, EV_TX_SPACE};
#[allow(unused_imports)]
use crate::queues::Layout;
#[allow(unused_imports)]
use crate::testdev::bed::{Bed, MAC, OWNER};
#[allow(unused_imports)]
use crate::testdev::Which;
#[allow(unused_imports)]
use crate::Fatal;

/// A frame of `len` bytes addressed to `dst`, filled with a recognisable pattern.
pub(super) fn frame(len: usize, dst: [u8; 6], id: u8) -> Vec<u8> {
    let mut f: Vec<u8> = (0..len).map(|i| id.wrapping_add(i as u8)).collect();
    if len >= 6 {
        f[..6].copy_from_slice(&dst);
    }
    f
}

pub(super) fn to_us(len: usize, id: u8) -> Vec<u8> {
    frame(len, MAC, id)
}

pub(super) fn pop(bed: &mut Bed) -> Option<Vec<u8>> {
    let mut buf = [0u8; MAX_FRAME];
    match bed.client().rx.pop(&mut buf) {
        Ok(Some(n)) => Some(buf[..n].to_vec()),
        Ok(None) => None,
        Err(e) => panic!("{e:?}"),
    }
}

pub(super) fn attached() -> Bed {
    let mut bed = Bed::new(16, 16);
    bed.attach(16, OWNER).expect("attach");
    bed
}

/// The engine produces into the first declared ring (`rx`) and consumes the
/// second (`tx`); a swapped or extended `Ring<...>` declaration in
/// `idl/net.midl` fails here instead of corrupting traffic.
#[test]
fn engine_roles_match_the_midl_declaration() {
    use messenger_generated::os_lazy_net_nic_v1 as nic;
    use messenger_generated::rings::Side;
    assert_eq!(nic::ATTACH_RING_RINGS.len(), 2);
    assert_eq!(nic::ATTACH_RING_RINGS[0].name, nic::RING_RX.name);
    assert_eq!(nic::ATTACH_RING_RINGS[1].name, nic::RING_TX.name);
    assert_eq!(nic::RING_RX.producer, Side::Server);
    assert_eq!(nic::RING_TX.producer, Side::Client);
}

#[test]
fn layout_rules() {
    assert!(Layout::new(256, 256).is_some());
    assert!(Layout::new(2, 2).is_some());
    for bad in [0u16, 1, 3, 100, 257, 512, 1024] {
        assert!(Layout::new(bad, 16).is_none(), "{bad}");
        assert!(Layout::new(16, bad).is_none(), "{bad}");
    }
    let l = Layout::new(256, 256).unwrap();
    assert!(l.tx_queue >= 6670 && l.rx_slots > l.tx_queue && l.tx_slots == l.rx_slots + 256 * 2048);
    assert_eq!(l.total, l.tx_slots + 256 * 2048);
    for offset in [l.rx_queue, l.tx_queue, l.rx_slots, l.tx_slots] {
        assert_eq!(offset % 4096, 0);
    }
}

#[test]
fn a_received_frame_reaches_the_client_intact() {
    let mut bed = attached();
    let f = to_us(60, 1);
    assert!(bed.dev.deliver(&f));
    let out = bed.pump();
    assert_eq!(out.rx_delivered, 1);
    assert_eq!(pop(&mut bed), Some(f));
    assert_eq!(pop(&mut bed), None);
    let s = bed.engine.stats();
    assert_eq!((s.rx_frames, s.rx_bytes, s.rx_dropped), (1, 60, 0));
    assert_eq!(bed.dev.rx_kicks, 1, "the recycled buffer is kicked");
    assert_eq!(
        bed.engine.queues().rx_in_flight(),
        16,
        "every buffer is back with the device"
    );
}

#[test]
fn receive_length_boundaries() {
    let mut bed = attached();
    for len in [13usize, 14, 1514, 1515] {
        assert!(bed.dev.deliver(&to_us(len, len as u8)));
    }
    bed.pump();
    assert_eq!(pop(&mut bed).map(|f| f.len()), Some(14));
    assert_eq!(pop(&mut bed).map(|f| f.len()), Some(1514));
    assert_eq!(pop(&mut bed), None);
    let s = bed.engine.stats();
    assert_eq!(
        (s.runts, s.oversize, s.rx_dropped, s.rx_frames),
        (1, 1, 2, 2)
    );
    bed.assert_guards();
}

#[test]
fn bad_completions_are_counted_not_delivered() {
    let mut bed = attached();
    // Header only: an empty frame is a runt.
    bed.dev.deliver_raw(&NetHdr::PLAIN.encode(), HDR_LEN as u32);
    // Shorter than the packet header.
    bed.dev.deliver_raw(&[0; 5], 5);
    // A length beyond the slot: the copy is clamped and the report rejected.
    bed.dev.deliver_raw(&[0; 64], SLOT_BYTES as u32 + 1);
    bed.dev.deliver_raw(&[0; 64], u32::MAX);
    // An offload header where none was negotiated.
    let mut offload = NetHdr::PLAIN;
    offload.flags = 2;
    let mut bytes = offload.encode().to_vec();
    bytes.extend_from_slice(&to_us(60, 3));
    bed.dev.deliver_raw(&bytes, bytes.len() as u32);
    bed.pump();
    assert_eq!(pop(&mut bed), None);
    let s = bed.engine.stats();
    assert_eq!(s.rx_frames, 0);
    assert_eq!(s.rx_dropped, 5);
    assert_eq!(s.runts, 1);
    assert_eq!(
        s.ring_errors, 4,
        "device misbehaviour is reported as ring errors"
    );
    assert_eq!(
        bed.engine.queues().rx_in_flight(),
        16,
        "every bad buffer was recycled"
    );
}

#[test]
fn the_receive_filter() {
    let other = [0x52, 0x54, 0, 1, 2, 3];
    let mut bed = attached();
    let cases = [
        (frame(60, MAC, 1), true),
        (frame(60, [0xFF; 6], 2), true),
        (frame(60, [0x01, 0, 0x5E, 0, 0, 1], 3), true),
        (frame(60, other, 4), false),
    ];
    for (f, _) in &cases {
        bed.dev.deliver(f);
    }
    bed.pump();
    for (f, accepted) in &cases {
        assert_eq!(pop(&mut bed).is_some(), *accepted, "{:02x?}", &f[..6]);
    }
    assert_eq!(bed.engine.stats().rx_dropped, 1);

    // Promiscuous lets the stranger's frame through; Off drops everything.
    assert_eq!(
        bed.engine.set_rx_mode(OWNER, 2),
        Ok(Some(RxMode::Promiscuous))
    );
    bed.dev.deliver(&frame(60, other, 5));
    bed.pump();
    assert!(pop(&mut bed).is_some());
    assert_eq!(bed.engine.set_rx_mode(OWNER, 0), Ok(Some(RxMode::Off)));
    bed.dev.deliver(&frame(60, MAC, 6));
    bed.pump();
    assert!(pop(&mut bed).is_none());
    // Unknown modes are reported, changes by a stranger refused.
    assert_eq!(bed.engine.set_rx_mode(OWNER, 3), Ok(None));
    assert_eq!(bed.engine.set_rx_mode(OWNER + 1, 1), Err(CtlError::Denied));
    assert_eq!(bed.engine.rx_mode(), RxMode::Off);
}

#[test]
fn without_a_client_frames_are_dropped_and_buffers_recycled() {
    let mut bed = Bed::new(16, 16);
    for i in 0..40u8 {
        assert!(bed.dev.deliver(&to_us(60, i)));
        bed.pump();
    }
    let s = bed.engine.stats();
    assert_eq!((s.rx_frames, s.rx_dropped), (0, 40));
    assert_eq!(bed.engine.queues().rx_in_flight(), 16);
}

#[test]
fn a_client_that_never_drains_loses_frames_not_buffers() {
    let mut bed = attached();
    for i in 0..100u8 {
        bed.dev.deliver(&to_us(60, i));
        bed.pump();
    }
    let s = bed.engine.stats();
    assert_eq!(s.rx_frames, 16, "the client's ring holds 16");
    assert_eq!(s.rx_dropped, 84);
    assert_eq!(bed.engine.queues().rx_in_flight(), 16);
    assert_eq!(
        pop(&mut bed).unwrap()[6],
        6u8.wrapping_add(0),
        "the oldest frames are the ones kept"
    );
}

mod session;
mod tx;
