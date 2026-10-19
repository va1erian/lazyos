//! `d13` against the Alder Lake box's own registers as its UEFI GOP left
//! them, read over `dbgd` (`docs/compat/alderlake/gpu/lazyos/`, decoded by
//! hand in `01-dbgd-capture.md`): 2560x1440 CVT-RB at 59.93 Hz on pipe A,
//! transcoder A, DDI B (HDMI), clocked by DPLL1.

use crate::adopt::{self, Display};
use crate::d13::{self, combo_port_clock_khz, pwr_well};
use crate::mmio::FakeMmio;
use crate::regs::{self, plane_ctl, DdiMode};
use alloc::vec::Vec;

const REGS: &str = include_str!("../../../docs/compat/alderlake/gpu/lazyos/02-mmio-gop.txt");
const GGTT: &str = include_str!("../../../docs/compat/alderlake/gpu/lazyos/03-ggtt-scanout.txt");

/// A register file holding every `0xOFFSET 0xVALUE` line of a `dbgd`
/// capture (the dumps leave zero dwords out: an absent offset reads 0, as it
/// did).
pub(crate) fn capture_mmio(files: &[&str]) -> FakeMmio {
    let mut mmio = FakeMmio::default();
    for line in files.iter().flat_map(|file| file.lines()) {
        let mut words = line.split_whitespace();
        let (Some(offset), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let parse = |word: &str| u32::from_str_radix(word.strip_prefix("0x")?, 16).ok();
        if let (Some(offset), Some(value)) = (parse(offset), parse(value)) {
            mmio.regs.insert(offset, value);
        }
    }
    mmio
}

fn gop_mmio() -> FakeMmio {
    capture_mmio(&[REGS, GGTT])
}

#[test]
fn the_capture_parses() {
    let mmio = gop_mmio();
    assert_eq!(mmio.regs[&0x4_6000], 0x0038_0518);
    assert_eq!(mmio.regs[&0x6_0400], 0x9001_0100);
    assert_eq!(mmio.regs[&0x80_0000], 0x4C80_0001);
}

#[test]
fn adopts_what_the_gop_left() {
    let mut mmio = gop_mmio();
    let file = Display::D13.read(&mut mmio);
    let adopted = Display::D13.decode(&file);
    assert_eq!(adopted.display, Display::D13);
    assert_eq!(adopted.pipes_enabled, 0b0001, "only pipe A runs");
    assert!(!adopted.edp_enabled);
    assert_eq!(adopted.cdclk_khz, 652_800);
    assert!(adopted.cdclk_pll_locked);
    assert!(adopted.power_well_1 && adopted.power_well_2);
    assert_eq!(adopted.dc_state_en, 0);
    assert_eq!(file.gated, ["PIPE_D"], "PW_D is off");

    let pipe = adopted.active.expect("pipe A is running");
    assert_eq!(pipe.pipe, 0);
    assert_eq!(pipe.port, Some(1), "DDI B");
    assert_eq!(pipe.func.mode, DdiMode::Hdmi);
    assert!(pipe.hdmi_link());
    assert_eq!(pipe.func.bits_per_component, Some(8));
    assert!(
        pipe.func.hsync_positive && !pipe.func.vsync_positive,
        "CVT-RB: +H -V"
    );
    assert_eq!(pipe.pll, Some(1), "DPLL1, not DPLL0 as under Linux");
    assert!(pipe.pll_ready);
    assert!(!pipe.ddi_clock_off);
    assert_eq!(pipe.pixel_clock_khz, Some(241_436));
    assert_eq!(pipe.refresh_millihz(), Some(59_934));
    assert_eq!((pipe.h.active, pipe.h.total), (2560, 2720));
    assert_eq!((pipe.h.sync_start, pipe.h.sync_end), (2608, 2640));
    assert_eq!((pipe.v.active, pipe.v.total), (1440, 1481));
    assert_eq!((pipe.v.sync_start, pipe.v.sync_end), (1443, 1448));
    assert_eq!(pipe.source, Some((2560, 1440)));
    assert!(pipe.dip_avi() && !pipe.dip_gcp() && !pipe.dip_spd());

    let plane = pipe.plane;
    assert!(plane.enabled);
    assert_eq!(plane.format, plane_ctl::Format::Xrgb8888);
    assert_eq!(plane.tiling, plane_ctl::Tiling::Linear);
    assert_eq!(plane.stride_bytes, 10_240);
    assert_eq!((plane.width, plane.height), (2560, 1440));
    assert_eq!((plane.surf, plane.surf_live), (0, 0));
    assert_eq!(plane.bytes(), 10_240 * 1440);

    let map = adopt::read_fb_map(&mut mmio, plane.surf, plane.bytes());
    assert_eq!((map.pages, map.present), (3600, 3600));
    assert_eq!(map.first, 0x4C80_0000, "the start of stolen memory (BDSM)");
    assert_eq!(map.last, 0x4C80_0000 + 3599 * 4096);
    assert!(map.contiguous);
}

#[test]
fn reading_writes_nothing_and_stays_inside_the_dbgd_allowlist() {
    let mut mmio = gop_mmio();
    let before = mmio.regs.clone();
    let file = Display::D13.read(&mut mmio);
    let _ = adopt::read_fb_map(&mut mmio, 0, 10_240 * 1440);
    // The beam reads of `vblank` too.
    let _ = crate::vblank::Beam::read(&mut mmio, 0);
    let _ = crate::vblank::PipeIrq::read(&mut mmio, 0);
    assert!(mmio.log.iter().all(|&(_, _, write)| !write), "a write");
    assert_eq!(mmio.regs, before);
    let ranges: Vec<_> = pciprobe::builtin::RANGES
        .iter()
        .filter(|r| (r.vendor, r.device, r.bar) == (0x8086, 0x46B3, 0))
        .collect();
    for &(offset, _, _) in &mmio.log {
        assert_eq!(offset % 4, 0, "{offset:#x} is not dword aligned");
        let offset = u64::from(offset);
        assert!(
            ranges
                .iter()
                .any(|r| r.start <= offset && offset + 4 <= r.end),
            "{offset:#x} is outside the 46b3 allowlist"
        );
    }
    let mut offsets: Vec<u32> = file.regs.iter().map(|r| r.offset).collect();
    offsets.sort_unstable();
    offsets.dedup();
    assert_eq!(offsets.len(), file.regs.len(), "a register read twice");
}

#[test]
fn a_pipe_behind_a_downed_well_is_not_read() {
    let mut mmio = gop_mmio();
    // PW1 and PW2 up, no pipe well: nothing of pipes A to D is touched.
    mmio.regs.insert(
        regs::PWR_WELL_CTL_DRIVER,
        pwr_well::PW1_STATE | pwr_well::PW2_STATE,
    );
    let file = Display::D13.read(&mut mmio);
    assert_eq!(file.gated, ["PIPE_A", "PIPE_B", "PIPE_C", "PIPE_D"]);
    for &(offset, _, _) in &mmio.log {
        assert!(
            !(0x6_0000..0x6_4000).contains(&offset) && !(0x7_0000..0x7_4000).contains(&offset),
            "{offset:#x} is behind a pipe well"
        );
    }
    assert_eq!(Display::D13.decode(&file).active, None);
}

#[test]
fn a_dead_bus_reads_four_registers_and_decodes_to_nothing() {
    struct Ones;
    impl crate::Mmio for Ones {
        fn read32(&mut self, _: u32) -> u32 {
            u32::MAX
        }
        fn write32(&mut self, _: u32, _: u32) {
            panic!("a write");
        }
    }
    let file = Display::D13.read(&mut Ones);
    assert!(file.looks_dead());
    assert_eq!(file.regs.len(), 4);
    let adopted = Display::D13.decode(&file);
    assert_eq!((adopted.pipes_enabled, adopted.active), (0, None));
    // A quiet device: nothing runs, nothing panics.
    let file = Display::D13.read(&mut FakeMmio::default());
    assert_eq!(Display::D13.decode(&file).active, None);
}

#[test]
fn the_port_comes_from_the_tgl_encoding() {
    let mut mmio = gop_mmio();
    // Port A (1 in bits 30:27) reads as DDI A, which Gen9's field would
    // have called "no port".
    mmio.regs.insert(0x6_0400, 0x8801_0000);
    mmio.regs.insert(d13::DPCLKA_CFGCR0, 0);
    let pipe = Display::D13
        .decode(&Display::D13.read(&mut mmio))
        .active
        .unwrap();
    assert_eq!((pipe.port, pipe.pll), (Some(0), Some(0)));
    // DPLL0 is off: the clock is unknown, not invented.
    assert!(!pipe.pll_ready);
    assert_eq!(pipe.pixel_clock_khz, None);
    // No port in the function: TRANS_CLK_SEL (port + 1 in bits 31:28).
    mmio.regs.insert(0x6_0400, 0x8001_0000);
    let pipe = Display::D13
        .decode(&Display::D13.read(&mut mmio))
        .active
        .unwrap();
    assert_eq!(pipe.port, Some(1));
    // A Type-C port: its clock is not decoded.
    mmio.regs.insert(0x6_0400, 0xA001_0000);
    let pipe = Display::D13
        .decode(&Display::D13.read(&mut mmio))
        .active
        .unwrap();
    assert_eq!(
        (pipe.port, pipe.pll, pipe.pixel_clock_khz),
        (Some(3), None, None)
    );
}

#[test]
fn clock_arithmetic() {
    // The capture's DPLL1: DCO 19.2 MHz x (440 + 3840/32768), P 7.
    assert_eq!(
        combo_port_clock_khz(0x003C_01B8, 0x60, Some(38_400)),
        Some(241_436)
    );
    // Linux's DPLL0 on the same box (`i915_shared_dplls_info`): its 70 Hz
    // mode asks 277.82 MHz, and the dividers i915 chose give 277.79.
    assert_eq!(
        combo_port_clock_khz(0x0018_01B2, 0x488, Some(38_400)),
        Some(277_790)
    );
    // Reserved dividers and an unknown reference give nothing.
    assert_eq!(combo_port_clock_khz(0x003C_01B8, 0x0, Some(38_400)), None);
    assert_eq!(combo_port_clock_khz(0x003C_01B8, 0x60, None), None);
    assert_eq!(
        combo_port_clock_khz(0x003C_01B8, 0x260, Some(38_400)),
        None,
        "Q = 0"
    );
    assert_eq!(combo_port_clock_khz(0, 0x60, Some(24_000)), None);
    assert_eq!(combo_port_clock_khz(u32::MAX, u32::MAX, Some(38_400)), None);
    // CDCLK: ratio 34 x 38.4 MHz / 2; with the PLL off, the decimal field.
    assert_eq!(
        d13::cdclk_khz(0x0038_0518, 0xC000_0022, 0x4000_0000),
        652_800
    );
    assert_eq!(
        d13::cdclk_khz(0x00B8_0518, 0xC000_0022, 0x4000_0000),
        326_400,
        "CD2X /2"
    );
    assert_eq!(
        d13::cdclk_khz(0x0038_0518, 0x0000_0022, 0x4000_0000),
        653_000
    );
    assert_eq!(d13::ref_khz(0xE000_0000), None);
}
