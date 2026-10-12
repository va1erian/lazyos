//! N and CTS for the HDMI audio clock regeneration packet.
//!
//! The sink rebuilds the audio sample clock from the TMDS clock with
//! `128 * fs = f_TMDS * N / CTS` (HDMI 1.4b section 7.2). The source sends
//! `N` and `CTS` in the clock regeneration packet; `N` is chosen so that the
//! regenerated clock jitter stays low (the spec's recommended table for the
//! standard pixel clocks) and `CTS` follows from it.
//!
//! The tables below are the HDMI specification's recommended values for 32,
//! 44.1 and 48 kHz (88.2/96 and 176.4/192 kHz reuse the 44.1/48 kHz `N`
//! multiplied by 2 and 4 with the same `CTS`), written from memory and
//! checked by the tests against the defining formula. The box's own mode
//! (2560x1440 at about 241.5 MHz) is not in the table, so [`select`] also
//! derives a value.

/// An audio sample rate HDMI can carry as LPCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rate {
    Hz32000,
    Hz44100,
    Hz48000,
    Hz88200,
    Hz96000,
    Hz176400,
    Hz192000,
}

impl Rate {
    pub const ALL: [Rate; 7] = [
        Rate::Hz32000,
        Rate::Hz44100,
        Rate::Hz48000,
        Rate::Hz88200,
        Rate::Hz96000,
        Rate::Hz176400,
        Rate::Hz192000,
    ];

    pub fn hz(self) -> u32 {
        match self {
            Rate::Hz32000 => 32_000,
            Rate::Hz44100 => 44_100,
            Rate::Hz48000 => 48_000,
            Rate::Hz88200 => 88_200,
            Rate::Hz96000 => 96_000,
            Rate::Hz176400 => 176_400,
            Rate::Hz192000 => 192_000,
        }
    }

    pub fn from_hz(hz: u32) -> Option<Rate> {
        Rate::ALL.into_iter().find(|r| r.hz() == hz)
    }

    /// The base family (32, 44.1 or 48 kHz) and the multiplier over it.
    fn family(self) -> (usize, u32) {
        match self {
            Rate::Hz32000 => (0, 1),
            Rate::Hz44100 => (1, 1),
            Rate::Hz48000 => (2, 1),
            Rate::Hz88200 => (1, 2),
            Rate::Hz96000 => (2, 2),
            Rate::Hz176400 => (1, 4),
            Rate::Hz192000 => (2, 4),
        }
    }

    /// The `N` used when the pixel clock is not in the table: the value
    /// every standard-clock row uses for the family (4096, 6272, 6144), scaled
    /// for the multiples.
    pub fn default_n(self) -> u32 {
        const BASE: [u32; 3] = [4096, 6272, 6144];
        let (f, m) = self.family();
        BASE[f] * m
    }
}

/// A clock regeneration pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NCts {
    pub n: u32,
    pub cts: u32,
    /// `CTS` is exact for this `N` (no fractional part to dither).
    pub exact: bool,
    /// The pair comes from the specification's table.
    pub from_table: bool,
}

/// One table row: TMDS clock in kHz (the clock the standard names, rounded as
/// i915 keys it) and `(N, CTS)` for 32, 44.1 and 48 kHz.
struct Row {
    khz: u32,
    nc: [(u32, u32); 3],
}

const fn row(khz: u32, nc: [(u32, u32); 3]) -> Row {
    Row { khz, nc }
}

/// HDMI recommended N and CTS (HDMI 1.4b tables 7-1 to 7-3; the 297 and
/// 594 MHz rows and their /1.001 neighbours from HDMI 2.0). A fractional
/// `CTS` (210937.5) is listed rounded down, the way i915 does.
const TABLE: [Row; 14] = [
    row(25_175, [(4576, 28125), (7007, 31250), (6864, 28125)]),
    row(25_200, [(4096, 25200), (6272, 28000), (6144, 25200)]),
    row(27_000, [(4096, 27000), (6272, 30000), (6144, 27000)]),
    row(27_027, [(4096, 27027), (6272, 30030), (6144, 27027)]),
    row(54_000, [(4096, 54000), (6272, 60000), (6144, 54000)]),
    row(54_054, [(4096, 54054), (6272, 60060), (6144, 54054)]),
    row(74_176, [(11648, 210937), (17836, 234375), (11648, 140625)]),
    row(74_250, [(4096, 74250), (6272, 82500), (6144, 74250)]),
    row(148_352, [(11648, 421875), (8918, 234375), (5824, 140625)]),
    row(148_500, [(4096, 148500), (6272, 165000), (6144, 148500)]),
    row(296_703, [(5824, 421875), (4459, 234375), (5824, 281250)]),
    row(297_000, [(3072, 222750), (4704, 247500), (5120, 247500)]),
    row(593_407, [(5824, 843750), (8918, 937500), (5824, 562500)]),
    row(594_000, [(3072, 445500), (9408, 990000), (6144, 594000)]),
];

/// Pixel clocks of the standard rows as exact rationals of Hz, for checks:
/// `(table kHz, numerator, denominator)` of the true clock in Hz.
pub fn exact_clock_hz(table_khz: u32) -> Option<(u64, u64)> {
    Some(match table_khz {
        25_175 => (25_200_000 * 1000, 1001),
        74_176 => (74_250_000 * 1000, 1001),
        148_352 => (148_500_000 * 1000, 1001),
        296_703 => (297_000_000 * 1000, 1001),
        593_407 => (594_000_000 * 1000, 1001),
        k if TABLE.iter().any(|r| r.khz == k) => (u64::from(k) * 1000, 1),
        _ => return None,
    })
}

/// The table's recommended pair, for a TMDS clock given in kHz exactly as the
/// standard names it (25175, 25200, 27000, ... as i915 keys them).
pub fn recommended(tmds_khz: u32, rate: Rate) -> Option<NCts> {
    let r = TABLE.iter().find(|r| r.khz == tmds_khz)?;
    let (f, m) = rate.family();
    let (n, cts) = r.nc[f];
    let exact = match exact_clock_hz(r.khz) {
        Some((num, den)) => {
            // cts * 128 * fs == f_hz * n, with f_hz = num / den.
            u64::from(cts) * 128 * u64::from(rate.hz()) * den == num * u64::from(n * m)
        }
        None => false,
    };
    Some(NCts {
        n: n * m,
        cts,
        exact,
        from_table: true,
    })
}

/// The `N` range the specification allows (HDMI 1.4b section 7.2.1):
/// `128 fs / 1500 Hz <= N <= 128 fs / 300 Hz`.
pub fn n_range(rate: Rate) -> (u32, u32) {
    let x = 128 * rate.hz();
    (x.div_ceil(1500), x / 300)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `CTS` for a given `N`: `f_TMDS * N / (128 fs)`, rounded to nearest, and
/// whether it was exact. `None` for a zero clock or an overflowing result.
pub fn cts_for(tmds_khz: u32, n: u32, rate: Rate) -> Option<(u32, bool)> {
    if tmds_khz == 0 || n == 0 {
        return None;
    }
    let num = u128::from(tmds_khz) * 1000 * u128::from(n);
    let den = 128 * u128::from(rate.hz());
    let cts = (num + den / 2) / den;
    Some((u32::try_from(cts).ok()?, num % den == 0))
}

/// The default-`N` pair for any clock (the hardware then also regenerates
/// `CTS` itself, so a fractional value here is only informative).
pub fn computed(tmds_khz: u32, rate: Rate) -> Option<NCts> {
    let n = rate.default_n();
    let (cts, exact) = cts_for(tmds_khz, n, rate)?;
    Some(NCts {
        n,
        cts,
        exact,
        from_table: false,
    })
}

/// An `N` inside the allowed range, as close to the default as possible,
/// for which `CTS` is an exact integer. For a clock like 241.5 MHz at
/// 44.1 kHz the default 6272 is inexact but any multiple of 336 is exact.
pub fn exact(tmds_khz: u32, rate: Rate) -> Option<NCts> {
    if tmds_khz == 0 {
        return None;
    }
    let f = u64::from(tmds_khz) * 1000;
    let den = 128 * u64::from(rate.hz());
    let step = den / gcd(f, den);
    let (lo, hi) = n_range(rate);
    let def = u64::from(rate.default_n());
    let below = def / step * step;
    let above = below + step;
    let pick = [below, above]
        .into_iter()
        .filter(|n| (u64::from(lo)..=u64::from(hi)).contains(n))
        .min_by_key(|n| n.abs_diff(def))?;
    let n = pick as u32;
    let (cts, _) = cts_for(tmds_khz, n, rate)?;
    Some(NCts {
        n,
        cts,
        exact: true,
        from_table: false,
    })
}

/// The pair to program: the specification's table for a standard clock,
/// otherwise the default `N` when it divides exactly, otherwise an exact `N`
/// in range, otherwise the default `N` with an inexact `CTS`.
pub fn select(tmds_khz: u32, rate: Rate) -> Option<NCts> {
    if let Some(r) = recommended(tmds_khz, rate) {
        return Some(r);
    }
    let c = computed(tmds_khz, rate)?;
    if c.exact {
        return Some(c);
    }
    Some(exact(tmds_khz, rate).unwrap_or(c))
}

/// The TMDS character clock for a pixel clock at `bits_per_component` deep
/// colour (8, 10, 12 or 16): the pixel clock times bpc / 8.
pub fn tmds_clock_khz(pixel_khz: u32, bits_per_component: u32) -> u32 {
    (u64::from(pixel_khz) * u64::from(bits_per_component) / 8) as u32
}

/// `AUD_CONFIG` pixel-clock index for the clocks the display engine knows
/// (the 25.2/1.001 ... 148.5 MHz rows). `// TO CONFIRM against dump`: from
/// memory of i915's `hsw_hdmi_audio_config`; other clocks fall back to the
/// 25.2 MHz entry with an explicit `N` there.
pub fn aud_config_clock_index(tmds_khz: u32) -> Option<u32> {
    Some(match tmds_khz {
        25_175 => 0,
        25_200 => 1,
        27_000 => 2,
        27_027 => 3,
        54_000 => 4,
        54_054 => 5,
        74_176 => 6,
        74_250 => 7,
        148_352 => 8,
        148_500 => 9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_defining_formula() {
        // 128 fs / N == f / CTS, so CTS * 128 fs == f * N within one unit of
        // CTS (the spec lists 210937.5 as 210937).
        for row in &TABLE {
            let (num, den) = exact_clock_hz(row.khz).unwrap();
            for rate in Rate::ALL {
                let r = recommended(row.khz, rate).unwrap();
                let ideal = num as f64 / den as f64 * f64::from(r.n)
                    / (128.0 * f64::from(rate.hz()));
                assert!(
                    (ideal - f64::from(r.cts)).abs() <= 1.0,
                    "{} kHz {:?}: N {} CTS {} vs {}",
                    row.khz,
                    rate,
                    r.n,
                    r.cts,
                    ideal
                );
                let (lo, hi) = n_range(rate);
                assert!((lo..=hi).contains(&r.n), "N {} out of range for {rate:?}", r.n);
            }
        }
    }

    #[test]
    fn spec_spot_values() {
        let r = recommended(148_500, Rate::Hz48000).unwrap();
        assert_eq!((r.n, r.cts, r.exact), (6144, 148500, true));
        let r = recommended(74_250, Rate::Hz44100).unwrap();
        assert_eq!((r.n, r.cts), (6272, 82500));
        let r = recommended(297_000, Rate::Hz32000).unwrap();
        assert_eq!((r.n, r.cts), (3072, 222750));
        // 74.25/1.001 MHz at 32 kHz: the half-integer CTS is not exact.
        let r = recommended(74_176, Rate::Hz32000).unwrap();
        assert_eq!((r.n, r.cts, r.exact), (11648, 210937, false));
        // Multiples keep CTS and scale N.
        let r = recommended(148_500, Rate::Hz192000).unwrap();
        assert_eq!((r.n, r.cts), (24576, 148500));
        let r = recommended(148_500, Rate::Hz88200).unwrap();
        assert_eq!((r.n, r.cts), (12544, 165000));
    }

    #[test]
    fn exactness_flag_is_honest() {
        for row in &TABLE {
            for rate in Rate::ALL {
                let r = recommended(row.khz, rate).unwrap();
                let (num, den) = exact_clock_hz(row.khz).unwrap();
                let lhs = u64::from(r.cts) * 128 * u64::from(rate.hz()) * den;
                let rhs = num * u64::from(r.n);
                assert_eq!(r.exact, lhs == rhs);
            }
        }
    }

    #[test]
    fn the_boxs_2560x1440_clock_is_not_in_the_table() {
        // CVT-RB 2560x1440@60: 241.5 MHz.
        assert!(recommended(241_500, Rate::Hz48000).is_none());
        // 48 kHz and 32 kHz divide exactly with the default N.
        let c = select(241_500, Rate::Hz48000).unwrap();
        assert_eq!((c.n, c.cts, c.exact, c.from_table), (6144, 241_500, true, false));
        let c = select(241_500, Rate::Hz32000).unwrap();
        assert_eq!((c.n, c.cts, c.exact), (4096, 241_500, true));
        // 44.1 kHz does not: the default N leaves a fraction, so an exact N
        // (a multiple of 336) near it is chosen.
        let d = computed(241_500, Rate::Hz44100).unwrap();
        assert_eq!((d.n, d.cts, d.exact), (6272, 268_333, false));
        let e = select(241_500, Rate::Hz44100).unwrap();
        assert!(e.exact);
        assert_eq!(e.n % 336, 0);
        assert!(e.n.abs_diff(6272) <= 336);
        let (lo, hi) = n_range(Rate::Hz44100);
        assert!((lo..=hi).contains(&e.n));
        // 241500e3 * N / (128 * 44100) is CTS exactly.
        assert_eq!(
            u64::from(e.cts) * 128 * 44_100,
            241_500_000 * u64::from(e.n)
        );
    }

    #[test]
    fn deep_colour_scales_the_clock() {
        assert_eq!(tmds_clock_khz(148_500, 8), 148_500);
        assert_eq!(tmds_clock_khz(148_500, 10), 185_625);
        assert_eq!(tmds_clock_khz(148_500, 12), 222_750);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert!(select(0, Rate::Hz48000).is_none());
        assert!(computed(0, Rate::Hz48000).is_none());
        assert!(exact(0, Rate::Hz48000).is_none());
        assert!(cts_for(u32::MAX, u32::MAX, Rate::Hz32000).is_none());
        let _ = select(u32::MAX, Rate::Hz192000);
        assert_eq!(Rate::from_hz(44_100), Some(Rate::Hz44100));
        assert_eq!(Rate::from_hz(22_050), None);
    }

    #[test]
    fn clock_index_matches_the_table_prefix() {
        for (i, khz) in [25_175, 25_200, 27_000, 27_027, 54_000, 54_054, 74_176, 74_250, 148_352, 148_500]
            .into_iter()
            .enumerate()
        {
            assert_eq!(aud_config_clock_index(khz), Some(i as u32));
            assert!(recommended(khz, Rate::Hz48000).is_some());
        }
        assert_eq!(aud_config_clock_index(241_500), None);
    }
}
