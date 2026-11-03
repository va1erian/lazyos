//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/btfw.rs`).
//!
//! [`run`] uses the bytes two ways:
//!
//! * as a firmware or controller-answer blob handed to every container and
//!   record parser (Intel image in all three layouts, version TLV, legacy
//!   record, boot parameters, DDC file, Realtek epatch): none may panic, and
//!   what parses must be walkable to the end;
//! * as a script for a loader: byte 0 picks the vendor (even Intel, odd
//!   Realtek), then records `tag:u8, length:u16 big endian, bytes` that become
//!   inputs: tag 0 an event packet, 1 a file, 2 "no file", 3 a tick (the
//!   first byte of the record times 100 ms later), 4 a detach. Every command a
//!   loader sends must be well formed, and a finished loader must stay quiet.

use std::vec::Vec;

use crate::intel::ddc;
use crate::intel::sfi::{Layout, Sfi};
use crate::intel::version::{parse_boot_params, parse_legacy, parse_tlv};
use crate::realtek::epatch;
use crate::{Input, Loader, Step, Vendor};

pub fn run(data: &[u8]) {
    parse_blobs(data);
    let _ = run_script(data);
}

fn parse_blobs(data: &[u8]) {
    for layout in [
        Layout::Rsa,
        Layout::Both { sbe_type: 0 },
        Layout::Both { sbe_type: 1 },
    ] {
        if let Ok(sfi) = Sfi::parse(data.to_vec(), layout) {
            let mut pos = sfi.body_start();
            while let Some(end) = sfi.next_group(pos) {
                assert!(end > pos);
                pos = end;
            }
            assert_eq!(pos, data.len(), "the groups tile the command stream");
        }
    }
    let _ = parse_tlv(data);
    let _ = parse_legacy(data);
    let _ = parse_boot_params(data);
    let _ = ddc::validate(data);
    for (lmp, rom, key) in [(0x8761, 0, 0), (0x8822, 3, 0), (0x8852, 1, 5)] {
        if let Ok(patch) = epatch::build(data, lmp, rom, key) {
            assert!(
                patch.len() <= data.len(),
                "a patch is never larger than its file"
            );
        }
    }
}

/// Cut `data` into `(tag, bytes)` records, stopping at a short one.
fn records(mut data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    while let [tag, hi, lo, rest @ ..] = data {
        let len = usize::from(u16::from_be_bytes([*hi, *lo]));
        if rest.len() < len || out.len() >= 4096 {
            break;
        }
        out.push((*tag, &rest[..len]));
        data = &rest[len..];
    }
    out
}

/// Play a script; the terminal step (`Done` or `Failed`) if it reached one.
pub fn run_script(data: &[u8]) -> Option<Step> {
    let (&selector, rest) = data.split_first()?;
    let mut loader = Loader::new(if selector % 2 == 0 {
        Vendor::Intel
    } else {
        Vendor::Realtek
    });
    let mut now = 0u64;
    let mut step = loader.step(now, Input::Start);
    check(&step);
    for (tag, bytes) in records(rest) {
        if finished(&step) {
            break;
        }
        let input = match tag % 5 {
            0 => Input::Event(bytes),
            1 => Input::File(bytes.to_vec()),
            2 => Input::NoFile,
            3 => {
                now += 100 * u64::from(bytes.first().copied().unwrap_or(0));
                Input::Tick
            }
            _ => Input::Detached,
        };
        step = loader.step(now, input);
        check(&step);
    }
    if !finished(&step) {
        return None;
    }
    // Nothing after the end changes it.
    assert_eq!(loader.step(now, Input::Detached), Step::Wait);
    assert_eq!(loader.step(now, Input::Tick), Step::Wait);
    Some(step)
}

fn finished(step: &Step) -> bool {
    matches!(step, Step::Done(_) | Step::Failed(_))
}

/// A command a loader sends is a whole HCI command packet.
fn check(step: &Step) {
    if let Step::Send(packet) = step {
        assert!(packet.len() >= 3);
        assert_eq!(usize::from(packet[2]), packet.len() - 3);
    }
}
