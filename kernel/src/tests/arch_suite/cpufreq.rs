//! The CPU clock report (`arch::cpufreq`): the MSR field decoders, the APERF/MPERF arithmetic at
//! its edges (zero intervals, wrapped counters, a core above its nominal rate), and that the live
//! path never reads an MSR a hypervisor might refuse.

use super::*;
use crate::arch::cpufreq::{
    busy_permille, effective_mhz, features, interval, ratio, HwpCaps, HwpRequest,
};

/// Field positions as the SDM gives them (vol. 4, `IA32_HWP_REQUEST` and `_CAPABILITIES`).
pub(super) fn decoders() -> Result<(), String> {
    check!(ratio(0x0000_1a00) == 0x1a, "PERF_STATUS ratio is bits 15:8");
    check!(
        ratio(0xffff_ffff_ffff_00ff) == 0,
        "ratio ignores the other bits"
    );
    let req = HwpRequest::decode(0x8000_0000_8000_2c04);
    check!(
        req == HwpRequest {
            min: 4,
            max: 0x2c,
            desired: 0,
            epp: 0x80
        },
        "HWP request decoded as {req:?}"
    );
    let caps = HwpCaps::decode(0x0104_0f2c);
    check!(
        caps == HwpCaps {
            highest: 0x2c,
            guaranteed: 0x0f,
            efficient: 0x04,
            lowest: 0x01
        },
        "HWP capabilities decoded as {caps:?}"
    );
    Ok(())
}

pub(super) fn arithmetic_edges() -> Result<(), String> {
    check!(
        effective_mhz(2000, 5, 0).is_none(),
        "an interval MPERF did not count is unknown"
    );
    check!(
        effective_mhz(2000, 0, 10) == Some(0),
        "APERF stopped reads 0 MHz"
    );
    check!(
        effective_mhz(1996, 400, 1000) == Some(798),
        "a core at 40% of nominal"
    );
    check!(
        effective_mhz(1500, 2900, 1000) == Some(4350),
        "turbo reads above nominal"
    );
    // Counters near the top of u64: the product must not overflow.
    check!(
        effective_mhz(5000, u64::MAX / 2, u64::MAX / 2) == Some(5000),
        "huge deltas overflowed"
    );
    let wrapped = 5u64.wrapping_sub(u64::MAX - 4);
    check!(wrapped == 10, "a wrapped counter's delta is {wrapped}");
    check!(busy_permille(0, 0) == 0, "an empty interval is idle");
    check!(busy_permille(250, 1000) == 250, "a quarter busy");
    check!(
        busy_permille(2000, 1000) == 1000,
        "MPERF ahead of the TSC is clamped"
    );
    Ok(())
}

/// Many pseudo-random readings: the results stay inside what the inputs allow.
pub(super) fn arithmetic_soak() -> Result<(), String> {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for round in 0..200_000u32 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let tsc = 100 + (x % 6000);
        let d_mperf = 1 + ((x >> 8) % 1_000_000_000);
        let d_aperf = (x >> 20) % (d_mperf * 3);
        let mhz = effective_mhz(tsc, d_aperf, d_mperf).ok_or("lost a reading")?;
        check!(
            mhz <= tsc * 3,
            "round {round}: {mhz} MHz from a {tsc} MHz TSC at <3x"
        );
        let d_tsc = (x >> 4) % 2_000_000_000;
        check!(
            busy_permille(d_mperf, d_tsc) <= 1000,
            "round {round}: busy over 100%"
        );
    }
    Ok(())
}

/// Under QEMU (a hypervisor) `features` is `None` and no MSR is read; on bare metal the boot line
/// and two intervals are taken for real.
pub(super) fn live_path() -> Result<(), String> {
    match features() {
        None => {
            check!(
                interval().is_none(),
                "an interval was read under a hypervisor"
            );
        }
        Some(feat) => {
            crate::arch::cpufreq::report_boot();
            let _ = interval();
            if feat.aperf_mperf {
                let (mhz, c0, _) = interval().ok_or("no second interval with APERF/MPERF")?;
                check!(
                    mhz > 0 && c0 <= 1000,
                    "interval read {mhz} MHz, {c0} permille"
                );
            }
        }
    }
    Ok(())
}
