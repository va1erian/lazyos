//! The tick source (docs/real-pc-boot-plan.md H2): the choice boot made, the
//! decisions behind it, the tick against the CMOS RTC, PIT-dead detection
//! under load, and device interrupts through the 8259 in virtual-wire mode.
//!
//! Every test runs on whichever source boot chose, so the suite proves both
//! when run twice: a plain image (the PIT under QEMU) and one built with
//! `LAZYOS_TIMER=lapic` (or booted with `-machine pit=off`, which takes the
//! APIC path by detection).

use super::*;
use crate::arch::timer_cal::{self, PitVerdict};
use crate::arch::{lapic, pic, timer};

mod delivery;
mod rtc;

pub(super) const CASES: &[(&str, Test)] = &[
    ("timer_boot_choice", boot_choice),
    ("timer_pit_verdicts", pit_verdicts),
    ("timer_cpuid_crystal", cpuid_crystal),
    ("timer_lapic_count_bounds", lapic_count_bounds),
    ("timer_tick_matches_rtc", rtc::tick_matches_rtc),
    ("timer_wallclock_follows_rtc", rtc::wallclock_follows_rtc),
    (
        "timer_pit_probe_no_false_trigger",
        delivery::pit_probe_no_false_trigger,
    ),
    ("timer_mask_stops_the_tick", delivery::mask_stops_the_tick),
    ("timer_mask_unmask_soak", delivery::mask_unmask_soak),
    (
        "timer_keyboard_irq_through_pic",
        delivery::keyboard_irq_through_pic,
    ),
    (
        "timer_mouse_irq_through_cascade",
        delivery::mouse_irq_through_cascade,
    ),
];

/// A lone runnable kernel task, so a real tick resumes the test.
fn kernel_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `body` with interrupts on and only the PIC lines in `lines` unmasked
/// (line 0 is the tick, whatever its source), restoring every mask and
/// `IF=0` afterwards.
fn with_lines<T>(lines: &[u8], body: impl FnOnce() -> T) -> T {
    let saved: [bool; 16] = core::array::from_fn(|l| pic::is_masked(l as u8));
    for line in 0..16u8 {
        // The cascade must stay open for a slave line to reach the CPU.
        let open = lines.contains(&line) || (line == 2 && lines.iter().any(|&l| l >= 8));
        pic::set_masked(line, !open);
    }
    x86_64::instructions::interrupts::enable();
    let result = body();
    x86_64::instructions::interrupts::disable();
    for (line, masked) in saved.into_iter().enumerate() {
        pic::set_masked(line as u8, masked);
    }
    result
}

fn info() -> Result<&'static timer::Info, String> {
    timer::info().ok_or_else(|| "timer::init never ran".into())
}

/// Boot made a choice, reported it, and honoured `LAZYOS_TIMER=lapic`; the
/// APIC source runs periodically with a sane count.
fn boot_choice() -> Result<(), String> {
    let info = info()?;
    serial_println!(
        "TEST:timer_boot_choice:INFO:lapic={} source={} clock_hz={} pit={:?} reference={}",
        info.lapic,
        info.source,
        info.clock_hz,
        info.pit,
        info.reference.map_or("none", |r| r.name())
    );
    check!(
        info.lapic == timer::lapic_tick(),
        "Info and the live flag disagree"
    );
    if info.forced {
        check!(info.lapic, "LAZYOS_TIMER=lapic but the PIT is the tick");
    }
    if matches!(info.pit, PitVerdict::Frozen | PitVerdict::NoIrq) {
        check!(info.lapic, "the PIT is {:?} and still the tick", info.pit);
    }
    if info.lapic {
        let mode = lapic::current_mode().ok_or("APIC tick without an APIC")?;
        let expected = timer_cal::lapic_count(info.clock_hz, lapic::DIVISOR).ok_or("bad count")?;
        check!(
            lapic::period() == expected,
            "APIC period {} != {expected} ({mode:?})",
            lapic::period()
        );
    } else {
        check!(
            info.clock_hz == timer_cal::PIT_HZ,
            "PIT clock {}",
            info.clock_hz
        );
    }
    Ok(())
}

/// The PIT verdict: frozen only over a long enough window; a counter that
/// moves is alive unless its IRQ0 was looked for and never came.
fn pit_verdicts() -> Result<(), String> {
    use timer_cal::{pit_verdict as v, FROZEN_READS, FROZEN_WINDOW_NS};
    let w = Some(FROZEN_WINDOW_NS);
    check!(
        v(0, 50, w, None) == PitVerdict::Frozen,
        "frozen over the window"
    );
    check!(
        v(0, 50, Some(FROZEN_WINDOW_NS - 1), None) == PitVerdict::Unknown,
        "short window"
    );
    check!(
        v(0, FROZEN_READS, None, None) == PitVerdict::Frozen,
        "frozen by reads"
    );
    check!(
        v(0, FROZEN_READS - 1, None, None) == PitVerdict::Unknown,
        "too few reads"
    );
    check!(v(1, 3, w, None) == PitVerdict::Alive, "one change is alive");
    check!(v(8, 9, w, Some(true)) == PitVerdict::Alive, "IRQ0 seen");
    check!(
        v(8, 9, w, Some(false)) == PitVerdict::NoIrq,
        "IRQ0 never seen"
    );
    // A frozen counter is frozen whatever the IRR said.
    check!(
        v(0, 50, w, Some(true)) == PitVerdict::Frozen,
        "frozen with IRQ0"
    );
    Ok(())
}

/// CPUID leaf 0x15 decoding: a reported crystal wins, the model table only
/// fills an Intel ECX of 0, and no ratio means no crystal.
fn cpuid_crystal() -> Result<(), String> {
    use timer_cal::crystal;
    // Arrow Lake-S as it reports itself: ratio 2:250 (assumed), 38.4 MHz.
    let c = crystal(true, 6, 0xC6, 2, 250, 38_400_000).ok_or("reported crystal")?;
    check!(c.hz == 38_400_000 && c.reported, "reported {c:?}");
    check!(c.tsc_hz == 4_800_000_000, "TSC {}", c.tsc_hz);
    // ECX 0 on the same part: the table's 38.4 MHz, flagged as not reported.
    let c = crystal(true, 6, 0xC6, 2, 250, 0).ok_or("table crystal")?;
    check!(c.hz == 38_400_000 && !c.reported, "table {c:?}");
    // Skylake client: 24 MHz.
    check!(
        crystal(true, 6, 0x5E, 2, 300, 0).map(|c| c.hz) == Some(24_000_000),
        "Skylake"
    );
    // Unknown model, AMD, or no ratio: nothing.
    check!(crystal(true, 6, 0x3A, 2, 300, 0).is_none(), "unknown model");
    check!(
        crystal(false, 0x19, 0x21, 2, 300, 0).is_none(),
        "AMD without ECX"
    );
    check!(
        crystal(true, 6, 0xC6, 0, 250, 38_400_000).is_none(),
        "EAX 0"
    );
    check!(crystal(true, 6, 0xC6, 2, 0, 38_400_000).is_none(), "EBX 0");
    // The cross-check tolerance is 2%.
    check!(timer_cal::agrees(38_400_000, 38_900_000), "1.3% agrees");
    check!(!timer_cal::agrees(38_400_000, 39_400_000), "2.6% disagrees");
    check!(
        !timer_cal::agrees(24_000_000, 1_000_000_000),
        "QEMU's 1 GHz bus"
    );
    Ok(())
}

/// The APIC initial count for 100 Hz: exact for the crystal and QEMU's bus,
/// refused when it would overflow or be uselessly coarse.
fn lapic_count_bounds() -> Result<(), String> {
    use timer_cal::lapic_count;
    check!(lapic_count(38_400_000, 16) == Some(24_000), "38.4 MHz / 16");
    check!(
        lapic_count(1_000_000_000, 16) == Some(625_000),
        "QEMU 1 GHz / 16"
    );
    check!(lapic_count(24_000_000, 1) == Some(240_000), "24 MHz / 1");
    check!(lapic_count(1_000_000, 16).is_none(), "too slow");
    check!(lapic_count(u64::MAX, 1).is_none(), "overflow");
    check!(timer_cal::rate(625_000, 10_000_000) == 62_500_000, "rate");
    check!(timer_cal::rate(5, 0) == 0, "empty window");
    Ok(())
}
