//! The arithmetic and the decisions behind the tick-source choice, kept free
//! of hardware access so the suite can drive them with made-up readings.

/// Scheduler tick rate, Hz.
pub const HZ: u64 = 100;

/// The PIT's input clock, Hz.
pub const PIT_HZ: u64 = 1_193_182;

/// How far a calibration may disagree with CPUID's crystal before the
/// measurement wins (2%: a hypervisor's made-up leaf 0x15, a wrong table).
pub const CROSS_CHECK_PERMILLE: u64 = 20;

/// The core crystal clock CPUID leaf 0x15 implies, and whether the
/// processor reported it or it came from the family table below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crystal {
    pub hz: u64,
    pub reported: bool,
    /// TSC frequency (`hz * EBX / EAX`).
    pub tsc_hz: u64,
}

/// Decode leaf 0x15 (`eax` denominator, `ebx` numerator, `ecx` crystal Hz).
///
/// The Intel SDM (Vol. 3A, "Local APIC timer") says that when leaf 0x15
/// enumerates the TSC/crystal ratio (EBX != 0), the APIC timer runs at the
/// core crystal clock. When ECX is 0 the crystal must come from the model:
/// the SDM's table (6th/7th generation Core: 24 MHz; Goldmont: 19.2 MHz;
/// Denverton: 25 MHz, the same table Linux's `native_calibrate_tsc` uses),
/// and 38.4 MHz for the Meteor Lake, Arrow Lake and Lunar Lake client parts,
/// which is an assumption (those parts are believed to report ECX
/// themselves); the caller cross-checks it against a measured clock and logs
/// that it was assumed.
pub fn crystal(
    intel: bool,
    family: u32,
    model: u32,
    eax: u32,
    ebx: u32,
    ecx: u32,
) -> Option<Crystal> {
    if eax == 0 || ebx == 0 {
        return None;
    }
    let (hz, reported) = if ecx != 0 {
        (u64::from(ecx), true)
    } else if intel && family == 6 {
        let hz = match model {
            0x4E | 0x5E | 0x8E | 0x9E => 24_000_000,
            0x5C => 19_200_000,
            0x5F => 25_000_000,
            0xAA | 0xAC | 0xB5 | 0xBD | 0xC5 | 0xC6 => 38_400_000,
            _ => return None,
        };
        (hz, false)
    } else {
        return None;
    };
    Some(Crystal {
        hz,
        reported,
        tsc_hz: hz * u64::from(ebx) / u64::from(eax),
    })
}

/// Whether `measured` is within [`CROSS_CHECK_PERMILLE`] of `expected`.
pub fn agrees(expected: u64, measured: u64) -> bool {
    expected.abs_diff(measured) * 1000 <= expected * CROSS_CHECK_PERMILLE
}

/// Frequency from `count` events over `ns` nanoseconds (0 for an empty window).
pub fn rate(count: u64, ns: u64) -> u64 {
    if ns == 0 {
        return 0;
    }
    (u128::from(count) * 1_000_000_000 / u128::from(ns)) as u64
}

/// The APIC timer's initial count for one tick, given its input clock and
/// divisor; `None` when it does not fit the 32-bit register or is too small
/// to be a sane period (under 1000 counts, i.e. a clock under 1.6 MHz).
pub fn lapic_count(clock_hz: u64, divisor: u64) -> Option<u32> {
    let count = clock_hz / divisor.max(1) / HZ;
    u32::try_from(count).ok().filter(|&c| c >= 1000)
}

/// Verdict of the PIT probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitVerdict {
    /// The counter moves (and IRQ0 reached the PIC, when that was checked).
    Alive,
    /// The counter did not move over a long enough window: clock-gated or
    /// absent.
    Frozen,
    /// It counts, but no IRQ0 reached the PIC (an HPET legacy route, or a
    /// chipset that no longer wires the PIT to IRQ0).
    NoIrq,
    /// The counter never moved, but the window was too short to say (no
    /// reference clock and too few reads): assume alive, as before H2.
    Unknown,
}

/// Shortest window, in nanoseconds, over which a counter that never moves
/// is declared frozen. A live PIT changes every 838 ns, so 2 ms is over two
/// thousand periods; under QEMU TCG the PIT follows the host clock, so slow
/// emulation only makes the window hold more reads, never fewer changes.
pub const FROZEN_WINDOW_NS: u64 = 2_000_000;

/// Reads without a reference clock after which a counter that never moved
/// is frozen (at about 1 us per port read on hardware, ~100 ms).
pub const FROZEN_READS: u64 = 100_000;

/// Judge the probe: `changes` distinct counter values seen over `reads`
/// reads spanning `window_ns` (None: no reference clock), and whether IRQ0
/// was seen in the PIC's request register (None: not checked).
pub fn pit_verdict(
    changes: u64,
    reads: u64,
    window_ns: Option<u64>,
    irq0: Option<bool>,
) -> PitVerdict {
    if changes == 0 {
        let long_enough = match window_ns {
            Some(ns) => ns >= FROZEN_WINDOW_NS,
            None => reads >= FROZEN_READS,
        };
        return if long_enough {
            PitVerdict::Frozen
        } else {
            PitVerdict::Unknown
        };
    }
    if irq0 == Some(false) {
        PitVerdict::NoIrq
    } else {
        PitVerdict::Alive
    }
}
