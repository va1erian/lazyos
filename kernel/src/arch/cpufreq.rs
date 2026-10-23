//! What clock the CPU really runs at (the boxes run LazyWeb far slower than QEMU's host does).
//!
//! LazyOS never asks the CPU for a speed: no P-state request (`IA32_PERF_CTL`), no HWP request, no
//! turbo. A real PC runs at whatever its firmware left at handoff, which can be far below what the
//! part can do. `HW:CPU:CLOCK` says what that is, once at boot; in a `LAZYOS_PERF=1` kernel
//! `PERF:clock` says it again for every report interval, while the desktop works, with the share
//! of the interval the core was out of `hlt` (C0) measured by the counters, not by sampling ticks.
//!
//! Effective clock = TSC rate x APERF/MPERF: both count only in C0, MPERF at the TSC's (nominal)
//! rate and APERF at the core's actual one. The MSRs are read only when CPUID announces them, and
//! only on bare metal: a hypervisor may announce a counter and then refuse the read (#GP).

use core::arch::x86_64::{__cpuid, _rdtsc};
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Once;

use super::msr;

const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_PERF_CTL: u32 = 0x199;
const IA32_PM_ENABLE: u32 = 0x770;
const IA32_HWP_CAPABILITIES: u32 = 0x771;
const IA32_HWP_REQUEST: u32 = 0x774;
/// "Genu" of "GenuineIntel" (CPUID.0:EBX).
const INTEL_EBX: u32 = 0x756e_6547;
/// How long the boot measurement spins.
const BOOT_SPIN_US: u64 = 20_000;

/// What CPUID says the core can report about its clock.
#[derive(Clone, Copy)]
pub struct Features {
    /// APERF/MPERF (CPUID.6:ECX bit 0).
    pub aperf_mperf: bool,
    /// Intel's `IA32_PERF_STATUS`/`IA32_PERF_CTL` (EIST, CPUID.1:ECX bit 7, on Intel only).
    pub perf_ctl: bool,
    /// HWP's MSRs (CPUID.6:EAX bit 7, on Intel only).
    pub hwp: bool,
    /// CPUID.16h base and maximum frequency, 0 when not reported.
    pub base_mhz: u32,
    pub max_mhz: u32,
}

/// The features, or `None` under a hypervisor (see the module comment).
pub fn features() -> Option<Features> {
    let leaf0 = __cpuid(0);
    let leaf1 = __cpuid(1);
    if leaf1.ecx & (1 << 31) != 0 {
        return None;
    }
    let intel = leaf0.ebx == INTEL_EBX;
    let max_leaf = leaf0.eax;
    let leaf6 = (max_leaf >= 6).then(|| __cpuid(6));
    let leaf16 = (max_leaf >= 0x16).then(|| __cpuid(0x16));
    Some(Features {
        aperf_mperf: leaf6.is_some_and(|l| l.ecx & 1 != 0),
        perf_ctl: intel && leaf1.ecx & (1 << 7) != 0,
        hwp: intel && leaf6.is_some_and(|l| l.eax & (1 << 7) != 0),
        base_mhz: leaf16.map_or(0, |l| l.eax & 0xffff),
        max_mhz: leaf16.map_or(0, |l| l.ebx & 0xffff),
    })
}

/// One reading of the three counters.
#[derive(Clone, Copy)]
pub struct Sample {
    pub tsc: u64,
    pub aperf: u64,
    pub mperf: u64,
}

/// Read the counters. The caller has checked [`Features::aperf_mperf`].
pub fn sample() -> Sample {
    // SAFETY: `rdtsc` only reads a counter.
    let tsc = unsafe { _rdtsc() };
    Sample {
        tsc,
        mperf: msr::read(IA32_MPERF),
        aperf: msr::read(IA32_APERF),
    }
}

/// The TSC's rate in MHz, 0 before calibration (`clock` counts TSC cycles per 10 ms tick).
pub fn tsc_mhz() -> u64 {
    super::clock::cycles_per_tick() / 10_000
}

/// The core's clock while in C0 between two samples, in MHz.
pub fn effective_mhz(tsc_mhz: u64, d_aperf: u64, d_mperf: u64) -> Option<u64> {
    (d_mperf != 0).then(|| (tsc_mhz as u128 * d_aperf as u128 / d_mperf as u128) as u64)
}

/// The share of the interval spent in C0, in thousandths (MPERF ticks at the TSC's rate in C0).
pub fn busy_permille(d_mperf: u64, d_tsc: u64) -> u64 {
    if d_tsc == 0 {
        return 0;
    }
    (d_mperf as u128 * 1000 / d_tsc as u128).min(1000) as u64
}

/// The bus-ratio field (bits 15:8) of `IA32_PERF_STATUS` / `IA32_PERF_CTL`.
pub fn ratio(perf: u64) -> u64 {
    (perf >> 8) & 0xff
}

/// `IA32_HWP_REQUEST`'s fields (performance levels, about 100 MHz each on Core parts).
#[derive(Debug, PartialEq, Eq)]
pub struct HwpRequest {
    pub min: u8,
    pub max: u8,
    pub desired: u8,
    pub epp: u8,
}

impl HwpRequest {
    pub fn decode(raw: u64) -> HwpRequest {
        HwpRequest {
            min: raw as u8,
            max: (raw >> 8) as u8,
            desired: (raw >> 16) as u8,
            epp: (raw >> 24) as u8,
        }
    }
}

/// `IA32_HWP_CAPABILITIES`' fields.
#[derive(Debug, PartialEq, Eq)]
pub struct HwpCaps {
    pub highest: u8,
    pub guaranteed: u8,
    pub efficient: u8,
    pub lowest: u8,
}

impl HwpCaps {
    pub fn decode(raw: u64) -> HwpCaps {
        HwpCaps {
            highest: raw as u8,
            guaranteed: (raw >> 8) as u8,
            efficient: (raw >> 16) as u8,
            lowest: (raw >> 24) as u8,
        }
    }
}

/// What the boot measured, printed after `HW:CPU:CLOCK`.
struct BootReport {
    feat: Features,
    tsc: u64,
    /// (effective MHz, C0 permille) of the boot spin, when APERF/MPERF could be read.
    spin: Option<(Option<u64>, u64)>,
    /// (`IA32_PERF_STATUS`, `IA32_PERF_CTL`) bus ratios.
    perf: Option<(u64, u64)>,
    /// `None`: no HWP; `Some(None)`: HWP present but off.
    hwp: Option<Option<(HwpRequest, HwpCaps)>>,
}

impl BootReport {
    fn measure(feat: Features) -> BootReport {
        let tsc = tsc_mhz();
        let spin = (feat.aperf_mperf && tsc != 0).then(|| {
            let (a, b) = spin_pair(tsc * BOOT_SPIN_US);
            let d_mperf = b.mperf.wrapping_sub(a.mperf);
            (
                effective_mhz(tsc, b.aperf.wrapping_sub(a.aperf), d_mperf),
                busy_permille(d_mperf, b.tsc.wrapping_sub(a.tsc)),
            )
        });
        let perf = feat.perf_ctl.then(|| {
            (
                ratio(msr::read(IA32_PERF_STATUS)),
                ratio(msr::read(IA32_PERF_CTL)),
            )
        });
        let hwp = feat.hwp.then(|| {
            (msr::read(IA32_PM_ENABLE) & 1 != 0).then(|| {
                (
                    HwpRequest::decode(msr::read(IA32_HWP_REQUEST)),
                    HwpCaps::decode(msr::read(IA32_HWP_CAPABILITIES)),
                )
            })
        });
        BootReport {
            feat,
            tsc,
            spin,
            perf,
            hwp,
        }
    }
}

impl fmt::Display for BootReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let feat = self.feat;
        write!(
            f,
            " tsc={}MHz base={}MHz max={}MHz",
            self.tsc, feat.base_mhz, feat.max_mhz
        )?;
        match self.spin {
            Some((Some(mhz), c0)) => write!(f, " effective={mhz}MHz c0={c0}permille")?,
            Some((None, _)) => write!(f, " effective=? (MPERF did not count)")?,
            None => write!(f, " effective=? (no APERF/MPERF)")?,
        }
        if let Some((status, ctl)) = self.perf {
            write!(f, " status_ratio={status} ctl_ratio={ctl}")?;
        }
        match &self.hwp {
            None => write!(f, " hwp=absent"),
            Some(None) => write!(f, " hwp=off"),
            Some(Some((r, c))) => write!(
                f,
                " hwp=on req={}..{} desired={} epp={} caps={}..{} guaranteed={} efficient={}",
                r.min, r.max, r.desired, r.epp, c.lowest, c.highest, c.guaranteed, c.efficient
            ),
        }
    }
}

/// Two samples `cycles` TSC cycles apart, spinning with interrupts off so the core stays in C0.
fn spin_pair(cycles: u64) -> (Sample, Sample) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let first = sample();
        let mut last = first;
        while last.tsc.wrapping_sub(first.tsc) < cycles {
            core::hint::spin_loop();
            last = sample();
        }
        (first, last)
    })
}

/// Print `HW:CPU:CLOCK` (a diagnostic for a PC with no serial port: it reaches `dbgd`'s boot log).
pub fn report_boot() {
    match features() {
        Some(feat) => crate::serial_println!("HW:CPU:CLOCK{}", BootReport::measure(feat)),
        None => crate::serial_println!("HW:CPU:CLOCK:SKIPPED (hypervisor) tsc={}MHz", tsc_mhz()),
    }
}

/// The counters at the previous [`interval`] (0, 0, 0 before the first).
static LAST: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// `PERF:clock`'s numbers since the previous call: (effective MHz, C0 permille, interval ms), or
/// `None` on the first call, under a hypervisor, or without APERF/MPERF.
#[allow(dead_code)] // Only a `lazyos_perf` kernel reports it.
pub fn interval() -> Option<(u64, u64, u64)> {
    static FEATURES: Once<Option<Features>> = Once::new();
    let feat = (*FEATURES.call_once(features))?;
    let tsc = tsc_mhz();
    if !feat.aperf_mperf || tsc == 0 {
        return None;
    }
    let now = sample();
    let prev = [
        LAST[0].swap(now.tsc, Ordering::Relaxed),
        LAST[1].swap(now.aperf, Ordering::Relaxed),
        LAST[2].swap(now.mperf, Ordering::Relaxed),
    ];
    if prev[0] == 0 {
        return None;
    }
    let d_tsc = now.tsc.wrapping_sub(prev[0]);
    let d_mperf = now.mperf.wrapping_sub(prev[2]);
    let mhz = effective_mhz(tsc, now.aperf.wrapping_sub(prev[1]), d_mperf)?;
    Some((mhz, busy_permille(d_mperf, d_tsc), d_tsc / (tsc * 1000)))
}
