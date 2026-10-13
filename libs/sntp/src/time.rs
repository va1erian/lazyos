//! NTP timestamps: 32.32 fixed point seconds since 1900-01-01, which wrap in
//! 2036. A timestamp alone does not say which 136-year era it is in, so
//! [`Timestamp::to_unix_ns`] takes the local clock as a pivot and picks the
//! era that puts the result nearest to it.

use crate::NS_PER_SEC;

/// Seconds from 1900-01-01 (the NTP epoch) to 1970-01-01 (the Unix epoch).
pub const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

/// Seconds in one era (2^32).
const ERA_SECS: i128 = 1 << 32;

/// An NTP timestamp: seconds in the high 32 bits, fraction in the low 32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const ZERO: Timestamp = Timestamp(0);

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn seconds(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub fn fraction(self) -> u32 {
        self.0 as u32
    }

    /// Unix nanoseconds, in the era nearest `pivot_ns` (Unix nanoseconds of
    /// the local clock). Saturates at the `i64` range.
    pub fn to_unix_ns(self, pivot_ns: i64) -> i64 {
        let era0 = i128::from(self.seconds()) - i128::from(NTP_UNIX_OFFSET);
        let fraction_ns = ((u128::from(self.fraction()) * 1_000_000_000) >> 32) as i128;
        let pivot = i128::from(pivot_ns.div_euclid(NS_PER_SEC));
        // The era whose copy of this second is closest to the pivot.
        let era = (pivot - era0 + ERA_SECS / 2).div_euclid(ERA_SECS);
        let ns = (era0 + era * ERA_SECS) * i128::from(NS_PER_SEC) + fraction_ns;
        ns.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// The timestamp of a Unix instant (the era is dropped, as on the wire).
    pub fn from_unix_ns(ns: i64) -> Timestamp {
        let seconds = i128::from(ns.div_euclid(NS_PER_SEC)) + i128::from(NTP_UNIX_OFFSET);
        let fraction = (u128::from(ns.rem_euclid(NS_PER_SEC) as u64) << 32) / 1_000_000_000;
        Timestamp((u64::from(seconds as u32) << 32) | fraction as u64)
    }

    pub fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub fn from_be_bytes(bytes: [u8; 8]) -> Timestamp {
        Timestamp(u64::from_be_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-13T00:00:00Z.
    const NOW: i64 = 1_791_849_600 * NS_PER_SEC;

    #[test]
    fn unix_epoch_is_the_well_known_ntp_second() {
        let t = Timestamp::from_unix_ns(0);
        assert_eq!(t.seconds(), 2_208_988_800);
        assert_eq!(t.fraction(), 0);
        assert_eq!(t.to_unix_ns(0), 0);
    }

    #[test]
    fn round_trip_is_within_a_nanosecond() {
        for ns in [
            NOW,
            NOW + 1,
            NOW + 499_999_999,
            NOW + 999_999_999,
            0,
            123_456_789,
        ] {
            let back = Timestamp::from_unix_ns(ns).to_unix_ns(ns);
            assert!((back - ns).abs() <= 1, "{ns} -> {back}");
        }
    }

    #[test]
    fn era_is_chosen_by_the_pivot() {
        // 2036-02-07T06:28:16Z is the rollover: NTP seconds 0 again.
        let rollover = (1i64 << 32) - NTP_UNIX_OFFSET;
        let just_after = Timestamp(5 << 32);
        let pivot = (rollover + 10) * NS_PER_SEC;
        assert_eq!(just_after.to_unix_ns(pivot), (rollover + 5) * NS_PER_SEC);
        // The same bits are era 0 (1900) only for a clock that old.
        let ancient = -NTP_UNIX_OFFSET * NS_PER_SEC;
        assert_eq!(
            just_after.to_unix_ns(ancient),
            (5 - NTP_UNIX_OFFSET) * NS_PER_SEC
        );
        // A clock just before the rollover and a server just after it.
        let before = (rollover - 3) * NS_PER_SEC;
        assert_eq!(just_after.to_unix_ns(before), (rollover + 5) * NS_PER_SEC);
    }

    #[test]
    fn a_stale_clock_still_lands_in_the_right_era_within_68_years() {
        let server = Timestamp::from_unix_ns(NOW);
        // A dead RTC reporting 2000-01-01.
        let rtc = 946_684_800 * NS_PER_SEC;
        assert_eq!(server.to_unix_ns(rtc) / NS_PER_SEC, NOW / NS_PER_SEC);
    }

    #[test]
    fn extremes_do_not_panic() {
        for pivot in [i64::MIN, i64::MAX, 0] {
            for bits in [0u64, u64::MAX, 1 << 63] {
                let _ = Timestamp(bits).to_unix_ns(pivot);
            }
        }
        let _ = Timestamp::from_unix_ns(i64::MIN);
        let _ = Timestamp::from_unix_ns(i64::MAX);
    }
}
