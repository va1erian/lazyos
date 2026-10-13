//! Time & Date logic: parsing what the user typed, local/UTC conversion, and
//! the taskbar clock format keys (`sys/time/*`, followed live by the
//! LazyShell taskbar).
//!
//! The wall clock and the zone belong to `timed` ([`System`]); the clock
//! format is plain confd keys, so it goes through the [`ConfigStore`].

use timezone::civil::{civil_from_days, days_from_civil, days_in_month};
use timezone::format::{format_clock_as, ClockFormat};
use timezone::{Zone, ZONES};

use crate::store::{ConfigStore, StoreError, Value};
use crate::system::{Now, NtpState, NtpStatus};

/// Earliest year the date field accepts (the kernel clock's own floor is
/// 1970; anything before the RTC's 19xx century is a typo).
const MIN_YEAR: i64 = 1970;
/// Last year accepted: the kernel refuses 2200 and later.
const MAX_YEAR: i64 = 2199;

/// `YYYY-MM-DD` as `(year, month, day)`, validated against the calendar.
pub fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    let text = text.trim();
    // Digits and separators only: `parse` would also accept a `+` sign.
    if !text.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    let mut parts = text.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return None;
    }
    let valid = (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day);
    valid.then_some((year, month, day))
}

/// `HH:MM` or `HH:MM:SS` (24-hour) as seconds since midnight.
pub fn parse_time(text: &str) -> Option<i64> {
    let fields: Vec<&str> = text.trim().split(':').collect();
    if !(2..=3).contains(&fields.len()) {
        return None;
    }
    let mut values = [0i64; 3];
    for (slot, field) in values.iter_mut().zip(&fields) {
        // Digits only: `parse` would also accept a sign.
        if field.is_empty() || field.len() > 2 || !field.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = field.parse().ok()?;
    }
    let [hour, minute, second] = values;
    (hour < 24 && minute < 60 && second < 60).then_some(hour * 3600 + minute * 60 + second)
}

/// The UTC instant of local wall time `local` (seconds since the epoch, as
/// read off a clock) in `zone`. The offset is looked up at the standard-time
/// guess, so a time inside a DST gap or overlap resolves to one of the two
/// readings instead of failing.
pub fn local_to_utc(zone: &Zone, local: i64) -> i64 {
    let guess = local - i64::from(zone.std_offset);
    local - i64::from(timezone::local(zone, guess).offset)
}

/// The UTC instant for the typed date and time in `zone`, or a message for
/// the status line.
pub fn typed_instant(zone: &Zone, date: &str, time: &str) -> Result<i64, String> {
    let (year, month, day) =
        parse_date(date).ok_or_else(|| String::from("Enter the date as YYYY-MM-DD."))?;
    let secs = parse_time(time).ok_or_else(|| String::from("Enter the time as HH:MM:SS."))?;
    Ok(local_to_utc(
        zone,
        days_from_civil(year, month, day) * 86_400 + secs,
    ))
}

/// `(YYYY-MM-DD, HH:MM:SS)` of `now` in its own zone, to prefill the fields.
pub fn fields(now: &Now) -> (String, String) {
    let local = now.unix + i64::from(now.offset);
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60
        ),
    )
}

/// `+02:00`-style text for `offset` seconds.
pub fn offset_text(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The one-line summary the page shows: `Thu 01 Oct 2026  14:03:07
/// (Europe/Paris, UTC+02:00)`.
pub fn summary(now: &Now) -> String {
    let seconds = ClockFormat {
        hour24: true,
        seconds: true,
    };
    let text = format_clock_as(now.unix + i64::from(now.offset), seconds);
    format!(
        "{}  ({}, {})",
        text.as_str(),
        now.zone,
        offset_text(now.offset)
    )
}

/// Index into [`ZONES`] of `name`.
pub fn zone_index(name: &str) -> Option<usize> {
    ZONES.iter().position(|zone| zone.name == name)
}

fn flag(store: &dyn ConfigStore, key: &str, default: bool) -> bool {
    match store.get(key) {
        Some(Value::Bool(value)) => value,
        _ => default,
    }
}

/// The stored taskbar clock format (missing keys are the defaults).
pub fn clock_format(store: &dyn ConfigStore) -> ClockFormat {
    let defaults = ClockFormat::default();
    ClockFormat {
        hour24: flag(store, timezone::CLOCK24_KEY, defaults.hour24),
        seconds: flag(store, timezone::SHOW_SECONDS_KEY, defaults.seconds),
    }
}

/// Store 24-hour (`true`) or 12-hour time.
pub fn set_clock24(store: &dyn ConfigStore, on: bool) -> Result<(), StoreError> {
    store.set(timezone::CLOCK24_KEY, Value::Bool(on))
}

/// Store whether the taskbar clock shows seconds.
pub fn set_show_seconds(store: &dyn ConfigStore, on: bool) -> Result<(), StoreError> {
    store.set(timezone::SHOW_SECONDS_KEY, Value::Bool(on))
}

/// Whether automatic time is on: `sys/time/ntp/enabled`, on when absent,
/// exactly as `timed` reads it.
pub fn ntp_enabled(store: &dyn ConfigStore) -> bool {
    flag(store, sntp::ENABLED_KEY, true)
}

/// Switch automatic time on or off.
pub fn set_ntp_enabled(store: &dyn ConfigStore, on: bool) -> Result<(), StoreError> {
    store.set(sntp::ENABLED_KEY, Value::Bool(on))
}

/// The configured server list, as typed (the default when none is stored).
pub fn ntp_servers(store: &dyn ConfigStore) -> String {
    match store.get(sntp::SERVERS_KEY) {
        Some(Value::Str(text)) if !text.trim().is_empty() => text,
        _ => String::from(sntp::DEFAULT_SERVER),
    }
}

/// Store the server list: one to [`sntp::servers::MAX_SERVERS`] entries, each
/// a name or address with an optional `:port`, which is what `timed` would
/// use; anything else is refused by name rather than silently skipped.
pub fn set_ntp_servers(store: &dyn ConfigStore, text: &str) -> Result<(), String> {
    let entries: Vec<&str> = text
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|entry| !entry.is_empty())
        .collect();
    let value = if entries.is_empty() {
        String::from(sntp::DEFAULT_SERVER)
    } else {
        if entries.len() > sntp::servers::MAX_SERVERS {
            return Err(format!(
                "Use at most {} servers.",
                sntp::servers::MAX_SERVERS
            ));
        }
        if let Some(bad) = entries
            .iter()
            .find(|entry| sntp::servers::parse(entry).count() != 1)
        {
            return Err(format!(
                "\"{bad}\" is not a server name (a host name or address, then optionally :port)."
            ));
        }
        entries.join(" ")
    };
    store
        .set(sntp::SERVERS_KEY, Value::Str(value))
        .map_err(|error| error.to_string())
}

/// `3 min ago`, `just now`, `2 h ago`.
pub fn age_text(seconds: i64) -> String {
    match seconds {
        ..=4 => String::from("just now"),
        5..=59 => format!("{seconds} s ago"),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// `12 ms` or `3.4 s`, signed.
pub fn offset_ms_text(ms: i64) -> String {
    let sign = if ms < 0 { "-" } else { "" };
    let abs = ms.unsigned_abs();
    if abs < 1_000 {
        format!("{sign}{abs} ms")
    } else {
        format!("{sign}{}.{} s", abs / 1_000, (abs % 1_000) / 100)
    }
}

/// The status line under the automatic time switch, short enough for the
/// page's half-width column. `on` is what the store says (so the line follows
/// a click at once, before `timed` has caught up); `status` is what `timed`
/// reports, `now_unix` the current time.
pub fn ntp_summary(on: bool, status: Option<&NtpStatus>, now_unix: i64) -> String {
    if !on {
        return String::from("Automatic time is off.");
    }
    let Some(status) = status else {
        return String::from("The time service is not running.");
    };
    let last = (status.last_sync_unix > 0).then(|| {
        format!(
            "Synchronized {} ({} off)",
            age_text(now_unix - status.last_sync_unix),
            offset_ms_text(status.offset_ms),
        )
    });
    match status.state {
        NtpState::Disabled => String::from("Turning automatic time on..."),
        NtpState::WaitingForNetwork => String::from("Waiting for the network."),
        NtpState::Syncing => last.unwrap_or_else(|| String::from("Contacting a time server...")),
        NtpState::Synchronized => last.unwrap_or_else(|| String::from("Synchronized.")),
        NtpState::Failed => match last {
            Some(text) => format!("No answer; trying again. {text}"),
            None => String::from("No answer from a time server."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn status(state: NtpState) -> NtpStatus {
        NtpStatus {
            enabled: true,
            state,
            server: "pool.ntp.org (162.159.200.1)".into(),
            offset_ms: -12,
            last_sync_unix: 1_000,
        }
    }

    #[test]
    fn automatic_time_defaults_on_and_round_trips() {
        let store = MemStore::new();
        assert!(ntp_enabled(&store));
        assert_eq!(ntp_servers(&store), "pool.ntp.org");
        set_ntp_enabled(&store, false).unwrap();
        assert!(!ntp_enabled(&store));
        store
            .set(sntp::ENABLED_KEY, Value::Str("no".into()))
            .unwrap();
        assert!(
            ntp_enabled(&store),
            "a value of the wrong kind is the default"
        );
    }

    #[test]
    fn servers_are_checked_like_timed_checks_them() {
        let store = MemStore::new();
        set_ntp_servers(&store, " time.example.net ,10.0.2.2:1123  ").unwrap();
        assert_eq!(ntp_servers(&store), "time.example.net 10.0.2.2:1123");
        // Empty means the default, stored explicitly.
        set_ntp_servers(&store, "  ").unwrap();
        assert_eq!(ntp_servers(&store), "pool.ntp.org");
        set_ntp_servers(&store, "keep.example").unwrap();
        for bad in ["a b c d e", "ok.example bad/host", "x:0", "[::1]", "-lead"] {
            assert!(set_ntp_servers(&store, bad).is_err(), "{bad}");
            assert_eq!(
                ntp_servers(&store),
                "keep.example",
                "{bad} must not be stored"
            );
        }
        let message = set_ntp_servers(&store, "good bad/host").unwrap_err();
        assert!(message.contains("bad/host"), "{message}");
    }

    #[test]
    fn ages_and_offsets_read_naturally() {
        assert_eq!(age_text(0), "just now");
        assert_eq!(age_text(30), "30 s ago");
        assert_eq!(age_text(180), "3 min ago");
        assert_eq!(age_text(7_300), "2 h ago");
        assert_eq!(age_text(3 * 86_400), "3 d ago");
        assert_eq!(age_text(-5), "just now");
        assert_eq!(offset_ms_text(12), "12 ms");
        assert_eq!(offset_ms_text(-12), "-12 ms");
        assert_eq!(offset_ms_text(3_450), "3.4 s");
        assert_eq!(offset_ms_text(-86_400_000), "-86400.0 s");
    }

    #[test]
    fn the_status_line_follows_the_store_first() {
        let synced = status(NtpState::Synchronized);
        assert_eq!(
            ntp_summary(false, Some(&synced), 1_180),
            "Automatic time is off."
        );
        assert_eq!(
            ntp_summary(true, Some(&synced), 1_180),
            "Synchronized 3 min ago (-12 ms off)"
        );
        assert_eq!(
            ntp_summary(true, None, 0),
            "The time service is not running."
        );
        assert!(ntp_summary(true, Some(&status(NtpState::Disabled)), 0).contains("Turning"));
        assert_eq!(
            ntp_summary(true, Some(&status(NtpState::WaitingForNetwork)), 0),
            "Waiting for the network."
        );
        let mut fresh = status(NtpState::Syncing);
        fresh.last_sync_unix = 0;
        assert_eq!(
            ntp_summary(true, Some(&fresh), 0),
            "Contacting a time server..."
        );
        let failed = ntp_summary(true, Some(&status(NtpState::Failed)), 1_100);
        assert!(failed.starts_with("No answer"), "{failed}");
        assert!(failed.contains("Synchronized 1 min ago"), "{failed}");
    }

    fn zone(name: &str) -> &'static Zone {
        timezone::find(name).unwrap()
    }

    #[test]
    fn dates_are_validated_against_the_calendar() {
        assert_eq!(parse_date("2026-10-01"), Some((2026, 10, 1)));
        assert_eq!(parse_date(" 2024-02-29 "), Some((2024, 2, 29)));
        for bad in [
            "2026-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "1969-12-31",
            "2200-01-01",
            "2026-10",
            "2026-10-01-1",
            "yyyy-mm-dd",
            "2026-+1-01",
            "",
        ] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn times_accept_minutes_or_seconds() {
        assert_eq!(parse_time("00:00"), Some(0));
        assert_eq!(parse_time("23:59:59"), Some(86_399));
        assert_eq!(parse_time("9:05"), Some(9 * 3600 + 300));
        for bad in [
            "24:00", "12:60", "12:00:60", "12", "1:2:3:4", "12::00", "123:00", "-1:00",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_time_converts_with_the_zone_offset() {
        // 2026-07-01 12:00 in Paris (CEST, +2) is 10:00 UTC.
        let local = days_from_civil(2026, 7, 1) * 86_400 + 12 * 3600;
        assert_eq!(local_to_utc(zone("Europe/Paris"), local), local - 7200);
        // In winter (CET, +1).
        let local = days_from_civil(2026, 1, 15) * 86_400 + 12 * 3600;
        assert_eq!(local_to_utc(zone("Europe/Paris"), local), local - 3600);
        assert_eq!(local_to_utc(zone("UTC"), local), local);
        assert_eq!(
            local_to_utc(zone("America/New_York"), local),
            local + 5 * 3600
        );
    }

    #[test]
    fn typed_instant_round_trips_through_fields() {
        let paris = zone("Europe/Paris");
        let unix = typed_instant(paris, "2026-10-01", "14:03:07").unwrap();
        let now = Now {
            unix,
            offset: timezone::local(paris, unix).offset,
            zone: paris.name.into(),
        };
        assert_eq!(
            fields(&now),
            ("2026-10-01".to_owned(), "14:03:07".to_owned())
        );
        assert_eq!(
            summary(&now),
            "Thu 01 Oct 2026  14:03:07  (Europe/Paris, UTC+02:00)"
        );
        assert!(typed_instant(paris, "2026-02-30", "10:00").is_err());
        assert!(typed_instant(paris, "2026-02-03", "25:00").is_err());
    }

    #[test]
    fn offsets_format_with_sign_and_minutes() {
        assert_eq!(offset_text(0), "UTC+00:00");
        assert_eq!(offset_text(19_800), "UTC+05:30");
        assert_eq!(offset_text(-18_000), "UTC-05:00");
    }

    #[test]
    fn clock_format_defaults_and_round_trips() {
        let store = MemStore::new();
        assert_eq!(clock_format(&store), ClockFormat::default());
        set_clock24(&store, false).unwrap();
        set_show_seconds(&store, true).unwrap();
        assert_eq!(
            clock_format(&store),
            ClockFormat {
                hour24: false,
                seconds: true
            }
        );
        // A value of the wrong kind falls back to the default.
        store
            .set(timezone::CLOCK24_KEY, Value::Str("no".into()))
            .unwrap();
        assert!(clock_format(&store).hour24);
    }

    #[test]
    fn every_zone_has_an_index() {
        for (i, zone) in ZONES.iter().enumerate() {
            assert_eq!(zone_index(zone.name), Some(i));
        }
        assert_eq!(zone_index("Mars/Olympus"), None);
    }
}
