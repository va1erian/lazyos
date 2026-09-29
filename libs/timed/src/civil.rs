//! Proleptic-Gregorian calendar arithmetic (Hinnant's algorithms), the same
//! maths the kernel's `wallclock` module uses, kept here so it is host-testable.

/// Days from 1970-01-01 to the civil date `year-month-day`.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: `(year, month 1..=12, day 1..=31)`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days in `month` of `year`.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}

/// Weekday of a day count since the epoch, `0` = Sunday (1970-01-01 was a
/// Thursday).
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// The day count of the `nth` `weekday` of `month` (`nth` 1..=4), or the last
/// one when `nth` is `0`.
pub fn nth_weekday(year: i64, month: u32, weekday_wanted: u32, nth: u32) -> i64 {
    if nth == 0 {
        let last = days_from_civil(year, month, days_in_month(year, month));
        let back = (weekday(last) + 7 - weekday_wanted) % 7;
        return last - i64::from(back);
    }
    let first = days_from_civil(year, month, 1);
    let ahead = (weekday_wanted + 7 - weekday(first)) % 7;
    first + i64::from(ahead) + 7 * i64::from(nth - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 1, 1) * 86_400, 1_767_225_600);
        assert_eq!(weekday(0), 4);
        assert_eq!(weekday(days_from_civil(2026, 3, 29)), 0);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
    }

    #[test]
    fn every_day_roundtrips() {
        for days in -800_000..800_000i64 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn nth_and_last_weekday() {
        // 2026: last Sunday of March is the 29th; second Sunday is the 8th.
        assert_eq!(nth_weekday(2026, 3, 0, 0), days_from_civil(2026, 3, 29));
        assert_eq!(nth_weekday(2026, 3, 0, 2), days_from_civil(2026, 3, 8));
        // First Sunday of November 2026 is the 1st (a Sunday itself).
        assert_eq!(nth_weekday(2026, 11, 0, 1), days_from_civil(2026, 11, 1));
        // Last Sunday of October 2026 is the 25th.
        assert_eq!(nth_weekday(2026, 10, 0, 0), days_from_civil(2026, 10, 25));
    }
}
