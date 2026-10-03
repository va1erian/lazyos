//! UTC date formatting for clock and validity messages, without a date
//! library: certificate errors must show the system time and the
//! certificate's bounds in a form a person can compare at a glance.

use std::time::{SystemTime, UNIX_EPOCH};

/// `secs` since the Unix epoch as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// The current system time, formatted by [`utc`].
pub fn now_utc() -> String {
    utc(now_secs())
}

/// Seconds since the epoch now; 0 for a clock before 1970.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`, exact for every `i64` day count in range here).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_767_225_600), "2026-01-01 00:00:00 UTC");
        assert_eq!(utc(1_791_051_045), "2026-10-03 18:10:45 UTC");
        assert_eq!(utc(4_102_444_799), "2099-12-31 23:59:59 UTC");
    }
}
