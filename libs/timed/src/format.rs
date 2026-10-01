//! Taskbar clock text: `Tue 29 Sep 2026  21:04`, formatted into a fixed buffer.
//!
//! [`ClockFormat`] selects 24- or 12-hour time and whether seconds show
//! (`Tue 29 Sep 2026  9:04:59 PM`). The compositor formats on every poll and
//! the user bump allocator never reclaims, so the text lives in a small
//! inline array instead of a `String`.

use core::fmt::{self, Write};

use crate::civil::{civil_from_days, weekday};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The longest text [`format_clock_as`] can produce, e.g.
/// `Wed 30 Sep 2026  12:59:59 PM` (years beyond four digits are clamped so the
/// bound holds).
pub const CLOCK_TEXT_MAX: usize = 32;

/// How the time of day is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockFormat {
    /// `23:04` rather than `11:04 PM`.
    pub hour24: bool,
    /// Append `:SS`.
    pub seconds: bool,
}

impl Default for ClockFormat {
    /// 24-hour, no seconds: the original taskbar clock.
    fn default() -> ClockFormat {
        ClockFormat {
            hour24: true,
            seconds: false,
        }
    }
}

impl ClockFormat {
    /// The widest line this format produces, for reserving screen space so
    /// the layout never shifts as the digits change.
    pub const fn widest(self) -> &'static str {
        match (self.hour24, self.seconds) {
            (true, false) => "Wed 30 Sep 2026  00:00",
            (true, true) => "Wed 30 Sep 2026  00:00:00",
            (false, false) => "Wed 30 Sep 2026  00:00 PM",
            (false, true) => "Wed 30 Sep 2026  00:00:00 PM",
        }
    }
}

/// A formatted clock line held inline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockText {
    buf: [u8; CLOCK_TEXT_MAX],
    len: usize,
}

impl ClockText {
    pub fn as_str(&self) -> &str {
        // The writer only ever appends ASCII from `write_str`, so this holds.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for ClockText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len + text.len();
        if end > CLOCK_TEXT_MAX {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Format `local_unix` (seconds since the epoch, already shifted by the zone
/// offset) as `Www DD Mon YYYY  HH:MM`.
pub fn format_clock(local_unix: i64) -> ClockText {
    format_clock_as(local_unix, ClockFormat::default())
}

/// Format `local_unix` in `format`: `Www DD Mon YYYY  ` then `HH:MM[:SS]`
/// (24-hour) or `H:MM[:SS] AM|PM` (12-hour, no leading zero, 12 for noon and
/// midnight).
pub fn format_clock_as(local_unix: i64, format: ClockFormat) -> ClockText {
    let days = local_unix.div_euclid(86_400);
    let secs = local_unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let mut text = ClockText {
        buf: [0; CLOCK_TEXT_MAX],
        len: 0,
    };
    // The buffer fits every year up to 9999; clamp so a garbage clock cannot
    // overflow it (the write then simply stops).
    let year = year.clamp(0, 9999);
    let _ = write!(
        text,
        "{} {:02} {} {:04}  ",
        WEEKDAYS[weekday(days) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
    );
    let (hour, minute, second) = (secs / 3600, (secs / 60) % 60, secs % 60);
    let _ = if format.hour24 {
        write!(text, "{hour:02}:{minute:02}")
    } else {
        let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
        write!(text, "{twelve}:{minute:02}")
    };
    if format.seconds {
        let _ = write!(text, ":{second:02}");
    }
    if !format.hour24 {
        let _ = text.write_str(if hour < 12 { " AM" } else { " PM" });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(format_clock(0).as_str(), "Thu 01 Jan 1970  00:00");
        // 2026-09-29 21:04:59 UTC.
        assert_eq!(
            format_clock(1_790_715_899).as_str(),
            "Tue 29 Sep 2026  21:04"
        );
        // 2000-02-29 (leap day), 23:59.
        assert_eq!(format_clock(951_868_799).as_str(), "Tue 29 Feb 2000  23:59");
    }

    #[test]
    fn local_offset_can_cross_midnight() {
        // 23:30 UTC on Dec 31 is already Jan 1 in +02:00.
        let utc = 1_798_759_800; // 2026-12-31 23:30:00 UTC
        assert_eq!(format_clock(utc).as_str(), "Thu 31 Dec 2026  23:30");
        assert_eq!(format_clock(utc + 7200).as_str(), "Fri 01 Jan 2027  01:30");
    }

    const H12: ClockFormat = ClockFormat {
        hour24: false,
        seconds: false,
    };
    const H24_SECONDS: ClockFormat = ClockFormat {
        hour24: true,
        seconds: true,
    };

    #[test]
    fn twelve_hour_and_seconds() {
        // 2026-09-29 21:04:59 UTC.
        let t = 1_790_715_899;
        assert_eq!(format_clock_as(t, H12).as_str(), "Tue 29 Sep 2026  9:04 PM");
        assert_eq!(
            format_clock_as(t, H24_SECONDS).as_str(),
            "Tue 29 Sep 2026  21:04:59"
        );
        let both = ClockFormat {
            hour24: false,
            seconds: true,
        };
        assert_eq!(
            format_clock_as(t, both).as_str(),
            "Tue 29 Sep 2026  9:04:59 PM"
        );
        // Midnight and noon are 12, not 0.
        assert_eq!(
            format_clock_as(0, H12).as_str(),
            "Thu 01 Jan 1970  12:00 AM"
        );
        assert_eq!(
            format_clock_as(12 * 3600, H12).as_str(),
            "Thu 01 Jan 1970  12:00 PM"
        );
        assert_eq!(format_clock(t), format_clock_as(t, ClockFormat::default()));
    }

    #[test]
    fn widest_is_at_least_as_long_as_any_line() {
        for hour24 in [false, true] {
            for seconds in [false, true] {
                let format = ClockFormat { hour24, seconds };
                for t in [0, 12 * 3600 - 1, 23 * 3600 + 59 * 60 + 59, 1_790_715_899] {
                    let text = format_clock_as(t, format);
                    assert!(text.as_str().len() <= format.widest().len(), "{text:?}");
                }
                assert!(format.widest().len() <= CLOCK_TEXT_MAX);
            }
        }
    }

    #[test]
    fn never_overflows_the_buffer() {
        for t in [i64::MIN / 2, -1, i64::MAX / 2] {
            assert!(format_clock(t).as_str().len() <= CLOCK_TEXT_MAX);
            let wide = ClockFormat {
                hour24: false,
                seconds: true,
            };
            assert!(format_clock_as(t, wide).as_str().len() <= CLOCK_TEXT_MAX);
        }
        // The widest realistic line fits with room to spare.
        assert!(format_clock(1_790_715_899).as_str().len() <= CLOCK_TEXT_MAX);
    }
}
