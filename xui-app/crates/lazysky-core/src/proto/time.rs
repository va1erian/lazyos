//! Timestamps as the protocol writes them and the short ages a feed shows.

/// Unix seconds of an RFC 3339 time (`2026-10-10T12:00:00.000Z`, or with a
/// `+HH:MM` offset). `None` for anything else; fractions are dropped.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut at = 19;
    if b.get(at) == Some(&b'.') {
        at += 1;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    let offset_minutes = match b.get(at)? {
        b'Z' | b'z' if at + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            let minutes = num(at + 1, at + 3)? * 60 + num(at + 4, at + 6)?;
            if *sign == b'+' {
                minutes
            } else {
                -minutes
            }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second.min(59) - offset_minutes * 60;
    u64::try_from(secs).ok()
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "now", "5m", "3h", "2d", "6w", "1y": how long before `now` (Unix
/// seconds) a post was written. A time in the future reads as "now".
pub fn short_age(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        604_800..=31_535_999 => format!("{}w", secs / 604_800),
        _ => format!("{}y", secs / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::record::rfc3339_utc;

    #[test]
    fn rfc3339_round_trips() {
        for secs in [0u64, 951_782_400, 1_791_711_296, 4_102_444_800] {
            assert_eq!(parse_rfc3339(&rfc3339_utc(secs)), Some(secs));
        }
    }

    #[test]
    fn offsets_and_fractions() {
        assert_eq!(
            parse_rfc3339("2026-10-11T11:34:56+02:00"),
            parse_rfc3339("2026-10-11T09:34:56Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-11T09:34:56.123456Z"),
            Some(1_791_711_296)
        );
        assert_eq!(
            parse_rfc3339("2026-10-11T04:34:56-05:00"),
            Some(1_791_711_296)
        );
    }

    #[test]
    fn nonsense_is_none() {
        for s in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-10-11T25:00:00Z",
            "2026-10-11T00:00:00",
            "2026-10-11T00:00:00Zjunk",
            "20261011T000000Z",
            "2026-10-11T00:00:00+0200",
        ] {
            assert_eq!(parse_rfc3339(s), None, "{s:?}");
        }
    }

    #[test]
    fn ages_are_short() {
        let now = 1_000_000_000;
        assert_eq!(short_age(now, now + 5), "now");
        assert_eq!(short_age(now, now - 59), "now");
        assert_eq!(short_age(now, now - 300), "5m");
        assert_eq!(short_age(now, now - 3 * 3600), "3h");
        assert_eq!(short_age(now, now - 2 * 86_400), "2d");
        assert_eq!(short_age(now, now - 21 * 86_400), "3w");
        assert_eq!(short_age(now, now - 400 * 86_400), "1y");
    }
}
