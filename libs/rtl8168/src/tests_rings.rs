//! Host tests: the rings under the shared `nicdrv` engine with a real client,
//! the FCS trim, and a chip that lies.

use std::vec::Vec;

use framering::{ring_bytes, Consumer, Producer, Ring, MAX_FRAME};
use nicdrv::{Engine, Fatal, NicRings};

use crate::desc::{rx_err, EOR, FCS_BYTES, FS, LS, OWN};
use crate::fake::{Fake, Memory, BUS, FCS, MAC};
use crate::regs::*;
use crate::rings::{Layout, Rings, SLOT_BYTES};
use crate::setup;

const OWNER: u64 = 7;
const SLOTS: u32 = 16;
/// The engine's frame limit: the standard MTU plus the Ethernet header.
const MAX_FRAME_LEN: usize = 1514;

struct NoBell;

impl nicdrv::Doorbell for NoBell {
    fn ring(&mut self, _queue: u16) {}
}

/// The engine over the rings over the model, with an attached client.
struct Bench {
    fake: Fake,
    engine: Engine<Rings<Fake>>,
    rx: Consumer,
    tx: Producer,
    layout: Layout,
    dma: Memory,
    _client: Memory,
}

impl Bench {
    fn new(rx_entries: u16, tx_entries: u16) -> Bench {
        let layout = Layout::new(rx_entries, tx_entries).unwrap();
        let dma = Memory::new(layout.total);
        let fake = Fake::new(&dma);
        let mut regs = fake.clone();
        setup::reset(&mut regs, || {}).unwrap();
        // SAFETY: the block is the rings' alone and outlives them (`Bench`).
        let rings = unsafe { Rings::new(regs, dma.block(), rx_entries, tx_entries) }.unwrap();
        let mut engine = Engine::new(rings, MAC, MAX_FRAME_LEN, true);
        let one = ring_bytes(SLOTS);
        let client = Memory::new(one * 2);
        // SAFETY: both rings lie inside the client memory, kept in `Bench`.
        let (rx, tx) = unsafe {
            (
                Ring::create(client.va, one, SLOTS).unwrap(),
                Ring::create(client.va.add(one), one, SLOTS).unwrap(),
            )
        };
        // SAFETY: as above.
        unsafe { engine.attach(OWNER, SLOTS, client.va, one * 2) }.unwrap();
        Bench {
            fake,
            engine,
            rx: rx.consumer(),
            tx: tx.producer(),
            layout,
            dma,
            _client: client,
        }
    }

    fn pump(&mut self) -> nicdrv::PumpOutcome {
        self.engine.pump(&mut NoBell).unwrap()
    }

    fn received(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; MAX_FRAME];
        while let Ok(Some(n)) = self.rx.pop(&mut buf) {
            out.push(buf[..n].to_vec());
        }
        out
    }

    /// `opts1` of receive descriptor `index`, as the chip last left it.
    fn rx_opts1(&self, index: usize) -> u32 {
        let at = self.layout.rx_ring + index * 16;
        // SAFETY: inside the DMA block.
        unsafe { (self.dma.va.add(at) as *const u32).read_volatile() }
    }
}

fn frame(len: usize, id: u8) -> Vec<u8> {
    let mut f: Vec<u8> = (0..len).map(|i| id.wrapping_add(i as u8)).collect();
    f[..6].copy_from_slice(&MAC);
    f
}

#[test]
fn layout_rules() {
    assert!(Layout::new(8, 8).is_some());
    assert!(Layout::new(256, 256).is_some());
    for bad in [0u16, 4, 12, 512, 1024] {
        assert!(Layout::new(bad, 8).is_none(), "{bad}");
        assert!(Layout::new(8, bad).is_none(), "{bad}");
    }
    let l = Layout::new(64, 32).unwrap();
    assert_eq!(l.total, l.tx_slots + 32 * SLOT_BYTES);
    for offset in [l.rx_ring, l.tx_ring, l.rx_slots, l.tx_slots] {
        assert_eq!(offset % 256, 0, "the chip needs 256-byte aligned rings");
        assert_eq!(offset % 4096, 0);
    }
}

#[test]
fn rings_program_the_chip() {
    let bench = Bench::new(32, 16);
    let f = &bench.fake;
    let ring = |lo, hi| u64::from(f.reg32(lo)) | u64::from(f.reg32(hi)) << 32;
    assert_eq!(ring(RDSAR_LO, RDSAR_HI), BUS + bench.layout.rx_ring as u64);
    assert_eq!(ring(TNPDS_LO, TNPDS_HI), BUS + bench.layout.tx_ring as u64);
    assert_eq!(f.reg8(CHIP_CMD), cmd::RX_ENABLE | cmd::TX_ENABLE);
    assert_eq!(
        f.reg8(CFG9346),
        cfg9346::LOCK,
        "config registers locked again"
    );
    assert_eq!(f.reg16(RX_MAX_SIZE), RX_MAX_FRAME);
    assert!(usize::from(RX_MAX_FRAME) < SLOT_BYTES);
    assert_eq!(f.reg8(MAX_TX_PACKET), MAX_TX_UNITS);
    assert_eq!(
        f.reg16(CPLUS_CMD) & (cplus::RX_VLAN | cplus::RX_CHECKSUM),
        0
    );
    let rx_cfg = f.reg32(RX_CONFIG);
    assert_ne!(rx_cfg & rx_config::ACCEPT_MY_PHYS, 0);
    assert_ne!(rx_cfg & rx_config::ACCEPT_BROADCAST, 0);
    assert_eq!(f.reg32(MAR0), u32::MAX);
    // Every receive descriptor is the chip's; only the last ends the ring.
    for index in 0..32 {
        let opts1 = bench.rx_opts1(index);
        assert_ne!(opts1 & OWN, 0, "{index}");
        assert_eq!(opts1 & EOR != 0, index == 31, "{index}");
        assert_eq!(opts1 & 0x3FFF, SLOT_BYTES as u32);
    }
    assert_eq!(bench.engine.queues().tx_free(), 15);
}

#[test]
fn frames_cross_in_both_directions_and_wrap() {
    let mut bench = Bench::new(8, 8);
    for round in 0..200u32 {
        let incoming = frame(60 + (round as usize % 1455), round as u8);
        assert!(bench.fake.deliver(&incoming), "round {round}: no buffer");
        let outgoing = frame(60 + (round as usize % 1000), !round as u8);
        bench.tx.push(&outgoing).unwrap();
        let out = bench.pump();
        assert_eq!(out.rx_delivered, 1, "round {round}");
        assert_eq!(out.tx_sent, 1, "round {round}");
        assert_eq!(bench.received(), std::vec![incoming]);
        assert_eq!(bench.fake.0.borrow_mut().sent.pop(), Some(outgoing));
    }
    let stats = bench.engine.stats();
    assert_eq!((stats.rx_frames, stats.tx_frames), (200, 200));
    assert_eq!(stats.rx_dropped + stats.tx_dropped + stats.ring_errors, 0);
}

/// The chip leaves the 4-byte FCS on every frame and counts it in the
/// descriptor length. The client must get the frame without it, at every
/// size, including the largest the engine accepts.
#[test]
fn the_fcs_is_trimmed_at_every_size() {
    let mut bench = Bench::new(8, 8);
    for len in [14usize, 15, 60, 61, 1000, 1513, MAX_FRAME_LEN] {
        let incoming = frame(len, len as u8);
        let next = bench.fake.0.borrow().rx_head_for_test();
        assert!(bench.fake.deliver(&incoming), "{len}");
        // What the descriptor reports is the frame plus the FCS.
        assert_eq!(
            (bench.rx_opts1(next) & 0x3FFF) as usize,
            len + FCS_BYTES,
            "reported length for {len}"
        );
        bench.pump();
        let got = bench.received();
        assert_eq!(got, std::vec![incoming], "{len}");
        assert!(
            !got[0].ends_with(&FCS),
            "FCS leaked into a {len}-byte frame"
        );
    }
    let stats = bench.engine.stats();
    assert_eq!(stats.oversize + stats.runts + stats.rx_dropped, 0);
}

#[test]
fn a_frame_one_byte_over_the_limit_is_oversize_not_delivered() {
    let mut bench = Bench::new(8, 8);
    // 1515 bytes + FCS = 1519 in the descriptor: below the slot, above the
    // limit once trimmed, so it is the trim that decides.
    assert!(bench.fake.deliver(&frame(MAX_FRAME_LEN + 1, 1)));
    // And a frame whose descriptor length equals the limit *without* the trim
    // (1514 including FCS = a 1510-byte frame) is delivered whole.
    let ok = frame(MAX_FRAME_LEN - FCS_BYTES, 2);
    assert!(bench.fake.deliver(&ok));
    bench.pump();
    assert_eq!(bench.received(), std::vec![ok]);
    assert_eq!(bench.engine.stats().oversize, 1);
}

#[test]
fn descriptor_lengths_too_short_for_a_frame_are_runts() {
    let mut bench = Bench::new(8, 8);
    // Shorter than the FCS alone, exactly the FCS, a frame of 13 bytes (one
    // short of an Ethernet header) after the trim.
    for wire_len in [0usize, 3, 4, 17] {
        assert!(
            bench.fake.deliver_raw(&std::vec![0xAB; wire_len], 0),
            "{wire_len}"
        );
    }
    let good = frame(64, 9);
    assert!(bench.fake.deliver(&good));
    bench.pump();
    assert_eq!(bench.received(), std::vec![good]);
    assert_eq!(bench.engine.stats().runts, 4);
}

#[test]
fn short_frames_are_padded_to_the_ethernet_minimum() {
    let mut bench = Bench::new(8, 8);
    let short = frame(42, 5);
    bench.tx.push(&short).unwrap();
    bench.pump();
    let sent = bench.fake.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].len(), 60);
    assert_eq!(&sent[0][..42], &short[..]);
    assert!(sent[0][42..].iter().all(|b| *b == 0), "padding is zeros");
    // A full-size frame is sent as is.
    let full = frame(MAX_FRAME_LEN, 6);
    bench.tx.push(&full).unwrap();
    bench.pump();
    assert_eq!(bench.fake.sent()[1], full);
    assert_eq!(bench.fake.0.borrow().doorbells, 2);
}

#[test]
fn a_full_transmit_ring_waits_for_the_chip() {
    let mut bench = Bench::new(8, 8);
    bench.fake.0.borrow_mut().auto_tx = false;
    for id in 0..12u8 {
        bench.tx.push(&frame(60, id)).unwrap();
    }
    let out = bench.pump();
    assert_eq!(out.tx_sent, 7, "entries - 1 frames fit in flight");
    assert_eq!(bench.engine.queues().tx_free(), 0);
    assert_eq!(bench.engine.queues().tx_in_flight(), 7);
    bench.fake.complete_tx();
    let out = bench.pump();
    assert_eq!(out.tx_sent, 5);
    bench.fake.complete_tx();
    bench.pump();
    let ids: Vec<u8> = bench.fake.sent().iter().map(|f| f[6]).collect();
    assert_eq!(
        ids,
        (0..12u8).map(|id| id.wrapping_add(6)).collect::<Vec<_>>()
    );
    assert_eq!(bench.engine.queues().tx_reaped_total(), 12);
}

#[test]
fn the_receive_ring_refills_after_overflow() {
    let mut bench = Bench::new(8, 8);
    let mut accepted = 0;
    for id in 0..20u8 {
        accepted += usize::from(bench.fake.deliver(&frame(100, id)));
    }
    assert_eq!(accepted, 8, "the chip holds every descriptor");
    assert_eq!(bench.pump().rx_delivered, 8);
    assert!(bench.fake.deliver(&frame(100, 99)), "buffers came back");
    assert_eq!(bench.pump().rx_delivered, 1);
}

#[test]
fn error_bits_and_spread_frames_are_dropped_and_counted() {
    let mut bench = Bench::new(16, 8);
    let wire = |len: usize| {
        let mut w = frame(len, 1);
        w.extend_from_slice(&FCS);
        w
    };
    assert!(bench.fake.deliver_raw(&wire(100), rx_err::CRC));
    assert!(bench.fake.deliver_raw(&wire(100), rx_err::RES));
    assert!(bench.fake.deliver_raw(&wire(100), rx_err::RWT));
    assert!(bench.fake.deliver_raw(&wire(100), rx_err::RUNT));
    // A frame spread over two descriptors (longer than a slot): both halves.
    assert!(bench.fake.deliver_raw(&std::vec![7u8; 3000], 0));
    let good = frame(64, 4);
    assert!(bench.fake.deliver(&good));
    bench.pump();
    assert_eq!(bench.received(), std::vec![good]);
    let s = bench.engine.stats();
    assert_eq!(s.runts, 1);
    assert_eq!(
        s.ring_errors, 5,
        "CRC, RES and RWT frames and both halves of the spread frame"
    );
}

#[test]
fn a_lying_chip_cannot_push_the_driver_out_of_bounds() {
    let mut bench = Bench::new(8, 8);
    // A completion with an absurd length on the next descriptor is one bad
    // frame, dropped.
    bench.fake.lie_rx(0, FS | LS | 0x3FFF);
    bench.pump();
    assert!(bench.received().is_empty());
    // Transmit completions for descriptors never sent change nothing.
    for index in 0..8 {
        bench.fake.lie_tx_done(index);
    }
    bench.pump();
    assert_eq!(bench.engine.queues().tx_free(), 7);
    // And the chip still works afterwards.
    let good = frame(80, 9);
    let head = bench.fake.0.borrow().rx_head_for_test();
    assert_eq!(head, 0, "the model's head did not move with the lie");
    // The lie consumed descriptor 0 for the driver; the model moves on too.
    bench.fake.0.borrow_mut().skip_rx_for_test();
    assert!(bench.fake.deliver(&good));
    bench.pump();
    assert_eq!(bench.received(), std::vec![good]);
}

#[test]
fn completions_out_of_ring_order_are_fatal_on_the_second_sighting() {
    let mut bench = Bench::new(8, 8);
    // Descriptor 3 done while 0..=2 are still the chip's, and the driver is
    // looking at 0: not a frame, a broken chip.
    bench.fake.lie_rx(1, FS | LS | 64);
    assert!(bench.engine.pump(&mut NoBell).is_ok(), "once may be a race");
    assert!(
        matches!(
            bench.engine.pump(&mut NoBell),
            Err(Fatal::Hardware(why)) if why.contains("rx")
        ),
        "twice is not"
    );
}

#[test]
fn a_lone_out_of_order_sighting_that_heals_is_forgiven() {
    let mut bench = Bench::new(8, 8);
    bench.fake.lie_rx(1, FS | LS | 64);
    bench.pump();
    // The chip catches up: descriptor 0 completes, then 1 is genuinely next.
    assert!(bench.fake.deliver(&frame(70, 1)));
    bench.fake.lie_rx(1, OWN | SLOT_BYTES as u32);
    bench.pump();
    assert_eq!(bench.received().len(), 1);
    for _ in 0..4 {
        bench.pump();
    }
}

#[test]
fn transmit_completions_out_of_order_are_fatal() {
    let mut bench = Bench::new(8, 8);
    bench.fake.0.borrow_mut().auto_tx = false;
    for id in 0..3u8 {
        bench.tx.push(&frame(60, id)).unwrap();
    }
    bench.pump();
    // The chip finishes the second frame but not the first.
    bench.fake.lie_tx_done(1);
    assert!(bench.engine.pump(&mut NoBell).is_ok());
    assert!(matches!(
        bench.engine.pump(&mut NoBell),
        Err(Fatal::Hardware(why)) if why.contains("tx")
    ));
}

/// Seeded: honest traffic interleaved with lies. With no lies every frame
/// must cross intact and in order; with lies the driver must not panic, must
/// never hand the client more than a frame, and a `Fatal` is an acceptable
/// verdict on a lying chip (the binary restarts).
#[test]
fn seeded_traffic_with_a_hostile_chip() {
    fuzzkit::for_seeds("rtl8168::seeded_traffic_with_a_hostile_chip", |_, rng| {
        let sizes = [8u16, 16, 64, 256];
        let hostile = rng.one_in(2);
        let mut bench = Bench::new(*rng.pick(&sizes), *rng.pick(&sizes));
        let (mut want_rx, mut want_tx) = (Vec::new(), Vec::new());
        let (mut got_rx, mut got_tx) = (Vec::new(), Vec::new());
        let mut dead = false;
        for step in 0..rng.range(50, 400) {
            match rng.below(6) {
                0 | 1 if want_rx.len() - got_rx.len() < SLOTS as usize - 1 => {
                    let f = frame(rng.range(60, 1514) as usize, step as u8);
                    if bench.fake.deliver(&f) {
                        want_rx.push(f);
                    }
                }
                2 | 3 => {
                    let f = frame(rng.range(60, 1514) as usize, step as u8);
                    if bench.tx.push(&f).is_ok() {
                        want_tx.push(f);
                    }
                }
                4 if hostile => {
                    let index = rng.below(256) as u32;
                    bench.fake.lie_rx(index, rng.next_u32());
                }
                _ => {
                    match bench.engine.pump(&mut NoBell) {
                        Ok(_) => {}
                        Err(Fatal::Hardware(_)) => {
                            assert!(hostile, "an honest chip was called a liar");
                            dead = true;
                            break;
                        }
                        Err(other) => panic!("{other:?}"),
                    }
                    got_rx.extend(bench.received());
                    got_tx.append(&mut bench.fake.0.borrow_mut().sent);
                }
            }
        }
        if dead {
            return;
        }
        for _ in 0..4 {
            if bench.engine.pump(&mut NoBell).is_err() {
                assert!(hostile);
                return;
            }
            got_rx.extend(bench.received());
            got_tx.append(&mut bench.fake.0.borrow_mut().sent);
        }
        assert!(got_rx.iter().all(|f| f.len() <= MAX_FRAME_LEN));
        if !hostile {
            let key = |v: &Vec<Vec<u8>>| v.iter().map(|f| (f.len(), f[6])).collect::<Vec<_>>();
            assert_eq!(key(&got_rx), key(&want_rx));
            assert_eq!(key(&got_tx), key(&want_tx));
        }
    });
}
