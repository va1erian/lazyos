//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/nvme.rs`), so a crash found by one replays under the
//! other.
//!
//! [`run`] reads a mode byte, then:
//!
//! * `0`: the rest is an Identify Controller page (zero padded);
//! * `1`: the rest is an Identify Namespace page;
//! * `2`: the rest is a stream of completion entries;
//! * `3`: the rest scripts a vectored transfer for the PRP planner;
//! * otherwise: the rest is everything a hostile controller answers
//!   (register reads and memory the controller "wrote"), and the driver
//!   brings it up, transfers and shuts it down.
//!
//! Nothing may panic or hang, and whatever is accepted must be consistent.

use std::cell::Cell;
use std::vec::Vec;

use crate::cmd::{Completion, CQE_BYTES};
use crate::identify::{ControllerInfo, Namespace, PAGE_BYTES};
use crate::prp::{self, Cursor};
use crate::{Controller, Op, Pages, Platform, MAX_INFLIGHT, PAGE};

pub fn run(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode {
        0 => identify_controller(rest),
        1 => identify_namespace(rest),
        2 => completions(rest),
        3 => plan(rest),
        _ => hostile(rest),
    }
}

fn page_of(rest: &[u8]) -> Vec<u8> {
    let mut page = rest.to_vec();
    page.resize(PAGE_BYTES, 0);
    page
}

fn identify_controller(rest: &[u8]) {
    let page = page_of(rest);
    let info = ControllerInfo::parse(&page).expect("a full page parses");
    for text in [
        info.serial.as_str(),
        info.model.as_str(),
        info.firmware.as_str(),
    ] {
        assert!(text.bytes().all(|byte| (0x20..0x7F).contains(&byte)));
    }
    if let Some(bytes) = info.max_transfer(12) {
        assert!(bytes >= 4096 && bytes.is_power_of_two());
    }
}

fn identify_namespace(rest: &[u8]) {
    if let Ok(namespace) = Namespace::parse(&page_of(rest)) {
        assert!(namespace.blocks > 0);
        assert!((512..=65536).contains(&namespace.block_bytes));
        assert!(namespace.block_bytes.is_power_of_two());
        let _ = namespace.bytes();
    }
}

fn completions(rest: &[u8]) {
    for raw in rest.as_chunks::<CQE_BYTES>().0 {
        let entry = Completion::decode(raw);
        assert!(entry.sct < 8);
        let again = Completion::decode(&entry.encode());
        assert_eq!(again.cid, entry.cid);
        assert_eq!(again.phase, entry.phase);
        assert_eq!(again.ok(), entry.ok());
    }
}

/// Segments and a page map from the bytes; plan the whole transfer and
/// check every command.
fn plan(rest: &[u8]) {
    let mut bytes = rest.iter().copied();
    let mut next = || bytes.next().unwrap_or(0);
    let block = 512usize << (next() % 4);
    let max = block * (1 + usize::from(next() % 64));
    let mut segments = Vec::new();
    let mut virt = 0x1000_0000u64;
    for _ in 0..(1 + next() % 8) {
        let offset = u64::from(u16::from_le_bytes([next(), next()]) % 4096) & !3;
        let len = if next() & 1 == 0 {
            block * (1 + usize::from(next() % 16))
        } else {
            usize::from(u16::from_le_bytes([next(), next()]) % 20000)
        };
        segments.push((virt + offset, len));
        virt += 0x100_0000;
    }
    // Scatter pages: phys page = virt page scrambled, sometimes contiguous.
    let scramble = u64::from(next());
    let translate = move |virt: u64| {
        let page = virt / PAGE;
        let phys_page = if scramble & 1 == 0 {
            page
        } else {
            page.wrapping_mul(2654435761) % (1 << 30)
        };
        Some(phys_page * PAGE + virt % PAGE)
    };
    let total: usize = segments.iter().map(|&(_, len)| len).sum();
    let mut cursor = Cursor::default();
    cursor.advance(&segments, 0);
    let mut done = 0usize;
    for _ in 0..10_000 {
        if done >= total {
            break;
        }
        match prp::plan(&segments, cursor, max, block, &translate) {
            Ok(plan) => {
                assert!(prp::valid(&plan), "invalid plan {plan:?}");
                assert!(plan.bytes > 0 && plan.bytes <= max && plan.bytes % block == 0);
                cursor.advance(&segments, plan.bytes);
                done += plan.bytes;
            }
            Err(_) => return,
        }
    }
    assert!(done <= total);
}

/// A controller whose every answer comes from the fuzz input.
struct Hostile<'a> {
    data: &'a [u8],
    at: Cell<usize>,
    clock: Cell<u64>,
}

impl Hostile<'_> {
    fn byte(&self) -> u8 {
        let at = self.at.get();
        self.at.set(at + 1);
        if self.data.is_empty() {
            0xFF
        } else {
            self.data[at % self.data.len()] ^ (at / self.data.len()) as u8
        }
    }
}

impl Platform for Hostile<'_> {
    fn read32(&self, _offset: usize) -> u32 {
        u32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }
    fn write32(&self, _offset: usize, _value: u32) {}
    fn read_mem(&self, _phys: u64, buf: &mut [u8]) {
        for byte in buf.iter_mut() {
            *byte = self.byte();
        }
    }
    fn write_mem(&self, _phys: u64, _data: &[u8]) {}
    fn now_ns(&self) -> u64 {
        let now = self.clock.get() + 250_000_000;
        self.clock.set(now);
        now
    }
}

fn hostile(rest: &[u8]) {
    let platform = Hostile {
        data: rest,
        at: Cell::new(0),
        clock: Cell::new(0),
    };
    let pages = Pages {
        admin_sq: 0x1000,
        admin_cq: 0x2000,
        io_sq: 0x3000,
        io_cq: 0x4000,
        identify: 0x5000,
        prp_lists: core::array::from_fn::<u64, MAX_INFLIGHT, _>(|index| {
            0x10_000 + index as u64 * PAGE
        }),
    };
    let Ok(mut controller) = Controller::init(&platform, pages) else {
        return;
    };
    assert!(controller.namespace.blocks > 0);
    let segments = [(0x100_0000u64, 8192usize)];
    let mut polls = 0;
    let mut wait = |ready: &dyn Fn() -> bool| {
        polls += 1;
        polls < 64 && ready()
    };
    let identity = |virt: u64| Some(virt);
    let _ = controller.transfer(&platform, Op::Read, 0, &segments, &identity, &mut wait);
    let _ = controller.flush(&platform, &mut wait);
    let _ = controller.shutdown(&platform);
}
