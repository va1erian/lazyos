//! `adopt` against the box's real register dump (`01-intel_reg-dump.txt`,
//! Linux's state: 2560x1440 at 70 Hz on DDI B, pipe A) and against
//! hostile register values.

use crate::adopt::{self, Adopted, RegFile, GGTT_OFFSET};
use crate::mmio::FakeMmio;
use crate::regs::{plane_ctl, DdiMode};
use alloc::vec::Vec;
use std::string::String;

const DUMP: &str = include_str!("../../../docs/compat/kabylake/gpu/linux/01-intel_reg-dump.txt");

/// A register file holding every `NAME (0xOFFSET): 0xVALUE` line of the dump.
fn dump_mmio() -> FakeMmio {
    let mut mmio = FakeMmio::default();
    for line in DUMP.lines() {
        let Some((_, rest)) = line.split_once("(0x") else {
            continue;
        };
        let Some((offset, rest)) = rest.split_once("): 0x") else {
            continue;
        };
        let value: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
        if let (Ok(offset), Ok(value)) = (
            u32::from_str_radix(offset, 16),
            u32::from_str_radix(&value, 16),
        ) {
            mmio.regs.insert(offset, value);
        }
    }
    mmio
}

#[test]
fn the_dump_fixture_parses() {
    let mmio = dump_mmio();
    assert_eq!(mmio.regs[&0x4_6000], 0x0800_02A1);
    assert_eq!(mmio.regs[&0x6_0400], 0x9003_0000);
    assert!(mmio.regs.len() > 300);
}

#[test]
fn reads_the_boxs_display_state() {
    let mut mmio = dump_mmio();
    let file = RegFile::read(&mut mmio);
    let adopted = Adopted::decode(&file);
    assert_eq!(adopted.pipes_enabled, 0b001, "only pipe A runs");
    assert!(!adopted.edp_enabled);
    assert_eq!(adopted.cdclk_khz, 337_500);
    assert!(adopted.cdclk_pll_locked);
    assert!(adopted.power_well_1 && adopted.power_well_2);
    assert_eq!(adopted.dc_state_en, 0);

    let pipe = adopted.active.expect("pipe A is running");
    assert_eq!(pipe.pipe, 0);
    assert_eq!(pipe.port, Some(1), "DDI B");
    assert_eq!(pipe.func.mode, DdiMode::Hdmi);
    assert!(pipe.hdmi_link());
    assert_eq!(pipe.func.bits_per_component, Some(8));
    assert_eq!(pipe.pll, Some(1));
    assert!(pipe.pll_hdmi_mode);
    assert!(!pipe.ddi_clock_off);
    assert_eq!(pipe.pixel_clock_khz, Some(277_820));
    assert_eq!(pipe.refresh_millihz(), Some(69_927));
    assert_eq!((pipe.h.active, pipe.h.total), (2560, 2670));
    assert_eq!((pipe.h.sync_start, pipe.h.sync_end), (2583, 2615));
    assert_eq!((pipe.v.active, pipe.v.total), (1440, 1488));
    assert_eq!((pipe.v.sync_start, pipe.v.sync_end), (1474, 1482));
    assert!(pipe.func.hsync_positive && pipe.func.vsync_positive);
    // PIPESRC is not in the dump (reads 0): not believed.
    assert_eq!(pipe.source, None);
    assert!(pipe.dip_gcp() && pipe.dip_avi() && pipe.dip_spd());

    let plane = pipe.plane;
    assert!(plane.enabled);
    assert_eq!(plane.format, plane_ctl::Format::Xrgb8888);
    assert_eq!(plane.tiling, plane_ctl::Tiling::Y);
    assert_eq!(plane.stride_bytes, 10_240);
    assert_eq!((plane.width, plane.height), (2560, 1440));
    assert_eq!((plane.x, plane.y), (0, 0));
    assert_eq!(plane.surf, 0x0110_0000);
    // Y tiles are 32 lines: 1440 is 45 whole tile rows.
    assert_eq!(plane.bytes(), 10_240 * 1440);
}

#[test]
fn reading_writes_nothing_and_stays_inside_the_dbgd_allowlist() {
    let mut mmio = dump_mmio();
    let before = mmio.regs.clone();
    let file = RegFile::read(&mut mmio);
    let _ = adopt::read_fb_map(&mut mmio, 0x0110_0000, 2560 * 4 * 1440);
    assert!(mmio.log.iter().all(|&(_, _, write)| !write), "a write");
    assert_eq!(mmio.regs, before);
    let ranges: Vec<_> = pciprobe::builtin::RANGES
        .iter()
        .filter(|r| (r.vendor, r.device, r.bar) == (0x8086, 0x5916, 0))
        .collect();
    for &(offset, _, _) in &mmio.log {
        assert_eq!(offset % 4, 0, "{offset:#x} is not dword aligned");
        let offset = u64::from(offset);
        assert!(
            ranges.iter().any(|r| r.start <= offset && offset < r.end),
            "{offset:#x} is outside the HD 620 allowlist"
        );
    }
    // Every named register was read once, in a row without duplicates.
    let mut offsets: Vec<u32> = file.regs.iter().map(|r| r.offset).collect();
    offsets.sort_unstable();
    offsets.dedup();
    assert_eq!(offsets.len(), file.regs.len());
}

#[test]
fn a_framebuffer_in_the_ggtt_is_mapped_page_by_page() {
    let mut mmio = FakeMmio::default();
    let surf = 0x0110_0000u32;
    // 3 pages at stolen memory 0xBE000000, contiguous, present.
    for i in 0..3u64 {
        let at = GGTT_OFFSET + (surf >> 12) * 8 + i as u32 * 8;
        mmio.regs.insert(at, (0xBE00_0000u64 + i * 4096) as u32 | 3);
    }
    let map = adopt::read_fb_map(&mut mmio, surf, 3 * 4096);
    assert_eq!((map.pages, map.present), (3, 3));
    assert_eq!((map.first, map.last), (0xBE00_0000, 0xBE00_2000));
    assert!(map.contiguous);

    // A hole in the middle: not contiguous, and counted as missing.
    mmio.regs.remove(&(GGTT_OFFSET + (surf >> 12) * 8 + 8));
    let map = adopt::read_fb_map(&mut mmio, surf, 3 * 4096);
    assert_eq!(map.present, 2);
    assert!(!map.contiguous);

    // An entry above 4 GiB keeps its high dword (bits 38:12).
    let at = GGTT_OFFSET + (surf >> 12) * 8;
    mmio.regs.insert(at, 0x0000_1003);
    mmio.regs.insert(at + 4, 0x7);
    let map = adopt::read_fb_map(&mut mmio, surf, 4096);
    assert_eq!(map.first, 0x7_0000_1000);
}

#[test]
fn hostile_values_do_not_panic_or_overrun() {
    // Out of the table: nothing is read.
    let mut mmio = FakeMmio::default();
    let map = adopt::read_fb_map(&mut mmio, !0xFFF, 1 << 40);
    assert!(!map.contiguous && mmio.log.is_empty());
    let map = adopt::read_fb_map(&mut mmio, 0, 0);
    assert!(!map.contiguous && mmio.log.is_empty());
    // A huge surface is walked only to the cap.
    let map = adopt::read_fb_map(&mut mmio, 0, u64::MAX);
    assert_eq!(map.pages, adopt::MAX_FB_PAGES);
    assert_eq!(mmio.log.len(), 2 * adopt::MAX_FB_PAGES as usize);

    // A bus that answers all ones everywhere is "dead", and decoding it
    // still gives a value, not a panic.
    struct Ones;
    impl crate::Mmio for Ones {
        fn read32(&mut self, _: u32) -> u32 {
            u32::MAX
        }
        fn write32(&mut self, _: u32, _: u32) {
            panic!("a write");
        }
    }
    let file = RegFile::read(&mut Ones);
    assert!(file.looks_dead());
    // Nothing past the always-on registers is read from a dead bus.
    assert!(file.regs.len() < 20, "{} registers", file.regs.len());
    let adopted = Adopted::decode(&file);
    assert_eq!(adopted.pipes_enabled, 0);
    assert_eq!(adopted.active, None);

    // The same on a quiet device: nothing is running.
    let file = RegFile::read(&mut FakeMmio::default());
    assert!(!file.looks_dead());
    let adopted = Adopted::decode(&file);
    assert_eq!(adopted.active, None);
    assert_eq!(adopted.pipes_enabled, 0);
}

#[test]
fn blocks_behind_a_downed_well_are_not_read() {
    use crate::regs::{self, pwr_well};
    let mut mmio = dump_mmio();
    // Well 1 only: pipe A and its DDI answer, pipes B and C and the audio
    // block (well 2) are not touched.
    mmio.regs
        .insert(regs::PWR_WELL_CTL_DRIVER, pwr_well::PW1_STATE | 0x1);
    let file = RegFile::read(&mut mmio);
    for gated in ["PIPE_B", "PIPE_C", "AUDIO", "DDI_BUF_CTL_B"] {
        assert!(file.gated.iter().any(|g| g == gated), "{gated} was read");
    }
    assert!(!file.gated.iter().any(|g| g == "PIPE_A"));
    for &(offset, _, _) in &mmio.log {
        assert!(
            !(0x7_1000..0x7_3000).contains(&offset) && !(0x6_5000..0x6_5200).contains(&offset),
            "{offset:#x} is behind well 2"
        );
    }
    assert_eq!(Adopted::decode(&file).active.map(|p| p.pipe), Some(0));
    // No well up: only the global registers.
    mmio.regs.insert(regs::PWR_WELL_CTL_DRIVER, 0);
    let file = RegFile::read(&mut mmio);
    assert!(file.gated.iter().any(|g| g == "PIPE_A"));
    assert_eq!(Adopted::decode(&file).active, None);
}
