//! `gen9` against the Kaby Lake box's registers as its UEFI GOP left them,
//! read over `dbgd` (`docs/compat/kabylake/gpu/lazyos/`), where `tests_adopt`
//! has Linux's state: the firmware's own mode, 2560x1440 CVT-RB at 59.95 Hz
//! on pipe A, DDI B (HDMI) from DPLL1, CDCLK 675 MHz, a linear plane.

use crate::adopt::{self, Display};
use crate::regs::{plane_ctl, DdiMode};
use crate::tests_d13::capture_mmio;
use alloc::vec::Vec;

const REGS: &str = include_str!("../../../docs/compat/kabylake/gpu/lazyos/02-mmio-gop.txt");
const GGTT: &str = include_str!("../../../docs/compat/kabylake/gpu/lazyos/03-ggtt-scanout.txt");

#[test]
fn adopts_what_the_gop_left() {
    let mut mmio = capture_mmio(&[REGS, GGTT]);
    let file = Display::Gen9.read(&mut mmio);
    let adopted = Display::Gen9.decode(&file);
    assert_eq!(adopted.display, Display::Gen9);
    assert_eq!(adopted.pipes_enabled, 0b001);
    assert_eq!(adopted.cdclk_khz, 675_000, "not Linux's 337.5 MHz");
    assert!(adopted.cdclk_pll_locked && adopted.power_well_1 && adopted.power_well_2);

    let pipe = adopted.active.expect("pipe A is running");
    assert_eq!((pipe.pipe, pipe.port, pipe.pll), (0, Some(1), Some(1)));
    assert_eq!(pipe.func.mode, DdiMode::Hdmi);
    assert!(pipe.pll_ready && !pipe.ddi_clock_off);
    assert_eq!(pipe.pixel_clock_khz, Some(241_500));
    assert_eq!(pipe.refresh_millihz(), Some(59_950));
    assert_eq!(
        (pipe.h.active, pipe.h.total, pipe.v.active, pipe.v.total),
        (2560, 2720, 1440, 1481)
    );
    assert!(pipe.dip_avi() && !pipe.dip_gcp() && !pipe.dip_spd());

    let plane = pipe.plane;
    assert!(plane.enabled);
    assert_eq!(plane.format, plane_ctl::Format::Xrgb8888);
    assert_eq!(plane.tiling, plane_ctl::Tiling::Linear);
    assert_eq!(
        (plane.stride_bytes, plane.width, plane.height),
        (10_240, 2560, 1440)
    );
    assert_eq!(plane.surf, 0);
    let map = adopt::read_fb_map(&mut mmio, plane.surf, plane.bytes());
    assert_eq!(
        (map.pages, map.present, map.first),
        (3600, 3600, 0xBE00_0000)
    );
    assert!(map.contiguous);
}

#[test]
fn reading_the_gop_state_writes_nothing_and_stays_allowed() {
    let mut mmio = capture_mmio(&[REGS, GGTT]);
    let _ = Display::Gen9.read(&mut mmio);
    let _ = crate::vblank::Beam::read(&mut mmio, 0);
    let _ = crate::vblank::PipeIrq::read(&mut mmio, 0);
    assert!(mmio.log.iter().all(|&(_, _, write)| !write));
    let ranges: Vec<_> = pciprobe::builtin::RANGES
        .iter()
        .filter(|r| (r.vendor, r.device, r.bar) == (0x8086, 0x5916, 0))
        .collect();
    for &(offset, _, _) in &mmio.log {
        let offset = u64::from(offset);
        assert!(
            ranges
                .iter()
                .any(|r| r.start <= offset && offset + 4 <= r.end),
            "{offset:#x} is outside the HD 620 allowlist"
        );
    }
}
