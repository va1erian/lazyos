//! Host tests: controller bring-up and the command rings against the model,
//! the stream descriptor, format words and the play cursor.

use crate::controller::{Controller, ControllerError, ENTRIES, RING_BYTES};
use crate::cursor::Cursor;
use crate::fake::{qemu_output, Fake, Memory, OUT0};
use crate::format;
use crate::regs::*;
use crate::stream::{write_bdl, OutStream, BDL_ENTRY};
use crate::verbs::{param, VerbError, Verbs};

fn nap() {}

fn up(fake: &Fake, memory: &Memory) -> Result<Controller<Fake>, ControllerError> {
    // SAFETY: the memory outlives the controller in every test and is its own.
    unsafe { Controller::new(fake.clone(), memory.rings(), nap) }
}

#[test]
fn bring_up_finds_the_codec_and_runs_the_rings() {
    let memory = Memory::new(RING_BYTES);
    let fake = Fake::new(&memory, qemu_output());
    let mut controller = up(&fake, &memory).unwrap();
    assert_eq!(controller.codec, 0);
    assert_eq!((controller.caps.inputs, controller.caps.outputs), (4, 4));
    assert_eq!(controller.output_stream(0), Some(OUT0));
    assert_eq!(controller.output_stream(4), None);
    assert_ne!(fake.read32(GCTL) & gctl::CRST, 0);
    assert_eq!(fake.read8(CORBCTL) & ring::CORB_RUN, ring::CORB_RUN);
    assert_eq!(fake.read8(RIRBCTL) & ring::RIRB_DMAEN, ring::RIRB_DMAEN);
    // A parameter round trip, then enough to wrap both rings twice.
    assert_eq!(controller.param(0, 1, param::FUNCTION_TYPE), Ok(1));
    for round in 0..(2 * u32::from(ENTRIES) + 7) {
        let want = if round % 2 == 0 { 1 } else { 2 << 16 | 2 };
        let id = if round % 2 == 0 {
            param::FUNCTION_TYPE
        } else {
            param::NODE_COUNT
        };
        assert_eq!(controller.param(0, 1, id), Ok(want), "round {round}");
    }
}

#[test]
fn the_first_present_codec_is_used() {
    let memory = Memory::new(RING_BYTES);
    let fake = Fake::new(&memory, qemu_output());
    fake.0.borrow_mut().present = 0b0100;
    assert_eq!(up(&fake, &memory).unwrap().codec, 2);
    fake.0.borrow_mut().present = 0;
    assert_eq!(up(&fake, &memory).err(), Some(ControllerError::NoCodec));
}

#[test]
fn unsolicited_responses_are_skipped() {
    let memory = Memory::new(RING_BYTES);
    let fake = Fake::new(&memory, qemu_output());
    let mut controller = up(&fake, &memory).unwrap();
    fake.0.borrow_mut().unsolicited = true;
    for _ in 0..300 {
        assert_eq!(controller.param(0, 1, param::FUNCTION_TYPE), Ok(1));
    }
}

#[test]
fn a_dead_link_times_out() {
    let memory = Memory::new(RING_BYTES);
    let fake = Fake::new(&memory, qemu_output());
    let mut controller = up(&fake, &memory).unwrap();
    fake.0.borrow_mut().silent = true;
    assert_eq!(
        controller.param(0, 1, param::FUNCTION_TYPE),
        Err(VerbError::Timeout)
    );
    // The link comes back: the stale slot does not confuse the next answer.
    fake.0.borrow_mut().silent = false;
    assert_eq!(controller.param(0, 1, param::FUNCTION_TYPE), Ok(1));
}

#[test]
fn a_controller_without_256_entry_rings_is_refused() {
    let memory = Memory::new(RING_BYTES);
    let fake = Fake::new(&memory, qemu_output());
    fake.0.borrow_mut().regs[CORBSIZE as usize] = 0x30;
    assert_eq!(up(&fake, &memory).err(), Some(ControllerError::RingSize));
}

#[test]
fn format_words() {
    assert_eq!(format::encode(48000, 16, 2), Some(0x0011));
    assert_eq!(format::encode(44100, 16, 2), Some(0x4011));
    assert_eq!(format::encode(96000, 16, 2), Some(0x0811));
    assert_eq!(format::encode(192000, 24, 2), Some(0x1831));
    assert_eq!(format::encode(8000, 16, 1), Some(0x0510));
    assert_eq!(format::encode(22050, 16, 2), Some(0x4111));
    assert_eq!(format::encode(11025, 8, 2), Some(0x4301));
    assert_eq!(format::encode(12345, 16, 2), None);
    assert_eq!(format::encode(48000, 12, 2), None);
    assert_eq!(format::encode(48000, 16, 0), None);
    assert_eq!(format::encode(48000, 16, 17), None);
    let qemu = 1 << 17 | 0x1FC;
    assert!(format::supports_rate(qemu, 48000) && format::supports_rate(qemu, 44100));
    assert!(!format::supports_rate(qemu, 8000) && !format::supports_rate(qemu, 192000));
    assert!(format::supports_size(qemu, 16) && !format::supports_size(qemu, 24));
}

#[test]
fn the_cursor_counts_periods_across_wraps() {
    let mut cursor = Cursor::new(1000, 4).unwrap();
    assert_eq!(cursor.advance(500), 0);
    assert_eq!(cursor.advance(1500), 1);
    assert_eq!(cursor.next_slot(), 1);
    assert_eq!(cursor.advance(3999), 2);
    // Wrapped past the end: periods 3 (slot 3) done.
    assert_eq!(cursor.advance(100), 1);
    assert_eq!(cursor.next_slot(), 0);
    assert_eq!(cursor.completed(), 4);
    // A position outside the buffer is ignored, not trusted.
    assert_eq!(cursor.advance(4000), 0);
    assert_eq!(cursor.advance(u32::MAX), 0);
    assert_eq!(cursor.advance(1100), 1);
    assert!(Cursor::new(0, 4).is_none() && Cursor::new(1000, 0).is_none());
    assert!(Cursor::new(u32::MAX, 4).is_none());
}

#[test]
fn a_stream_is_reset_programmed_and_run() {
    let memory = Memory::new(RING_BYTES + 4096);
    let fake = Fake::new(&memory, qemu_output());
    let mut controller = up(&fake, &memory).unwrap();
    let stream = OutStream {
        base: controller.output_stream(0).unwrap(),
    };
    let regs = controller.regs_mut();
    assert!(stream.reset(regs, nap));
    let bdl = crate::fake::BUS + RING_BYTES as u64;
    // SAFETY: the BDL area lies inside the test memory and nothing reads it.
    unsafe { write_bdl(memory.va.add(RING_BYTES), 0x6000_0000, 2048, 4) };
    stream.program(regs, 1, bdl, 4 * 2048, 4, 0x0011);
    assert_eq!(regs.read32(OUT0 + sd::BDPL), bdl as u32);
    assert_eq!(regs.read32(OUT0 + sd::CBL), 8192);
    assert_eq!(regs.read16(OUT0 + sd::LVI), 3);
    assert_eq!(regs.read16(OUT0 + sd::FMT), 0x0011);
    assert_eq!(regs.read8(OUT0 + sd::CTL + 2), 0x10, "stream tag 1");
    // SAFETY: inside the test memory.
    let entry = unsafe { core::slice::from_raw_parts(memory.va.add(RING_BYTES + BDL_ENTRY), 16) };
    assert_eq!(
        entry,
        [0x00, 0x08, 0, 0x60, 0, 0, 0, 0, 0, 8, 0, 0, 1, 0, 0, 0]
    );
    assert!(stream.run(regs, true, nap));
    fake.play(3000);
    assert_eq!(stream.position(regs), 3000);
    assert_eq!(stream.take_status(regs), sdsts::BCIS);
    assert_eq!(stream.take_status(regs), 0, "status is write-one-to-clear");
    assert!(stream.run(regs, false, nap));
    fake.play(100);
    assert_eq!(
        stream.position(regs),
        3000,
        "a stopped stream does not move"
    );
}
