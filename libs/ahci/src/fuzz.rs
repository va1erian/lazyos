//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/ahci.rs`), so a crash found by one replays under the
//! other.
//!
//! [`run`] reads a mode byte, then:
//!
//! * `0`: the rest is an IDENTIFY DEVICE page (zero padded);
//! * `1`: the rest scripts a vectored transfer for the PRDT planner;
//! * otherwise: the rest is everything a hostile HBA answers (every
//!   register read and every byte of memory the "HBA wrote"), and the driver
//!   brings up the controller and its ports, transfers, flushes and shuts
//!   down.
//!
//! Nothing may panic or hang, and whatever is accepted must be consistent.

use std::cell::Cell;
use std::vec::Vec;

use crate::cmd::{self, Cursor, MAX_PRD};
use crate::identify::{Disk, IDENTIFY_BYTES};
use crate::port::PortPages;
use crate::{Hba, Op, Platform, MAX_SLOTS};

pub fn run(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode {
        0 => identify(rest),
        1 => plan(rest),
        _ => hostile(rest),
    }
}

fn identify(rest: &[u8]) {
    let mut page = [0u8; IDENTIFY_BYTES];
    let len = rest.len().min(IDENTIFY_BYTES);
    page[..len].copy_from_slice(&rest[..len]);
    if let Ok(disk) = Disk::parse(&page) {
        assert!(disk.sectors > 0 && disk.sectors < 1 << 48);
        assert!(disk.physical_bytes >= 512);
        for text in [
            disk.model.as_str(),
            disk.serial.as_str(),
            disk.firmware.as_str(),
        ] {
            assert!(text.bytes().all(|byte| (0x20..0x7F).contains(&byte)));
        }
        let _ = disk.bytes();
    }
}

/// Segments and a page map from the bytes; plan the whole transfer and
/// check every command.
fn plan(rest: &[u8]) {
    let mut bytes = rest.iter().copied();
    let mut next = || bytes.next().unwrap_or(0);
    let max = 512 * (1 + usize::from(next() % 64));
    let s64a = next() & 1 == 0;
    let mut segments = Vec::new();
    let mut virt = 0x1000_0000u64;
    for _ in 0..(1 + next() % 8) {
        let offset = u64::from(u16::from_le_bytes([next(), next()]) % 4096) & !1;
        let len = if next() & 1 == 0 {
            512 * (1 + usize::from(next() % 16))
        } else {
            usize::from(u16::from_le_bytes([next(), next()]) % 20000)
        };
        segments.push((virt + offset, len));
        virt += 0x100_0000;
    }
    let scramble = u64::from(next());
    let translate = move |virt: u64| {
        let page = virt / 4096;
        let phys_page = match scramble % 3 {
            0 => page,
            1 => page.wrapping_mul(2654435761) % (1 << 30),
            _ => page + (1 << 20),
        };
        Some(phys_page * 4096 + virt % 4096)
    };
    let total: usize = segments.iter().map(|&(_, len)| len).sum();
    let mut cursor = Cursor::default();
    cursor.advance(&segments, 0);
    let mut done = 0usize;
    for _ in 0..10_000 {
        if done >= total {
            break;
        }
        match cmd::plan(&segments, cursor, max, 512, s64a, &translate) {
            Ok(plan) => {
                assert!(plan.count > 0 && plan.count <= MAX_PRD);
                assert!(plan.bytes > 0 && plan.bytes <= max && plan.bytes % 512 == 0);
                let sum: usize = plan.entries().iter().map(|prd| prd.bytes as usize).sum();
                assert_eq!(sum, plan.bytes);
                for prd in plan.entries() {
                    assert!(prd.bytes >= 2 && prd.bytes % 2 == 0);
                    assert!(prd.addr % 2 == 0);
                    assert!(prd.bytes as usize <= cmd::MAX_PRD_BYTES);
                    if !s64a {
                        assert!(prd.addr + u64::from(prd.bytes) <= 1 << 32);
                    }
                }
                cursor.advance(&segments, plan.bytes);
                done += plan.bytes;
            }
            Err(_) => return,
        }
    }
    assert!(done <= total);
}

/// An HBA whose every answer comes from the fuzz input.
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
        let now = self.clock.get() + 100_000_000;
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
    let Ok(hba) = Hba::init(&platform) else {
        return;
    };
    let identity = |virt: u64| Some(virt);
    for index in 0..4 {
        let pages = PortPages {
            list: 0x10_0000 + index as u64 * 0x20_0000,
            fis: 0x10_0400 + index as u64 * 0x20_0000,
            tables: core::array::from_fn::<u64, MAX_SLOTS, _>(|slot| {
                0x11_0000 + slot as u64 * 0x1000 + index as u64 * 0x20_0000
            }),
            identify: 0x18_0000 + index as u64 * 0x20_0000,
        };
        let Ok(mut port) = hba.open_port(&platform, index, pages) else {
            continue;
        };
        assert!(port.disk.sectors > 0);
        let segments = [(0x100_0000u64, 8192usize)];
        let mut polls = 0;
        let mut wait = |ready: &dyn Fn() -> bool| {
            polls += 1;
            polls < 64 && ready()
        };
        let _ = port.transfer(&platform, Op::Read, 0, &segments, &identity, &mut wait);
        let _ = port.transfer(&platform, Op::Write, 7, &segments, &identity, &mut wait);
        let _ = port.flush(&platform, &mut wait);
        let _ = port.shutdown(&platform, &mut wait);
    }
}
