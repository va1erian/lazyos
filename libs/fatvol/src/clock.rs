//! Time for a FAT volume: unix seconds in, DOS date and time out and back.
//!
//! A FAT directory entry holds a local-time stamp with no zone. LazyOS keeps
//! no zone of its own at this layer, so a stamp is read and written as UTC:
//! a file made here shows the same wall-clock time here every time, which is
//! all a daemon with no zone database can promise.

use alloc::rc::Rc;
use core::cell::Cell;

use fatfs::{Date, DateTime, Time, TimeProvider};

/// The earliest and latest instants a DOS stamp holds.
const DOS_EPOCH_YEAR: i64 = 1980;
const DOS_LAST_YEAR: i64 = 2107;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted = (month + 9) % 12;
    let doy = (153 * shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse: `(year, month, day)` of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The DOS stamp of `unix` seconds, clamped to what a stamp can hold.
pub fn to_datetime(unix: i64) -> DateTime {
    let first = days_from_civil(DOS_EPOCH_YEAR, 1, 1) * 86_400;
    let last = days_from_civil(DOS_LAST_YEAR + 1, 1, 1) * 86_400 - 1;
    let unix = unix.clamp(first, last);
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let second_of_day = unix.rem_euclid(86_400);
    let date = Date::new(year as u16, month as u16, day as u16);
    let time = Time::new(
        (second_of_day / 3600) as u16,
        (second_of_day % 3600 / 60) as u16,
        (second_of_day % 60) as u16,
        0,
    );
    DateTime::new(date, time)
}

/// Unix seconds of a DOS date (midnight).
pub fn date_to_unix(date: Date) -> i64 {
    days_from_civil(i64::from(date.year), i64::from(date.month), i64::from(date.day)) * 86_400
}

/// Unix seconds of a DOS date and time.
pub fn to_unix(stamp: DateTime) -> i64 {
    date_to_unix(stamp.date)
        + i64::from(stamp.time.hour) * 3600
        + i64::from(stamp.time.min) * 60
        + i64::from(stamp.time.sec)
}

/// The volume's clock: the daemon stores the wall time here before each
/// request, so the library never calls a syscall of its own.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    now: Rc<Cell<i64>>,
}

impl Clock {
    pub fn new(unix: i64) -> Clock {
        Clock {
            now: Rc::new(Cell::new(unix)),
        }
    }

    pub fn set(&self, unix: i64) {
        self.now.set(unix);
    }

    pub fn get(&self) -> i64 {
        self.now.get()
    }
}

impl TimeProvider for Clock {
    fn get_current_date(&self) -> Date {
        to_datetime(self.now.get()).date
    }

    fn get_current_date_time(&self) -> DateTime {
        to_datetime(self.now.get())
    }
}
