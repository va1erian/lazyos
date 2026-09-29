//! Taskbar clock text: `Tue 29 Sep 2026  21:04`, formatted into a fixed buffer.
//!
//! The compositor calls this once a minute and the user bump allocator never
//! reclaims, so the text lives in a small inline array instead of a `String`.

use core::fmt::{self, Write};

use crate::civil::{civil_from_days, weekday};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The longest text [`format_clock`] can produce, e.g. `Wed 30 Sep 2026  23:59`
/// (years beyond four digits are clamped so the bound holds).
pub const CLOCK_TEXT_MAX: usize = 24;

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
        "{} {:02} {} {:04}  {:02}:{:02}",
        WEEKDAYS[weekday(days) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        secs / 3600,
        (secs / 60) % 60,
    );
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

    #[test]
    fn never_overflows_the_buffer() {
        for t in [i64::MIN / 2, -1, i64::MAX / 2] {
            assert!(format_clock(t).as_str().len() <= CLOCK_TEXT_MAX);
        }
        // The widest realistic line fits with room to spare.
        assert!(format_clock(1_790_715_899).as_str().len() <= CLOCK_TEXT_MAX);
    }
}
