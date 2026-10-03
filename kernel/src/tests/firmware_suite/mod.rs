//! Firmware robustness (H1 of `docs/real-pc-boot-plan.md`): legacy-device
//! presence probes, the boot-log ring, the logical screen and framebuffer
//! clipping, and the on-screen panic report. The memory-map and device-table
//! parts live in `mem_suite::regions` and `dev_suite::capacity`.

use super::*;

mod probes;
mod screen;

use probes::*;
use screen::*;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "fw_ata_floating_bus_is_absent_at_once",
        ata_floating_bus_is_absent_at_once,
    ),
    ("fw_ata_waits_are_bounded", ata_waits_are_bounded),
    ("fw_ata_probe_soak", ata_probe_soak),
    ("fw_i8042_probe_cases", i8042_probe_cases),
    ("fw_com1_probe_cases", com1_probe_cases),
    ("fw_syslog_reads_the_ring", syslog_reads_the_ring),
    ("fw_logical_fit_cases", logical_fit_cases),
    ("fw_view_blits_stay_inside", view_blits_stay_inside),
    (
        "fw_framebuffer_geometry_sanitized",
        framebuffer_geometry_sanitized,
    ),
    ("fw_klog_ring_wraps_and_tails", klog_ring_wraps_and_tails),
    ("fw_klog_soak_live_ring", klog_soak_live_ring),
    ("fw_panic_report_renders", panic_report_renders),
    (
        "fw_framebuffer_is_write_combining",
        framebuffer_is_write_combining,
    ),
];
