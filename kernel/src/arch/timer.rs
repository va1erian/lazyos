//! The scheduler tick source (docs/real-pc-boot-plan.md H2).
//!
//! The tick is 100 Hz from the 8254 PIT through 8259 IRQ0, as it always was,
//! unless the PIT does not tick: Intel platforms since about 2019 commonly
//! clock-gate the 8254, and then nothing would ever schedule. At boot the
//! PIT is probed against a reference clock (the ACPI PM timer, else the
//! HPET); when it is frozen or its IRQ0 never reaches the PIC, or when the
//! image is built with `LAZYOS_TIMER=lapic`, the local APIC timer runs in
//! periodic mode at 100 Hz instead, with the 8259 kept in virtual-wire mode.
//!
//! The APIC timer is calibrated from, in order: CPUID leaf 0x15 (the core
//! crystal, which the APIC timer runs at when that leaf enumerates it),
//! cross-checked against the PM timer or HPET when one exists; the PM timer;
//! the HPET; the PIT, only when it is alive. TSC-deadline mode is not used.
//!
//! Everything above the tick is unchanged: `TICKS` still advances once per
//! period plus the interrupts-off catch-up of issue #344 (`clock`), whose
//! TSC rate comes from the same calibration. PIC line 0 stays "the tick"
//! for the rest of the kernel: `pic::set_masked(0, _)` masks the APIC timer
//! when it is the source. One line reports the choice:
//! `HW:TIMER:<pit|lapic> <calibration source> <timer input clock Hz>`.

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Once;

use super::io::{inb, outb};
use super::refclock::{self, RefClock, Stopwatch};
use super::timer_cal::{self, Crystal, PitVerdict, HZ, PIT_HZ};
use super::{acpi_tables, clock, lapic, pic};

/// Whether the APIC timer is the tick (set once at boot).
static LAPIC_TICK: AtomicBool = AtomicBool::new(false);
static INFO: Once<Info> = Once::new();

/// What [`init`] found and chose; the suite reads it.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub lapic: bool,
    pub forced: bool,
    pub pit: PitVerdict,
    pub reference: Option<RefClock>,
    /// The calibration source named in the `HW:TIMER` line.
    pub source: &'static str,
    /// The tick timer's input clock: 1193182 for the PIT, the APIC timer's
    /// undivided clock for the APIC.
    pub clock_hz: u64,
}

/// Calibration window against a reference clock or the PIT.
const WINDOW_NS: u64 = 50_000_000;
/// PIT channel-2 count for [`WINDOW_NS`].
const PIT_WINDOW_COUNT: u16 = (PIT_HZ * WINDOW_NS / 1_000_000_000) as u16;
/// Spin bound for any calibration wait.
const SPINS: u64 = 50_000_000;
/// The IRQ0 check waits this long for the PIT's request to show in the IRR.
const IRQ_WINDOW_NS: u64 = 50_000_000;

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Whether the APIC timer is the tick.
pub fn lapic_tick() -> bool {
    LAPIC_TICK.load(Ordering::Relaxed)
}

/// What boot chose (`None` before [`init`]).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn info() -> Option<&'static Info> {
    INFO.get()
}

/// Acknowledge the tick interrupt at whichever controller raised it.
///
/// # Safety
/// Call once from the tick handler, after the tick was counted.
pub unsafe fn end_of_tick() {
    if lapic_tick() {
        lapic::eoi();
    } else {
        pic::end_of_interrupt(0);
    }
}

/// Whether this tick entry is an APIC timer interrupt that was already
/// pending when the tick was masked; if so it is acknowledged here and must
/// be ignored. The 8259 never delivers a request latched on a masked line,
/// but the local APIC delivers a vector already in its IRR whatever the LVT
/// mask says now, and the kernel relies on "line 0 masked" meaning no tick
/// (tests and drivers enable interrupts with the tick off).
pub fn stale_tick() -> bool {
    if lapic_tick() && lapic::timer_masked() {
        lapic::eoi();
        return true;
    }
    false
}

/// Mask or unmask the APIC tick (the `pic::set_masked(0, _)` path when the
/// APIC is the source). Unmasking starts counting periods from now.
pub fn set_tick_masked(masked: bool) {
    if !masked {
        clock::resync();
    }
    lapic::set_timer_masked(masked);
}

/// Whether the APIC tick is masked.
pub fn tick_masked() -> bool {
    lapic::timer_masked()
}

/// Result of [`probe_pit`].
#[derive(Clone, Copy, Debug)]
pub struct PitProbe {
    pub verdict: PitVerdict,
    pub changes: u64,
    pub reads: u64,
    pub window_ns: Option<u64>,
    pub irq0: Option<bool>,
}

/// Probe PIT channel 0: does its count move, over a window timed by
/// `reference` (or a read count without one)? With `check_irq`, a moving PIT
/// must also raise IRQ0 in the PIC's request register within 50 ms. Run with
/// interrupts off (a serviced IRQ0 clears the request bit).
pub fn probe_pit(reference: Option<RefClock>, check_irq: bool) -> PitProbe {
    let mut watch = reference.map(Stopwatch::start);
    let mut last = refclock::pit_count();
    let (mut changes, mut reads, mut window_ns) = (0, 0, None);
    while reads < timer_cal::FROZEN_READS {
        reads += 1;
        let now = refclock::pit_count();
        if now != last {
            changes += 1;
            last = now;
        }
        if let Some(watch) = watch.as_mut() {
            let ns = watch.elapsed_ns();
            window_ns = Some(ns);
            if ns >= timer_cal::FROZEN_WINDOW_NS {
                break;
            }
        }
        if changes >= 8 {
            break;
        }
    }
    let irq0 = match reference {
        Some(clock) if check_irq && changes > 0 => Some(irq0_requested(clock)),
        _ => None,
    };
    PitProbe {
        verdict: timer_cal::pit_verdict(changes, reads, window_ns, irq0),
        changes,
        reads,
        window_ns,
        irq0,
    }
}

/// Whether IRQ0 shows in the master PIC's request register within
/// [`IRQ_WINDOW_NS`] (five PIT periods).
fn irq0_requested(clock: RefClock) -> bool {
    let mut watch = Stopwatch::start(clock);
    for _ in 0..SPINS {
        if pic::requested(0) {
            return true;
        }
        if watch.elapsed_ns() >= IRQ_WINDOW_NS {
            return false;
        }
    }
    false
}

/// Choose and start the tick source. Runs once from `idt::init_hardware`,
/// after the PIC is remapped and the PIT programmed, before interrupts are
/// enabled.
pub fn init() {
    let platform = acpi_tables::platform();
    let reference = refclock::find(platform);
    let probe = probe_pit(reference, true);
    let forced = option_env!("LAZYOS_TIMER") == Some("lapic");
    crate::serial_println!(
        "timer: PIT {:?} ({} changes in {} reads over {} us, IRQ0 {:?}), reference {}{}",
        probe.verdict,
        probe.changes,
        probe.reads,
        probe.window_ns.unwrap_or(0) / 1000,
        probe.irq0,
        reference.map_or("none", |r| r.name()),
        if forced { ", LAZYOS_TIMER=lapic" } else { "" }
    );
    log_cpu_features();
    let pit_ok = matches!(probe.verdict, PitVerdict::Alive | PitVerdict::Unknown);
    let mut info = Info {
        lapic: false,
        forced,
        pit: probe.verdict,
        reference,
        source: "pit",
        clock_hz: PIT_HZ,
    };
    if !pit_ok || forced {
        match start_lapic(platform, reference, pit_ok) {
            Ok((source, clock_hz)) => {
                info = Info {
                    lapic: true,
                    source,
                    clock_hz,
                    ..info
                };
            }
            Err(why) => crate::serial_println!("timer: no APIC tick: {why}; keeping the PIT"),
        }
    }
    if !info.lapic {
        if pit_ok {
            // SAFETY: once, before interrupts are enabled (we are in
            // `init_hardware`); it borrows PIT channel 2 and port 0x61.
            unsafe { clock::calibrate(HZ as u32) };
        } else {
            crate::serial_println!("timer: WARNING: the PIT does not tick and nothing replaces it");
            info.source = "none";
        }
    }
    crate::serial_println!(
        "HW:TIMER:{} {} {}",
        if info.lapic { "lapic" } else { "pit" },
        info.source,
        info.clock_hz
    );
    INFO.call_once(|| info);
}

/// Bring up the APIC, calibrate it, mask the PIT's line and start the
/// periodic APIC tick. Returns the calibration source and the timer clock.
fn start_lapic(
    platform: Option<&::acpi::Platform>,
    reference: Option<RefClock>,
    pit_ok: bool,
) -> Result<(&'static str, u64), &'static str> {
    let madt = platform.and_then(|p| p.madt.as_ref().ok());
    lapic::init(madt.map(|m| m.lapic_address))?;
    let calibration = calibrate(reference, pit_ok).ok_or("no calibration source")?;
    let count = timer_cal::lapic_count(calibration.lapic_hz, lapic::DIVISOR)
        .ok_or("calibrated APIC timer rate out of range")?;
    pic::set_masked(0, true);
    clock::set_rate(calibration.tsc_hz / HZ);
    LAPIC_TICK.store(true, Ordering::Relaxed);
    lapic::start_periodic(count);
    crate::serial_println!(
        "timer: APIC timer {} Hz / {} -> {count} per tick, TSC {} Hz",
        calibration.lapic_hz,
        lapic::DIVISOR,
        calibration.tsc_hz
    );
    Ok((calibration.source, calibration.lapic_hz))
}

/// A calibration of the APIC timer clock (undivided) and the TSC.
#[derive(Clone, Copy, Debug)]
struct Calibration {
    source: &'static str,
    lapic_hz: u64,
    tsc_hz: u64,
}

/// CPUID 0x15 when the CPU enumerates the crystal, checked against a
/// measurement when one is possible; else the measurement alone.
fn calibrate(reference: Option<RefClock>, pit_ok: bool) -> Option<Calibration> {
    let crystal = cpuid_crystal();
    let measured = match reference {
        Some(clock) => measure(clock),
        // SAFETY: interrupts are off and the kernel owns PIT channel 2.
        None if pit_ok => unsafe { measure_pit() },
        None => None,
    };
    match (crystal, measured) {
        (Some(c), Some(m)) if timer_cal::agrees(c.hz, m.lapic_hz) => Some(from_crystal(c)),
        (Some(c), Some(m)) => {
            crate::serial_println!(
                "timer: CPUID 0x15 crystal {} Hz disagrees with {} ({} Hz); using {}",
                c.hz,
                m.source,
                m.lapic_hz,
                m.source
            );
            Some(m)
        }
        (Some(c), None) => {
            crate::serial_println!(
                "timer: CPUID 0x15 crystal {} Hz unverified (no reference)",
                c.hz
            );
            Some(from_crystal(c))
        }
        (None, measured) => measured,
    }
}

fn from_crystal(c: Crystal) -> Calibration {
    if !c.reported {
        crate::serial_println!(
            "timer: CPUID 0x15 ECX=0; crystal {} Hz from the model table",
            c.hz
        );
    }
    Calibration {
        source: if c.reported {
            "cpuid15"
        } else {
            "cpuid15-table"
        },
        lapic_hz: c.hz,
        tsc_hz: c.tsc_hz,
    }
}

/// The core crystal from CPUID leaf 0x15, when enumerated.
fn cpuid_crystal() -> Option<Crystal> {
    use core::arch::x86_64::__cpuid;
    let leaf0 = __cpuid(0);
    if leaf0.eax < 0x15 {
        return None;
    }
    let intel = (leaf0.ebx, leaf0.edx, leaf0.ecx) == (0x756E_6547, 0x4965_6E69, 0x6C65_746E);
    let sig = __cpuid(1).eax;
    let mut family = (sig >> 8) & 0xF;
    let mut model = (sig >> 4) & 0xF;
    if family == 0xF {
        family += (sig >> 20) & 0xFF;
    }
    if family == 6 || family >= 0xF {
        model |= ((sig >> 16) & 0xF) << 4;
    }
    let leaf = __cpuid(0x15);
    timer_cal::crystal(intel, family, model, leaf.eax, leaf.ebx, leaf.ecx)
}

/// Time the APIC timer and the TSC over [`WINDOW_NS`] of `clock`.
fn measure(clock: RefClock) -> Option<Calibration> {
    lapic::start_free_run();
    let mut watch = Stopwatch::start(clock);
    let (apic0, tsc0) = (lapic::remaining(), rdtsc());
    let mut ns = 0;
    for _ in 0..SPINS {
        ns = watch.elapsed_ns();
        if ns >= WINDOW_NS {
            break;
        }
    }
    let (apic1, tsc1) = (lapic::remaining(), rdtsc());
    if ns < WINDOW_NS {
        return None;
    }
    calibration(clock.name(), apic0, apic1, tsc1.wrapping_sub(tsc0), ns)
}

/// Time the APIC timer and the TSC over a [`WINDOW_NS`] channel-2 count.
///
/// # Safety
/// Interrupts off; takes PIT channel 2 and port 0x61 for the duration and
/// leaves the speaker off.
unsafe fn measure_pit() -> Option<Calibration> {
    let gate = inb(0x61);
    outb(0x61, gate & !0x03);
    outb(0x43, 0xB0); // channel 2, lobyte/hibyte, mode 0
    outb(0x42, (PIT_WINDOW_COUNT & 0xFF) as u8);
    outb(0x42, (PIT_WINDOW_COUNT >> 8) as u8);
    lapic::start_free_run();
    let (apic0, tsc0) = (lapic::remaining(), rdtsc());
    outb(0x61, (gate & !0x02) | 0x01);
    let done = (0..SPINS).any(|_| inb(0x61) & 0x20 != 0);
    let (apic1, tsc1) = (lapic::remaining(), rdtsc());
    outb(0x61, gate);
    if !done {
        return None;
    }
    let ns = u64::from(PIT_WINDOW_COUNT) * 1_000_000_000 / PIT_HZ;
    calibration("pit", apic0, apic1, tsc1.wrapping_sub(tsc0), ns)
}

fn calibration(
    source: &'static str,
    apic0: u32,
    apic1: u32,
    tsc: u64,
    ns: u64,
) -> Option<Calibration> {
    let counted = u64::from(apic0.checked_sub(apic1)?) * lapic::DIVISOR;
    let lapic_hz = timer_cal::rate(counted, ns);
    (lapic_hz > 0).then_some(Calibration {
        source,
        lapic_hz,
        tsc_hz: timer_cal::rate(tsc, ns),
    })
}

/// Record the timer-related CPU features on the boot log.
fn log_cpu_features() {
    use core::arch::x86_64::__cpuid;
    let max = __cpuid(0).eax;
    let ecx1 = __cpuid(1).ecx;
    let arat = max >= 6 && __cpuid(6).eax & (1 << 2) != 0;
    let invariant =
        __cpuid(0x8000_0000).eax >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 8) != 0;
    crate::serial_println!(
        "timer: cpu x2apic={} tsc-deadline={} (unused) arat={} invariant-tsc={}",
        ecx1 & (1 << 21) != 0,
        ecx1 & (1 << 24) != 0,
        arat,
        invariant
    );
}
