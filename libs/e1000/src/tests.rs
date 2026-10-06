//! Host tests: bring-up against the card model, the rings under the shared
//! `nicdrv` engine with a real client, and a card that lies.

use std::vec::Vec;

use framering::{ring_bytes, Consumer, Producer, Ring, MAX_FRAME};
use nicdrv::{Engine, NicRings};

use crate::desc::rx_status;
use crate::fake::{Fake, Memory, BUS};
use crate::regs::*;
use crate::rings::{Layout, Rings};
use crate::setup::{self, SetupError};

const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const OWNER: u64 = 7;
const SLOTS: u32 = 16;

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
    _dma: Memory,
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
        let mut engine = Engine::new(rings, MAC, 1514, true);
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
            _dma: dma,
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
}

fn frame(len: usize, id: u8) -> Vec<u8> {
    let mut f: Vec<u8> = (0..len).map(|i| id.wrapping_add(i as u8)).collect();
    f[..6].copy_from_slice(&MAC);
    f
}

#[test]
fn model_ids() {
    assert_eq!(crate::model(0x8086, 0x100E), Some("82540EM"));
    assert_eq!(
        crate::model(0x8086, 0x10D3),
        None,
        "e1000e is not this driver"
    );
    assert_eq!(crate::model(0x1AF4, 0x100E), None);
}

#[test]
fn reset_and_station_address() {
    let memory = Memory::new(4096);
    let fake = Fake::new(&memory);
    let mut regs = fake.clone();
    fake.set_reg(IMS, int::ALL);
    setup::reset(&mut regs, || {}).unwrap();
    assert_eq!(fake.reg(CTRL) & ctrl::RST, 0);
    assert_ne!(fake.reg(CTRL) & ctrl::SLU, 0);
    assert_eq!(setup::mac(&mut regs, || {}), Ok(MAC));
    // Without a valid receive address the EEPROM is read.
    fake.set_reg(RAH0, 0);
    assert_eq!(setup::mac(&mut regs, || {}), Ok(MAC));
    // A group or zero address is never a station address.
    fake.0.borrow_mut().eeprom = [0x0001, 0, 0];
    assert_eq!(setup::mac(&mut regs, || {}), Err(SetupError::NoMac));
    fake.0.borrow_mut().eeprom = [0, 0, 0];
    assert_eq!(setup::mac(&mut regs, || {}), Err(SetupError::NoMac));
    assert!(setup::link_up(&regs));
}

#[test]
fn a_reset_that_never_finishes_times_out() {
    let memory = Memory::new(4096);
    let fake = Fake::new(&memory);
    fake.0.borrow_mut().reset_reads = u32::MAX;
    let mut regs = fake.clone();
    let mut naps = 0;
    assert_eq!(
        setup::reset(&mut regs, || naps += 1),
        Err(SetupError::ResetTimeout)
    );
    assert!(naps >= 1000, "gave up after {naps} naps");
}

#[test]
fn layout_rules() {
    assert!(Layout::new(8, 8).is_some());
    assert!(Layout::new(256, 256).is_some());
    for bad in [0u16, 4, 12, 512] {
        assert!(Layout::new(bad, 8).is_none(), "{bad}");
        assert!(Layout::new(8, bad).is_none(), "{bad}");
    }
    let l = Layout::new(64, 32).unwrap();
    assert_eq!(l.total, l.tx_slots + 32 * 2048);
    for offset in [l.rx_ring, l.tx_ring, l.rx_slots, l.tx_slots] {
        assert_eq!(offset % 4096, 0);
    }
}

#[test]
fn rings_program_the_card() {
    let bench = Bench::new(32, 16);
    let f = &bench.fake;
    assert_eq!(u64::from(f.reg(RDBAL)) | u64::from(f.reg(RDBAH)) << 32, BUS);
    assert_eq!(f.reg(RDLEN), 32 * 16);
    assert_eq!(f.reg(RDT), 31, "every descriptor but the gap is the card's");
    assert_eq!(f.reg(TDLEN), 16 * 16);
    assert_eq!(f.reg(TDT), 0);
    let rctl = f.reg(RCTL);
    assert_ne!(rctl & rctl::EN, 0);
    assert_ne!(rctl & rctl::SECRC, 0);
    assert_ne!(f.reg(TCTL) & tctl::EN, 0);
    assert_eq!(bench.engine.queues().rx_in_flight(), 31);
    assert_eq!(bench.engine.queues().tx_free(), 15);
}

#[test]
fn frames_cross_in_both_directions_and_wrap() {
    let mut bench = Bench::new(8, 8);
    for round in 0..200u32 {
        let incoming = frame(60 + (round as usize % 1400), round as u8);
        assert!(bench.fake.deliver(&incoming), "round {round}: no buffer");
        let outgoing = frame(42 + (round as usize % 1000), !round as u8);
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

#[test]
fn a_full_transmit_ring_waits_for_the_card() {
    let mut bench = Bench::new(8, 8);
    bench.fake.0.borrow_mut().auto_tx = false;
    for id in 0..12u8 {
        bench.tx.push(&frame(60, id)).unwrap();
    }
    let out = bench.pump();
    assert_eq!(out.tx_sent, 7, "entries - 1 frames fit in flight");
    assert_eq!(bench.engine.queues().tx_free(), 0);
    bench.fake.complete_tx();
    let out = bench.pump();
    assert_eq!(out.tx_sent, 5);
    bench.fake.complete_tx();
    let sent = &bench.fake.0.borrow().sent;
    let ids: Vec<u8> = sent.iter().map(|f| f[6]).collect();
    assert_eq!(
        ids,
        (0..12u8).map(|id| id.wrapping_add(6)).collect::<Vec<_>>()
    );
}

#[test]
fn the_receive_ring_refills_after_overflow() {
    let mut bench = Bench::new(8, 8);
    let mut accepted = 0;
    for id in 0..20u8 {
        accepted += usize::from(bench.fake.deliver(&frame(100, id)));
    }
    assert_eq!(accepted, 7, "the card holds all but the gap");
    assert_eq!(bench.pump().rx_delivered, 7);
    assert!(bench.fake.deliver(&frame(100, 99)), "buffers came back");
    assert_eq!(bench.pump().rx_delivered, 1);
}

#[test]
fn bad_frames_are_dropped_and_counted() {
    let mut bench = Bench::new(16, 8);
    // Spread over two descriptors (longer than a slot), a runt, an
    // oversize frame, then a good one.
    assert!(bench.fake.deliver(&frame(3000, 1)));
    assert!(bench.fake.deliver(&frame(10, 2)));
    assert!(bench.fake.deliver(&frame(1600, 3)));
    let good = frame(64, 4);
    assert!(bench.fake.deliver(&good));
    bench.pump();
    assert_eq!(bench.received(), std::vec![good]);
    let s = bench.engine.stats();
    assert_eq!((s.runts, s.oversize), (1, 1));
    assert_eq!(s.ring_errors, 2, "both halves of the spread frame");
}

#[test]
fn a_lying_card_cannot_push_the_driver_out_of_bounds() {
    let mut bench = Bench::new(8, 8);
    // Completions the card had no buffer for, with absurd lengths and every
    // error bit, on every descriptor, the gap included.
    for index in 0..8 {
        bench
            .fake
            .lie_rx(index, u16::MAX, rx_status::DD | rx_status::EOP, 0);
    }
    bench.pump();
    for index in 0..8 {
        bench
            .fake
            .lie_rx(index, 64, rx_status::DD | rx_status::EOP, 0xFF);
    }
    bench.pump();
    assert!(bench.received().is_empty());
    // Transmit completions for descriptors never sent change nothing.
    for index in 0..8 {
        bench.fake.lie_tx_done(index);
    }
    bench.pump();
    assert_eq!(bench.engine.queues().tx_free(), 7);
    // And the card still works afterwards.
    let good = frame(80, 9);
    assert!(bench.fake.deliver(&good));
    bench.pump();
    assert_eq!(bench.received(), std::vec![good]);
}

/// Seeded: honest traffic interleaved with lies. With no lies every frame
/// must cross intact and in order; with lies the driver must not panic, must
/// never hand the client more than a frame, and must keep working.
#[test]
fn seeded_traffic_with_a_hostile_card() {
    fuzzkit::for_seeds("e1000::seeded_traffic_with_a_hostile_card", |_, rng| {
        let sizes = [8u16, 16, 64, 256];
        let hostile = rng.one_in(2);
        let mut bench = Bench::new(*rng.pick(&sizes), *rng.pick(&sizes));
        let (mut want_rx, mut want_tx) = (Vec::new(), Vec::new());
        let (mut got_rx, mut got_tx) = (Vec::new(), Vec::new());
        for step in 0..rng.range(50, 400) {
            match rng.below(6) {
                // The client drains only when the driver pumps: never queue
                // more than its ring holds, or the engine (rightly) drops.
                0 | 1 if want_rx.len() - got_rx.len() < SLOTS as usize - 1 => {
                    let f = frame(rng.range(14, 1514) as usize, step as u8);
                    if bench.fake.deliver(&f) {
                        want_rx.push(f);
                    }
                }
                2 | 3 => {
                    let f = frame(rng.range(14, 1514) as usize, step as u8);
                    if bench.tx.push(&f).is_ok() {
                        want_tx.push(f);
                    }
                }
                4 if hostile => {
                    let index = rng.below(256) as u32;
                    let length = rng.next_u32() as u16;
                    bench.fake.lie_rx(index, length, rng.byte(), rng.byte());
                }
                _ => {
                    bench.pump();
                    got_rx.extend(bench.received());
                    got_tx.append(&mut bench.fake.0.borrow_mut().sent);
                }
            }
        }
        for _ in 0..4 {
            bench.pump();
            got_rx.extend(bench.received());
            got_tx.append(&mut bench.fake.0.borrow_mut().sent);
        }
        assert!(got_rx.iter().all(|f| f.len() <= 1514));
        if !hostile {
            assert_eq!(
                got_rx.iter().map(|f| (f.len(), f[6])).collect::<Vec<_>>(),
                want_rx.iter().map(|f| (f.len(), f[6])).collect::<Vec<_>>()
            );
            assert_eq!(
                got_tx.iter().map(|f| (f.len(), f[6])).collect::<Vec<_>>(),
                want_tx.iter().map(|f| (f.len(), f[6])).collect::<Vec<_>>()
            );
        }
    });
}

#[test]
fn interrupt_causes_clear_on_read() {
    let memory = Memory::new(4096);
    let fake = Fake::new(&memory);
    let mut regs = fake.clone();
    setup::enable_interrupts(&mut regs);
    assert_eq!(fake.reg(IMS), int::WANTED);
    fake.raise(int::LSC | int::RXT0);
    assert_eq!(setup::take_causes(&mut regs), int::LSC | int::RXT0);
    assert_eq!(setup::take_causes(&mut regs), 0, "reading ICR clears it");
}
