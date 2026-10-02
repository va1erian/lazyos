//! The taskbar clock text (the logic `xuid`'s `clock.rs` had, issues #370 and
//! #157).
//!
//! The instant comes from the kernel wall clock (UTC), so the clock is right
//! even without `timed`; `timed` only names the zone, and the offset (with
//! DST) is applied here from the shared `timezone` table, so a DST change
//! lands on the right minute. An unknown or absent zone name means UTC.
//!
//! The format (24- or 12-hour, seconds shown) is the Settings app's Time &
//! Date page: the confd keys [`CLOCK24_KEY`] and [`SHOW_SECONDS_KEY`], read
//! through [`format_from`].

use confd::Value;
use timezone::format::format_clock_as;

pub use timezone::{ClockFormat, CLOCK24_KEY, SHOW_SECONDS_KEY};

/// The clock line for UTC second `unix` in the zone called `zone`, e.g.
/// `Tue 29 Sep 2026  21:04` or `Tue 29 Sep 2026  9:04:59 PM`.
pub fn text(unix: i64, zone: Option<&str>, format: ClockFormat) -> String {
    let zone = zone
        .and_then(timezone::find)
        .unwrap_or_else(timezone::default_zone);
    let local = unix + i64::from(timezone::local(zone, unix).offset);
    String::from(format_clock_as(local, format).as_str())
}

/// The widest line `format` produces; the bar reserves its width so the
/// window entries never shift when the digits change.
pub const fn widest(format: ClockFormat) -> &'static str {
    format.widest()
}

/// The format the confd values of [`CLOCK24_KEY`] and [`SHOW_SECONDS_KEY`]
/// select; a missing or mistyped value keeps that flag's default.
pub fn format_from(clock24: Option<&Value>, seconds: Option<&Value>) -> ClockFormat {
    let defaults = ClockFormat::default();
    let flag = |value: Option<&Value>, default| match value {
        Some(Value::Bool(on)) => *on,
        _ => default,
    };
    ClockFormat {
        hour24: flag(clock24, defaults.hour24),
        seconds: flag(seconds, defaults.seconds),
    }
}

/// UTC whole seconds from the kernel's centisecond wall clock.
pub const fn unix_from_centis(centis: u64) -> i64 {
    (centis / 100) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-29 19:04:00 UTC (a Tuesday).
    const T: i64 = 1_790_708_640;

    #[test]
    fn utc_is_the_default_and_unknown_zones_fall_back_to_it() {
        let f = ClockFormat::default();
        assert_eq!(text(T, None, f), "Tue 29 Sep 2026  19:04");
        assert_eq!(text(T, Some("Mars/Olympus"), f), "Tue 29 Sep 2026  19:04");
    }

    #[test]
    fn a_known_zone_applies_its_offset_with_dst() {
        let f = ClockFormat::default();
        // Paris is on summer time (UTC+2) at the end of September.
        assert_eq!(text(T, Some("Europe/Paris"), f), "Tue 29 Sep 2026  21:04");
        // Tokyo has no DST (UTC+9): the date rolls over.
        assert_eq!(text(T, Some("Asia/Tokyo"), f), "Wed 30 Sep 2026  04:04");
    }

    #[test]
    fn twelve_hour_time_with_seconds() {
        let f = ClockFormat {
            hour24: false,
            seconds: true,
        };
        assert_eq!(
            text(T + 59, Some("Europe/Paris"), f),
            "Tue 29 Sep 2026  9:04:59 PM"
        );
        assert!(text(T, None, f).len() <= widest(f).len());
    }

    #[test]
    fn the_format_comes_from_the_confd_flags() {
        assert_eq!(format_from(None, None), ClockFormat::default());
        let on = Value::Bool(true);
        let off = Value::Bool(false);
        let f = format_from(Some(&off), Some(&on));
        assert!(!f.hour24 && f.seconds);
        // A mistyped value keeps the default.
        let junk = Value::Str("no".into());
        assert_eq!(
            format_from(Some(&junk), Some(&junk)),
            ClockFormat::default()
        );
    }

    #[test]
    fn centiseconds_truncate_to_seconds() {
        assert_eq!(unix_from_centis(12_345), 123);
        let f = ClockFormat::default();
        assert!(text(0, None, f).len() <= widest(f).len());
    }
}
