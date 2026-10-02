//! Host tests: ring state machines against a model controller, TRB and
//! context layouts, register helpers.

use std::vec;
use std::vec::Vec;

use crate::context::{
    slot_state, EndpointContext, EndpointType, InputContext, SlotContext, SlotState,
};
use crate::regs::{self, portsc, Speed, Structural};
use crate::ring::{erst_entry, EventRing, ProducerRing, TrbMem};
use crate::trb::{self, kind, request, Trb, CHAIN, CYCLE, IDT, IOC};
use crate::Error;

mod bulk;
mod extcap;
mod route;

/// A segment in ordinary memory at a pretend bus address.
struct VecMem {
    trbs: Vec<Trb>,
    phys: u64,
}

impl VecMem {
    fn new(len: usize, phys: u64) -> VecMem {
        VecMem {
            trbs: vec![Trb::default(); len],
            phys,
        }
    }
}

impl TrbMem for VecMem {
    fn len(&self) -> usize {
        self.trbs.len()
    }
    fn phys(&self) -> u64 {
        self.phys
    }
    fn read(&self, index: usize) -> Trb {
        self.trbs[index]
    }
    fn write(&mut self, index: usize, trb: Trb) {
        self.trbs[index] = trb;
    }
}

/// The controller's side of a producer ring: consume what the cycle bit
/// hands over, follow Links, toggle on Toggle Cycle. A Link met inside a
/// chained TD must chain too (xHCI 4.11.5.1), or the TD would end there.
struct Consumer {
    index: usize,
    cycle: bool,
    in_td: bool,
}

impl Consumer {
    fn new() -> Consumer {
        Consumer {
            index: 0,
            cycle: true,
            in_td: false,
        }
    }

    /// The next TRB the controller would execute and its bus address.
    fn next<M: TrbMem>(&mut self, mem: &M) -> Option<(u64, Trb)> {
        for _ in 0..2 {
            let trb = mem.read(self.index);
            if trb.cycle() != self.cycle {
                return None;
            }
            if trb.kind() == kind::LINK {
                assert_eq!(trb.parameter, mem.phys(), "link target");
                assert_eq!(trb.control & CHAIN != 0, self.in_td, "link chain bit");
                self.index = 0;
                if trb.control & trb::TOGGLE_CYCLE != 0 {
                    self.cycle = !self.cycle;
                }
                continue;
            }
            let at = mem.phys() + self.index as u64 * 16;
            self.index += 1;
            self.in_td = trb.control & CHAIN != 0;
            return Some((at, trb));
        }
        panic!("two links in a row");
    }
}

const RING_PHYS: u64 = 0x10_0000;

#[test]
fn producer_ring_wraps_for_many_laps() {
    let mut ring = ProducerRing::new(VecMem::new(8, RING_PHYS)).unwrap();
    assert_eq!(ring.dequeue_pointer(), RING_PHYS | 1);
    let mut controller = Consumer::new();
    let mut expected = 0u64;
    for round in 0..200u64 {
        let batch = 1 + (round % 3) as usize;
        let trbs: Vec<Trb> = (0..batch)
            .map(|n| Trb {
                parameter: expected + n as u64,
                ..trb::enable_slot()
            })
            .collect();
        let last = ring.enqueue(&trbs, true).unwrap();
        let mut seen = Vec::new();
        while let Some((at, trb)) = controller.next(ring.mem()) {
            seen.push((at, trb));
        }
        assert_eq!(seen.len(), batch, "round {round}");
        for (n, (_, trb)) in seen.iter().enumerate() {
            assert_eq!(trb.parameter, expected + n as u64);
            assert_eq!(trb.control & CHAIN != 0, n + 1 < batch, "chain bits");
        }
        assert_eq!(seen.last().unwrap().0, last, "completion pointer");
        ring.retire(last).unwrap();
        assert_eq!(ring.in_flight(), 0);
        expected += batch as u64;
    }
}

#[test]
fn a_full_ring_refuses_and_frees_on_retire() {
    let mut ring = ProducerRing::new(VecMem::new(8, RING_PHYS)).unwrap();
    assert_eq!(ring.free(), 6);
    let mut pointers = Vec::new();
    for _ in 0..6 {
        pointers.push(ring.enqueue(&[trb::no_op_command()], false).unwrap());
    }
    assert_eq!(
        ring.enqueue(&[trb::no_op_command()], false),
        Err(Error::RingFull)
    );
    // Retiring the third completes the first three.
    ring.retire(pointers[2]).unwrap();
    assert_eq!((ring.in_flight(), ring.free()), (3, 3));
    // A unit that does not fit is refused whole.
    assert_eq!(
        ring.enqueue(&[trb::no_op_command(); 4], true),
        Err(Error::RingFull)
    );
    assert_eq!(ring.in_flight(), 3);
    assert_eq!(ring.enqueue(&[], false), Err(Error::RingFull));
}

#[test]
fn hostile_completion_pointers_are_refused() {
    let mut ring = ProducerRing::new(VecMem::new(8, RING_PHYS)).unwrap();
    let first = ring.enqueue(&[trb::no_op_command()], false).unwrap();
    for bad in [
        0,
        RING_PHYS - 16,
        RING_PHYS + 8,      // misaligned
        RING_PHYS + 7 * 16, // the Link
        RING_PHYS + 8 * 16, // past the segment
        RING_PHYS + 3 * 16, // inside, but not in flight
        u64::MAX,
    ] {
        assert_eq!(ring.retire(bad), Err(Error::BadPointer), "{bad:#x}");
    }
    ring.retire(first).unwrap();
    assert_eq!(ring.retire(first), Err(Error::BadPointer), "retired twice");
}

#[test]
fn rings_refuse_bad_buffers() {
    assert!(ProducerRing::new(VecMem::new(7, RING_PHYS)).is_err());
    assert!(ProducerRing::new(VecMem::new(8, RING_PHYS + 16)).is_err());
    assert!(EventRing::new(VecMem::new(8, RING_PHYS)).is_err());
    assert!(EventRing::new(VecMem::new(16, RING_PHYS + 32)).is_err());
}

#[test]
fn event_ring_follows_the_controller_cycle() {
    const LEN: usize = 16;
    let mut ring = EventRing::new(VecMem::new(LEN, RING_PHYS)).unwrap();
    assert_eq!(ring.pop(), None, "zeroed ring is empty");
    assert_eq!(
        erst_entry(RING_PHYS, LEN as u16),
        [RING_PHYS as u32, 0, 16, 0]
    );
    // The controller's producer side, written straight into the memory the
    // ring reads (the ring owns it, so go through a raw handle).
    let (mut at, mut cycle) = (0usize, true);
    let mut produced = 0u64;
    let mut consumed = 0u64;
    for burst in 1..40usize {
        for _ in 0..(burst % LEN) {
            let mut event = Trb {
                parameter: produced,
                status: (trb::code::SUCCESS as u32) << 24,
                control: (kind::COMMAND_COMPLETION as u32) << 10,
            };
            if cycle {
                event.control |= CYCLE;
            }
            ring_mem(&mut ring).write(at, event);
            produced += 1;
            at += 1;
            if at == LEN {
                at = 0;
                cycle = !cycle;
            }
        }
        while let Some(event) = ring.pop() {
            assert_eq!(event.parameter, consumed);
            assert_eq!(event.kind(), kind::COMMAND_COMPLETION);
            assert_eq!(event.completion_code(), trb::code::SUCCESS);
            consumed += 1;
        }
        assert_eq!(consumed, produced, "burst {burst}");
        let erdp = ring.erdp();
        assert_eq!(erdp & !0xF, RING_PHYS + at as u64 * 16);
        assert_eq!(erdp & regs::rt::ERDP_EHB, regs::rt::ERDP_EHB);
    }
}

/// Mutable access to an event ring's memory, standing in for the controller.
fn ring_mem(ring: &mut EventRing<VecMem>) -> &mut VecMem {
    ring.mem_mut()
}

#[test]
fn control_transfer_layout() {
    // GET_DESCRIPTOR(device, 18 bytes): setup, IN data, OUT status.
    let setup = request::get_descriptor(1, 0, 18);
    let (trbs, count) = trb::control_transfer(&setup, 0xABC0);
    assert_eq!(count, 3);
    assert_eq!(trbs[0].kind(), kind::SETUP_STAGE);
    assert_eq!(
        trbs[0].parameter.to_le_bytes(),
        [0x80, 6, 0, 1, 0, 0, 18, 0]
    );
    assert_eq!(trbs[0].status, 8);
    assert_eq!(trbs[0].control & IDT, IDT);
    assert_eq!((trbs[0].control >> 16) & 3, 3, "TRT: IN data stage");
    assert_eq!(trbs[1].kind(), kind::DATA_STAGE);
    assert_eq!((trbs[1].parameter, trbs[1].status), (0xABC0, 18));
    assert_eq!((trbs[1].control >> 16) & 1, 1, "data stage IN");
    assert_eq!(trbs[2].kind(), kind::STATUS_STAGE);
    assert_eq!((trbs[2].control >> 16) & 1, 0, "status stage OUT");
    assert_eq!(trbs[2].control & IOC, IOC);
    // SET_CONFIGURATION(1): no data, status IN.
    let (trbs, count) = trb::control_transfer(&request::set_configuration(1), 0);
    assert_eq!(count, 2);
    assert_eq!(trbs[0].parameter.to_le_bytes(), [0, 9, 1, 0, 0, 0, 0, 0]);
    assert_eq!((trbs[0].control >> 16) & 3, 0, "TRT: no data");
    assert_eq!((trbs[1].control >> 16) & 1, 1, "status stage IN");
    // HID class requests to interface 0.
    assert_eq!(
        request::set_protocol(0, true).immediate_bytes(),
        [0x21, 0x0B, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        request::set_idle(2).immediate_bytes(),
        [0x21, 0x0A, 0, 0, 2, 0, 0, 0]
    );
    // The report descriptor is asked of the interface (bmRequestType 0x81).
    assert_eq!(
        request::get_report_descriptor(1, 74).immediate_bytes(),
        [0x81, 0x06, 0, 0x22, 1, 0, 74, 0]
    );
}

#[test]
fn command_trbs_carry_slot_and_endpoint() {
    let addr = trb::address_device(0x2000, 3, false);
    assert_eq!(
        (addr.kind(), addr.slot(), addr.parameter),
        (kind::ADDRESS_DEVICE, 3, 0x2000)
    );
    assert_eq!(addr.control & trb::BSR, 0);
    assert_ne!(trb::address_device(0x2000, 3, true).control & trb::BSR, 0);
    let stop = trb::stop_endpoint(5, 3);
    assert_eq!(
        (stop.kind(), stop.slot(), stop.endpoint()),
        (kind::STOP_ENDPOINT, 5, 3)
    );
    let normal = trb::interrupt_in(0x4000, 8);
    assert_eq!((normal.kind(), normal.status), (kind::NORMAL, 8));
    assert_eq!(normal.control & (IOC | trb::ISP), IOC | trb::ISP);
    // Event field decoders.
    let event = Trb {
        parameter: 3 << 24,
        status: (13 << 24) | 5,
        control: (7 << 24) | (3 << 16) | ((kind::TRANSFER_EVENT as u32) << 10) | CYCLE,
    };
    assert_eq!(
        (
            event.kind(),
            event.completion_code(),
            event.residual(),
            event.slot(),
            event.endpoint(),
            event.port()
        ),
        (kind::TRANSFER_EVENT, 13, 5, 7, 3, 3)
    );
}

#[test]
fn input_context_layout() {
    for csz64 in [false, true] {
        let stride = if csz64 { 16 } else { 8 };
        let mut buffer = vec![0xFFFF_FFFFu32; 33 * stride];
        let mut input = InputContext::new(&mut buffer, csz64).unwrap();
        input
            .slot(&SlotContext {
                entries: 3,
                ..SlotContext::root(Speed::High, 2)
            })
            .unwrap();
        input
            .endpoint(
                3,
                &EndpointContext::interrupt(EndpointType::InterruptIn, 8, 0, 6, 0x1234_5000 | 1),
            )
            .unwrap();
        assert_eq!(input.added(), 0b1001, "slot and DCI 3");
        let d = input.dwords();
        assert_eq!(d[0], 0, "no drop flags");
        assert_eq!(d[stride], 3 << 20 | 3 << 27, "slot: speed, entries");
        assert_eq!(d[stride + 1], 2 << 16, "slot: root port");
        let ep = 4 * stride;
        assert_eq!(d[ep], 6 << 16, "interval");
        assert_eq!(d[ep + 1], 3 << 1 | 7 << 3 | 8 << 16, "cerr, type, packet");
        assert_eq!((d[ep + 2], d[ep + 3]), (0x1234_5001, 0));
        assert_eq!(d[ep + 4], 8 | 8 << 16, "avg trb, max esit");
        assert!(
            d[stride * 2..ep].iter().all(|&w| w == 0),
            "untouched contexts are cleared"
        );
    }
    let mut small = vec![0u32; 32 * 8];
    assert!(InputContext::new(&mut small, false).is_err());
}

#[test]
fn input_context_refuses_bad_fields() {
    let mut buffer = vec![0u32; 33 * 8];
    let mut input = InputContext::new(&mut buffer, false).unwrap();
    let slot = SlotContext::root(Speed::Full, 1);
    assert!(input
        .slot(&SlotContext {
            root_port: 0,
            ..slot
        })
        .is_err());
    assert!(input.slot(&SlotContext { entries: 0, ..slot }).is_err());
    assert!(input
        .slot(&SlotContext {
            route: 1 << 20,
            ..slot
        })
        .is_err());
    let ep = EndpointContext::control(8, 0x1000 | 1);
    assert!(input.endpoint(0, &ep).is_err());
    assert!(input.endpoint(32, &ep).is_err());
    assert!(input
        .endpoint(
            1,
            &EndpointContext {
                max_packet: 0,
                ..ep
            }
        )
        .is_err());
    assert!(input
        .endpoint(1, &EndpointContext { interval: 16, ..ep })
        .is_err());
    assert!(input
        .endpoint(
            1,
            &EndpointContext {
                dequeue: 0x1008,
                ..ep
            }
        )
        .is_err());
    assert!(input.add(32).is_err());
    input.endpoint(1, &ep).unwrap();
    input.ep0_max_packet(64).unwrap();
    assert_eq!(input.dwords()[2 * 8 + 1] >> 16, 64);
    assert!(input.ep0_max_packet(0).is_err());
}

#[test]
fn output_slot_state() {
    let mut device = [0u32; 8];
    device[3] = 2 << 27 | 5;
    assert_eq!(slot_state(&device), Some((SlotState::Addressed, 5)));
    assert_eq!(slot_state(&device[..2]), None);
}

#[test]
fn register_helpers() {
    let s = Structural::decode(0x0800_0840);
    assert_eq!((s.max_slots, s.max_interrupters, s.max_ports), (0x40, 8, 8));
    assert_eq!(regs::scratchpad_count((1 << 21) | (3 << 27)), 32 + 3);
    assert!(regs::context_64(0x4) && !regs::context_64(0));
    assert_eq!(regs::extended_caps(0x0050_0000), Some(0x140));
    assert_eq!(regs::extended_caps(0), None);
    assert_eq!(regs::doorbell(0x2003, 2), 0x2008);
    assert_eq!(
        (
            regs::dci(0x00),
            regs::dci(0x81),
            regs::dci(0x02),
            regs::dci(0x8F)
        ),
        (1, 3, 4, 31)
    );
    // Intervals: full speed 10 ms -> 2^6 microframes (8 ms); high speed
    // bInterval 7 -> 2^6.
    assert_eq!(regs::interrupt_interval(Speed::Full, 10), 6);
    assert_eq!(regs::interrupt_interval(Speed::Full, 1), 3);
    assert_eq!(regs::interrupt_interval(Speed::Full, 255), 10);
    assert_eq!(regs::interrupt_interval(Speed::High, 7), 6);
    // A high-speed device cannot ask for more than 1000 polls a second.
    assert_eq!(regs::interrupt_interval(Speed::High, 0), 3);
    assert_eq!(regs::interrupt_interval(Speed::High, 1), 3);
    assert_eq!(regs::interrupt_interval(Speed::Super, 4), 3);
    assert_eq!(regs::interrupt_interval(Speed::High, 5), 4);
    assert_eq!(regs::interrupt_interval(Speed::High, 200), 15);
    assert_eq!(Speed::of_port(3 << 10 | portsc::CCS), Some(Speed::High));
    assert_eq!(Speed::of_port(0), None);
    assert_eq!(Speed::Low.default_max_packet0(), 8);
    assert_eq!(Speed::Full.default_max_packet0(), 64);
    assert_eq!(Speed::Super.default_max_packet0(), 512);
}

#[test]
fn portsc_writes_never_echo_dangerous_bits() {
    let current = portsc::CCS | portsc::PED | portsc::PP | portsc::CSC | portsc::PRC | 3 << 10;
    let reset = portsc::set(current, portsc::PR);
    assert_eq!(reset & portsc::PED, 0, "echoing PED would disable the port");
    assert_eq!(
        reset & portsc::CHANGES,
        0,
        "echoing change bits would clear them"
    );
    assert_eq!(reset & (portsc::PP | portsc::PR), portsc::PP | portsc::PR);
    let ack = portsc::ack_changes(current);
    assert_eq!(ack & portsc::CHANGES, portsc::CSC | portsc::PRC);
    assert_eq!(ack & (portsc::PED | portsc::PR), 0);
}

#[test]
fn abandon_skips_what_a_halted_endpoint_left() {
    let mut ring = ProducerRing::new(VecMem::new(8, 0x4000)).unwrap();
    let (trbs, count) = trb::control_transfer(&request::get_descriptor(1, 0, 18), 0x9000);
    let last = ring.enqueue(&trbs[..count], true).unwrap();
    assert_eq!(ring.abandon(), (0x4000 + 3 * 16) | 1, "next TRB, cycle 1");
    assert_eq!(ring.in_flight(), 0);
    assert_eq!(
        ring.retire(last),
        Err(Error::BadPointer),
        "nothing in flight"
    );
    // Past the Link (slot 7) the cycle state flips.
    ring.enqueue(&trbs[..count], true).unwrap();
    ring.abandon();
    ring.enqueue(&[trb::no_op_command()], false).unwrap();
    assert_eq!(ring.abandon(), 0x4000);
    assert_eq!(
        trb::set_tr_dequeue(2, 3, 0x4000 | 1),
        Trb {
            parameter: 0x4001,
            status: 0,
            control: (kind::SET_TR_DEQUEUE as u32) << 10 | 2 << 24 | 3 << 16,
        }
    );
}
