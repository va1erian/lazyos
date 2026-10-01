//! The taskbar clock text (the logic `xuid`'s `clock.rs` had, issue #370).
//!
//! The instant comes from the kernel wall clock (UTC), so the clock is right
//! even without `timed`; `timed` only names the zone, and the offset (with
//! DST) is applied here from the shared `timezone` table, so a DST change
//! lands on the right minute. An unknown or absent zone name means UTC.

use timezone::format::format_clock;

/// The widest line the format produces; the bar reserves its width so the
/// window entries never shift when the digits change.
pub const WIDEST: &str = "Wed 30 Sep 2026  00:00";

/// The clock line for UTC second `unix` in the zone called `zone`, e.g.
/// `Tue 29 Sep 2026  21:04`.
pub fn text(unix: i64, zone: Option<&str>) -> String {
    let zone = zone
        .and_then(timezone::find)
        .unwrap_or_else(timezone::default_zone);
    let local = unix + i64::from(timezone::local(zone, unix).offset);
    String::from(format_clock(local).as_str())
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
        assert_eq!(text(T, None), "Tue 29 Sep 2026  19:04");
        assert_eq!(text(T, Some("Mars/Olympus")), "Tue 29 Sep 2026  19:04");
    }

    #[test]
    fn a_known_zone_applies_its_offset_with_dst() {
        // Paris is on summer time (UTC+2) at the end of September.
        assert_eq!(text(T, Some("Europe/Paris")), "Tue 29 Sep 2026  21:04");
        // Tokyo has no DST (UTC+9): the date rolls over.
        assert_eq!(text(T, Some("Asia/Tokyo")), "Wed 30 Sep 2026  04:04");
    }

    #[test]
    fn centiseconds_truncate_to_seconds() {
        assert_eq!(unix_from_centis(12_345), 123);
        assert!(text(0, None).len() <= WIDEST.len());
    }
}
