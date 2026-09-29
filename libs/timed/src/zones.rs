//! The built-in zone table and the pure UTC-to-local resolution.
//!
//! A zone is a standard offset plus at most one recurring DST rule. A rule
//! names two transitions per year, each "the nth weekday of a month at a
//! time-of-day read on a stated clock": UTC (the EU rule), local standard time
//! or local daylight time (the US and Australian rules). Southern-hemisphere
//! zones have `start` later in the year than `end`, so DST spans New Year.

use crate::civil::{civil_from_days, nth_weekday};

/// Which clock a transition's time-of-day is read on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clock {
    /// UTC (the EU rule: 01:00 UTC at both transitions).
    Utc,
    /// Local standard time (a DST start "at 02:00", before the clock moves).
    Standard,
    /// Local daylight time (a DST end "at 02:00", before the clock moves back).
    Daylight,
}

/// One yearly transition: the `nth` (1..=4, or `0` for the last) `weekday`
/// (0 = Sunday) of `month`, at `secs` after midnight on `clock`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transition {
    pub month: u32,
    pub nth: u32,
    pub weekday: u32,
    pub secs: i64,
    pub clock: Clock,
}

/// A recurring DST rule: daylight time runs from `start` to `end`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rule {
    pub start: Transition,
    pub end: Transition,
    /// Daylight-saving shift added to the standard offset, in seconds.
    pub save: i32,
}

/// One built-in zone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Zone {
    pub name: &'static str,
    /// Standard offset from UTC in seconds.
    pub std_offset: i32,
    pub std_abbrev: &'static str,
    pub dst_abbrev: &'static str,
    pub rule: Option<Rule>,
}

/// Local-time parameters of a zone at one instant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Local {
    /// Offset from UTC in seconds, DST included.
    pub offset: i32,
    pub dst: bool,
    pub abbrev: &'static str,
}

const HOUR: i64 = 3600;
const SUN: u32 = 0;

const fn at(month: u32, nth: u32, secs: i64, clock: Clock) -> Transition {
    Transition {
        month,
        nth,
        weekday: SUN,
        secs,
        clock,
    }
}

/// EU: last Sunday of March to last Sunday of October, 01:00 UTC.
const EU: Rule = Rule {
    start: at(3, 0, HOUR, Clock::Utc),
    end: at(10, 0, HOUR, Clock::Utc),
    save: 3600,
};

/// US: second Sunday of March 02:00 standard to first Sunday of November
/// 02:00 daylight.
const US: Rule = Rule {
    start: at(3, 2, 2 * HOUR, Clock::Standard),
    end: at(11, 1, 2 * HOUR, Clock::Daylight),
    save: 3600,
};

/// Australia (NSW): first Sunday of October 02:00 standard to first Sunday of
/// April 03:00 daylight.
const AU: Rule = Rule {
    start: at(10, 1, 2 * HOUR, Clock::Standard),
    end: at(4, 1, 3 * HOUR, Clock::Daylight),
    save: 3600,
};

const fn zone(
    name: &'static str,
    hours_tenths: i32,
    std_abbrev: &'static str,
    dst_abbrev: &'static str,
    rule: Option<Rule>,
) -> Zone {
    Zone {
        name,
        std_offset: hours_tenths * 360,
        std_abbrev,
        dst_abbrev,
        rule,
    }
}

/// Every zone `timed` accepts. Offsets are in tenths of an hour for brevity.
pub const ZONES: &[Zone] = &[
    zone("UTC", 0, "UTC", "UTC", None),
    zone("Europe/London", 0, "GMT", "BST", Some(EU)),
    zone("Europe/Paris", 10, "CET", "CEST", Some(EU)),
    zone("Europe/Berlin", 10, "CET", "CEST", Some(EU)),
    zone("America/New_York", -50, "EST", "EDT", Some(US)),
    zone("America/Chicago", -60, "CST", "CDT", Some(US)),
    zone("America/Los_Angeles", -80, "PST", "PDT", Some(US)),
    zone("Asia/Kolkata", 55, "IST", "IST", None),
    zone("Asia/Tokyo", 90, "JST", "JST", None),
    zone("Australia/Sydney", 100, "AEST", "AEDT", Some(AU)),
];

/// The default zone (UTC, the first table row).
pub fn default_zone() -> &'static Zone {
    &ZONES[0]
}

/// The zone called exactly `name`, or `None` (names are case-sensitive and
/// never normalised, so a typo is refused instead of guessed).
pub fn find(name: &str) -> Option<&'static Zone> {
    ZONES.iter().find(|zone| zone.name == name)
}

/// The UTC instant of `transition` in `year` for a zone with `std_offset`.
fn instant(transition: &Transition, year: i64, std_offset: i32, save: i32) -> i64 {
    let day = nth_weekday(year, transition.month, transition.weekday, transition.nth);
    let local = day * 86_400 + transition.secs;
    local
        - match transition.clock {
            Clock::Utc => 0,
            Clock::Standard => i64::from(std_offset),
            Clock::Daylight => i64::from(std_offset) + i64::from(save),
        }
}

/// Whether `zone` is on daylight time at UTC instant `unix`.
fn in_dst(zone: &Zone, unix: i64) -> bool {
    let Some(rule) = &zone.rule else {
        return false;
    };
    // The year is read in local standard time, which every transition is far
    // (months) from a year boundary in, so the start/end pair is the right one.
    let (year, _, _) = civil_from_days((unix + i64::from(zone.std_offset)).div_euclid(86_400));
    let start = instant(&rule.start, year, zone.std_offset, rule.save);
    let end = instant(&rule.end, year, zone.std_offset, rule.save);
    if start < end {
        unix >= start && unix < end
    } else {
        // Southern hemisphere: DST spans the new year.
        unix >= start || unix < end
    }
}

/// The local-time parameters of `zone` at UTC instant `unix`.
pub fn local(zone: &Zone, unix: i64) -> Local {
    if in_dst(zone, unix) {
        let save = zone.rule.map_or(0, |rule| rule.save);
        Local {
            offset: zone.std_offset + save,
            dst: true,
            abbrev: zone.dst_abbrev,
        }
    } else {
        Local {
            offset: zone.std_offset,
            dst: false,
            abbrev: zone.std_abbrev,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civil::days_from_civil;

    fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60
    }

    fn zone_of(name: &str) -> &'static Zone {
        find(name).expect("zone in table")
    }

    #[test]
    fn unknown_and_malformed_names_are_rejected() {
        for bad in [
            "",
            "utc",
            "Europe/Pariss",
            "europe/paris",
            " UTC",
            "UTC ",
            "Mars/Olympus",
            "../etc",
            "Europe/Paris\0",
        ] {
            assert!(find(bad).is_none(), "{bad:?} accepted");
        }
        assert!(find("UTC").is_some());
    }

    #[test]
    fn default_zone_is_utc() {
        assert_eq!(default_zone().name, crate::DEFAULT_ZONE);
        assert_eq!(default_zone().std_offset, 0);
    }

    #[test]
    fn table_names_are_unique_and_sane() {
        for (i, zone) in ZONES.iter().enumerate() {
            assert!(ZONES[i + 1..].iter().all(|other| other.name != zone.name));
            assert!(zone.std_offset.abs() < 15 * 3600);
            assert!(zone.name.len() <= 32);
        }
    }

    #[test]
    fn fixed_offset_zones_never_shift() {
        for name in ["UTC", "Asia/Tokyo", "Asia/Kolkata"] {
            let zone = zone_of(name);
            for month in 1..=12 {
                let got = local(zone, utc(2026, month, 15, 12, 0));
                assert_eq!(got.offset, zone.std_offset);
                assert!(!got.dst);
            }
        }
        assert_eq!(local(zone_of("Asia/Tokyo"), 0).offset, 9 * 3600);
        assert_eq!(local(zone_of("Asia/Kolkata"), 0).offset, 19_800);
        assert_eq!(local(zone_of("UTC"), 0).abbrev, "UTC");
    }

    #[test]
    fn eu_transition_instants() {
        // 2026: starts Sun 29 Mar 01:00 UTC, ends Sun 25 Oct 01:00 UTC.
        let paris = zone_of("Europe/Paris");
        let start = utc(2026, 3, 29, 1, 0);
        let end = utc(2026, 10, 25, 1, 0);
        assert_eq!(local(paris, start - 1).offset, 3600);
        assert!(!local(paris, start - 1).dst);
        assert_eq!(local(paris, start).offset, 7200);
        assert_eq!(local(paris, start).abbrev, "CEST");
        assert_eq!(local(paris, end - 1).offset, 7200);
        assert_eq!(local(paris, end).offset, 3600);
        assert_eq!(local(paris, end).abbrev, "CET");
        // London moves at the same UTC instant.
        let london = zone_of("Europe/London");
        assert_eq!(local(london, start - 1).offset, 0);
        assert_eq!(local(london, start).offset, 3600);
        assert_eq!(local(london, start).abbrev, "BST");
        assert_eq!(local(london, end).abbrev, "GMT");
    }

    #[test]
    fn us_transition_instants() {
        // 2026: starts Sun 8 Mar 02:00 EST (07:00 UTC), ends Sun 1 Nov
        // 02:00 EDT (06:00 UTC).
        let ny = zone_of("America/New_York");
        let start = utc(2026, 3, 8, 7, 0);
        let end = utc(2026, 11, 1, 6, 0);
        assert_eq!(local(ny, start - 1).offset, -5 * 3600);
        assert_eq!(local(ny, start).offset, -4 * 3600);
        assert!(local(ny, start).dst);
        assert_eq!(local(ny, end - 1).offset, -4 * 3600);
        assert_eq!(local(ny, end).offset, -5 * 3600);
        // Los Angeles is three hours later in UTC terms.
        let la = zone_of("America/Los_Angeles");
        let la_start = utc(2026, 3, 8, 10, 0);
        assert_eq!(local(la, la_start - 1).offset, -8 * 3600);
        assert_eq!(local(la, la_start).offset, -7 * 3600);
        assert_eq!(local(la, la_start).abbrev, "PDT");
    }

    #[test]
    fn southern_hemisphere_spans_the_new_year() {
        // Sydney 2026: DST ends Sun 5 Apr 03:00 AEDT (Apr 4 16:00 UTC) and
        // starts Sun 4 Oct 02:00 AEST (Oct 3 16:00 UTC).
        let syd = zone_of("Australia/Sydney");
        let end = utc(2026, 4, 4, 16, 0);
        let start = utc(2026, 10, 3, 16, 0);
        assert_eq!(local(syd, utc(2026, 1, 1, 0, 0)).offset, 11 * 3600);
        assert!(local(syd, utc(2026, 1, 1, 0, 0)).dst);
        assert_eq!(local(syd, end - 1).offset, 11 * 3600);
        assert_eq!(local(syd, end).offset, 10 * 3600);
        assert_eq!(local(syd, end).abbrev, "AEST");
        assert_eq!(local(syd, utc(2026, 7, 1, 0, 0)).offset, 10 * 3600);
        assert_eq!(local(syd, start - 1).offset, 10 * 3600);
        assert_eq!(local(syd, start).offset, 11 * 3600);
        assert_eq!(local(syd, utc(2026, 12, 31, 23, 59)).offset, 11 * 3600);
        // The same instant is summer in Sydney and winter in Paris.
        let jan = utc(2026, 1, 15, 0, 0);
        assert!(local(syd, jan).dst && !local(zone_of("Europe/Paris"), jan).dst);
    }

    #[test]
    fn year_boundaries_and_extremes_do_not_panic() {
        for zone in ZONES {
            for unix in [
                0,
                -1,
                i32::MIN as i64,
                utc(1999, 12, 31, 23, 59),
                utc(2038, 1, 19, 3, 14),
                utc(2199, 12, 31, 23, 59),
            ] {
                let got = local(zone, unix);
                assert!((got.offset - zone.std_offset).abs() <= 3600);
            }
        }
    }

    #[test]
    fn dst_hours_per_year_match_the_calendar() {
        // Soak: walk 2020-2035 hourly; DST must be one contiguous span per
        // year (two edges for a northern zone, two for a southern one).
        for name in ["Europe/Paris", "America/New_York", "Australia/Sydney"] {
            let zone = zone_of(name);
            let mut edges = 0;
            let mut prev = local(zone, utc(2020, 1, 1, 0, 0)).dst;
            let mut t = utc(2020, 1, 1, 1, 0);
            while t < utc(2036, 1, 1, 0, 0) {
                let now = local(zone, t).dst;
                if now != prev {
                    edges += 1;
                }
                prev = now;
                t += 3600;
            }
            assert_eq!(edges, 16 * 2, "{name}: {edges} DST edges");
        }
    }
}
