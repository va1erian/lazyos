//! Calendar arithmetic for archive timestamps: zip's MS-DOS date and time,
//! 7z's Windows FILETIME, and a `YYYY-MM-DD HH:MM` display form. Times are
//! UTC throughout (an archive does not say which zone a DOS time was in).

/// Days from 1970-01-01 to `year-month-day` (proleptic Gregorian).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of day `days` since 1970-01-01: `(year, month, day)`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// An MS-DOS `(date, time)` pair as Unix seconds, or `None` when either half
/// is out of range (a zero date is common in hand-made zips).
pub fn from_dos(date: u16, time: u16) -> Option<i64> {
    let year = 1980 + i64::from(date >> 9);
    let month = u32::from((date >> 5) & 0x0f);
    let day = u32::from(date & 0x1f);
    let hour = i64::from(time >> 11);
    let minute = i64::from((time >> 5) & 0x3f);
    let second = i64::from(time & 0x1f) * 2;
    if !(1..=12).contains(&month) || day == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Unix seconds as an MS-DOS `(date, time)` pair, clamped to DOS's range
/// (1980–2107).
pub fn to_dos(unix: i64) -> (u16, u16) {
    let min = days_from_civil(1980, 1, 1) * 86_400;
    let max = days_from_civil(2107, 12, 31) * 86_400 + 86_399;
    let unix = unix.clamp(min, max);
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = (((secs / 3600) as u16) << 11)
        | ((((secs / 60) % 60) as u16) << 5)
        | ((secs % 60) / 2) as u16;
    (date, time)
}

/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_EPOCH: i64 = 116_444_736_000_000_000;

/// A Windows FILETIME as Unix seconds.
pub fn from_filetime(ticks: u64) -> Option<i64> {
    let ticks = i64::try_from(ticks).ok()?;
    Some((ticks - FILETIME_EPOCH).div_euclid(10_000_000))
}

/// `YYYY-MM-DD HH:MM` for Unix seconds.
pub fn display(unix: i64) -> String {
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for days in [-1000, 0, 1, 10_957, 20_000, 50_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn dos_times_round_trip_to_two_seconds() {
        let unix = days_from_civil(2026, 10, 4) * 86_400 + 13 * 3600 + 7 * 60 + 42;
        let (date, time) = to_dos(unix);
        assert_eq!(from_dos(date, time), Some(unix));
        assert_eq!(display(unix), "2026-10-04 13:07");
    }

    #[test]
    fn an_invalid_dos_date_is_unknown() {
        assert_eq!(from_dos(0, 0), None);
    }

    #[test]
    fn filetime_epoch_is_the_unix_epoch() {
        assert_eq!(from_filetime(FILETIME_EPOCH as u64), Some(0));
    }
}
